//! Another input as a layer: an input's picture shown in a second input,
//! sized and placed like any layer, over that input's own background — drawn
//! once for both, and this frame's picture, not the last one.

use std::sync::mpsc;

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand};
use varda::mixer::MainSource;

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

/// One pixel of an `Rgba16Float` texture, as RGBA.
fn pixel(
    gpu: &varda::renderer::context::GpuContext,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
) -> [f32; 4] {
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 256,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(256),
                rows_per_image: Some(1),
            },
        },
        wgpu::Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
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
    std::array::from_fn(|i| f16(u16::from_le_bytes([data[i * 2], data[i * 2 + 1]])))
}

#[test]
fn an_input_shown_small_in_another_over_its_background() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let (first, second) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    for channel in &state.mixer.channels[..2] {
        for deck in &channel.decks {
            send_cmd(
                &mut app,
                EngineCommand::RemoveDeck {
                    deck_uuid: deck.uuid.clone(),
                },
            );
        }
    }
    // The first input: red. The second: a blue background, and the first
    // input over it at half size.
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: first.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: second.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: second.clone(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: first.clone(),
            },
        },
    ));
    let state = app.build_engine_state();
    let transform = state.mixer.channels[1]
        .decks
        .iter()
        .find(|d| d.uuid == layer)
        .and_then(|d| d.effects.iter().find(|e| e.name == "transform"))
        .expect("a new layer carries a transform")
        .uuid
        .clone();
    send_cmd(
        &mut app,
        EngineCommand::SetEffectParam {
            effect_uuid: transform,
            name: "scale".into(),
            value: varda::params::ParamValue::Float(0.5),
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(second),
            fade: false,
        },
    );
    let mixer = app.mixer_ref();
    assert!(
        mixer.channels()[1].decks[1].deck.transparent(),
        "the layer shows what is under it where its input is clear"
    );

    // On the very first frame.
    app.render_mixer_frame();
    let mixer = app.mixer_ref();
    let composite = &mixer.channels()[1].composite_texture;
    let (w, h) = (composite.width(), composite.height());
    let centre = pixel(app.gpu_context(), composite, w / 2, h / 2);
    let corner = pixel(app.gpu_context(), composite, 4, 4);
    assert!(
        centre[0] > 0.9 && centre[2] < 0.1,
        "the first input in the middle: {centre:?}"
    );
    assert!(
        corner[2] > 0.9 && corner[0] < 0.1,
        "the background round it: {corner:?}"
    );
    assert!(
        mixer.has_no_tap_targets(),
        "shown this frame, with no second copy kept"
    );
}

/// A layer showing another input fits that input's picture at whatever
/// resolution: changed after the layer was made, the picture is the
/// master's new size, so a layer centring it at its own size still fills
/// its frame — not bars round it, as if the picture were the old size.
#[test]
fn a_layer_showing_an_input_fills_its_frame_after_the_resolution_changes() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let (first, second) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    for channel in &state.mixer.channels[..2] {
        for deck in &channel.decks {
            send_cmd(
                &mut app,
                EngineCommand::RemoveDeck {
                    deck_uuid: deck.uuid.clone(),
                },
            );
        }
    }
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: first.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: second.clone(),
            source: varda::scene::TapSourceConfig::Channel { uuid: first },
        },
    ));
    send_cmd(
        &mut app,
        EngineCommand::SetDeckScalingMode {
            deck_uuid: layer,
            mode: varda::deck::ScalingMode::Center,
        },
    );
    let (w, h) = {
        let composite = &app.mixer_ref().channels()[1].composite_texture;
        (composite.width(), composite.height())
    };
    // A third larger each way.
    let (bigger_w, bigger_h) = (w / 3 * 4 / 2 * 2, h / 3 * 4 / 2 * 2);
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: bigger_w,
            height: bigger_h,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(second),
            fade: false,
        },
    );
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let composite = &app.mixer_ref().channels()[1].composite_texture;
    assert_eq!((composite.width(), composite.height()), (bigger_w, bigger_h), "resized");
    let corner = pixel(app.gpu_context(), composite, 4, 4);
    assert!(
        corner[0] > 0.9 && corner[3] > 0.98,
        "the input's picture right to the corner: {corner:?}"
    );
}

/// Turn a layer's crop border on: a white frame round the middle half.
fn border(app: &mut VardaApp, channel: usize, layer: &str) {
    let state = app.build_engine_state();
    let crop = state.mixer.channels[channel]
        .decks
        .iter()
        .find(|d| d.uuid == layer)
        .and_then(|d| d.effects.iter().find(|e| e.name == "crop"))
        .expect("a new layer carries a crop")
        .uuid
        .clone();
    for (name, value) in [
        ("crop_left", varda::params::ParamValue::Float(0.25)),
        ("crop_right", varda::params::ParamValue::Float(0.75)),
        ("crop_bottom", varda::params::ParamValue::Float(0.25)),
        ("crop_top", varda::params::ParamValue::Float(0.75)),
        ("border_width", varda::params::ParamValue::Float(40.0)),
        ("border_mode", varda::params::ParamValue::Bool(true)),
    ] {
        send_cmd(
            app,
            EngineCommand::SetEffectParam {
                effect_uuid: crop.clone(),
                name: name.into(),
                value,
            },
        );
    }
}

/// Deleting an input takes the layers showing it in other inputs with it —
/// kept, one showed only its border where the picture had been.
#[test]
fn deleting_an_input_takes_the_layers_showing_it() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let shown = id(send_cmd(&mut app, EngineCommand::AddChannel));
    let first = app.build_engine_state().mixer.channels[0].uuid.clone();
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: first,
            source: varda::scene::TapSourceConfig::Channel {
                uuid: shown.clone(),
            },
        },
    ));
    border(&mut app, 0, &layer);
    let result = send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: shown,
        },
    );
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
    let state = app.build_engine_state();
    assert!(
        !state.mixer.channels[0]
            .decks
            .iter()
            .any(|d| d.uuid == layer),
        "the layer went with the input it showed"
    );
}

/// A layer whose input is not in the show (a show saved with one gone)
/// draws nothing: not its border either.
#[test]
fn a_layer_showing_a_lost_input_draws_nothing() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let state = app.build_engine_state();
    let first = state.mixer.channels[0].uuid.clone();
    for deck in &state.mixer.channels[0].decks {
        send_cmd(
            &mut app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: first.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: first,
            source: varda::scene::TapSourceConfig::Channel {
                uuid: "gone0001".into(),
            },
        },
    ));
    border(&mut app, 0, &layer);
    app.render_mixer_frame();
    let mixer = app.mixer_ref();
    let composite = &mixer.channels()[0].composite_texture;
    let (w, h) = (composite.width(), composite.height());
    // On the border's line, left of the middle.
    let on_border = pixel(app.gpu_context(), composite, w / 4 + 2, h / 2);
    assert!(
        on_border[2] > 0.9 && on_border[0] < 0.1,
        "only the blue under it: {on_border:?}"
    );
}

/// Another input shown small and to one side is drawn straight into its
/// place — a quarter of the frame drawn, not all of it and then shrunk —
/// and lands exactly where the transform puts it, the right way round.
/// Into a frame of its own: drawn onto its input from the source, it draws
/// no frame of its own (see `tests/direct_layers.rs`).
#[test]
fn an_input_moved_and_scaled_is_drawn_in_its_place_in_one_pass() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    // Red on the left, green on the right.
    let picture = dir.path().join("halves.png");
    image::RgbaImage::from_fn(64, 36, |x, _| {
        if x < 32 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 255, 0, 255])
        }
    })
    .save(&picture)
    .unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetCompositor {
            direct_layers: Some(false),
            merge_layer_passes: None,
            slow_layers_less_often: None,
            skip_blank_channels: None,
            keep_still_text: None,
            merge_main_passes: None,
            pass_main_through: None,
            keep_still_clips: None,
            keep_still_inputs: None,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let (first, second) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    for channel in &state.mixer.channels[..2] {
        for deck in &channel.decks {
            send_cmd(
                &mut app,
                EngineCommand::RemoveDeck {
                    deck_uuid: deck.uuid.clone(),
                },
            );
        }
    }
    id(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: first.clone(),
            path: picture,
        },
    ));
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: second.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: second.clone(),
            source: varda::scene::TapSourceConfig::Channel { uuid: first },
        },
    ));
    let state = app.build_engine_state();
    let transform = state.mixer.channels[1]
        .decks
        .iter()
        .find(|d| d.uuid == layer)
        .and_then(|d| d.effects.iter().find(|e| e.name == "transform"))
        .expect("a new layer carries a transform")
        .uuid
        .clone();
    // Half size, its centre moved right and up: it covers 0.45–0.95
    // across and 0.15–0.65 down, red turning green at 0.7.
    for (name, value) in [("scale", 0.5), ("pos_x", 0.2), ("pos_y", -0.1)] {
        send_cmd(
            &mut app,
            EngineCommand::SetEffectParam {
                effect_uuid: transform.clone(),
                name: name.into(),
                value: varda::params::ParamValue::Float(value),
            },
        );
    }
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(second),
            fade: false,
        },
    );
    app.render_mixer_frame();
    app.render_mixer_frame();
    let mixer = app.mixer_ref();
    let composite = &mixer.channels()[1].composite_texture;
    let (w, h) = (composite.width() as f32, composite.height() as f32);
    let at = |x: f32, y: f32| pixel(app.gpu_context(), composite, (x * w) as u32, (y * h) as u32);
    let red = |p: [f32; 4]| p[0] > 0.9 && p[1] < 0.1 && p[2] < 0.1;
    let green = |p: [f32; 4]| p[1] > 0.9 && p[0] < 0.1 && p[2] < 0.1;
    let blue = |p: [f32; 4]| p[2] > 0.9 && p[0] < 0.1 && p[1] < 0.1;
    for (x, y, want, what) in [
        (0.46, 0.4, red as fn([f32; 4]) -> bool, "red just inside the left edge"),
        (0.68, 0.4, red, "red left of the middle"),
        (0.72, 0.4, green, "green right of the middle"),
        (0.94, 0.4, green, "green just inside the right edge"),
        (0.7, 0.16, red, "just inside the top"),
        (0.44, 0.4, blue, "the background left of it"),
        (0.96, 0.4, blue, "right of it"),
        (0.7, 0.14, blue, "above it"),
        (0.7, 0.66, blue, "below it"),
    ] {
        let p = at(x, y);
        let p = if what == "just inside the top" {
            // Where red turns green: either will do, but not blue.
            assert!(!blue(p), "{what} at {x},{y}: {p:?}");
            continue;
        } else {
            p
        };
        assert!(want(p), "{what} at {x},{y}: {p:?}");
    }
    let tap = mixer.channels()[1]
        .decks
        .iter()
        .find(|slot| slot.deck.uuid() == layer)
        .unwrap();
    let share = tap.deck.drawn_share();
    assert!(
        (share - 0.25).abs() < 0.01,
        "a quarter of the frame drawn, not all of it: {share}"
    );
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

/// How a picture-in-picture is drawn onto its input: into a frame of its
/// own, or straight from its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drawn {
    IntoItsFrame,
    FromSource,
}

/// A picture-in-picture with a white border and rounded corners — the crop
/// then the transform — drawn in one pass looks as it did drawn whole,
/// cropped and shrunk: the same over the whole frame but for the odd pixel
/// on an anti-aliased edge.
#[test]
fn a_bordered_rounded_picture_in_picture_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(false, false, false, Drawn::IntoItsFrame);
    drawn_in_one_pass_looks_the_same(false, false, false, Drawn::FromSource);
}

/// The same, ringed in a circle — a thick coloured ring, whose colour both
/// ways must take alike.
#[test]
fn a_circled_picture_in_picture_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(true, false, false, Drawn::IntoItsFrame);
    drawn_in_one_pass_looks_the_same(true, false, false, Drawn::FromSource);
}

/// Flipped both ways, with a crop off to one side: the transform mirrors the
/// cropped picture — its kept part and border with it — the same in one pass
/// as in two, and a point of it shows the corner mirrored there.
#[test]
fn a_flipped_picture_in_picture_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(false, true, false, Drawn::IntoItsFrame);
    drawn_in_one_pass_looks_the_same(false, true, false, Drawn::FromSource);
}

/// A layer not keeping its input's transparency, the input clear in a
/// corner: drawn in one pass, it is flattened over black there as it was
/// drawn whole over black, then cropped and shrunk — black, not the layer
/// under it, the same both ways.
#[test]
fn a_picture_in_picture_flattened_over_black_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(false, false, true, Drawn::IntoItsFrame);
    drawn_in_one_pass_looks_the_same(false, false, true, Drawn::FromSource);
}

/// Two copies of one picture-in-picture, the first drawn in one pass and the
/// second the old way — its crop modulated, at no depth, so the pass drawing
/// the picture leaves it to the passes after: whole, then cropped, then
/// shrunk. Flattened: the picture clear in its top right quarter, neither
/// layer keeping its transparency. `drawn`: the first drawn in that one pass
/// into a frame of its own, or straight onto its input from the source (see
/// `tests/direct_layers.rs`).
fn drawn_in_one_pass_looks_the_same(circle: bool, flip: bool, flattened: bool, drawn: Drawn) {
    use varda::params::ParamValue::{Bool, Color, Float};
    let direct = drawn == Drawn::FromSource;
    // White, or orange: 0.9, 0.5, 0.1 in sRGB is about 0.79, 0.21, 0.01 in
    // the linear light the composite holds.
    let (ring, width, linear) = if circle {
        ([0.9, 0.5, 0.1, 1.0], 60.0, [0.787, 0.214, 0.010])
    } else {
        ([1.0, 1.0, 1.0, 1.0], 25.0, [1.0, 1.0, 1.0])
    };
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let picture = dir.path().join("quarters.png");
    let clear = if flattened { 0 } else { 255 };
    image::RgbaImage::from_fn(64, 36, |x, y| match (x < 32, y < 18) {
        (true, true) => image::Rgba([255, 0, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, clear]),
        (true, false) => image::Rgba([255, 255, 0, 255]),
        (false, false) => image::Rgba([0, 255, 255, 255]),
    })
    .save(&picture)
    .unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetCompositor {
            direct_layers: Some(direct),
            merge_layer_passes: None,
            slow_layers_less_often: None,
            skip_blank_channels: None,
            keep_still_text: None,
            merge_main_passes: None,
            pass_main_through: None,
            keep_still_clips: None,
            keep_still_inputs: None,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    for channel in &state.mixer.channels {
        for deck in &channel.decks {
            send_cmd(
                &mut app,
                EngineCommand::RemoveDeck {
                    deck_uuid: deck.uuid.clone(),
                },
            );
        }
    }
    while app.build_engine_state().mixer.channels.len() < 3 {
        send_cmd(&mut app, EngineCommand::AddChannel);
    }
    let channels: Vec<String> = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|c| c.uuid.clone())
        .collect();
    id(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: channels[0].clone(),
            path: picture,
        },
    ));
    // The same layer in two inputs.
    let mut layers = Vec::new();
    let mut crops = Vec::new();
    for channel in &channels[1..3] {
        id(send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: channel.clone(),
                color: [0.0, 0.0, 1.0, 1.0],
            },
        ));
        let layer = id(send_cmd(
            &mut app,
            EngineCommand::AddTapDeck {
                channel_uuid: channel.clone(),
                source: varda::scene::TapSourceConfig::Channel {
                    uuid: channels[0].clone(),
                },
            },
        ));
        let state = app.build_engine_state();
        let effects = &state
            .mixer
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.uuid == layer)
            .unwrap()
            .effects;
        let uuid_of = |name: &str| effects.iter().find(|e| e.name == name).unwrap().uuid.clone();
        let (crop, transform) = (uuid_of("crop"), uuid_of("transform"));
        // Flipped, the crop is off to the left, so a flip done before the
        // crop rather than after it would show.
        let left = if flip { 0.3 } else { 0.1 };
        for (effect, name, value) in [
            (&transform, "flip_x", Bool(flip)),
            (&transform, "flip_y", Bool(flip)),
            (&crop, "crop_left", Float(left)),
            (&crop, "crop_right", Float(0.9)),
            (&crop, "crop_bottom", Float(0.05)),
            (&crop, "crop_top", Float(0.95)),
            (&crop, "border_mode", Bool(true)),
            (&crop, "border_width", Float(width)),
            (&crop, "border_color", Color(ring)),
            (&crop, "border_radius", Float(155.0)),
            (&crop, "border_circle", Bool(circle)),
            (&transform, "scale", Float(0.4)),
            (&transform, "pos_x", Float(0.22)),
            (&transform, "pos_y", Float(0.18)),
        ] {
            send_cmd(
                &mut app,
                EngineCommand::SetEffectParam {
                    effect_uuid: effect.clone(),
                    name: name.into(),
                    value,
                },
            );
        }
        if flattened {
            send_cmd(
                &mut app,
                EngineCommand::SetDeckTransparent {
                    deck_uuid: layer.clone(),
                    transparent: false,
                },
            );
        }
        layers.push(layer);
        crops.push(crop);
    }
    send_cmd(
        &mut app,
        EngineCommand::AddLfo {
            waveform: varda::modulation::LFOWaveform::Sine,
            frequency: 1.0,
        },
    );
    let lfo = app.build_engine_state().modulation.sources[0].uuid.clone();
    send_cmd(
        &mut app,
        EngineCommand::AssignModulation {
            target: format!("fx_{}:crop_left", crops[1]),
            source_id: lfo,
            amount: 0.0,
        },
    );
    for channel in &channels[1..3] {
        send_cmd(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channel.clone()),
                fade: false,
            },
        );
        app.render_mixer_frame();
    }
    // Both on screen at once: the overlay of the second over Main.
    send_cmd(
        &mut app,
        EngineCommand::SetChannelOverlay {
            channel_uuid: channels[1].clone(),
            overlay: true,
        },
    );
    app.render_mixer_frame();
    app.render_mixer_frame();
    let mixer = app.mixer_ref();
    let deck = |ch: usize, uuid: &str| {
        mixer.channels()[ch]
            .decks
            .iter()
            .find(|slot| slot.deck.uuid() == uuid)
            .unwrap()
            .deck
            .drawn_share()
    };
    let (one_pass, old_way) = (deck(1, &layers[0]), deck(2, &layers[1]));
    if direct {
        assert_eq!(mixer.channels()[1].drawn_onto, 1, "drawn onto its input");
    } else {
        assert!(one_pass < 0.2, "drawn in its place: {one_pass}");
    }
    assert_eq!(mixer.channels()[2].drawn_onto, 0, "the old way: a frame of its own");
    assert!(old_way > 0.99, "drawn whole: {old_way}");

    let a = frame_pixels(app.gpu_context(), &mixer.channels()[1].composite_texture);
    let b = frame_pixels(app.gpu_context(), &mixer.channels()[2].composite_texture);
    let off = |p: &[f32; 4], q: &[f32; 4]| p.iter().zip(q).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max);
    let differing = a.iter().zip(&b).filter(|(p, q)| off(p, q) > 0.05).count();
    let ringed = a
        .iter()
        .filter(|p| (0..3).all(|c| (p[c] - linear[c]).abs() < 0.03))
        .count();
    assert!(ringed > 1000, "the border is drawn: {ringed} pixels of its colour");
    // The picture covers about a ninth of the frame; its edges and the
    // border's are a few thousand pixels, anti-aliased a little differently.
    let share = differing as f64 / a.len() as f64;
    assert!(share < 0.004, "{differing} of {} pixels differ", a.len());

    if flip {
        // The picture spans x 0.52–0.92 and y 0.48–0.88 of the frame. At
        // (0.62, 0.58) the flipped picture shows its point (0.75, 0.75) —
        // the bottom right quarter, cyan; unflipped, that point is cropped
        // away (picture x 0.25 < 0.3) and the blue below shows.
        let texture = &mixer.channels()[1].composite_texture;
        let (width, height) = (texture.width() as usize, texture.height() as usize);
        let spot = (0.58 * height as f64) as usize * width + (0.62 * width as f64) as usize;
        for frame in [&a, &b] {
            let pixel = frame[spot];
            assert!(
                pixel[0] < 0.1 && pixel[1] > 0.9 && pixel[2] > 0.9,
                "the mirrored corner, cyan: {pixel:?}"
            );
        }
    }
    if flattened {
        // At (0.82, 0.58) the picture shows its point (0.75, 0.25): the
        // clear quarter, black over the blue layer under it.
        let texture = &mixer.channels()[1].composite_texture;
        let (width, height) = (texture.width() as usize, texture.height() as usize);
        let spot = (0.58 * height as f64) as usize * width + (0.82 * width as f64) as usize;
        for frame in [&a, &b] {
            let pixel = frame[spot];
            assert!(
                (0..3).all(|c| pixel[c] < 0.02) && pixel[3] > 0.99,
                "the clear quarter, black: {pixel:?}"
            );
        }
    }
}

/// Edge contrast of a row of an `Rgba16Float` texture between `from` and
/// `to` across: the mean squared step from each pixel to the next.
fn edge_energy(pixels: &[[f32; 4]], width: u32, y: u32, from: u32, to: u32) -> f32 {
    let row = &pixels[(y * width) as usize..((y + 1) * width) as usize];
    let steps: Vec<f32> = (from..to)
        .map(|x| (row[x as usize + 1][0] - row[x as usize][0]).powi(2))
        .collect();
    steps.iter().sum::<f32>() / steps.len() as f32
}

/// Edge energy (see `edge_energy`) of black and white bars `bar` pixels wide,
/// at the show's own size, shown in another input as a layer at each of
/// `scales` by its transform.
fn edges_at_scales(bar: u32, scales: &[f32]) -> Option<Vec<f32>> {
    let gpu = common::headless_gpu()?;
    let dir = tempfile::tempdir().unwrap();
    let picture = dir.path().join("bars.png");
    image::RgbaImage::from_fn(1920, 1080, |x, _| {
        if (x / bar).is_multiple_of(2) {
            image::Rgba([0, 0, 0, 255])
        } else {
            image::Rgba([255, 255, 255, 255])
        }
    })
    .save(&picture)
    .unwrap();
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let (first, second) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    for channel in &state.mixer.channels[..2] {
        for deck in &channel.decks {
            send_cmd(
                &mut app,
                EngineCommand::RemoveDeck {
                    deck_uuid: deck.uuid.clone(),
                },
            );
        }
    }
    id(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: first.clone(),
            path: picture,
        },
    ));
    let layer = id(send_cmd(
        &mut app,
        EngineCommand::AddTapDeck {
            channel_uuid: second.clone(),
            source: varda::scene::TapSourceConfig::Channel { uuid: first },
        },
    ));
    let state = app.build_engine_state();
    let transform = state.mixer.channels[1]
        .decks
        .iter()
        .find(|d| d.uuid == layer)
        .and_then(|d| d.effects.iter().find(|e| e.name == "transform"))
        .expect("a new layer carries a transform")
        .uuid
        .clone();
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(second),
            fade: false,
        },
    );
    let mut energy = Vec::new();
    for &scale in scales {
        send_cmd(
            &mut app,
            EngineCommand::SetEffectParam {
                effect_uuid: transform.clone(),
                name: "scale".into(),
                value: varda::params::ParamValue::Float(scale),
            },
        );
        app.render_mixer_frame();
        app.render_mixer_frame();
        let composite = &app.mixer_ref().channels()[1].composite_texture;
        let pixels = frame_pixels(app.gpu_context(), composite);
        energy.push(edge_energy(&pixels, 1920, 540, 760, 1160));
    }
    Some(energy)
}

/// An input shown as a layer and scaled by its transform keeps its edges:
/// one bilinear tap blurred it — a window enlarged into its frame
/// (KJ's app capture, 1.86×) and a 4K screen shrunk into one (0.89×, a
/// fifth of its text's edges gone, measured on an M5 Max) — where the
/// layer's own picture, drawn the same, came out sharp. Bars two pixels
/// wide: text is drawn in stems a pixel or two wide.
#[test]
fn an_input_scaled_as_a_layer_keeps_its_edges() {
    let Some(energy) = edges_at_scales(2, &[1.0, 0.891, 1.86]) else {
        return;
    };
    // At its own size exactly as it is; a little smaller and enlarged,
    // sharper than one bilinear tap (0.394 and 0.118 at these sizes: the
    // bicubic gives 0.412 and 0.129).
    assert!((energy[0] - 0.5).abs() < 1e-3, "at its own size: {energy:?}");
    assert!(energy[1] > 0.403, "a little smaller: {energy:?}");
    assert!(energy[2] > 0.124, "enlarged: {energy:?}");
}

/// A layer shrinking in a Merge takes no jump in sharpness where bicubic
/// meets one tap (about its own size) or averaging (a quarter smaller) —
/// a jump of a quarter of its edges' contrast there before. Each pair of
/// sizes either side of one, close enough that the picture moves by a hair.
/// (The averaging still steps at whole numbers of texels, a few hundredths,
/// as every picture drawn smaller always has.) Bars five pixels wide:
/// narrower ones beat with the pixels at these sizes, whatever the filter.
#[test]
fn an_input_shrinking_as_a_layer_does_not_jump_in_sharpness() {
    let pairs = [(0.9806, 0.9801), (0.801, 0.799)];
    let scales: Vec<f32> = pairs.iter().flat_map(|&(a, b)| [a, b]).collect();
    let Some(energy) = edges_at_scales(5, &scales) else {
        return;
    };
    for (i, (a, b)) in pairs.iter().enumerate() {
        let (ea, eb) = (energy[2 * i], energy[2 * i + 1]);
        assert!(
            (ea - eb).abs() < 0.02 * ea.max(eb),
            "between {a} and {b}: {ea:.4} and {eb:.4} ({energy:?})"
        );
    }
}

/// A screen `w` × `h` as its capture brings it: red rising left to right,
/// green top to bottom. Its texture too, kept while the view is shown.
fn a_screen(
    gpu: &varda::renderer::context::GpuContext,
    w: u32,
    h: u32,
) -> (wgpu::Texture, wgpu::TextureView) {
    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("a screen"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let part = |at: u32, of: u32| ((at as f32 + 0.5) / of as f32 * 255.0).round() as u8;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[part(x, w), part(y, h), 128, 255]);
        }
    }
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        size,
    );
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// A screen capture shown centred looks the same when its capture comes
/// bigger — as it does while another input of the same screen is zoomed
/// into: the whole screen still, not its middle enlarged. Drawn by the
/// deck's own pass.
#[test]
fn a_centred_capture_looks_the_same_when_it_comes_bigger() {
    a_centred_capture_comes_bigger(false);
}

/// And shrunk by its Transform: drawn by the pass that draws a picture
/// into its place.
#[test]
fn a_centred_capture_shrunk_looks_the_same_when_it_comes_bigger() {
    a_centred_capture_comes_bigger(true);
}

/// A screen's input, centred — `shrunk` to half its size by a Transform,
/// or as it is — shown its screen as it comes fitted, then twice as big:
/// the same picture both times.
fn a_centred_capture_comes_bigger(shrunk: bool) {
    use varda::deck::{Deck, Effect, ScalingMode, ScreenCaptureState};
    use varda::params::ParamValue::Float;
    use varda::screen_capture::backend::{CaptureConfig, TargetIdentity};
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let show: (u32, u32) = (64, 36);
    let twice = (show.0 * 2, show.1 * 2);
    let fitted = a_screen(&gpu, show.0, show.1);
    let bigger = a_screen(&gpu, twice.0, twice.1);
    let audio = varda::audio::AudioData::default();
    let modulation = varda::modulation::ModulationEngine::new();
    let effect = |name: &str| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("shaders")
            .join(format!("{name}.fs"));
        let shader = varda::isf::ISFShader::from_file(path).expect("the shader");
        Effect::new(&gpu, shader).expect("an effect")
    };
    let shows = |deck: &mut Deck, view: &wgpu::TextureView, size: (u32, u32)| {
        deck.external_source_view = Some(view.clone());
        deck.set_external_source_size(size.0, size.1);
        let mut cmds = Vec::new();
        deck.render(&gpu, &audio, &modulation, 0, &mut cmds)
            .expect("drawn");
        gpu.submit(cmds);
        assert!(deck.gpu_error().is_none(), "drawn without a fault");
        frame_pixels(&gpu, &deck.texture)
    };
    // The part of the frame compared, clear of the picture's edges: all of
    // it, or the half-size picture in its middle.
    let (across, down) = if shrunk {
        (18..46u32, 11..25u32)
    } else {
        (2..62, 2..34)
    };
    let state = ScreenCaptureState {
        capture_id: 1,
        identity: TargetIdentity::Display {
            label: "Display 1".into(),
            uuid: None,
        },
        config: CaptureConfig {
            scale_to: Some(show),
            ..Default::default()
        },
        given: Vec::new(),
        listed_as: None,
        presentation: None,
    };
    let mut deck =
        Deck::new_from_screen_capture(&gpu, state, "Display 1", show.0, show.1, show.0, show.1)
            .expect("a screen's input");
    deck.set_scaling_mode(ScalingMode::Center);
    if shrunk {
        deck.add_effect(effect("crop"));
        deck.add_effect(effect("transform"));
        deck.effects[1].params.set("scale", Float(0.5));
    }
    let before = shows(&mut deck, &fitted.1, show);
    let after = shows(&mut deck, &bigger.1, twice);
    let at = |x: u32, y: u32| (y * show.0 + x) as usize;
    let (left, right) = (at(across.start, 18), at(across.end - 1, 18));
    assert!(
        before[right][0] - before[left][0] > 0.5,
        "shrunk {shrunk}: the whole screen across, {:?} to {:?}",
        before[left],
        before[right]
    );
    let mut furthest = 0.0f32;
    for y in down {
        for x in across.clone() {
            for part in 0..4 {
                let apart = (before[at(x, y)][part] - after[at(x, y)][part]).abs();
                furthest = furthest.max(apart);
            }
        }
    }
    assert!(
        furthest < 0.03,
        "shrunk {shrunk}: the picture changed by {furthest} when it came bigger"
    );
}

/// A screen `w` × `h` as its capture brings it, each pixel as `pixel` says
/// (sRGB): its texture, kept while its views are shown, its view as light
/// and its view as it was coded (see `ScreenCaptureManager::coded_view`).
fn a_coded_screen(
    gpu: &varda::renderer::context::GpuContext,
    w: u32,
    h: u32,
    pixel: impl Fn(u32, u32) -> [u8; 4],
) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
    let size = wgpu::Extent3d {
        width: w,
        height: h,
        depth_or_array_layers: 1,
    };
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("a coded screen"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
    });
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&pixel(x, y));
        }
    }
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        size,
    );
    let light = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let coded = texture.create_view(&wgpu::TextureViewDescriptor {
        format: Some(wgpu::TextureFormat::Rgba8Unorm),
        ..Default::default()
    });
    (texture, light, coded)
}

/// A screen capture drawn smaller is averaged as it was coded, where its
/// input has its view as coded: lines black and white by turns, two to a
/// pixel, come out the grey half-way between them as coded — as dark as
/// anything else that shrinks a picture makes them — not the lighter grey
/// half-way between them as light. Drawn by the deck's own pass.
#[test]
fn a_capture_drawn_smaller_is_averaged_as_it_was_coded() {
    a_capture_comes_smaller(false);
}

/// And shrunk by its Transform, four lines to a pixel: drawn by the pass
/// that draws a picture into its place.
#[test]
fn a_capture_shrunk_is_averaged_as_it_was_coded() {
    a_capture_comes_smaller(true);
}

/// A screen's input, fitted — `shrunk` to half its size by a Transform, or
/// as it is — shown a screen twice the show's size, its columns black and
/// white by turns: read as coded, then as light.
fn a_capture_comes_smaller(shrunk: bool) {
    use varda::deck::{Deck, Effect, ScalingMode, ScreenCaptureState};
    use varda::params::ParamValue::Float;
    use varda::screen_capture::backend::{CaptureConfig, TargetIdentity};
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    if !gpu.reads_a_texture_two_ways() {
        eprintln!("Skipping: this GPU reads a texture one way");
        return;
    }
    let show: (u32, u32) = (64, 36);
    let twice = (show.0 * 2, show.1 * 2);
    let (_screen, light, coded) = a_coded_screen(&gpu, twice.0, twice.1, |x, _| {
        if x % 2 == 0 {
            [255; 4]
        } else {
            [0, 0, 0, 255]
        }
    });
    let audio = varda::audio::AudioData::default();
    let modulation = varda::modulation::ModulationEngine::new();
    let effect = |name: &str| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("shaders")
            .join(format!("{name}.fs"));
        let shader = varda::isf::ISFShader::from_file(path).expect("the shader");
        Effect::new(&gpu, shader).expect("an effect")
    };
    let shows = |deck: &mut Deck, coded: Option<&wgpu::TextureView>| {
        deck.external_source_view = Some(light.clone());
        deck.external_source_coded_view = coded.cloned();
        deck.set_external_source_size(twice.0, twice.1);
        let mut cmds = Vec::new();
        deck.render(&gpu, &audio, &modulation, 0, &mut cmds)
            .expect("drawn");
        gpu.submit(cmds);
        assert!(deck.gpu_error().is_none(), "drawn without a fault");
        frame_pixels(&gpu, &deck.texture)
    };
    // The part of the frame looked at, clear of the picture's edges: all of
    // it, or the half-size picture in its middle.
    let (across, down) = if shrunk {
        (18..46u32, 11..25u32)
    } else {
        (2..62, 2..34)
    };
    let state = ScreenCaptureState {
        capture_id: 1,
        identity: TargetIdentity::Display {
            label: "Display 1".into(),
            uuid: None,
        },
        config: CaptureConfig {
            scale_to: Some(show),
            ..Default::default()
        },
        given: Vec::new(),
        listed_as: None,
        presentation: None,
    };
    let mut deck =
        Deck::new_from_screen_capture(&gpu, state, "Display 1", show.0, show.1, show.0, show.1)
            .expect("a screen's input");
    deck.set_scaling_mode(ScalingMode::Fit);
    if shrunk {
        deck.add_effect(effect("crop"));
        deck.add_effect(effect("transform"));
        deck.effects[1].params.set("scale", Float(0.5));
    }
    let as_coded = shows(&mut deck, Some(&coded));
    let as_light = shows(&mut deck, None);
    // Half-way between black and white as coded (sRGB 0.5), as light.
    let grey = ((0.5f32 + 0.055) / 1.055).powf(2.4);
    let at = |x: u32, y: u32| (y * show.0 + x) as usize;
    for y in down {
        for x in across.clone() {
            for part in 0..3 {
                let (dark, pale) = (as_coded[at(x, y)][part], as_light[at(x, y)][part]);
                assert!(
                    (dark - grey).abs() < 0.02,
                    "shrunk {shrunk}, ({x}, {y}): {dark} read as coded, not {grey}"
                );
                assert!(
                    (pale - 0.5).abs() < 0.02,
                    "shrunk {shrunk}, ({x}, {y}): {pale} read as light"
                );
            }
        }
    }
}

/// A window an odd pixel narrower and lower than the show, centred, is
/// drawn a pixel for a pixel: half a pixel off, as centred exactly, each
/// pixel drawn was a blend of two (a window 1499 × 827 centred in a 4K
/// show, measured on an M5 Max: four fifths of its edges' contrast gone).
#[test]
fn a_centred_capture_an_odd_pixel_smaller_is_drawn_pixel_for_pixel() {
    // The frame's pixel (x, y) shows the screen's (x − 1, y − 1).
    drawn_pixel_for_pixel(varda::deck::ScalingMode::Center, (63, 35), true);
}

/// A window as wide as the show and an odd pixel lower — a window made as
/// big as its screen, under the menu bar — fitted: its bars whole pixels.
#[test]
fn a_fitted_capture_as_wide_as_the_show_is_drawn_pixel_for_pixel() {
    // (x, y) shows the screen's (x, y − 1).
    drawn_pixel_for_pixel(varda::deck::ScalingMode::Fit, (64, 35), false);
}

/// And one an odd pixel higher, filling the show: whole pixels cut.
#[test]
fn a_filling_capture_as_wide_as_the_show_is_drawn_pixel_for_pixel() {
    // (x, y) shows the screen's (x, y).
    drawn_pixel_for_pixel(varda::deck::ScalingMode::Fill, (64, 37), true);
}

/// A screen's input shown by `mode`, in a show of 64 × 36, a screen
/// `source` big that it draws a pixel for a pixel — squares a pixel big,
/// white and black by turns, its first one white: each pixel of the frame
/// is one of the screen's, white or black (`first_is_white`: the frame's
/// first one would be white), none the grey of two blended. Read as coded
/// and as light; drawn by the deck's own pass, then moved four pixels
/// across by a Transform — the squares as they were — by the pass that
/// draws a picture into its place.
fn drawn_pixel_for_pixel(mode: varda::deck::ScalingMode, source: (u32, u32), first_is_white: bool) {
    use varda::deck::{Deck, Effect, ScreenCaptureState};
    use varda::params::ParamValue::Float;
    use varda::screen_capture::backend::{CaptureConfig, TargetIdentity};
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    if !gpu.reads_a_texture_two_ways() {
        eprintln!("Skipping: this GPU reads a texture one way");
        return;
    }
    let show: (u32, u32) = (64, 36);
    let (_screen, light, coded) = a_coded_screen(&gpu, source.0, source.1, |x, y| {
        if (x + y) % 2 == 0 {
            [255; 4]
        } else {
            [0, 0, 0, 255]
        }
    });
    let audio = varda::audio::AudioData::default();
    let modulation = varda::modulation::ModulationEngine::new();
    for (as_coded, moved) in [(true, false), (false, false), (true, true), (false, true)] {
        let state = ScreenCaptureState {
            capture_id: 1,
            identity: TargetIdentity::Display {
                label: "Display 1".into(),
                uuid: None,
            },
            config: CaptureConfig {
                scale_to: Some(show),
                ..Default::default()
            },
            given: Vec::new(),
            listed_as: None,
            presentation: None,
        };
        let mut deck =
            Deck::new_from_screen_capture(&gpu, state, "Display 1", show.0, show.1, show.0, show.1)
                .expect("a screen's input");
        deck.set_scaling_mode(mode);
        if moved {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("shaders")
                .join("transform.fs");
            let shader = varda::isf::ISFShader::from_file(path).expect("the shader");
            deck.add_effect(Effect::new(&gpu, shader).expect("an effect"));
            // Four of the show's 64 pixels: the squares fall where they did.
            deck.effects[0].params.set("pos_x", Float(0.0625));
        }
        deck.external_source_view = Some(light.clone());
        deck.external_source_coded_view = as_coded.then(|| coded.clone());
        deck.set_external_source_size(source.0, source.1);
        let mut cmds = Vec::new();
        deck.render(&gpu, &audio, &modulation, 0, &mut cmds)
            .expect("drawn");
        gpu.submit(cmds);
        assert!(deck.gpu_error().is_none(), "drawn without a fault");
        let frame = frame_pixels(&gpu, &deck.texture);
        // Clear of the picture's edges, moved or not.
        for y in 2..34u32 {
            for x in 8..56u32 {
                let white = ((x + y) % 2 == 0) == first_is_white;
                let want = if white { 1.0 } else { 0.0 };
                for &got in &frame[(y * show.0 + x) as usize][..3] {
                    assert!(
                        (got - want).abs() < 0.02,
                        "{mode:?} {source:?}, as coded {as_coded}, moved {moved}, ({x}, {y}): {got}, not {want}"
                    );
                }
            }
        }
    }
}

/// A capture's picture has a number: none before its first frame, another
/// with each frame put in its texture, the same while none is — what is
/// drawn from it need not be drawn again then (`keep_still_inputs`).
#[test]
fn a_captures_picture_has_another_number_with_each_frame() {
    use varda::screen_capture::backend::{mock_targets, CaptureConfig};
    use varda::screen_capture::ScreenCaptureManager;
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut captures = ScreenCaptureManager::new_disabled();
    let target = mock_targets().remove(0);
    let config = CaptureConfig {
        scale_to: Some((64, 36)),
        ..Default::default()
    };
    let (id, _, _) = captures
        .open_mock(&target, config, &gpu.device)
        .expect("open");
    assert_eq!(captures.picture(id), None, "no picture yet: no number");
    let needed = std::collections::HashSet::from([id]);
    let nothing = std::collections::HashSet::new();
    // Frames put in till the number is another than `than`.
    let another = |captures: &mut ScreenCaptureManager, than: Option<u64>| {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while captures.picture(id) == than && std::time::Instant::now() < until {
            captures.update_selective(&gpu, &needed, varda::host_clock::host_seconds());
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        captures.picture(id)
    };
    let first = another(&mut captures, None);
    assert!(first.is_some(), "a picture: a number");
    assert!(captures.has_picture(id), "with its picture");
    // No frame put in — not looked at: the same picture, the same number.
    for _ in 0..5 {
        captures.update_selective(&gpu, &nothing, varda::host_clock::host_seconds());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(captures.picture(id), first, "nothing put in: the same");
    let second = another(&mut captures, first);
    assert!(second.is_some() && second != first, "another frame: another number");
    let third = another(&mut captures, second);
    assert!(
        third.is_some() && third != second && third != first,
        "and another: no number twice"
    );
    captures.release(id);
}

/// A capture's own two views — as light, and as it was coded — show the
/// same picture at its own size: the one as coded reads its code values,
/// which the pass that draws it makes light once. Read as light already
/// they were made light twice, and the picture dark.
#[test]
fn a_captures_view_as_coded_shows_what_its_view_as_light_does() {
    use varda::deck::{Deck, ScalingMode, ScreenCaptureState};
    use varda::screen_capture::backend::{mock_targets, CaptureConfig};
    use varda::screen_capture::ScreenCaptureManager;
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    if !gpu.reads_a_texture_two_ways() {
        eprintln!("Skipping: this GPU reads a texture one way");
        return;
    }
    let show: (u32, u32) = (64, 36);
    let mut captures = ScreenCaptureManager::new_disabled();
    let target = mock_targets().remove(0);
    let config = CaptureConfig {
        scale_to: Some(show),
        ..Default::default()
    };
    let (id, w, h) = captures
        .open_mock(&target, config.clone(), &gpu.device)
        .expect("open");
    assert_eq!((w, h), show);
    let needed = std::collections::HashSet::from([id]);
    let until = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !captures.has_picture(id) && std::time::Instant::now() < until {
        captures.update_selective(&gpu, &needed, varda::host_clock::host_seconds());
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(captures.has_picture(id), "a picture");
    let light = captures.texture_view(id).expect("its view").clone();
    let coded = captures.coded_view(id).expect("its view as coded").clone();
    assert_ne!(light, coded, "another view of it");

    let audio = varda::audio::AudioData::default();
    let modulation = varda::modulation::ModulationEngine::new();
    let state = ScreenCaptureState {
        capture_id: id,
        identity: target.identity(),
        config,
        given: Vec::new(),
        listed_as: None,
        presentation: None,
    };
    let mut deck =
        Deck::new_from_screen_capture(&gpu, state, &target.label, w, h, show.0, show.1)
            .expect("a screen's input");
    deck.set_scaling_mode(ScalingMode::Fit);
    let mut shows = |coded: Option<&wgpu::TextureView>| {
        deck.external_source_view = Some(light.clone());
        deck.external_source_coded_view = coded.cloned();
        deck.set_external_source_size(w, h);
        let mut cmds = Vec::new();
        deck.render(&gpu, &audio, &modulation, 0, &mut cmds)
            .expect("drawn");
        gpu.submit(cmds);
        assert!(deck.gpu_error().is_none(), "drawn without a fault");
        frame_pixels(&gpu, &deck.texture)
    };
    let as_light = shows(None);
    let as_coded = shows(Some(&coded));
    let brightest = as_light.iter().map(|pixel| pixel[1]).fold(0.0f32, f32::max);
    assert!(brightest > 0.5, "a picture to tell by: {brightest}");
    let mut furthest = 0.0f32;
    for (one, other) in as_light.iter().zip(&as_coded) {
        for part in 0..4 {
            furthest = furthest.max((one[part] - other[part]).abs());
        }
    }
    assert!(
        furthest < 0.01,
        "read as coded the picture is {furthest} from itself read as light"
    );
    captures.release(id);
}
