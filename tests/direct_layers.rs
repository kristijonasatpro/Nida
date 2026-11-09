//! Layers that are only their live source placed — another input (a tap),
//! a camera, a screen; cropped, bordered, moved, scaled — drawn straight onto
//! their input from that source, not into a frame of their own the input then
//! reads (`Channel::direct_layers`): at 4K that frame is ~130 MB written and
//! read again a layer each frame. The picture is the same either way.

use std::sync::mpsc;

use varda::app::VardaApp;
use varda::channel::BlendMode;
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

fn ok(result: &CommandResult) {
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
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

/// An app with two inputs and nothing in them, Main in Program mode: the
/// inputs' ids.
fn two_inputs() -> Option<(VardaApp, Vec<String>)> {
    let gpu = common::headless_gpu()?;
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    ok(&send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));
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
    while app.build_engine_state().mixer.channels.len() < 2 {
        send_cmd(&mut app, EngineCommand::AddChannel);
    }
    let channels = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|c| c.uuid.clone())
        .collect();
    Some((app, channels))
}

/// A layer in `channel` showing input `of`.
fn tap(app: &mut VardaApp, channel: &str, of: &str) -> String {
    id(send_cmd(
        app,
        EngineCommand::AddTapDeck {
            channel_uuid: channel.to_string(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: of.to_string(),
            },
        },
    ))
}

/// `layer`'s effect called `name`, by UUID.
fn effect(app: &VardaApp, layer: &str, name: &str) -> String {
    let state = app.build_engine_state();
    let deck = state
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == layer)
        .expect("the layer");
    deck.effects
        .iter()
        .find(|e| e.name == name)
        .expect("its effect")
        .uuid
        .clone()
}

fn set(app: &mut VardaApp, effect: &str, name: &str, value: varda::params::ParamValue) {
    ok(&send_cmd(
        app,
        EngineCommand::SetEffectParam {
            effect_uuid: effect.to_string(),
            name: name.into(),
            value,
        },
    ));
}

fn direct(app: &mut VardaApp, on: bool) {
    ok(&send_cmd(
        app,
        EngineCommand::SetCompositor {
            direct_layers: Some(on),
            merge_layer_passes: None,
        },
    ));
}

/// Input 2's picture now, and the layers drawn onto it from their sources.
fn second(app: &mut VardaApp) -> (Vec<[f32; 4]>, usize) {
    app.render_mixer_frame();
    app.render_mixer_frame();
    let channel = &app.mixer_ref().channels()[1];
    (
        frame_pixels(app.gpu_context(), &channel.composite_texture),
        channel.drawn_onto,
    )
}

/// The show: input 1 a picture with a clear corner; input 2 a blue
/// background under four layers of input 1 — whole and see-through at 0.8,
/// cropped with a rounded white border and placed small, placed and Added
/// at 0.6, and whole, flattened over black, Screened at 0.9. The layers'
/// UUIDs.
fn wall(app: &mut VardaApp, channels: &[String], dir: &std::path::Path) -> Vec<String> {
    use varda::params::ParamValue::{Bool, Color, Float};
    let picture = dir.join("quarters.png");
    image::RgbaImage::from_fn(64, 36, |x, y| match (x < 32, y < 18) {
        (true, true) => image::Rgba([255, (x * 6) as u8, 0, 255]),
        (false, true) => image::Rgba([0, 255, 0, 0]),
        (true, false) => image::Rgba([255, 255, (y * 7) as u8, 160]),
        (false, false) => image::Rgba([0, 255, 255, 255]),
    })
    .save(&picture)
    .unwrap();
    id(send_cmd(
        app,
        EngineCommand::AddImageDeck {
            channel_uuid: channels[0].clone(),
            path: picture,
        },
    ));
    id(send_cmd(
        app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: channels[1].clone(),
            color: [0.1, 0.2, 0.8, 1.0],
        },
    ));
    let layers: Vec<String> = (0..4).map(|_| tap(app, &channels[1], &channels[0])).collect();
    ok(&send_cmd(
        app,
        EngineCommand::SetDeckOpacity {
            deck_uuid: layers[0].clone(),
            opacity: 0.8,
        },
    ));
    ok(&send_cmd(
        app,
        EngineCommand::SetDeckTransparent {
            deck_uuid: layers[0].clone(),
            transparent: true,
        },
    ));
    let (crop, transform) = (effect(app, &layers[1], "crop"), effect(app, &layers[1], "transform"));
    for (effect, name, value) in [
        (&crop, "crop_left", Float(0.1)),
        (&crop, "crop_right", Float(0.9)),
        (&crop, "crop_bottom", Float(0.05)),
        (&crop, "crop_top", Float(0.95)),
        (&crop, "border_mode", Bool(true)),
        (&crop, "border_width", Float(25.0)),
        (&crop, "border_color", Color([1.0, 1.0, 1.0, 1.0])),
        (&crop, "border_radius", Float(155.0)),
        (&transform, "scale", Float(0.4)),
        (&transform, "pos_x", Float(0.22)),
        (&transform, "pos_y", Float(0.18)),
    ] {
        set(app, effect, name, value);
    }
    let transform = effect(app, &layers[2], "transform");
    for (name, value) in [("scale", 0.5), ("pos_x", -0.2), ("pos_y", -0.15)] {
        set(app, &transform, name, Float(value));
    }
    for (layer, mode, opacity) in [(&layers[2], BlendMode::Add, 0.6), (&layers[3], BlendMode::Screen, 0.9)] {
        ok(&send_cmd(
            app,
            EngineCommand::SetDeckBlendMode {
                deck_uuid: layer.clone(),
                mode,
            },
        ));
        ok(&send_cmd(
            app,
            EngineCommand::SetDeckOpacity {
                deck_uuid: layer.clone(),
                opacity,
            },
        ));
    }
    ok(&send_cmd(
        app,
        EngineCommand::SetDeckTransparent {
            deck_uuid: layers[3].clone(),
            transparent: false,
        },
    ));
    send_cmd(
        app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[1].clone()),
            fade: false,
        },
    );
    layers
}

/// The most two pictures differ by anywhere, relative to the colour there:
/// a layer's own frame rounds its picture to half floats on the way.
fn furthest(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter()
        .zip(b)
        .flat_map(|(p, q)| p.iter().zip(q).map(|(x, y)| (x - y).abs() / y.abs().max(1.0)))
        .fold(0.0, f32::max)
}

/// Four layers of another input — whole, cropped and bordered and placed,
/// Added, Screened, see-through and flattened — drawn from their source
/// look as they did drawn into their own frames, but for the half-float
/// rounding those frames did; and none of the four draws a frame of its own.
#[test]
fn layers_drawn_from_their_source_look_as_drawn_into_frames_of_their_own() {
    let Some((mut app, channels)) = two_inputs() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    wall(&mut app, &channels, dir.path());
    let (from_source, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 4, "all four drawn from their source");
    for slot in app.mixer_ref().channels()[1].decks.iter().skip(1) {
        assert!(slot.deck.fps() > 0.0, "its rate still shows: {}", slot.deck.fps());
    }
    direct(&mut app, false);
    let (own_frames, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 0, "each into its own frame");
    let costs = |app: &VardaApp| -> Vec<f32> {
        app.mixer_ref().channels()[1]
            .decks
            .iter()
            .skip(1)
            .map(|slot| slot.render_cost_us)
            .collect()
    };
    assert!(costs(&app).iter().all(|&us| us > 0.0), "their frames cost: {:?}", costs(&app));
    let lightest = own_frames.iter().map(|p| p[1]).fold(0.0, f32::max);
    let darkest = own_frames.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
    assert!(lightest - darkest > 0.3, "a picture, not a wash: {darkest}..{lightest}");
    let off = furthest(&from_source, &own_frames);
    assert!(off < 2e-3, "the same picture: {off} apart at most");
    // Drawn from their source again, what their own frames cost is not
    // counted against the frame's time any more (`decks_over_budget`).
    direct(&mut app, true);
    second(&mut app);
    assert!(costs(&app).iter().all(|&us| us == 0.0), "{:?}", costs(&app));
}

/// A layer whose own frame something else shows — its thumbnail on screen,
/// or it being the layer selected (edited, shown alone) — draws that frame;
/// the rest are still drawn from their source.
#[test]
fn a_layer_whose_frame_is_shown_draws_it() {
    let Some((mut app, channels)) = two_inputs() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let layers = wall(&mut app, &channels, dir.path());
    app.set_previewed_decks(vec![layers[1].clone()]);
    let (_, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 3, "its thumbnail on screen: its own frame");
    app.set_previewed_decks(Vec::new());
    let at = app.mixer_ref().channels()[1]
        .decks
        .iter()
        .position(|slot| slot.deck.uuid() == layers[2])
        .unwrap();
    app.set_ui_selected_deck(Some((1, at)));
    let (_, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 3, "selected: its own frame");
}

/// A layer drawn from its source and then not — compared, its thumbnail
/// shown, set to a rate of its own — draws its own frame that very frame,
/// though it now draws once a second: else its frame, last drawn before,
/// shows a picture long gone.
#[test]
fn a_layer_back_in_its_own_frame_draws_it_at_once() {
    let Some((mut app, channels)) = two_inputs() else {
        return;
    };
    let colour = id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: channels[0].clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let layer = tap(&mut app, &channels[1], &channels[0]);
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[1].clone()),
            fade: false,
        },
    );
    let (red, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 1);
    assert!(red[0][0] > 0.9 && red[0][1] < 0.1, "red: {:?}", red[0]);
    ok(&send_cmd(
        &mut app,
        EngineCommand::SetDeckColor {
            deck_uuid: colour,
            color: [0.0, 1.0, 0.0, 1.0],
        },
    ));
    ok(&send_cmd(
        &mut app,
        EngineCommand::SetDeckRenderFps {
            deck_uuid: layer,
            render_fps: varda::channel::DeckRenderFps::Fixed(1),
        },
    ));
    app.render_mixer_frame();
    assert_eq!(app.mixer_ref().channels()[1].drawn_onto, 0, "held at its own rate");
    let channel = &app.mixer_ref().channels()[1];
    let green = frame_pixels(app.gpu_context(), &channel.composite_texture);
    assert!(
        green[0][1] > 0.9 && green[0][0] < 0.1,
        "this frame's green, not a frame from before: {:?}",
        green[0]
    );
}

/// A layer drawn at a rate of its own holds its picture between its frames,
/// so it draws them: drawn from its live source it would move every frame,
/// and its rate change as its thumbnail came and went (Codex).
#[test]
fn a_layer_held_at_a_rate_of_its_own_draws_its_frames() {
    let Some((mut app, channels)) = two_inputs() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let layers = wall(&mut app, &channels, dir.path());
    ok(&send_cmd(
        &mut app,
        EngineCommand::SetDeckRenderFps {
            deck_uuid: layers[1].clone(),
            render_fps: varda::channel::DeckRenderFps::Fixed(15),
        },
    ));
    let (_, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 3, "the one held at 15 a second draws its frames");
}

/// A layer drawn from its source, then muted — no longer shown, its own
/// frame never drawn — draws that frame once, for its thumbnail: else the
/// thumbnail stays empty, as nothing draws a muted layer (Codex).
#[test]
fn a_layer_muted_after_being_drawn_from_its_source_draws_its_frame_once() {
    let Some((mut app, channels)) = two_inputs() else {
        return;
    };
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: channels[0].clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let layer = tap(&mut app, &channels[1], &channels[0]);
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channels[1].clone()),
            fade: false,
        },
    );
    let (_, drawn_onto) = second(&mut app);
    assert_eq!(drawn_onto, 1, "drawn from its source");
    ok(&send_cmd(
        &mut app,
        EngineCommand::SetDeckMute {
            deck_uuid: layer.clone(),
            mute: true,
        },
    ));
    app.render_mixer_frame();
    let mixer = app.mixer_ref();
    let slot = mixer.channels()[1]
        .decks
        .iter()
        .find(|slot| slot.deck.uuid() == layer)
        .unwrap();
    let own = frame_pixels(app.gpu_context(), &slot.deck.texture);
    assert!(own[0][0] > 0.9 && own[0][3] > 0.9, "its own frame, red: {:?}", own[0]);
}
