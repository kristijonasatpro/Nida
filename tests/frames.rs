//! A layer's frame (Input Settings → Position → Frame): a `frame_*` filter
//! that holds the layer's picture — scaled into its window, cut to the
//! frame's shape, a moving outline round it, clear outside — and runs before
//! the layer's Transform, so the layer moves and scales frame and all.

use std::sync::mpsc;

use varda::app::VardaApp;
use varda::engine::value::mask::{EffectMask, MaskShape, LAYER_MASK_EFFECT};
use varda::engine::{CommandResult, EngineCommand};
use varda::mixer::MainSource;
use varda::params::ParamValue;

mod common;

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    varda::testing::reply_of(app, reply_rx)
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

/// A show on Main with one input: blue under red. Its channel and the red
/// layer.
fn blue_under_red(app: &mut VardaApp) -> (String, String) {
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
    let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
    for color in [[0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]] {
        id(send_cmd(
            app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: channel.clone(),
                color,
            },
        ));
    }
    let red = app.build_engine_state().mixer.channels[0].decks[1].uuid.clone();
    send_cmd(
        app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel.clone()),
            fade: false,
        },
    );
    (channel, red)
}

/// The channel's picture, drawn twice so every pass has run.
fn picture(app: &mut VardaApp) -> (Vec<[f32; 4]>, u32, u32) {
    app.render_mixer_frame();
    app.render_mixer_frame();
    let texture = &app.mixer_ref().channels()[0].composite_texture;
    let (w, h) = (texture.width(), texture.height());
    (frame_pixels(app.gpu_context(), texture), w, h)
}

/// The names of the layer's effects, in order.
fn chain(app: &VardaApp, deck: &str) -> Vec<String> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .unwrap()
        .effects
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

/// The colour at (x, y), each 0 to 1 across and down the picture.
fn at(pixels: &[[f32; 4]], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    pixels[((y * h as f32) as u32 * w + (x * w as f32) as u32) as usize]
}

/// The layer's own picture.
fn red(px: [f32; 4]) -> bool {
    px[0] > 0.9 && px[1] < 0.1 && px[2] < 0.1
}

/// What is under the layer.
fn blue(px: [f32; 4]) -> bool {
    px[2] > 0.9 && px[0] < 0.1 && px[1] < 0.1
}

/// One of a frame's lines: these tests make them green.
fn green(px: [f32; 4]) -> bool {
    px[1] > 0.5 && px[0] < 0.4 && px[2] < 0.4
}

/// Every frame's name, as the registry lists them.
fn frame_names(app: &VardaApp) -> Vec<String> {
    app.build_engine_state()
        .registry
        .frames
        .iter()
        .map(|frame| frame.name.clone())
        .collect()
}

fn set(app: &mut VardaApp, effect: &str, name: &str, value: ParamValue) {
    send_cmd(
        app,
        EngineCommand::SetEffectParam {
            effect_uuid: effect.to_string(),
            name: name.to_string(),
            value,
        },
    );
}

/// Put `deck` in `frame`, the frame's lines green and without their glow —
/// told apart so from the red picture and the blue under it: the frame's
/// effect.
fn framed_green(app: &mut VardaApp, deck: &str, frame: &str) -> String {
    let effect = id(send_cmd(
        app,
        EngineCommand::SetLayerFrame {
            deck_uuid: deck.to_string(),
            frame: Some(frame.to_string()),
        },
    ));
    set(app, &effect, "palette", ParamValue::Long(0));
    set(app, &effect, "main_colour", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
    set(app, &effect, "accent_colour", ParamValue::Color([0.0, 1.0, 0.0, 1.0]));
    set(app, &effect, "glow", ParamValue::Float(0.0));
    effect
}

/// A setting of one of `deck`'s effects, as the state shows it.
fn setting(app: &VardaApp, effect: &str, name: &str) -> ParamValue {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .flat_map(|d| &d.effects)
        .find(|e| e.uuid == effect)
        .expect("the effect")
        .params
        .params
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no setting {name}"))
        .value
}

/// Every frame holds its layer's picture in its window and is clear
/// outside: the picture shows in the middle, what is under the layer shows
/// at the picture's sides, and the frame's lines lie between — none of them
/// in the middle of the window.
#[test]
fn every_frame_holds_the_picture_and_is_clear_outside() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let names = frame_names(&app);
    assert!(names.len() >= 14, "the frames: {names:?}");
    for name in &names {
        framed_green(&mut app, &layer, name);
        let (pixels, wide, high) = picture(&mut app);
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(red(middle), "{name}: the picture in the middle: {middle:?}");
        for (x, y) in [(0.01, 0.4), (0.99, 0.6)] {
            let side = at(&pixels, wide, high, x, y);
            assert!(blue(side), "{name}: clear outside at {x}, {y}: {side:?}");
        }
        let mut lines = 0_usize;
        for (i, px) in pixels.iter().enumerate() {
            if !green(*px) {
                continue;
            }
            lines += 1;
            let (x, y) = ((i as u32 % wide) as f32 / wide as f32, (i as u32 / wide) as f32 / high as f32);
            assert!(
                !((0.2..0.8).contains(&x) && (0.2..0.8).contains(&y)),
                "{name}: a line in the window at {x}, {y}"
            );
        }
        let share = lines as f32 / pixels.len() as f32;
        assert!(
            (0.001..0.3).contains(&share),
            "{name}: {:.2}% of the picture is its lines",
            share * 100.0
        );
    }
}

/// The frame runs before the layer's Transform: the layer scaled to half
/// is half the size frame and all — its lines within the layer's place,
/// what is under it showing beyond them.
#[test]
fn a_frame_goes_where_its_layer_goes() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    framed_green(&mut app, &layer, "frame_glow_border");
    assert_eq!(chain(&app, &layer), ["crop", "frame_glow_border", "transform"]);
    let transform = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == layer)
        .unwrap()
        .effects
        .iter()
        .find(|e| e.name == "transform")
        .expect("the layer's transform")
        .uuid
        .clone();
    set(&mut app, &transform, "scale", ParamValue::Float(0.5));
    let (pixels, wide, high) = picture(&mut app);
    let middle = at(&pixels, wide, high, 0.5, 0.5);
    assert!(red(middle), "the picture in the middle: {middle:?}");
    let beyond = at(&pixels, wide, high, 0.15, 0.15);
    assert!(blue(beyond), "what is under, beyond the layer: {beyond:?}");
    let mut lines = 0_usize;
    for (i, px) in pixels.iter().enumerate() {
        if !green(*px) {
            continue;
        }
        lines += 1;
        let (x, y) = ((i as u32 % wide) as f32 / wide as f32, (i as u32 / wide) as f32 / high as f32);
        assert!(
            (0.24..0.76).contains(&x) && (0.24..0.76).contains(&y),
            "a line outside the layer's place, at {x}, {y}"
        );
    }
    assert!(lines > 100, "the frame's lines: {lines}");
}

/// Ask for `name` on `layer` — or, with None, for no frame.
fn frame(app: &mut VardaApp, layer: &str, name: Option<&str>) -> CommandResult {
    send_cmd(
        app,
        EngineCommand::SetLayerFrame {
            deck_uuid: layer.to_string(),
            frame: name.map(str::to_string),
        },
    )
}

/// Asking for a frame puts it before Transform, and the layer's own mask
/// stays last; None takes the frame away, and Undo brings it back.
#[test]
fn a_frame_is_set_before_transform_and_taken_away() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    assert_eq!(chain(&app, &layer), ["crop", "transform"]);
    id(frame(&mut app, &layer, Some("frame_hud")));
    assert_eq!(chain(&app, &layer), ["crop", "frame_hud", "transform"]);

    id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: layer.clone(),
            mask: Some(EffectMask {
                shape: MaskShape::Ellipse {
                    x: 0.3,
                    y: 0.3,
                    width: 0.4,
                    height: 0.4,
                },
                feather: 0.0,
                invert: false,
            }),
        },
    ));
    id(frame(&mut app, &layer, Some("frame_panel")));
    assert_eq!(
        chain(&app, &layer),
        ["crop", "frame_panel", "transform", LAYER_MASK_EFFECT],
        "one frame, before Transform, the mask last"
    );

    let gone = id(frame(&mut app, &layer, None));
    assert!(gone.is_empty(), "no frame, no effect to name: {gone}");
    assert_eq!(chain(&app, &layer), ["crop", "transform", LAYER_MASK_EFFECT]);
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(
        chain(&app, &layer),
        ["crop", "frame_panel", "transform", LAYER_MASK_EFFECT],
        "Undo brings the frame back"
    );
}

/// A frame put in another's place keeps what was changed on the first:
/// its size here.
#[test]
fn another_frame_keeps_what_was_changed_on_the_first() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let hud = id(frame(&mut app, &layer, Some("frame_hud")));
    set(&mut app, &hud, "window", ParamValue::Float(0.7));
    let panel = id(frame(&mut app, &layer, Some("frame_panel")));
    assert_ne!(panel, hud, "another effect");
    let size = setting(&app, &panel, "window");
    assert!(
        matches!(size, ParamValue::Float(v) if (v - 0.7).abs() < 1e-6),
        "the size set on the first frame: {size:?}"
    );
}

/// What was left as it came on the first frame is the new frame's own:
/// its palette here (the HUD frame's is 1, the Tech panel's 2).
#[test]
fn another_frame_takes_its_own_for_what_was_left_as_it_came() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let hud = id(frame(&mut app, &layer, Some("frame_hud")));
    let first = setting(&app, &hud, "palette");
    assert!(matches!(first, ParamValue::Long(1)), "the HUD frame's palette: {first:?}");
    let panel = id(frame(&mut app, &layer, Some("frame_panel")));
    let palette = setting(&app, &panel, "palette");
    assert!(matches!(palette, ParamValue::Long(2)), "the panel's own palette: {palette:?}");
}

/// A name that is no frame — another effect's — is refused, and the layer
/// left as it was.
#[test]
fn a_name_that_is_no_frame_is_refused() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    id(frame(&mut app, &layer, Some("frame_hud")));
    let before = chain(&app, &layer);
    for name in ["transform", "no_such_frame"] {
        let refused = frame(&mut app, &layer, Some(name));
        assert!(matches!(refused, CommandResult::Err { .. }), "{name}: {refused:?}");
        assert_eq!(chain(&app, &layer), before, "{name}: left as it was");
    }
}

/// Shape makes the window square whatever the picture: clear beside it,
/// and — the whole picture showing in it — clear above the picture too.
#[test]
fn shape_makes_the_window_square() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let effect = framed_green(&mut app, &layer, "frame_glow_border");
    set(&mut app, &effect, "shape", ParamValue::Long(3));
    let (pixels, wide, high) = picture(&mut app);
    let (beside, above) = square_window_gaps(wide, high);
    let middle = at(&pixels, wide, high, 0.5, 0.5);
    assert!(red(middle), "the picture in the middle: {middle:?}");
    let side = at(&pixels, wide, high, beside, 0.5);
    assert!(blue(side), "clear beside a square window: {side:?}");
    let gap = at(&pixels, wide, high, 0.5, above);
    assert!(blue(gap), "the whole picture leaves the window clear above it: {gap:?}");
}

/// Fill the frame has the picture cover the window, where it otherwise
/// shows whole inside it; it is still cut to the window.
#[test]
fn fill_has_the_picture_cover_the_window() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let effect = framed_green(&mut app, &layer, "frame_corners");
    set(&mut app, &effect, "shape", ParamValue::Long(3));
    set(&mut app, &effect, "fill", ParamValue::Bool(true));
    let (pixels, wide, high) = picture(&mut app);
    let (beside, above) = square_window_gaps(wide, high);
    let filled = at(&pixels, wide, high, 0.5, above);
    assert!(red(filled), "the picture fills the window: {filled:?}");
    let side = at(&pixels, wide, high, beside, 0.5);
    assert!(blue(side), "and is cut to it: {side:?}");
}

/// Where to look at a square window (Shape 1:1, Size 0.86) in a wide
/// picture: beside it, half way to the picture's edge; and in it above the
/// whole picture, half way between that picture's top and the window's.
fn square_window_gaps(wide: u32, high: u32) -> (f32, f32) {
    let aspect = wide as f32 / high as f32;
    assert!(aspect > 1.2, "a wide picture: {wide}x{high}");
    // The window is 0.86 of the height each way; the whole picture in it
    // is as wide as the window, and so less high.
    let (window, whole) = (0.43, 0.43 / aspect);
    ((0.5 - window / aspect) / 2.0, 0.5 - f32::midpoint(whole, window))
}

/// A shaped frame cuts the picture to its shape: filling the HUD frame,
/// the picture reaches into its corner but not past the cut across it.
#[test]
fn a_shaped_frame_cuts_the_picture_to_its_shape() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let effect = framed_green(&mut app, &layer, "frame_hud");
    set(&mut app, &effect, "fill", ParamValue::Bool(true));
    let (pixels, wide, high) = picture(&mut app);
    let aspect = wide as f32 / high as f32;
    // The frame's corner, from the middle in picture heights: its window
    // (0.86 each way) and the step its outline stands outside it.
    let corner = (0.43 * aspect + 0.02, 0.45);
    let point = |inset: f32| (0.5 - (corner.0 - inset) / aspect, 0.5 - (corner.1 - inset));
    // Its corner is cut 0.04 along each side: a point 0.006 in from both
    // sides is past the cut, one 0.035 in is within it.
    let (x, y) = point(0.006);
    let past = at(&pixels, wide, high, x, y);
    assert!(blue(past), "past the cut corner, the picture cut away: {past:?}");
    let (x, y) = point(0.035);
    let within = at(&pixels, wide, high, x, y);
    assert!(red(within), "within the corner, the picture: {within:?}");
}

/// Fill the frame covers the frame's whole shape, the parts of it that
/// stand out too: the picture reaches up into the Notched frame's tab.
#[test]
fn fill_reaches_into_a_frames_tab() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let effect = framed_green(&mut app, &layer, "frame_notched");
    set(&mut app, &effect, "fill", ParamValue::Bool(true));
    let (pixels, wide, high) = picture(&mut app);
    let aspect = wide as f32 / high as f32;
    // The frame's sides stand 0.022 outside its window (0.86 each way);
    // its tab rises 0.026 above the top one, over the middle quarter.
    let outer = (0.43 * aspect + 0.022, 0.452);
    // In the tab: half way up it, beside the bar that fills and empties.
    let in_tab = at(
        &pixels,
        wide,
        high,
        0.5 + 0.23 * outer.0 / aspect,
        0.5 - (outer.1 + 0.013),
    );
    assert!(red(in_tab), "the picture in the tab: {in_tab:?}");
}

/// What each frame costs the GPU at 3840×2160: a layer's picture drawn and
/// waited for with the frame on it, less the same without one — a
/// measurement, run by hand
/// (`cargo test --release --test frames -- --ignored --nocapture`). The
/// picture is a still one, kept from frame to frame without a frame on it:
/// a frame, which moves, has it drawn every frame — that is in the figure.
#[test]
#[ignore = "a measurement"]
fn cost_of_each_frame_at_4k() {
    use varda::audio::AudioData;
    use varda::deck::{Deck, Effect};
    use varda::isf::ISFShader;
    use varda::mixer::Mixer;
    use varda::modulation::{AnalyzerValues, AudioValues};
    use varda::renderer::tonemap::TonemapMode;
    const W4: u32 = 3840;
    const H4: u32 = 2160;
    const FRAMES: u32 = 30;
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let time = |frame: Option<&std::path::Path>| -> f64 {
        let mut mixer = Mixer::new(&ctx, W4, H4).expect("mixer");
        mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
        let mut deck = Deck::new_solid_color(&ctx, [0.8, 0.1, 0.1, 1.0], W4, H4).expect("deck");
        if let Some(path) = frame {
            let shader = ISFShader::from_file(path).expect("shader");
            deck.add_effect(Effect::new(&ctx, shader).expect("effect"));
        }
        mixer.channel_mut(0).expect("a channel").add_deck(deck);
        let audio = AudioData::default();
        let audio_values = AudioValues {
            sources: std::collections::HashMap::default(),
        };
        let analyzer_values = AnalyzerValues::default();
        let mut draw = |n: u32| {
            let inputs = varda::mixer::FrameInputs {
                audio_data: &audio,
                audio_values: &audio_values,
                analyzer_values: &analyzer_values,
                beat_time: None,
                transport: None,
                free_run_time: Some(f64::from(n) as f32 / 60.0),
            };
            mixer.render(&ctx, &inputs, 60, &[]).expect("render");
            ctx.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .ok();
        };
        for n in 0..5 {
            draw(n);
        }
        let started = std::time::Instant::now();
        for n in 5..5 + FRAMES {
            draw(n);
        }
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES)
    };
    // The first timing of all is the GPU warming up: taken, and let go.
    time(None);
    let alone = time(None);
    println!("the picture alone: {alone:.2} ms a frame");
    let mut frames: Vec<std::path::PathBuf> =
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders"))
            .expect("shaders dir")
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension().is_some_and(|ext| ext == "fs")
                    && path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("frame_"))
            })
            .collect();
    frames.sort();
    for path in &frames {
        let name = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let ms = time(Some(path));
        println!("{name}: {ms:.2} ms a frame ({:+.2} over the picture alone)", ms - alone);
    }
}
