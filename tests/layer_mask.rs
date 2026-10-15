//! A layer's own mask (Input Settings → Mask): a shape cut out of the
//! layer, so what is under it shows through — or, inverted, only the shape
//! kept — its edge feathered without darkening; carried by one effect, kept
//! last in the layer's chain.

use std::sync::mpsc;

use varda::app::VardaApp;
use varda::engine::value::mask::{EffectMask, MaskShape, LAYER_MASK_EFFECT};
use varda::engine::{CommandResult, EffectTarget, EngineCommand};
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

fn ellipse(feather: f32, invert: bool) -> EffectMask {
    EffectMask {
        shape: MaskShape::Ellipse {
            x: 0.3,
            y: 0.3,
            width: 0.4,
            height: 0.4,
        },
        feather,
        invert,
    }
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

#[test]
fn a_layers_mask_cuts_a_hole_that_shows_what_is_under_it() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, red) = blue_under_red(&mut app);
    let at = |pixels: &[[f32; 4]], w: u32, h: u32, x: f32, y: f32| {
        pixels[((y * h as f32) as u32 * w + (x * w as f32) as u32) as usize]
    };
    let (pixels, w, h) = picture(&mut app);
    assert!(at(&pixels, w, h, 0.5, 0.5)[0] > 0.95, "red all over at first");

    id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red.clone(),
            mask: Some(ellipse(0.0, false)),
        },
    ));
    let (pixels, w, h) = picture(&mut app);
    let middle = at(&pixels, w, h, 0.5, 0.5);
    assert!(middle[2] > 0.95 && middle[0] < 0.05, "blue through the hole: {middle:?}");
    let corner = at(&pixels, w, h, 0.1, 0.1);
    assert!(corner[0] > 0.95 && corner[2] < 0.05, "red round it: {corner:?}");

    id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red,
            mask: Some(ellipse(0.0, true)),
        },
    ));
    let (pixels, w, h) = picture(&mut app);
    let middle = at(&pixels, w, h, 0.5, 0.5);
    assert!(middle[0] > 0.95, "inverted, only the shape kept: {middle:?}");
    let corner = at(&pixels, w, h, 0.1, 0.1);
    assert!(corner[2] > 0.95 && corner[0] < 0.05, "blue round it: {corner:?}");
}

/// Across a feathered edge the layer fades out — red giving way to the blue
/// under it, never darker in between, as layers are straight alpha.
#[test]
fn a_feathered_hole_fades_the_layer_out_without_darkening() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, red) = blue_under_red(&mut app);
    id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red,
            mask: Some(ellipse(120.0, false)),
        },
    ));
    let (pixels, w, h) = picture(&mut app);
    let row = h / 2;
    let mut faded = 0;
    for x in 0..w / 2 {
        let p = pixels[(row * w + x) as usize];
        if p[0] > 0.2 && p[0] < 0.8 {
            faded += 1;
            assert!((p[0] + p[2] - 1.0).abs() < 0.03, "at {x}: {p:?}");
        }
    }
    assert!(faded > 10, "a soft edge: {faded} pixels between");
}

/// Undo puts back the mask as it was before a change, not just whether
/// there was one.
#[test]
fn undo_puts_a_changed_mask_back() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, red) = blue_under_red(&mut app);
    let first = ellipse(0.0, false);
    for mask in [first.clone(), ellipse(80.0, true)] {
        id(send_cmd(
            &mut app,
            EngineCommand::SetLayerMask {
                deck_uuid: red.clone(),
                mask: Some(mask),
            },
        ));
    }
    send_cmd(&mut app, EngineCommand::Undo);
    let state = app.build_engine_state();
    let carrier = state
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == red)
        .and_then(|d| d.effects.iter().find(|e| e.name == LAYER_MASK_EFFECT))
        .expect("still masked");
    assert_eq!(carrier.mask, Some(first));
}

/// However the chain is changed — an effect moved after it, one pasted in,
/// a copy of the mask itself pasted — the layer keeps one mask, last. One
/// with no shape does nothing; a second is refused.
#[test]
fn a_layers_mask_stays_one_and_last_however_the_chain_changes() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, red) = blue_under_red(&mut app);
    // Added as an effect, before it has a shape: the layer still shows.
    let carrier = id(send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: EffectTarget::Deck(red.clone()),
            shader_name: LAYER_MASK_EFFECT.into(),
        },
    ));
    let (pixels, w, h) = picture(&mut app);
    assert!(
        pixels[((h / 2) * w + w / 2) as usize][0] > 0.95,
        "a mask with no shape does nothing"
    );
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::AddEffect {
                target: EffectTarget::Deck(red.clone()),
                shader_name: LAYER_MASK_EFFECT.into(),
            },
        ),
        CommandResult::Err { .. }
    ));
    assert_eq!(
        id(send_cmd(
            &mut app,
            EngineCommand::SetLayerMask {
                deck_uuid: red.clone(),
                mask: Some(ellipse(0.0, false)),
            },
        )),
        carrier,
        "the one already there"
    );
    let invert = id(send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: EffectTarget::Deck(red.clone()),
            shader_name: "invert".into(),
        },
    ));
    let last = chain(&app, &red).len() - 1;
    let at = chain(&app, &red).iter().position(|n| n == "invert").unwrap();
    send_cmd(
        &mut app,
        EngineCommand::MoveEffect {
            target: EffectTarget::Deck(red.clone()),
            from_idx: at,
            to_idx: last,
        },
    );
    assert_eq!(
        chain(&app, &red).last().map(String::as_str),
        Some(LAYER_MASK_EFFECT),
        "an effect moved after it"
    );
    for source in [invert, carrier] {
        send_cmd(
            &mut app,
            EngineCommand::Copy {
                source: varda::engine::ClipboardSource::Effect(source),
                include_arrangement: false,
            },
        );
        send_cmd(
            &mut app,
            EngineCommand::Paste {
                target: varda::engine::PasteTarget::IntoChain(EffectTarget::Deck(red.clone())),
            },
        );
    }
    let names = chain(&app, &red);
    assert_eq!(names.iter().filter(|n| *n == LAYER_MASK_EFFECT).count(), 1, "{names:?}");
    assert_eq!(names.last().map(String::as_str), Some(LAYER_MASK_EFFECT), "{names:?}");
    send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red.clone(),
            mask: None,
        },
    );
    assert!(!chain(&app, &red).iter().any(|n| n == LAYER_MASK_EFFECT));
}

/// The mask is one effect, kept last as other effects are added; set again
/// it is the same effect; taken away, it is gone. A mask with no area, or
/// for no such layer, is refused.
#[test]
fn a_layers_mask_is_one_effect_kept_last() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, red) = blue_under_red(&mut app);
    let carrier = id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red.clone(),
            mask: Some(ellipse(0.0, false)),
        },
    ));
    assert_eq!(chain(&app, &red).last().map(String::as_str), Some(LAYER_MASK_EFFECT));
    let again = id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red.clone(),
            mask: Some(ellipse(40.0, true)),
        },
    ));
    assert_eq!(again, carrier, "the same effect");
    id(send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: EffectTarget::Deck(red.clone()),
            shader_name: "invert".into(),
        },
    ));
    let names = chain(&app, &red);
    assert_eq!(names.iter().filter(|n| *n == LAYER_MASK_EFFECT).count(), 1);
    assert_eq!(names.last().map(String::as_str), Some(LAYER_MASK_EFFECT), "{names:?}");
    assert!(names.contains(&"invert".to_string()));

    let none = EffectMask {
        shape: MaskShape::Rectangle {
            x: 0.5,
            y: 0.5,
            width: 0.0,
            height: 0.2,
        },
        feather: 0.0,
        invert: false,
    };
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetLayerMask {
                deck_uuid: red.clone(),
                mask: Some(none),
            },
        ),
        CommandResult::Err { .. }
    ));
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetLayerMask {
                deck_uuid: "nosuchdk".into(),
                mask: Some(ellipse(0.0, false)),
            },
        ),
        CommandResult::Err { .. }
    ));
    send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: red.clone(),
            mask: None,
        },
    );
    assert!(!chain(&app, &red).iter().any(|n| n == LAYER_MASK_EFFECT));
}
