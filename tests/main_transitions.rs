//! Main's transitions: Merge moves the layers both sides have from where
//! they are to where they will be — the rest fading — and ends on the new
//! side exactly; Wipe, Slide and Iris show the new picture where they say.

use std::sync::mpsc;

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand};
use varda::mixer::{MainSource, MainTransition};
use varda::params::ParamValue::Float;

mod common;

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn id(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected an id, got {other:?}"),
    }
}

fn f16(bits: u16) -> f32 {
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f32::from(bits & 0x3ff);
    let mag = if exp == 0 {
        frac * 2f32.powi(-24)
    } else {
        (1.0 + frac / 1024.0) * 2f32.powi(exp - 15)
    };
    if bits >> 15 == 1 {
        -mag
    } else {
        mag
    }
}

/// Every pixel of an `Rgba16Float` texture, row by row.
fn frame_pixels(gpu: &varda::renderer::context::GpuContext, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        texture.size(),
    );
    gpu.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let at = (y * row + x * 8) as usize;
            out.push(std::array::from_fn(|c| {
                f16(u16::from_le_bytes([data[at + c * 2], data[at + c * 2 + 1]]))
            }));
        }
    }
    out
}

/// A show on Main, every channel emptied, with `n` channels: their UUIDs.
fn empty_show(app: &mut VardaApp, n: usize) -> Vec<String> {
    send_cmd(
        app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    for deck in state.mixer.channels.iter().flat_map(|c| &c.decks) {
        send_cmd(
            app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    while app.build_engine_state().mixer.channels.len() < n {
        send_cmd(app, EngineCommand::AddChannel);
    }
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|c| c.uuid.clone())
        .collect()
}

/// A picture of four coloured quarters: red, green over yellow, cyan.
fn quarters(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("quarters.png");
    image::RgbaImage::from_fn(64, 36, |x, y| match (x < 32, y < 18) {
        (true, true) => image::Rgba([255, 0, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, 255]),
        (true, false) => image::Rgba([255, 255, 0, 255]),
        (false, false) => image::Rgba([0, 255, 255, 255]),
    })
    .save(&path)
    .unwrap();
    path
}

/// The effect named `name` of the layer `deck`.
fn effect(app: &VardaApp, deck: &str, name: &str) -> String {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .unwrap()
        .effects
        .iter()
        .find(|e| e.name == name)
        .unwrap()
        .uuid
        .clone()
}

fn set(app: &mut VardaApp, effect: &str, name: &str, value: varda::params::ParamValue) {
    send_cmd(
        app,
        EngineCommand::SetEffectParam {
            effect_uuid: effect.into(),
            name: name.into(),
            value,
        },
    );
}

/// `channel` on Main at once, then `next` taken with `transition` over the
/// longest time, stopped at `progress`: Main's picture then.
fn taken_at(
    app: &mut VardaApp,
    channel: &str,
    next: &str,
    transition: MainTransition,
    progress: f32,
) -> (Vec<[f32; 4]>, u32, u32) {
    taken_from_at(
        app,
        MainSource::Channel(channel.into()),
        MainSource::Channel(next.into()),
        transition,
        progress,
    )
}

/// As [`taken_at`] for any two sources. The take is held at `progress`
/// however long the frames take — its time made so long a frame moves it
/// not at all — and is still running when the picture is read.
fn taken_from_at(
    app: &mut VardaApp,
    from: MainSource,
    next: MainSource,
    transition: MainTransition,
    progress: f32,
) -> (Vec<[f32; 4]>, u32, u32) {
    send_cmd(
        app,
        EngineCommand::TakeToMain {
            source: from,
            fade: false,
        },
    );
    send_cmd(app, EngineCommand::SetMainTransition { transition });
    send_cmd(app, EngineCommand::SetMainFadeTime { secs: 10.0 });
    app.render_mixer_frame();
    send_cmd(
        app,
        EngineCommand::TakeToMain {
            source: next,
            fade: true,
        },
    );
    for _ in 0..3 {
        let fade = app.mixer_mut().main_mut().fade.as_mut().expect("taking");
        fade.progress = progress;
        fade.duration = 1.0e9;
        app.render_mixer_frame();
    }
    assert!(app.mixer_ref().main().fade.is_some(), "still taking when read");
    let texture = app.mixer_ref().composite_texture();
    let (w, h) = (texture.width(), texture.height());
    (frame_pixels(app.gpu_context(), texture), w, h)
}

fn at(pixels: &[[f32; 4]], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    pixels[((y * h as f32) as u32 * w + (x * w as f32) as u32) as usize]
}

fn cyan(p: [f32; 4]) -> bool {
    p[0] < 0.05 && p[1] > 0.9 && p[2] > 0.9
}

/// Two inputs with the same picture: small in the top-left quarter in one,
/// the whole frame in the other. Their UUIDs, and the small one's layer.
fn small_then_whole(app: &mut VardaApp, dir: &std::path::Path) -> (String, String, String) {
    let channels = empty_show(app, 2);
    let picture = quarters(dir);
    let mut layers = Vec::new();
    for channel in &channels {
        layers.push(id(send_cmd(
            app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        )));
    }
    let transform = effect(app, &layers[0], "transform");
    set(app, &transform, "scale", Float(0.5));
    set(app, &transform, "pos_x", Float(-0.25));
    set(app, &transform, "pos_y", Float(-0.25));
    (channels[0].clone(), channels[1].clone(), layers[0].clone())
}

/// Halfway through a merge, the picture both inputs show is halfway between
/// its two places and sizes — whole, not two faded copies — and nothing is
/// outside it; a dissolve would show the new input faintly everywhere.
#[test]
fn a_merge_moves_and_resizes_what_both_inputs_show() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, whole, _) = small_then_whole(&mut app, dir.path());
    let (pixels, w, h) = taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.5);
    // Halfway: the top-left three quarters of the frame each way.
    assert!(cyan(at(&pixels, w, h, 0.6, 0.6)), "its last quarter, whole: {:?}", at(&pixels, w, h, 0.6, 0.6));
    let within = at(&pixels, w, h, 0.3, 0.3);
    assert!(within[3] > 0.98, "opaque, not two halves over each other: {within:?}");
    let outside = at(&pixels, w, h, 0.85, 0.85);
    assert!(outside[3] < 0.02 && outside[1] < 0.02, "nothing outside it: {outside:?}");

    let (pixels, w, h) = taken_at(&mut app, &small, &whole, MainTransition::Fade, 0.5);
    let faint = at(&pixels, w, h, 0.85, 0.85);
    assert!(faint[1] > 0.3 && faint[1] < 0.7, "a dissolve, by contrast: {faint:?}");
}

/// Two inputs each showing the same channel through a tap, at two places —
/// the operator's case: one tapped camera channel, centred in one input,
/// to the left in the next. Halfway through a merge the picture is whole
/// and halfway between its places, not faded out in one and in at the
/// other.
#[test]
fn a_merge_moves_a_tapped_channel_between_its_places() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 3);
    let (source, centred, left) = (&channels[0], &channels[1], &channels[2]);
    send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: source.clone(),
            path: quarters(dir.path()),
        },
    );
    for (channel, pos_x) in [(centred, 0.0), (left, -0.661)] {
        let tap = id(send_cmd(
            &mut app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.clone(),
                source: varda::scene::TapSourceConfig::Channel {
                    uuid: source.clone(),
                },
            },
        ));
        let transform = effect(&app, &tap, "transform");
        set(&mut app, &transform, "scale", Float(0.659));
        set(&mut app, &transform, "pos_x", Float(pos_x));
        set(&mut app, &transform, "pos_y", Float(0.0));
    }
    let (pixels, w, h) = taken_at(&mut app, centred, left, MainTransition::Merge, 0.5);
    // Halfway the picture spans -0.16..0.50 across: whole at 0.45, nothing
    // at 0.75 — where a dissolve would leave the centred one half there.
    let inside = at(&pixels, w, h, 0.45, 0.5);
    assert!(inside[3] > 0.98, "whole where it is halfway: {inside:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// The operator's own steps: a channel with a picture under a tap of
/// another channel, duplicated, and the copy's tap moved to the left. A
/// merge from the original to the copy moves the tapped picture — both
/// layers matched — rather than fading it out in one place and in at the
/// other.
#[test]
fn a_merge_moves_a_tap_into_a_duplicated_channel() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let (source, original) = (&channels[0], &channels[1]);
    send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: source.clone(),
            path: quarters(dir.path()),
        },
    );
    let tap = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: original.clone(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: source.clone(),
            },
        },
    ));
    let transform = effect(&app, &tap, "transform");
    set(&mut app, &transform, "scale", Float(0.659));
    send_cmd(
        &mut app,
        EngineCommand::Duplicate {
            source: varda::engine::ClipboardSource::Channel(original.clone()),
        },
    );
    let state = app.build_engine_state();
    let copy = state
        .mixer
        .channels
        .iter()
        .find(|c| &c.uuid != source && &c.uuid != original)
        .expect("the copy");
    let copy_uuid = copy.uuid.clone();
    let copy_tap = copy.decks[0].uuid.clone();
    let copy_transform = effect(&app, &copy_tap, "transform");
    set(&mut app, &copy_transform, "pos_x", Float(-0.661));
    let (pixels, w, h) = taken_at(&mut app, original, &copy_uuid, MainTransition::Merge, 0.5);
    let inside = at(&pixels, w, h, 0.45, 0.5);
    assert!(inside[3] > 0.98, "whole where it is halfway: {inside:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// The same moving picture in two inputs — a generator, or a clip — small
/// in the middle of one, to the left in the next: the operator found a merge
/// moved a still but faded a clip or a generator. `add` puts the layer in a
/// channel and gives its UUID.
fn merge_moving_layer(
    app: &mut VardaApp,
    add: impl Fn(&mut VardaApp, &str) -> String,
) -> (Vec<[f32; 4]>, u32, u32) {
    let channels = empty_show(app, 2);
    for (channel, pos_x) in [(&channels[0], 0.0), (&channels[1], -0.661)] {
        let layer = add(app, channel);
        let transform = effect(app, &layer, "transform");
        set(app, &transform, "scale", Float(0.659));
        set(app, &transform, "pos_x", Float(pos_x));
        set(app, &transform, "pos_y", Float(0.0));
    }
    taken_at(app, &channels[0], &channels[1], MainTransition::Merge, 0.5)
}

#[test]
fn a_merge_moves_a_generator_between_its_places() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (pixels, w, h) = merge_moving_layer(&mut app, |app, channel| {
        id(send_cmd(
            app,
            EngineCommand::AddDeck {
                channel_uuid: channel.into(),
                shader_name: "checkerboard".into(),
            },
        ))
    });
    let inside = at(&pixels, w, h, 0.45, 0.5);
    assert!(inside[3] > 0.98, "whole where it is halfway: {inside:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// An animated generator — plasma moves with time — merged with frames
/// drawn over real time, as on a desk: still moved, not faded.
#[test]
fn a_merge_moves_an_animated_generator_over_time() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    for (channel, pos_x) in [(&channels[0], 0.0), (&channels[1], -0.661)] {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddDeck {
                channel_uuid: channel.clone(),
                shader_name: "plasma".into(),
            },
        ));
        let transform = effect(&app, &layer, "transform");
        set(&mut app, &transform, "scale", Float(0.659));
        set(&mut app, &transform, "pos_x", Float(pos_x));
        set(&mut app, &transform, "pos_y", Float(0.0));
    }
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[0].clone()),
            fade: false,
        },
    );
    send_cmd(&mut app, EngineCommand::SetMainTransition { transition: MainTransition::Merge });
    send_cmd(&mut app, EngineCommand::SetMainFadeTime { secs: 10.0 });
    for _ in 0..10 {
        app.render_mixer_frame();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[1].clone()),
            fade: true,
        },
    );
    for _ in 0..30 {
        let fade = app.mixer_mut().main_mut().fade.as_mut().expect("taking");
        fade.progress = 0.5;
        fade.duration = 1.0e9;
        app.render_mixer_frame();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    let texture = app.mixer_ref().composite_texture();
    let (w, h) = (texture.width(), texture.height());
    let pixels = frame_pixels(app.gpu_context(), texture);
    let inside = at(&pixels, w, h, 0.45, 0.5);
    assert!(inside[3] > 0.98, "whole where it is halfway: {inside:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// The first report exactly: two inputs each tapping one channel that
/// plays a clip, at two places.
#[test]
fn a_merge_moves_a_tapped_clip_channel_between_its_places() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=64x36:r=25:d=4"])
        .args(["-c:v", "mpeg4", clip.to_str().unwrap()])
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("ffmpeg not available — skipping");
        return;
    }
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 3);
    let (source, centred, left) = (&channels[0], &channels[1], &channels[2]);
    send_cmd(
        &mut app,
        EngineCommand::AddVideoDeck {
            channel_uuid: source.clone(),
            path: clip.clone(),
        },
    );
    for (channel, pos_x) in [(centred, 0.0), (left, -0.661)] {
        let tap = id(send_cmd(
            &mut app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.clone(),
                source: varda::scene::TapSourceConfig::Channel {
                    uuid: source.clone(),
                },
            },
        ));
        let transform = effect(&app, &tap, "transform");
        set(&mut app, &transform, "scale", Float(0.659));
        set(&mut app, &transform, "pos_x", Float(pos_x));
        set(&mut app, &transform, "pos_y", Float(0.0));
    }
    // Let the clip load and show a frame before the take.
    for _ in 0..20 {
        app.render_mixer_frame();
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    let (pixels, w, h) = taken_at(&mut app, centred, left, MainTransition::Merge, 0.5);
    let inside = at(&pixels, w, h, 0.45, 0.5);
    assert!(inside[3] > 0.98, "whole where it is halfway: {inside:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// Two copies of an animated generator — each running its own clock, the
/// second made later — one whole in frame, the other mostly off the left
/// edge. Halfway, the part only the first holds is the first's picture at
/// full strength — not half faded out with the second, which has nothing
/// of it — while where both hold it the two cross over, so the take ends on
/// the second's own frame.
#[test]
fn a_merge_keeps_the_part_only_one_copy_holds_whole() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let add = |app: &mut VardaApp, channel: &str, pos_x: f32| {
        let layer = id(send_cmd(
            app,
            EngineCommand::AddDeck {
                channel_uuid: channel.into(),
                shader_name: "plasma".into(),
            },
        ));
        let transform = effect(app, &layer, "transform");
        set(app, &transform, "scale", Float(0.659));
        set(app, &transform, "pos_x", Float(pos_x));
        set(app, &transform, "pos_y", Float(0.0));
    };
    add(&mut app, &channels[0], 0.0);
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[0].clone()),
            fade: false,
        },
    );
    // The first copy's clock runs on before the second exists.
    for _ in 0..40 {
        app.render_mixer_frame();
    }
    add(&mut app, &channels[1], -0.661);
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    let deck_pixel = |app: &VardaApp, channel: usize, x: f32, y: f32| {
        let deck = &app.mixer_ref().channels()[channel].decks[0].deck;
        let texture = &deck.texture;
        let (dw, dh) = (texture.width(), texture.height());
        at(&frame_pixels(app.gpu_context(), texture), dw, dh, x, y)
    };
    // Halfway, the picture spans -0.16..0.50 across. The second copy holds
    // only its last 0.26 (what of it is inside its frame); 0.3 of the way
    // across — at 0.04 on the screen, at 0.37 in the first copy's frame — is
    // the first copy's alone.
    let merged = at(&pixels, w, h, 0.0374, 0.5);
    let old = deck_pixel(&app, 0, 0.3682, 0.5);
    let off = |p: [f32; 4], q: [f32; 4]| p.iter().zip(&q).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(off(merged, old) < 0.03, "the first copy's, whole: {merged:?} vs {old:?}");
}

/// The operator's case exactly: a picture whole in one input, small and
/// mostly off the left edge in the next (scale 0.649 at -0.731), slid out
/// with a merge. Halfway the picture is solid everywhere it is — the part
/// on its way out of the frame too, which faded out as the take went on.
#[test]
fn a_merge_slides_a_picture_off_the_edge_as_one_picture() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    let mut layers = Vec::new();
    for channel in &channels {
        layers.push(id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        )));
    }
    let transform = effect(&app, &layers[1], "transform");
    set(&mut app, &transform, "scale", Float(0.6489));
    set(&mut app, &transform, "pos_x", Float(-0.7313));
    set(&mut app, &transform, "pos_y", Float(0.0));
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    // Halfway the picture spans -0.28..0.55 across, 0.09..0.91 down. At
    // 0.2 across it is the part bound for outside the frame.
    let leaving = at(&pixels, w, h, 0.2, 0.5);
    assert!(leaving[3] > 0.98, "solid where it leaves the frame: {leaving:?}");
    let ahead = at(&pixels, w, h, 0.5, 0.5);
    assert!(ahead[3] > 0.98, "and where it stays: {ahead:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// A picture half off the left edge in one input and half off the right in
/// the next: each input's own picture holds only one half. Halfway through
/// the merge — the picture filling the frame — both halves are there.
#[test]
fn a_merge_puts_together_a_picture_each_side_holds_half_of() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    for (channel, pos_x) in [(&channels[0], -0.5), (&channels[1], 0.5)] {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        let transform = effect(&app, &layer, "transform");
        set(&mut app, &transform, "pos_x", Float(pos_x));
    }
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    for x in [0.25, 0.75] {
        let p = at(&pixels, w, h, x, 0.75);
        assert!(p[3] > 0.98, "solid at {x} across: {p:?}");
    }
    assert!(cyan(at(&pixels, w, h, 0.75, 0.75)), "its own bottom-right quarter where it belongs");
}

/// Layers at half strength, the picture sliding off the edge: halfway it is
/// at half strength everywhere — where the other side fills a gap as where
/// both cross over — not stronger where two pictures overlap.
#[test]
fn a_merge_at_half_strength_stays_at_half_strength() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    let mut layers = Vec::new();
    for channel in &channels {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        send_cmd(
            &mut app,
            EngineCommand::SetDeckOpacity {
                deck_uuid: layer.clone(),
                opacity: 0.5,
            },
        );
        layers.push(layer);
    }
    let transform = effect(&app, &layers[1], "transform");
    set(&mut app, &transform, "scale", Float(0.6489));
    set(&mut app, &transform, "pos_x", Float(-0.7313));
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    for x in [0.2, 0.5] {
        let p = at(&pixels, w, h, x, 0.5);
        assert!((p[3] - 0.5).abs() < 0.03, "half strength at {x} across: {p:?}");
    }
}

/// A turned picture's place is the whole frame on both sides: nothing to
/// fill, each side its own, and the merge ends on the new one exactly —
/// not with both turned pictures shown throughout.
#[test]
fn a_merge_of_turned_copies_ends_on_the_new_picture() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    for (channel, pos_x) in [(&channels[0], -0.25), (&channels[1], 0.25)] {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        let transform = effect(&app, &layer, "transform");
        set(&mut app, &transform, "scale", Float(0.3));
        set(&mut app, &transform, "rotation", Float(0.2));
        set(&mut app, &transform, "pos_x", Float(pos_x));
    }
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.999);
    let channel = app.mixer_ref().channels().iter().position(|c| c.uuid() == channels[1]).unwrap();
    let new = frame_pixels(app.gpu_context(), &app.mixer_ref().channels()[channel].composite_texture);
    let off = |p: &[f32; 4], q: &[f32; 4]| p.iter().zip(q).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    let differing = pixels.iter().zip(&new).filter(|(p, q)| off(p, q) > 0.03).count();
    assert!(differing * 200 < pixels.len(), "{differing} of {} pixels differ", w * h);
}

/// Copies coloured apart are two pictures: one's gap is not filled with the
/// other's colours. The picture sliding off the edge then fades where the
/// leaving copy has nothing — as a merge of two pictures does.
#[test]
fn a_merge_does_not_fill_from_a_copy_coloured_apart() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    let mut layers = Vec::new();
    for channel in &channels {
        layers.push(id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        )));
    }
    let brighter = id(send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: varda::engine::EffectTarget::Deck(layers[1].clone()),
            shader_name: "brightness_contrast".into(),
        },
    ));
    set(&mut app, &brighter, "brightness", Float(0.3));
    let transform = effect(&app, &layers[1], "transform");
    set(&mut app, &transform, "scale", Float(0.6489));
    set(&mut app, &transform, "pos_x", Float(-0.7313));
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    let leaving = at(&pixels, w, h, 0.2, 0.5);
    assert!((leaving[3] - 0.5).abs() < 0.05, "half there, from the first alone: {leaving:?}");
}

/// Identical copies wholly in frame at half strength: nothing is filled —
/// their edge pixels too stay at half strength, not stronger, all through
/// the take.
#[test]
fn copies_wholly_in_frame_keep_their_edges_at_half_strength() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    for channel in &channels {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        send_cmd(
            &mut app,
            EngineCommand::SetDeckOpacity {
                deck_uuid: layer.clone(),
                opacity: 0.5,
            },
        );
        let transform = effect(&app, &layer, "transform");
        set(&mut app, &transform, "scale", Float(0.659));
    }
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    // The first column whose centre is inside the picture (its left edge at
    // 0.1705 of the width).
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let column = (0.1705 * w as f32 - 0.5).ceil() as u32;
    let edge = pixels[((h / 2) * w + column) as usize];
    assert!((edge[3] - 0.5).abs() < 0.03, "half strength at its edge: {edge:?}");
}

/// A channel taken into a plain tap of it — small, mostly off the left
/// edge: the channel's own picture fills what the tap's has not, so the
/// part leaving the frame stays whole.
#[test]
fn a_channel_slid_off_the_edge_into_a_tap_of_it_stays_whole() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let (source, tapping) = (&channels[0], &channels[1]);
    send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: source.clone(),
            path: quarters(dir.path()),
        },
    );
    let tap = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: tapping.clone(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: source.clone(),
            },
        },
    ));
    let transform = effect(&app, &tap, "transform");
    set(&mut app, &transform, "scale", Float(0.6489));
    set(&mut app, &transform, "pos_x", Float(-0.7313));
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let (pixels, w, h) = taken_at(&mut app, source, tapping, MainTransition::Merge, 0.5);
    let leaving = at(&pixels, w, h, 0.2, 0.5);
    assert!(leaving[3] > 0.98, "solid where it leaves the frame: {leaving:?}");
}

/// The `n`th (from 0) effect named `name` of the layer `deck`.
fn nth_effect(app: &VardaApp, deck: &str, name: &str, n: usize) -> String {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .unwrap()
        .effects
        .iter()
        .filter(|e| e.name == name)
        .nth(n)
        .unwrap()
        .uuid
        .clone()
}

/// A crop after a Transform keeps another part of the picture when that
/// Transform moves it: copies moved apart before their crop are two
/// pictures, and one's gap is not filled with the other's. Each layer:
/// crop, Transform, crop keeping the right half, Transform; the first
/// Transforms at 0 and 0.25 across, the last at 0 and -0.75.
#[test]
fn copies_cropped_after_their_placement_are_not_filled_from_each_other() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    for (channel, first, last) in [(&channels[0], 0.0, 0.0), (&channels[1], 0.25, -0.75)] {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        for shader in ["crop", "transform"] {
            send_cmd(
                &mut app,
                EngineCommand::AddEffect {
                    target: varda::engine::EffectTarget::Deck(layer.clone()),
                    shader_name: shader.into(),
                },
            );
        }
        let first_transform = nth_effect(&app, &layer, "transform", 0);
        let second_crop = nth_effect(&app, &layer, "crop", 1);
        let last_transform = nth_effect(&app, &layer, "transform", 1);
        set(&mut app, &first_transform, "pos_x", Float(first));
        set(&mut app, &second_crop, "crop_left", Float(0.5));
        set(&mut app, &last_transform, "pos_x", Float(last));
    }
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    // Halfway the half spans 0.125..0.625; the second has nothing of its
    // left half (off the left edge in its own frame): at 0.25, the first's
    // alone, fading.
    let gap = at(&pixels, w, h, 0.25, 0.5);
    assert!(gap[3] < 0.9, "not filled from the other: {gap:?}");
}

/// A pixel whose centre falls exactly on a picture's right edge in its own
/// frame is its own picture's — the blit draws source 1 as inside — and not
/// the gap's too. Full-size copies at half strength, 0 then 0.4921875
/// across: halfway, column 1447 of 1920 lies on that edge.
#[test]
fn a_pixel_on_the_edge_of_a_gap_is_drawn_once() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    for (channel, pos_x) in [(&channels[0], 0.0), (&channels[1], 0.492_187_5)] {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        send_cmd(
            &mut app,
            EngineCommand::SetDeckOpacity {
                deck_uuid: layer.clone(),
                opacity: 0.5,
            },
        );
        let transform = effect(&app, &layer, "transform");
        set(&mut app, &transform, "pos_x", Float(pos_x));
    }
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    if w != 1920 {
        eprintln!("frame {w} wide, not 1920 — the edge falls elsewhere; skipping");
        return;
    }
    let edge = pixels[((h / 2) * w + 1447) as usize];
    assert!((edge[3] - 0.5).abs() < 0.03, "half strength on the edge: {edge:?}");
}

/// A picture wholly off the left edge in one input: its gap is all of it,
/// its own right edge included. Halfway into the same picture whole, at
/// half strength, column 727 of 1920 lies on that edge.
#[test]
fn a_picture_wholly_off_the_edge_is_filled_to_its_own_edge() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    for (channel, scale, pos_x) in [(&channels[0], 0.5, -0.992_187_5), (&channels[1], 1.0, 0.0)] {
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        ));
        send_cmd(
            &mut app,
            EngineCommand::SetDeckOpacity {
                deck_uuid: layer.clone(),
                opacity: 0.5,
            },
        );
        let transform = effect(&app, &layer, "transform");
        set(&mut app, &transform, "scale", Float(scale));
        set(&mut app, &transform, "pos_x", Float(pos_x));
    }
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    if w != 1920 {
        eprintln!("frame {w} wide, not 1920 — the edge falls elsewhere; skipping");
        return;
    }
    let edge = pixels[((h / 2) * w + 727) as usize];
    assert!((edge[3] - 0.5).abs() < 0.03, "half strength on its edge: {edge:?}");
}

/// A tap with two Transforms is not the channel's own picture: the first
/// (twice the size) keeps only the middle, which the second shows smaller.
/// Taken into it from the channel, its gap is not filled with the parts
/// the first Transform left out.
#[test]
fn a_tap_through_two_transforms_is_not_filled_from_its_channel() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let (source, tapping) = (&channels[0], &channels[1]);
    send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: source.clone(),
            path: quarters(dir.path()),
        },
    );
    let tap = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: tapping.clone(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: source.clone(),
            },
        },
    ));
    send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: varda::engine::EffectTarget::Deck(tap.clone()),
            shader_name: "transform".into(),
        },
    );
    let first = nth_effect(&app, &tap, "transform", 0);
    let second = nth_effect(&app, &tap, "transform", 1);
    set(&mut app, &first, "scale", Float(2.0));
    set(&mut app, &second, "scale", Float(0.5));
    set(&mut app, &second, "pos_x", Float(0.25));
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let (pixels, w, h) = taken_at(&mut app, source, tapping, MainTransition::Merge, 0.5);
    let gap = at(&pixels, w, h, 0.925, 0.5);
    assert!(gap[3] < 0.9, "not filled from the channel: {gap:?}");
}

/// Copies cropped apart are two pictures: a merge from the whole picture to
/// its right half, cropped, ends on that half as it is — not the whole
/// picture squeezed into the half's place.
#[test]
fn a_merge_between_copies_cropped_apart_ends_on_the_new_picture() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    let mut layers = Vec::new();
    for channel in &channels {
        layers.push(id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        )));
    }
    let crop = effect(&app, &layers[1], "crop");
    set(&mut app, &crop, "crop_left", Float(0.5));
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.999);
    let channel = app.mixer_ref().channels().iter().position(|c| c.uuid() == channels[1]).unwrap();
    let new = frame_pixels(app.gpu_context(), &app.mixer_ref().channels()[channel].composite_texture);
    let off = |p: &[f32; 4], q: &[f32; 4]| p.iter().zip(q).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    let differing = pixels.iter().zip(&new).filter(|(p, q)| off(p, q) > 0.03).count();
    assert!(differing * 200 < pixels.len(), "{differing} of {} pixels differ", w * h);
}

#[test]
fn a_merge_moves_a_clip_between_its_places() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let clip = dir.path().join("clip.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=s=64x36:r=25:d=4"])
        .args(["-c:v", "mpeg4", clip.to_str().unwrap()])
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("ffmpeg not available — skipping");
        return;
    }
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (pixels, w, h) = merge_moving_layer(&mut app, |app, channel| {
        id(send_cmd(
            app,
            EngineCommand::AddVideoDeck {
                channel_uuid: channel.into(),
                path: clip.clone(),
            },
        ))
    });
    let inside = at(&pixels, w, h, 0.45, 0.5);
    assert!(inside[3] > 0.98, "whole where it is halfway: {inside:?}");
    let behind = at(&pixels, w, h, 0.75, 0.5);
    assert!(behind[3] < 0.02, "gone from where it was: {behind:?}");
}

/// [`quarters`] with its top-right quarter clear.
fn quarters_clear_top_right(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("quarters-clear.png");
    image::RgbaImage::from_fn(64, 36, |x, y| match (x < 32, y < 18) {
        (true, true) => image::Rgba([255, 0, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, 0]),
        (true, false) => image::Rgba([255, 255, 0, 255]),
        (false, false) => image::Rgba([0, 255, 255, 255]),
    })
    .save(&path)
    .unwrap();
    path
}

/// The operator's pair: copies of a picture, one flagged transparent and
/// the other not. A picture keeps its transparency either way, so the two
/// are one picture: slid off the edge with a merge, the part leaving the
/// frame stays whole — and clear where the picture is, not flattened to
/// black.
#[test]
fn copies_of_a_picture_flagged_apart_slide_off_the_edge_as_one() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters_clear_top_right(dir.path());
    let mut layers = Vec::new();
    for channel in &channels {
        layers.push(id(send_cmd(
            &mut app,
            EngineCommand::AddImageDeck {
                channel_uuid: channel.clone(),
                path: picture.clone(),
            },
        )));
    }
    send_cmd(
        &mut app,
        EngineCommand::SetDeckTransparent {
            deck_uuid: layers[0].clone(),
            transparent: true,
        },
    );
    let transform = effect(&app, &layers[1], "transform");
    set(&mut app, &transform, "scale", Float(0.6489));
    set(&mut app, &transform, "pos_x", Float(-0.7313));
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    // Halfway the picture spans -0.28..0.55 across, 0.09..0.91 down: at 0.2
    // across, its right half, bound for outside the frame.
    let solid = at(&pixels, w, h, 0.2, 0.7);
    assert!(solid[3] > 0.98 && cyan(solid), "whole where it leaves the frame: {solid:?}");
    let clear = at(&pixels, w, h, 0.2, 0.3);
    assert!(clear[3] < 0.02, "and clear where it is clear: {clear:?}");
}

/// A channel whose own picture is half transparent — a picture at half
/// strength — taken into a tap of it slid mostly off the left edge. The
/// tap, its transparency turned off, shows the channel over black; the part
/// leaving the frame, filled from the channel's own picture, is flattened
/// so too: as strong as the part the tap holds itself.
#[test]
fn a_tap_flattening_its_channel_is_filled_flattened_too() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let (source, tapping) = (&channels[0], &channels[1]);
    let picture = id(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: source.clone(),
            path: quarters(dir.path()),
        },
    ));
    send_cmd(
        &mut app,
        EngineCommand::SetDeckOpacity {
            deck_uuid: picture,
            opacity: 0.5,
        },
    );
    let tap = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: tapping.clone(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: source.clone(),
            },
        },
    ));
    send_cmd(
        &mut app,
        EngineCommand::SetDeckTransparent {
            deck_uuid: tap.clone(),
            transparent: false,
        },
    );
    let transform = effect(&app, &tap, "transform");
    set(&mut app, &transform, "scale", Float(0.6489));
    set(&mut app, &transform, "pos_x", Float(-0.7313));
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let (pixels, w, h) = taken_at(&mut app, source, tapping, MainTransition::Merge, 0.5);
    // Halfway the picture spans -0.28..0.55 across: at 0.2 the part leaving
    // the frame, at 0.5 the part the tap holds — both its bottom right.
    let (leaving, held) = (at(&pixels, w, h, 0.2, 0.7), at(&pixels, w, h, 0.5, 0.7));
    let off = leaving.iter().zip(&held).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    assert!(off < 0.03, "the same where it leaves as where it stays: {leaving:?}, {held:?}");
    assert!((held[3] - 0.75).abs() < 0.03, "half the channel's, half the tap's: {held:?}");
}

/// A channel whose own picture is half transparent, and two inputs tapping
/// it — whole in the first, mostly off the left edge in the second —
/// flagged transparent or not as given. Halfway through a merge from the
/// first to the second: the pixel where the picture leaves the frame, and
/// one the second's own picture holds, both of its bottom-right quarter.
fn taps_flagged(whole: bool, leaving: bool) -> Option<([f32; 4], [f32; 4])> {
    let gpu = common::headless_gpu()?;
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 3);
    let (source, first, second) = (&channels[0], &channels[1], &channels[2]);
    let picture = id(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: source.clone(),
            path: quarters(dir.path()),
        },
    ));
    send_cmd(
        &mut app,
        EngineCommand::SetDeckOpacity {
            deck_uuid: picture,
            opacity: 0.5,
        },
    );
    for (channel, transparent, slid) in [(first, whole, false), (second, leaving, true)] {
        let tap = id(send_cmd(
            &mut app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.clone(),
                source: varda::scene::TapSourceConfig::Channel {
                    uuid: source.clone(),
                },
            },
        ));
        send_cmd(
            &mut app,
            EngineCommand::SetDeckTransparent {
                deck_uuid: tap.clone(),
                transparent,
            },
        );
        if slid {
            let transform = effect(&app, &tap, "transform");
            set(&mut app, &transform, "scale", Float(0.6489));
            set(&mut app, &transform, "pos_x", Float(-0.7313));
        }
    }
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let (pixels, w, h) = taken_at(&mut app, first, second, MainTransition::Merge, 0.5);
    Some((at(&pixels, w, h, 0.2, 0.7), at(&pixels, w, h, 0.5, 0.7)))
}

/// The operator's pair the other way round too: taps of one channel, one
/// keeping its transparency and the other flattening the channel over
/// black. Each tap's gap is filled from the channel itself, as that tap
/// shows it: the part leaving the frame is as strong as the part the
/// second holds, whichever way the two are flagged.
#[test]
fn taps_of_a_channel_flagged_apart_move_whole_either_way() {
    for (whole, leaving) in [(true, false), (false, true)] {
        let Some((at_edge, held)) = taps_flagged(whole, leaving) else {
            return;
        };
        let off = at_edge.iter().zip(&held).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(off < 0.03, "{whole}/{leaving}: the same where it leaves as where it stays: {at_edge:?}, {held:?}");
        assert!((held[3] - 0.75).abs() < 0.03, "{whole}/{leaving}: half from each tap: {held:?}");
    }
}

/// A tap round a loop shows its channel's picture of the frame before (see
/// `Mixer::prepare_taps`): its gap is filled from that same picture, not
/// this frame's. Two channels tap a third, which taps the first — so the
/// first, made first, renders before the third and shows it a frame late.
/// The first's tap is slid mostly off the left edge, the second's whole.
/// Held halfway from the second to the first, the third turns from red to
/// green: on that very frame the first's picture is the same where it
/// leaves the frame as where it holds it. Either way the taps are drawn:
/// as they come, keeping the channel's transparency — one picture of the
/// source moving between the two framings, each side's from its own frame
/// — or flattening it over black, each side's own, its gap filled. With
/// the first's tap faint, each frame shows at its own side's strength: the
/// second's on time, at full strength, the first's faintly.
#[test]
fn a_tap_showing_the_frame_before_is_filled_from_that_frame() {
    for transparent in [true, false] {
        for faint in [false, true] {
            tap_showing_the_frame_before(transparent, faint);
        }
    }
}

fn tap_showing_the_frame_before(transparent: bool, faint: bool) {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 3);
    let (late, whole, source) = (&channels[0], &channels[1], &channels[2]);
    let colour = id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: source.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    // The third shows the first, unseen: only the loop matters.
    let back = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: source.clone(),
            source: varda::scene::TapSourceConfig::Channel { uuid: late.clone() },
        },
    ));
    send_cmd(&mut app, EngineCommand::SetDeckMute { deck_uuid: back, mute: true });
    for (channel, slid) in [(late, true), (whole, false)] {
        let tap = id(send_cmd(
            &mut app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.clone(),
                source: varda::scene::TapSourceConfig::Channel { uuid: source.clone() },
            },
        ));
        send_cmd(
            &mut app,
            EngineCommand::SetDeckTransparent {
                deck_uuid: tap.clone(),
                transparent,
            },
        );
        if slid && faint {
            send_cmd(
                &mut app,
                EngineCommand::SetDeckOpacity {
                    deck_uuid: tap.clone(),
                    opacity: 0.1,
                },
            );
        }
        if slid {
            let transform = effect(&app, &tap, "transform");
            set(&mut app, &transform, "scale", Float(0.6489));
            set(&mut app, &transform, "pos_x", Float(-0.7313));
        }
    }
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(whole.clone()),
            fade: false,
        },
    );
    send_cmd(&mut app, EngineCommand::SetMainTransition { transition: MainTransition::Merge });
    send_cmd(&mut app, EngineCommand::SetMainFadeTime { secs: 10.0 });
    app.render_mixer_frame();
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(late.clone()),
            fade: true,
        },
    );
    let keep_halfway = |app: &mut VardaApp| {
        let fade = app.mixer_mut().main_mut().fade.as_mut().expect("taking");
        fade.progress = 0.5;
        fade.duration = 1.0e9;
    };
    for _ in 0..3 {
        keep_halfway(&mut app);
        app.render_mixer_frame();
    }
    send_cmd(
        &mut app,
        EngineCommand::SetDeckColor {
            deck_uuid: colour,
            color: [0.0, 1.0, 0.0, 1.0],
        },
    );
    keep_halfway(&mut app);
    app.render_mixer_frame();
    let texture = app.mixer_ref().composite_texture();
    let (w, h) = (texture.width(), texture.height());
    let pixels = frame_pixels(app.gpu_context(), texture);
    // Halfway the first's picture spans -0.28..0.55 across: at 0.2 the part
    // leaving the frame, at 0.5 the part it holds.
    let (leaving, held) = (at(&pixels, w, h, 0.2, 0.5), at(&pixels, w, h, 0.5, 0.5));
    let off = leaving.iter().zip(&held).map(|(p, q)| (p - q).abs()).fold(0.0, f32::max);
    assert!(
        off < 0.03,
        "transparent {transparent}, faint {faint}: the same where it leaves as where it stays: {leaving:?}, {held:?}"
    );
    if faint {
        assert!(
            held[1] > 0.45 && held[0] < 0.1,
            "transparent {transparent}: the second's green at its full strength, the first's red faint: {held:?}"
        );
    } else {
        assert!(
            held[0] > 0.4 && held[1] > 0.4,
            "transparent {transparent}: the first a frame late, the second on time: {held:?}"
        );
    }
}

/// Copies whose crop has a soft edge, or a border: either draws past the
/// place a merge knows of, so one's gap is not filled from the other's —
/// the part leaving the frame fades, as copies drawn apart do.
#[test]
fn copies_with_a_soft_or_bordered_crop_are_not_filled_from_each_other() {
    for (setting, value) in [
        ("feather", Float(0.05)),
        ("border_mode", varda::params::ParamValue::Bool(true)),
    ] {
        let Some(gpu) = common::headless_gpu() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
        let channels = empty_show(&mut app, 2);
        let picture = quarters(dir.path());
        let mut layers = Vec::new();
        for channel in &channels {
            let layer = id(send_cmd(
                &mut app,
                EngineCommand::AddImageDeck {
                    channel_uuid: channel.clone(),
                    path: picture.clone(),
                },
            ));
            let crop = effect(&app, &layer, "crop");
            set(&mut app, &crop, setting, value);
            layers.push(layer);
        }
        let transform = effect(&app, &layers[1], "transform");
        set(&mut app, &transform, "scale", Float(0.6489));
        set(&mut app, &transform, "pos_x", Float(-0.7313));
        let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
        let leaving = at(&pixels, w, h, 0.2, 0.7);
        assert!((leaving[3] - 0.5).abs() < 0.05, "{setting}: half there, from the first alone: {leaving:?}");
    }
}

/// Three channels: a source showing the quarters picture, and two each
/// showing it through a tap, framed by `first` and `second` (each given the
/// tap's crop and Transform effects). Their UUIDs: source, first, second.
fn framed_taps(
    app: &mut VardaApp,
    dir: &std::path::Path,
    first: impl Fn(&mut VardaApp, &str, &str),
    second: impl Fn(&mut VardaApp, &str, &str),
) -> (String, String, String) {
    framed_taps_of(app, quarters(dir), first, second)
}

/// As [`framed_taps`], the source showing `picture`.
fn framed_taps_of(
    app: &mut VardaApp,
    picture: std::path::PathBuf,
    first: impl Fn(&mut VardaApp, &str, &str),
    second: impl Fn(&mut VardaApp, &str, &str),
) -> (String, String, String) {
    let channels = empty_show(app, 3);
    send_cmd(
        app,
        EngineCommand::AddImageDeck {
            channel_uuid: channels[0].clone(),
            path: picture,
        },
    );
    for (channel, frame) in [(&channels[1], &first as &dyn Fn(&mut VardaApp, &str, &str)), (&channels[2], &second)] {
        let tap = id(send_cmd(
            app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.clone(),
                source: varda::scene::TapSourceConfig::Channel {
                    uuid: channels[0].clone(),
                },
            },
        ));
        let (crop, transform) = (effect(app, &tap, "crop"), effect(app, &tap, "transform"));
        frame(app, &crop, &transform);
    }
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    (channels[0].clone(), channels[1].clone(), channels[2].clone())
}

/// Whether the tap `channel` shows keeps its input's transparency — as a
/// new tap does — or flattens it over black, as a copy of a layout made
/// before Duplicate kept the flag does.
fn keep_transparency(app: &mut VardaApp, channel: &str, keep: bool) {
    let state = app.build_engine_state();
    let tap = state.mixer.channels.iter().find(|c| c.uuid == channel).unwrap().decks[0]
        .uuid
        .clone();
    send_cmd(
        app,
        EngineCommand::SetDeckTransparent {
            deck_uuid: tap,
            transparent: keep,
        },
    );
}

/// A tap cropped to the right half in one input and whole in the next:
/// the merge draws the source once, its crop travelling — halfway it keeps
/// the right three quarters, each part where it belongs — rather than the
/// two pictures crossing over, each squeezed into the box between them.
#[test]
fn a_crop_travels_with_its_picture() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, half, whole) = framed_taps(
        &mut app,
        dir.path(),
        |app, crop, _| set(app, crop, "crop_left", Float(0.5)),
        |_, _, _| {},
    );
    let (pixels, w, h) = taken_at(&mut app, &half, &whole, MainTransition::Merge, 0.5);
    // At 0.3 across, in the top-left quarter: red, the source at its place.
    let kept = at(&pixels, w, h, 0.3, 0.25);
    assert!(kept[0] > 0.9 && kept[1] < 0.1 && kept[3] > 0.98, "the source where it belongs: {kept:?}");
    let left_out = at(&pixels, w, h, 0.1, 0.25);
    assert!(left_out[3] < 0.02, "not yet kept: {left_out:?}");
}

/// The operator's bubble: a tap of the camera's channel cropped round with
/// a border, small in a corner, and the camera whole — taken from one into
/// the other, either way, and from the camera's own channel too. Halfway
/// the source is drawn once at a framing between the two: a box grown half
/// way with its corners rounded off, nothing in them — where two pictures
/// crossing over showed the whole one faintly — and the picture whole
/// inside. The take ends on the new input exactly. So too with taps not
/// keeping their input's transparency, on both sides or one — the
/// operator's copied layout, whose bubble faded across.
#[test]
fn a_round_picture_in_picture_opens_into_the_whole_picture() {
    for case in [
        "bubble into whole",
        "whole into bubble",
        "the channel into bubble",
        "bubble into whole, both flattened",
        "bubble into whole flattened",
        "whole flattened into bubble",
    ] {
        let Some(gpu) = common::headless_gpu() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
        let (source, round, whole) = framed_taps(&mut app, dir.path(), bubble, |_, _, _| {});
        if case.contains("flattened") {
            keep_transparency(&mut app, &whole, false);
        }
        if case.contains("both flattened") {
            keep_transparency(&mut app, &round, false);
        }
        let (from, to) = match case {
            "whole into bubble" | "whole flattened into bubble" => (&whole, &round),
            "the channel into bubble" => (&source, &round),
            _ => (&round, &whole),
        };
        let (pixels, w, h) = taken_at(&mut app, from, to, MainTransition::Merge, 0.5);
        // Halfway the picture spans 0.04..0.56 across, 0.33..0.99 down, its
        // corners rounded off by a third of its height.
        let corner = at(&pixels, w, h, 0.045, 0.34);
        assert!(corner[3] < 0.05, "{case}: nothing in the corner: {corner:?}");
        let middle = at(&pixels, w, h, 0.3, 0.66);
        assert!(middle[3] > 0.95, "{case}: the picture whole inside: {middle:?}");

        let (pixels, w, h) = taken_at(&mut app, from, to, MainTransition::Merge, 0.999);
        let channel = app.mixer_ref().channels().iter().position(|c| c.uuid() == *to).unwrap();
        let end = frame_pixels(app.gpu_context(), &app.mixer_ref().channels()[channel].composite_texture);
        let off = |p: &[f32; 4], q: &[f32; 4]| p.iter().zip(q).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        let differing = pixels.iter().zip(&end).filter(|(p, q)| off(p, q) > 0.03).count();
        assert!(differing * 200 < pixels.len(), "{case}: {differing} of {} pixels differ at the end", w * h);
    }
}

/// The operator's bubble: cropped round with a border, small in the bottom
/// left corner.
fn bubble(app: &mut VardaApp, crop: &str, transform: &str) {
    set(app, crop, "crop_left", Float(0.18));
    set(app, crop, "crop_right", Float(0.79));
    set(app, crop, "border_mode", varda::params::ParamValue::Bool(true));
    set(app, crop, "border_width", Float(17.0));
    set(app, crop, "border_radius", Float(360.0));
    set(app, crop, "border_circle", varda::params::ParamValue::Bool(true));
    set(app, transform, "pos_x", Float(-0.387));
    set(app, transform, "pos_y", Float(0.320));
    set(app, transform, "scale", Float(0.303));
}

/// The bubble keeping its input's transparency, taken into the whole picture
/// not keeping it, the input half see-through in its top right quarter and
/// clear in its bottom right: halfway black has come in half way under
/// both. Where the input is clear, black at half strength; where it is
/// half see-through, its colour as it shows over black — no brighter on the
/// way, as mixing it with black in straight colour would make it.
#[test]
fn a_tap_flattened_on_one_side_darkens_where_its_input_is_clear() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let picture = dir.path().join("quarters-see-through.png");
    image::RgbaImage::from_fn(64, 36, |x, y| match (x < 32, y < 18) {
        (true, true) => image::Rgba([255, 0, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, 128]),
        (true, false) => image::Rgba([255, 255, 0, 255]),
        (false, false) => image::Rgba([0, 255, 255, 0]),
    })
    .save(&picture)
    .unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, round, whole) = framed_taps_of(&mut app, picture, bubble, |_, _, _| {});
    keep_transparency(&mut app, &whole, false);
    let (pixels, w, h) = taken_at(&mut app, &round, &whole, MainTransition::Merge, 0.5);
    // Halfway the picture spans -0.02..0.63 across and 0.33..0.99 down: at
    // (0.437, 0.53) its point (0.7, 0.3), green at half strength — 0.5 of
    // it over black, at either end and on the way; at (0.437, 0.79) its
    // point (0.7, 0.7), clear.
    let half = at(&pixels, w, h, 0.437, 0.53);
    assert!(
        (half[1] - 0.502).abs() < 0.02 && (half[3] - 0.751).abs() < 0.02 && half[0] < 0.02 && half[2] < 0.02,
        "half see-through, its green as over black: {half:?}"
    );
    let clear = at(&pixels, w, h, 0.437, 0.79);
    assert!(
        (clear[3] - 0.5).abs() < 0.05 && clear[..3].iter().all(|&c| c < 0.02),
        "black half way in: {clear:?}"
    );
    // At (0.2, 0.8), its point (0.34, 0.71): yellow, whole.
    let yellow = at(&pixels, w, h, 0.2, 0.8);
    assert!(yellow[0] > 0.9 && yellow[1] > 0.9 && yellow[3] > 0.98, "the picture whole: {yellow:?}");
}

/// A picture of one-pixel black and white squares the master's size: any
/// change in how it is sampled shows.
fn checks(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("checks.png");
    image::RgbaImage::from_fn(1920, 1080, |x, y| {
        let v = if (x + y) % 2 == 0 { 255 } else { 0 };
        image::Rgba([v, v, v, 255])
    })
    .save(&path)
    .unwrap();
    path
}

/// Each framing a merge draws a source at, from either input, starts on
/// that input's own picture, and moves off it smoothly: a soft-edged crop
/// without a border taken into
/// one with — its soft edge whole, not cut at the crop; a picture moved and
/// scaled, uncropped, into the whole or into a crop — its edge as hard as
/// it was; the whole into a round picture-in-picture; and inputs drawn at
/// half size, as soft at the start as on air. And taps not keeping their
/// input's transparency, on both sides or one, the input clear in a
/// quarter: black there from the side flattening it, clear from the other.
#[test]
fn a_merge_starts_on_each_input_exactly() {
    type Frame = fn(&mut VardaApp, &str, &str);
    // The crop's edge between pixels, where a border coming in shows first.
    let soft: Frame = |app, crop, _| {
        set(app, crop, "crop_left", Float(0.2513));
        set(app, crop, "feather", Float(0.1));
    };
    let soft_bordered: Frame = |app, crop, _| {
        set(app, crop, "crop_left", Float(0.2513));
        set(app, crop, "feather", Float(0.1));
        set(app, crop, "border_mode", varda::params::ParamValue::Bool(true));
        set(app, crop, "border_width", Float(12.0));
    };
    let moved: Frame = |app, _, transform| set(app, transform, "scale", Float(0.659));
    let round: Frame = |app, crop, transform| {
        set(app, crop, "crop_left", Float(0.18));
        set(app, crop, "crop_right", Float(0.79));
        set(app, crop, "border_mode", varda::params::ParamValue::Bool(true));
        set(app, crop, "border_width", Float(17.0));
        set(app, crop, "border_circle", varda::params::ParamValue::Bool(true));
        set(app, transform, "scale", Float(0.303));
    };
    let cropped: Frame = |app, crop, _| set(app, crop, "crop_left", Float(0.25));
    let whole: Frame = |_, _, _| {};
    // Each side's tap keeping its input's transparency, or not.
    let cases: [(&str, Frame, Frame, bool, [bool; 2]); 9] = [
        ("soft into bordered", soft, soft_bordered, false, [true; 2]),
        ("moved into whole", moved, whole, false, [true; 2]),
        ("moved into cropped", moved, cropped, false, [true; 2]),
        ("whole into round", whole, round, false, [true; 2]),
        ("at half size", cropped, whole, true, [true; 2]),
        ("flattened, round into moved", round, moved, false, [false; 2]),
        ("round kept into whole flattened", round, whole, false, [true, false]),
        ("flattened, soft into bordered", soft, soft_bordered, false, [false; 2]),
        ("flattened at half size", cropped, whole, true, [false; 2]),
    ];
    for (case, first, second, half, keeps) in cases {
        for reverse in [false, true] {
            let Some(gpu) = common::headless_gpu() else {
                return;
            };
            let dir = tempfile::tempdir().unwrap();
            let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
            let picture = if keeps == [true; 2] {
                quarters(dir.path())
            } else {
                quarters_clear_top_right(dir.path())
            };
            let (source, a, b) = framed_taps_of(&mut app, picture, first, second);
            for (channel, keep) in [(&a, keeps[0]), (&b, keeps[1])] {
                keep_transparency(&mut app, channel, keep);
            }
            if half {
                id(send_cmd(
                    &mut app,
                    EngineCommand::AddImageDeck {
                        channel_uuid: source.clone(),
                        path: checks(dir.path()),
                    },
                ));
                let state = app.build_engine_state();
                for channel in [&a, &b] {
                    let tap = state.mixer.channels.iter().find(|c| c.uuid == *channel).unwrap().decks[0]
                        .uuid
                        .clone();
                    send_cmd(
                        &mut app,
                        EngineCommand::SetDeckRenderScale {
                            deck_uuid: tap,
                            scale: varda::deck::DeckRenderScale::Half,
                        },
                    );
                }
            }
            let (from, to) = if reverse { (&b, &a) } else { (&a, &b) };
            let (pixels, w, h) = taken_at(&mut app, from, to, MainTransition::Merge, 0.0);
            let channel = app.mixer_ref().channels().iter().position(|c| c.uuid() == *from).unwrap();
            let own = frame_pixels(app.gpu_context(), &app.mixer_ref().channels()[channel].composite_texture);
            let off = |p: &[f32; 4], q: &[f32; 4]| p.iter().zip(q).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
            let differing = pixels.iter().zip(&own).filter(|(p, q)| off(p, q) > 0.02).count();
            let worst = pixels
                .iter()
                .zip(&own)
                .enumerate()
                .max_by(|(_, x), (_, y)| off(x.0, x.1).total_cmp(&off(y.0, y.1)))
                .map(|(i, (p, q))| (i as u32 % w, i as u32 / w, *p, *q));
            assert!(
                differing == 0,
                "{case}, reverse {reverse}: {differing} of {} pixels differ at the start; worst {worst:?}",
                w * h
            );
            // And it leaves it gradually: nothing appears at once.
            let (next, _, _) = taken_at(&mut app, from, to, MainTransition::Merge, 0.001);
            let jump = next
                .iter()
                .zip(&pixels)
                .enumerate()
                .max_by(|(_, x), (_, y)| off(x.0, x.1).total_cmp(&off(y.0, y.1)))
                .map(|(i, (p, q))| (off(p, q), i as u32 % w, i as u32 / w, *p, *q))
                .unwrap();
            assert!(jump.0 < 0.02, "{case}, reverse {reverse}: a jump just after the start: {jump:?}");
        }
    }
}

/// At its end a merge is the new input as it is: nothing jumps when the
/// take completes.
#[test]
fn a_merge_ends_on_the_new_input_exactly() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, whole, _) = small_then_whole(&mut app, dir.path());
    let (pixels, w, h) = taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.999);
    let channel = app.mixer_ref().channels().iter().position(|c| c.uuid() == whole).unwrap();
    let new = frame_pixels(app.gpu_context(), &app.mixer_ref().channels()[channel].composite_texture);
    let off = |p: &[f32; 4], q: &[f32; 4]| p.iter().zip(q).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
    let differing = pixels.iter().zip(&new).filter(|(p, q)| off(p, q) > 0.03).count();
    assert!(differing * 200 < pixels.len(), "{differing} of {} pixels differ", w * h);
}

/// An input a merge cannot take apart — with an effect of its own, or a
/// layer that transitions by itself — dissolves: the new input faintly
/// where the merge would show nothing.
#[test]
fn an_input_a_merge_cannot_take_apart_dissolves_instead() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    for case in ["its own effect", "a layer's own transition"] {
        let mut app = VardaApp::new(gpu.clone(), &varda::testing::headless_config()).unwrap();
        let (small, whole, _) = small_then_whole(&mut app, dir.path());
        if case == "its own effect" {
            id(send_cmd(
                &mut app,
                EngineCommand::AddEffect {
                    target: varda::engine::EffectTarget::Channel(whole.clone()),
                    shader_name: "sepia".into(),
                },
            ));
        } else {
            let layer = app.build_engine_state().mixer.channels[1].decks[0].uuid.clone();
            send_cmd(
                &mut app,
                EngineCommand::SetAutoTransitionEnabled {
                    deck_uuid: layer,
                    enabled: true,
                },
            );
        }
        let (pixels, w, h) = taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.5);
        let faint = at(&pixels, w, h, 0.85, 0.85);
        assert!(faint[3] > 0.3 && faint[3] < 0.7, "{case}: dissolved: {faint:?}");
    }
}

/// A layer taken to Main on its own ends a merge at full strength, as Main
/// shows it alone — whatever its opacity in its input.
#[test]
fn a_layer_alone_ends_a_merge_at_full_strength() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, _, _) = small_then_whole(&mut app, dir.path());
    let alone = app.build_engine_state().mixer.channels[1].decks[0].uuid.clone();
    send_cmd(
        &mut app,
        EngineCommand::SetDeckOpacity {
            deck_uuid: alone.clone(),
            opacity: 0.25,
        },
    );
    let (pixels, w, h) = taken_from_at(
        &mut app,
        MainSource::Channel(small),
        MainSource::Deck(alone),
        MainTransition::Merge,
        0.999,
    );
    let end = at(&pixels, w, h, 0.85, 0.85);
    assert!(cyan(end) && end[3] > 0.98, "full strength: {end:?}");
}

/// A layer hidden under another in its input is drawn for a merge that
/// uncovers it: the layer over it moving away shows it, not a blank.
#[test]
fn a_merge_draws_what_a_moving_layer_uncovers() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, whole, _) = small_then_whole(&mut app, dir.path());
    // Under the whole picture in the next input, hidden there: red.
    let under = id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: whole.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let _ = under;
    send_cmd(
        &mut app,
        EngineCommand::ReorderDeck {
            channel_uuid: whole.clone(),
            from_idx: 1,
            to_idx: 0,
        },
    );
    app.render_mixer_frame();
    let (pixels, w, h) = taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.5);
    let uncovered = at(&pixels, w, h, 0.85, 0.85);
    assert!(uncovered[0] > 0.4 && uncovered[0] < 0.6, "red, half in: {uncovered:?}");
}

/// An input shown small in the next one as a layer of its own grows into,
/// or shrinks to, that layer's place.
#[test]
fn an_input_shown_in_the_next_shrinks_into_its_place() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    let picture = quarters(dir.path());
    id(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: channels[0].clone(),
            path: picture,
        },
    ));
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: channels[1].clone(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: channels[0].clone(),
            },
        },
    ));
    let transform = effect(&app, &layer, "transform");
    set(&mut app, &transform, "scale", Float(0.5));
    set(&mut app, &transform, "pos_x", Float(0.25));
    set(&mut app, &transform, "pos_y", Float(0.25));
    // Whole, to the bottom-right quarter: halfway, from a quarter of the
    // way in each way to the far corner.
    let (pixels, w, h) = taken_at(&mut app, &channels[0], &channels[1], MainTransition::Merge, 0.5);
    let red = at(&pixels, w, h, 0.3, 0.3);
    assert!(red[0] > 0.9 && red[1] < 0.05, "its first quarter there: {red:?}");
    let before = at(&pixels, w, h, 0.15, 0.15);
    assert!(before[3] < 0.02, "nothing before it: {before:?}");
}

/// Wipe: the new picture from the left, halfway across halfway through.
/// Slide: in from the right. Iris: from the middle out.
#[test]
fn wipe_slide_and_iris_show_the_new_picture_where_they_say() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    for (channel, color) in channels.iter().zip([[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]) {
        id(send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: channel.clone(),
                color,
            },
        ));
    }
    let (old, new) = (&channels[0], &channels[1]);
    let blue = |p: [f32; 4]| p[2] > 0.9 && p[0] < 0.05;
    let red = |p: [f32; 4]| p[0] > 0.9 && p[2] < 0.05;

    let (pixels, w, h) = taken_at(&mut app, old, new, MainTransition::Wipe, 0.5);
    assert!(blue(at(&pixels, w, h, 0.2, 0.5)) && red(at(&pixels, w, h, 0.8, 0.5)), "wipe");

    let (pixels, w, h) = taken_at(&mut app, old, new, MainTransition::Slide, 0.5);
    assert!(red(at(&pixels, w, h, 0.2, 0.5)) && blue(at(&pixels, w, h, 0.8, 0.5)), "slide");

    let (pixels, w, h) = taken_at(&mut app, old, new, MainTransition::Iris, 0.3);
    assert!(blue(at(&pixels, w, h, 0.5, 0.5)) && red(at(&pixels, w, h, 0.02, 0.02)), "iris");

    // Each starts with none of the new picture and ends with none of the
    // old — not a strip of it at the edge, nor its corners.
    for transition in [MainTransition::Wipe, MainTransition::Iris] {
        let (pixels, w, h) = taken_at(&mut app, old, new, transition, 0.0);
        assert!(red(at(&pixels, w, h, 0.01, 0.5)) && red(at(&pixels, w, h, 0.5, 0.5)), "{transition:?} starts old");
        // At 1 the take is done; a hair before it, the transition's end.
        let (pixels, w, h) = taken_at(&mut app, old, new, transition, 0.9999);
        for (x, y) in [(0.995, 0.5), (0.995, 0.995), (0.005, 0.005)] {
            assert!(blue(at(&pixels, w, h, x, y)), "{transition:?} ends new at {x},{y}: {:?}", at(&pixels, w, h, x, y));
        }
    }

    // A fade halfway between two opaque pictures: opaque, half each.
    let (pixels, w, h) = taken_at(&mut app, old, new, MainTransition::Fade, 0.5);
    let mid = at(&pixels, w, h, 0.5, 0.5);
    assert!(mid[3] > 0.98 && (mid[0] - 0.5).abs() < 0.05 && (mid[2] - 0.5).abs() < 0.05, "fade: {mid:?}");

    // Zoom halfway: a valid picture, opaque, not light with no coverage.
    let (pixels, ..) = taken_at(&mut app, old, new, MainTransition::Zoom, 0.5);
    assert!(
        pixels.iter().all(|p| p[3] > 0.98 && p[0] <= p[3] + 1e-3 && p[2] <= p[3] + 1e-3),
        "zoom keeps its coverage"
    );
}

/// A take goes by the transition it started with, even when another is
/// chosen before its first frame: that one's shader is made first.
#[test]
fn a_take_keeps_its_transition_when_another_is_chosen() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let channels = empty_show(&mut app, 2);
    for (channel, color) in channels.iter().zip([[1.0, 0.0, 0.0, 1.0], [0.0, 0.0, 1.0, 1.0]]) {
        id(send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: channel.clone(),
                color,
            },
        ));
    }
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[0].clone()),
            fade: false,
        },
    );
    send_cmd(&mut app, EngineCommand::SetMainFadeTime { secs: 10.0 });
    send_cmd(
        &mut app,
        EngineCommand::SetMainTransition {
            transition: MainTransition::Wipe,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[1].clone()),
            fade: true,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::SetMainTransition {
            transition: MainTransition::Slide,
        },
    );
    let fade = app.mixer_mut().main_mut().fade.as_mut().expect("taking");
    fade.progress = 0.5;
    fade.duration = 1.0e9;
    app.render_mixer_frame();
    let texture = app.mixer_ref().composite_texture();
    let (w, h) = (texture.width(), texture.height());
    let pixels = frame_pixels(app.gpu_context(), texture);
    let left = at(&pixels, w, h, 0.2, 0.5);
    assert!(left[2] > 0.9 && left[0] < 0.05, "wiped, not dissolved: {left:?}");
}

/// A fade weighs the two pictures: where the new one leaves the frame
/// transparent, the old fades out with it — half there halfway, none at the
/// end — rather than staying whole and going at once.
#[test]
fn a_fade_to_a_picture_in_picture_fades_the_old_one_out_everywhere() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, whole, _) = small_then_whole(&mut app, dir.path());
    let (pixels, w, h) = taken_at(&mut app, &whole, &small, MainTransition::Fade, 0.5);
    let outside = at(&pixels, w, h, 0.85, 0.85);
    assert!(outside[3] > 0.4 && outside[3] < 0.6, "the old one half there: {outside:?}");
    let (pixels, w, h) = taken_at(&mut app, &whole, &small, MainTransition::Fade, 0.9999);
    let outside = at(&pixels, w, h, 0.85, 0.85);
    assert!(outside[3] < 0.01, "and gone by the end: {outside:?}");
}

/// A layer on Main on its own shows even when it is muted in its input:
/// Main draws it whatever its place there.
#[test]
fn a_muted_layer_alone_on_main_still_shows() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, _, _) = small_then_whole(&mut app, dir.path());
    let alone = app.build_engine_state().mixer.channels[1].decks[0].uuid.clone();
    send_cmd(
        &mut app,
        EngineCommand::SetDeckMute {
            deck_uuid: alone.clone(),
            mute: true,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Deck(alone),
            fade: false,
        },
    );
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let texture = app.mixer_ref().composite_texture();
    let (w, h) = (texture.width(), texture.height());
    let pixels = frame_pixels(app.gpu_context(), texture);
    assert!(cyan(at(&pixels, w, h, 0.85, 0.85)), "{:?}", at(&pixels, w, h, 0.85, 0.85));
}

/// A merge that dissolves instead leaves its inputs drawn as ever: every
/// layer every frame only for a merge that is drawn as one.
#[test]
fn a_merge_that_dissolves_leaves_drawing_as_it_was() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, whole, _) = small_then_whole(&mut app, dir.path());
    let every = |app: &VardaApp| {
        app.mixer_ref()
            .channels()
            .iter()
            .filter(|c| c.draw_every_deck)
            .count()
    };
    taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.5);
    assert_eq!(every(&app), 2, "both inputs, drawn whole, for a merge");
    id(send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: varda::engine::EffectTarget::Channel(whole.clone()),
            shader_name: "sepia".into(),
        },
    ));
    taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.5);
    assert_eq!(every(&app), 0, "none, for one that dissolves");
}

/// A layer with a colour effect still moves in a merge: the effect leaves
/// it where it was.
#[test]
fn a_layer_with_a_colour_effect_still_moves() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (small, whole, layer) = small_then_whole(&mut app, dir.path());
    id(send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: varda::engine::EffectTarget::Deck(layer),
            shader_name: "brightness_contrast".into(),
        },
    ));
    let (pixels, w, h) = taken_at(&mut app, &small, &whole, MainTransition::Merge, 0.5);
    assert!(cyan(at(&pixels, w, h, 0.6, 0.6)), "{:?}", at(&pixels, w, h, 0.6, 0.6));
    assert!(at(&pixels, w, h, 0.85, 0.85)[3] < 0.02);
}
