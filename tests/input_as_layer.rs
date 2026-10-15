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

/// A picture-in-picture with a white border and rounded corners — the crop
/// then the transform — drawn in one pass looks as it did drawn whole,
/// cropped and shrunk: the same over the whole frame but for the odd pixel
/// on an anti-aliased edge.
#[test]
fn a_bordered_rounded_picture_in_picture_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(false, false, false);
}

/// The same, ringed in a circle — a thick coloured ring, whose colour both
/// ways must take alike.
#[test]
fn a_circled_picture_in_picture_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(true, false, false);
}

/// Flipped both ways, with a crop off to one side: the transform mirrors the
/// cropped picture — its kept part and border with it — the same in one pass
/// as in two, and a point of it shows the corner mirrored there.
#[test]
fn a_flipped_picture_in_picture_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(false, true, false);
}

/// A layer not keeping its input's transparency, the input clear in a
/// corner: drawn in one pass, it is flattened over black there as it was
/// drawn whole over black, then cropped and shrunk — black, not the layer
/// under it, the same both ways.
#[test]
fn a_picture_in_picture_flattened_over_black_drawn_in_one_pass_looks_the_same() {
    drawn_in_one_pass_looks_the_same(false, false, true);
}

/// Two copies of one picture-in-picture, the first drawn in one pass and the
/// second the old way — its crop modulated, at no depth, so the pass drawing
/// the picture leaves it to the passes after: whole, then cropped, then
/// shrunk. Flattened: the picture clear in its top right quarter, neither
/// layer keeping its transparency.
fn drawn_in_one_pass_looks_the_same(circle: bool, flip: bool, flattened: bool) {
    use varda::params::ParamValue::{Bool, Color, Float};
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
    assert!(one_pass < 0.2, "drawn in its place: {one_pass}");
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
