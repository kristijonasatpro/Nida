//! Round-trip command tests verifying the Step 5 delegation layer.
//!
//! Each test sends a command through `execute_command()` via the command
//! channel, then verifies the resulting state through `build_engine_state()`.

use varda::app::{AppConfig, VardaApp};
use varda::engine::types::DeckRenderScale;
use varda::engine::{CommandResult, EngineCommand, ErrorCode};

use clap::Parser;

mod common;

fn parse_args(args: &[&str]) -> AppConfig {
    AppConfig::parse_from(std::iter::once("varda").chain(args.iter().copied()))
}

fn headless_app() -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    let config = varda::testing::headless_config();
    // Once a GPU exists, a construction failure is a bug, not a reason to skip.
    Some(VardaApp::new(gpu, &config).expect("VardaApp::new"))
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

/// The UUID a creating command reports (see ui-engine-boundary.md WS1).
fn new_uuid(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    }
}

/// UUID of the channel currently at `idx`.
fn channel_uuid(app: &mut VardaApp, idx: usize) -> String {
    app.build_engine_state().mixer.channels[idx].uuid.clone()
}

// ── Surface Commands ────────────────────────────────────────────────

#[test]
fn surface_add_and_remove_roundtrip() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let r = send_cmd(
        &mut app,
        EngineCommand::AddSurface {
            name: "Test Surface".into(),
            source: varda::renderer::context::OutputSource::Master,
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    let state = app.build_engine_state();
    assert!(state
        .outputs
        .surfaces
        .iter()
        .any(|s| s.name == "Test Surface"));
    let uuid = state
        .outputs
        .surfaces
        .iter()
        .find(|s| s.name == "Test Surface")
        .unwrap()
        .uuid
        .clone();
    let r = send_cmd(&mut app, EngineCommand::RemoveSurface { uuid });
    assert!(matches!(r, CommandResult::Ok));
    let state = app.build_engine_state();
    assert!(!state
        .outputs
        .surfaces
        .iter()
        .any(|s| s.name == "Test Surface"));
}

#[test]
fn surface_duplicate_roundtrip() {
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::AddSurface {
            name: "Original".into(),
            source: varda::renderer::context::OutputSource::Master,
        },
    );
    let uuid = app
        .build_engine_state()
        .outputs
        .surfaces
        .iter()
        .find(|s| s.name == "Original")
        .unwrap()
        .uuid
        .clone();
    let r = send_cmd(&mut app, EngineCommand::DuplicateSurface { uuid });
    assert!(matches!(r, CommandResult::OkWithId { .. }));
    let state = app.build_engine_state();
    // Should have original + duplicate
    let originals: Vec<_> = state
        .outputs
        .surfaces
        .iter()
        .filter(|s| s.name.starts_with("Original"))
        .collect();
    assert!(originals.len() >= 2);
}

#[test]
fn surface_flip_and_move() {
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::AddSurface {
            name: "Flip Test".into(),
            source: varda::renderer::context::OutputSource::Master,
        },
    );
    let uuid = app
        .build_engine_state()
        .outputs
        .surfaces
        .iter()
        .find(|s| s.name == "Flip Test")
        .unwrap()
        .uuid
        .clone();
    let r = send_cmd(
        &mut app,
        EngineCommand::FlipSurfaceHorizontal { uuid: uuid.clone() },
    );
    assert!(matches!(r, CommandResult::Ok));
    let r = send_cmd(
        &mut app,
        EngineCommand::FlipSurfaceVertical { uuid: uuid.clone() },
    );
    assert!(matches!(r, CommandResult::Ok));
    let r = send_cmd(
        &mut app,
        EngineCommand::MoveSurface {
            uuid,
            dx: 0.1,
            dy: -0.1,
        },
    );
    assert!(matches!(r, CommandResult::Ok));
}

#[test]
fn surface_reorder_roundtrip() {
    use varda::surface::SurfaceReorderOp;
    let Some(mut app) = headless_app() else {
        return;
    };
    for name in ["A", "B", "C"] {
        send_cmd(
            &mut app,
            EngineCommand::AddSurface {
                name: name.into(),
                source: varda::renderer::context::OutputSource::Master,
            },
        );
    }
    let order = |app: &mut VardaApp| -> Vec<String> {
        app.build_engine_state()
            .outputs
            .surfaces
            .iter()
            .map(|s| s.name.clone())
            .collect()
    };
    assert_eq!(order(&mut app), vec!["A", "B", "C"]);

    let a = app
        .build_engine_state()
        .outputs
        .surfaces
        .iter()
        .find(|s| s.name == "A")
        .unwrap()
        .uuid
        .clone();

    // Index 0 = bottom, last = top. Bring A to front.
    let r = send_cmd(
        &mut app,
        EngineCommand::ReorderSurface {
            uuid: a.clone(),
            op: SurfaceReorderOp::ToFront,
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    assert_eq!(order(&mut app), vec!["B", "C", "A"]);

    // Nudge A down one step (toward back).
    let r = send_cmd(
        &mut app,
        EngineCommand::ReorderSurface {
            uuid: a,
            op: SurfaceReorderOp::Down,
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    assert_eq!(order(&mut app), vec!["B", "A", "C"]);

    // Unknown surface → NotFound.
    let r = send_cmd(
        &mut app,
        EngineCommand::ReorderSurface {
            uuid: "does-not-exist".into(),
            op: SurfaceReorderOp::ToBack,
        },
    );
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

// ── Sequence Commands ───────────────────────────────────────────────

#[test]
fn sequence_full_lifecycle() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch_a = channel_uuid(&mut app, 0);
    let ch_b = channel_uuid(&mut app, 1);
    let seq = new_uuid(send_cmd(&mut app, EngineCommand::CreateSequence));
    assert_eq!(app.build_engine_state().mixer.sequences.len(), 1);

    // Add steps
    send_cmd(
        &mut app,
        EngineCommand::AddFadeStep {
            sequence_uuid: seq.clone(),
            from_channel_uuid: ch_a,
            to_channel_uuid: ch_b,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::AddWaitStep {
            sequence_uuid: seq.clone(),
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::AddGoToStep {
            sequence_uuid: seq.clone(),
            step_index: 0,
        },
    );
    assert_eq!(app.build_engine_state().mixer.sequences[0].steps.len(), 3);

    // Remove middle step
    send_cmd(
        &mut app,
        EngineCommand::RemoveStep {
            sequence_uuid: seq.clone(),
            step_idx: 1,
        },
    );
    assert_eq!(app.build_engine_state().mixer.sequences[0].steps.len(), 2);

    // Move step
    send_cmd(
        &mut app,
        EngineCommand::MoveStep {
            sequence_uuid: seq.clone(),
            from: 0,
            to: 1,
        },
    );

    // Delete sequence
    let r = send_cmd(
        &mut app,
        EngineCommand::DeleteSequence {
            sequence_uuid: seq.clone(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    let state = app.build_engine_state();
    assert_eq!(state.mixer.sequences.len(), 0);
    assert!(state.mixer.sequences.iter().all(|s| s.uuid != seq));
}

#[test]
fn sequence_unknown_uuid_returns_not_found() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch_a = channel_uuid(&mut app, 0);
    let ch_b = channel_uuid(&mut app, 1);
    let r = send_cmd(
        &mut app,
        EngineCommand::DeleteSequence {
            sequence_uuid: "no-such-sequence".into(),
        },
    );
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    let r = send_cmd(
        &mut app,
        EngineCommand::AddFadeStep {
            sequence_uuid: "no-such-sequence".into(),
            from_channel_uuid: ch_a,
            to_channel_uuid: ch_b,
        },
    );
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

// ── Output Commands ─────────────────────────────────────────────────

#[test]
fn headless_output_create_and_stop() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let r = send_cmd(
        &mut app,
        EngineCommand::CreateHeadlessOutput {
            target: varda::renderer::context::OutputTarget::NdiSend {
                sender_name: "Test NDI".into(),
            },
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    // Verify engine state can be built after the headless output command.
    // Headless outputs (e.g. NDI send) do not necessarily appear in the
    // windows list, so we only assert the state builds without panicking.
    let _state = app.build_engine_state();
}

#[test]
fn headless_output_syphon_create_and_start() {
    let Some(mut app) = headless_app() else {
        return;
    };
    // Create a headless output targeting a Syphon server — the same path the
    // API takes for `POST /api/outputs/headless` with a SyphonServer target.
    // This proves the API is co-equal with the UI's Syphon protocol dropdown.
    let r = send_cmd(
        &mut app,
        EngineCommand::CreateHeadlessOutput {
            target: varda::renderer::context::OutputTarget::SyphonServer {
                server_name: "Test Syphon".into(),
            },
        },
    );
    assert!(matches!(r, CommandResult::Ok));

    // Starting the output activates the publisher on macOS; on other platforms
    // it must be rejected with Unavailable, mirroring the Syphon receive deck
    // path (cmd_add_syphon_deck).
    let output_uuid = app
        .build_engine_state()
        .outputs
        .windows
        .last()
        .expect("headless output created")
        .uuid
        .clone();
    let r = send_cmd(&mut app, EngineCommand::StartOutput { output_uuid });
    #[cfg(target_os = "macos")]
    assert!(matches!(r, CommandResult::Ok));
    #[cfg(not(target_os = "macos"))]
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::Unavailable,
            ..
        }
    ));

    let _state = app.build_engine_state();
}

// ── Stream Library Commands ─────────────────────────────────────────

#[test]
fn hls_library_add_remove() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let r = send_cmd(
        &mut app,
        EngineCommand::AddHlsLibraryEntry {
            url: "http://example.com/stream.m3u8".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    // Add duplicate — should be idempotent
    send_cmd(
        &mut app,
        EngineCommand::AddHlsLibraryEntry {
            url: "http://example.com/stream.m3u8".into(),
        },
    );
    // Remove
    let r = send_cmd(
        &mut app,
        EngineCommand::RemoveHlsLibraryEntry {
            url: "http://example.com/stream.m3u8".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
}

// ── DeckSource Kind Verification ────────────────────────────────────

fn deck_effect_names(app: &VardaApp, deck_uuid: &str) -> Vec<String> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck_uuid)
        .expect("deck exists")
        .effects
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

#[test]
fn a_new_deck_starts_with_crop_then_transform() {
    // Framing a source — crop it, then place it — is the first thing an
    // operator does with a new input, so every new deck comes with both.
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let solid = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let shader = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddDeck {
            channel_uuid: ch,
            shader_name: "plasma".into(),
        },
    ));
    for deck in [solid, shader] {
        assert_eq!(
            deck_effect_names(&app, &deck),
            ["crop", "transform"],
            "crop first, then transform"
        );
    }
}

/// Read back an `Rgba16Float` texture as linear-light RGBA.
fn read_rgba16f(ctx: &varda::renderer::context::GpuContext, tex: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (tex.width(), tex.height());
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = (w * 8).div_ceil(align) * align;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("fidelity readback"),
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    ctx.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| drop(tx.send(r)));
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().expect("map channel").expect("map ok");
    let data = buffer.slice(..).get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = (y * padded + x * 8) as usize;
            let c = |i: usize| {
                half::f16::from_le_bytes([data[p + 2 * i], data[p + 2 * i + 1]]).to_f32()
            };
            out.push([c(0), c(1), c(2), c(3)]);
        }
    }
    out
}

/// Linear light to an 8-bit sRGB code, as an sRGB output target writes it.
fn srgb8(linear: f32) -> u8 {
    let c = linear.clamp(0.0, 1.0);
    let encoded = if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

#[test]
fn a_full_resolution_still_reaches_the_output_pixel_exact() {
    // A slide made at the output resolution must come out exactly as it is in
    // the file: image decode → a new deck (with the crop and transform every
    // deck now carries) → channel → master under the default tonemap. Measured
    // before: ACES changed every pixel (mean 8.9 of 255, no pure white left)
    // and crop darkened the frame's outermost pixels.
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 1920,
            height: 1080,
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("slide.png");
    // Every code value, hard edges between neighbours, and both ends of the
    // range — right out to the frame's edges.
    let slide = image::RgbaImage::from_fn(1920, 1080, |x, y| {
        let stripe = if (x / 3 + y / 5) % 2 == 0 { 255 } else { 0 };
        image::Rgba([
            ((x * 37 + y * 11) % 256) as u8,
            ((x ^ y) % 256) as u8,
            stripe,
            255,
        ])
    });
    slide.save(&path).unwrap();
    let ch = channel_uuid(&mut app, 0);
    let deck = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: ch,
            path,
        },
    ));
    assert_eq!(deck_effect_names(&app, &deck), ["crop", "transform"]);
    send_cmd(&mut app, EngineCommand::SetCrossfader(0.0));
    for _ in 0..3 {
        app.render_mixer_frame();
    }

    let out = read_rgba16f(app.gpu_context(), app.mixer_ref().composite_texture());
    let mut wrong = 0usize;
    let mut first = None;
    for (i, px) in out.iter().enumerate() {
        let (x, y) = (i as u32 % 1920, i as u32 / 1920);
        let want = slide.get_pixel(x, y);
        let got = [srgb8(px[0]), srgb8(px[1]), srgb8(px[2])];
        if got != [want[0], want[1], want[2]] {
            wrong += 1;
            first.get_or_insert((x, y, got, [want[0], want[1], want[2]]));
        }
    }
    assert_eq!(
        wrong, 0,
        "{wrong} pixels differ from the file; first (x, y, got, file): {first:?}"
    );
}

/// A PNG layer is reported still — its rate counts redraws, not motion — and
/// a colour layer, drawn every frame, is not. The FPS popover leaves stills
/// out; averaged in, one idle image made a moving channel read 48.
#[test]
fn an_image_layer_is_reported_still_and_a_colour_layer_is_not() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("background.png");
    image::RgbaImage::from_pixel(64, 36, image::Rgba([10, 200, 90, 255]))
        .save(&path)
        .unwrap();
    let ch = channel_uuid(&mut app, 0);
    let image = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: ch.clone(),
            path,
        },
    ));
    let colour = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    app.render_mixer_frame();
    let state = app.build_engine_state();
    let still = |uuid: &str| {
        state.mixer.channels[0]
            .decks
            .iter()
            .find(|d| d.uuid == uuid)
            .map(|d| d.still)
            .expect("layer in the snapshot")
    };
    assert!(still(&image), "a PNG is drawn again only when it changes");
    assert!(!still(&colour), "a colour layer is drawn every frame");
}

/// Size of the texture the deck at `idx` of channel 0 renders into.
fn deck_texture_size(app: &VardaApp, idx: usize) -> (u32, u32) {
    let deck = &app.mixer_ref().channels()[0].decks[idx].deck;
    (deck.texture.width(), deck.texture.height())
}

#[test]
fn a_half_resolution_deck_draws_a_quarter_of_the_pixels_and_still_fills_the_frame() {
    // A generator at 4K computes 8.3 million pixels a frame; at Half it draws
    // a quarter of them and is scaled up in the mix. The deck must still cover
    // the whole frame, keep its fraction when the master changes, and keep it
    // across a save and reload.
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = headless_app_in(dir.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    assert_eq!(deck_texture_size(&app, 0), (1920, 1080));
    let r = send_cmd(
        &mut app,
        EngineCommand::SetDeckRenderScale {
            deck_uuid: deck.clone(),
            scale: DeckRenderScale::Half,
        },
    );
    assert!(matches!(r, CommandResult::Ok), "{r:?}");
    assert_eq!(
        deck_texture_size(&app, 0),
        (960, 540),
        "half width and height"
    );
    let state = app.build_engine_state();
    assert_eq!(
        state.mixer.channels[0].decks[0].render_scale,
        DeckRenderScale::Half
    );

    send_cmd(&mut app, EngineCommand::SetCrossfader(0.0));
    for _ in 0..2 {
        app.render_mixer_frame();
    }
    let out = read_rgba16f(app.gpu_context(), app.mixer_ref().composite_texture());
    for (x, y) in [(0, 0), (1919, 0), (0, 1079), (1919, 1079), (960, 540)] {
        let px = out[y * 1920 + x];
        assert!(
            px[2] > 0.99 && px[0] < 0.01,
            "({x}, {y}) is {px:?}: the deck must fill the frame"
        );
    }

    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 3840,
            height: 2160,
        },
    );
    assert_eq!(
        deck_texture_size(&app, 0),
        (1920, 1080),
        "still half of a 4K master"
    );

    assert!(matches!(
        send_cmd(&mut app, EngineCommand::SaveWorkspace),
        CommandResult::Ok
    ));
    drop(app);
    let mut app = headless_app_in(dir.path()).expect("second app");
    let _ = app.load_workspace();
    let state = app.build_engine_state();
    assert_eq!(
        state.mixer.channels[0].decks[0].render_scale,
        DeckRenderScale::Half
    );
    assert_eq!(
        deck_texture_size(&app, 0),
        (1920, 1080),
        "restored at half of 4K"
    );
}

#[test]
fn solid_color_deck_source_kind() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let state = app.build_engine_state();
    assert_eq!(state.mixer.channels[0].decks.len(), 1);
    assert_eq!(state.mixer.channels[0].decks[0].uuid, deck);
    // Solid color deck name is the hex color, not "Solid Color"
    assert!(!state.mixer.channels[0].decks[0].name.is_empty());
}

// ── Presets & ToggleParam ───────────────────────────────────────────

fn headless_app_in(dir: &std::path::Path) -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    let ws = dir.to_str().unwrap();
    let config = parse_args(&[
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        // See `varda::testing::headless_config`: enumeration raises the OS
        // camera prompt on every construction, and this suite constructs many.
        "--no-camera",
        "--workspace",
        ws,
    ]);
    // Once a GPU exists, a construction failure is a bug, not a reason to skip.
    Some(VardaApp::new(gpu, &config).expect("VardaApp::new"))
}

#[test]
fn toggle_param_flips_crossfader() {
    let Some(mut app) = headless_app() else {
        return;
    };
    // Default crossfader is 0.0; toggling snaps it to the opposite extreme (1.0).
    let r = send_cmd(
        &mut app,
        EngineCommand::ToggleParam {
            path: "crossfader".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    assert!((app.build_engine_state().mixer.crossfader - 1.0).abs() < 1e-5);
    // Toggling again snaps back to 0.0.
    send_cmd(
        &mut app,
        EngineCommand::ToggleParam {
            path: "crossfader".into(),
        },
    );
    assert!(app.build_engine_state().mixer.crossfader.abs() < 1e-5);
}

#[test]
fn load_deck_preset_unknown_name_errors() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let r = send_cmd(
        &mut app,
        EngineCommand::LoadDeckPreset {
            channel_uuid: ch,
            preset_name: "no-such-preset".into(),
        },
    );
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

#[test]
fn load_deck_preset_unknown_channel_errors() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let r = send_cmd(
        &mut app,
        EngineCommand::LoadDeckPreset {
            channel_uuid: "no-such-channel".into(),
            preset_name: "red".into(),
        },
    );
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

#[test]
fn load_channel_preset_unknown_name_errors() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let r = send_cmd(
        &mut app,
        EngineCommand::LoadChannelPreset {
            target_channel_uuid: None,
            preset_name: "no-such-preset".into(),
        },
    );
    assert!(matches!(
        r,
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

#[test]
fn deck_preset_save_then_load_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = headless_app_in(dir.path()) else {
        return;
    };
    // Create a source deck to save.
    let ch = channel_uuid(&mut app, 0);
    let deck = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    assert_eq!(app.build_engine_state().mixer.channels[0].decks.len(), 1);
    // Save it as a preset (writes to the temp workspace + refreshes the library).
    let r = send_cmd(
        &mut app,
        EngineCommand::SaveDeckPreset {
            deck_uuid: deck,
            name: "red".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    // Load the saved preset back into channel 0 → a second deck is appended.
    let r = send_cmd(
        &mut app,
        EngineCommand::LoadDeckPreset {
            channel_uuid: ch,
            preset_name: "red".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    assert_eq!(app.build_engine_state().mixer.channels[0].decks.len(), 2);
}

#[test]
fn channel_preset_save_then_load_appends_channel() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = headless_app_in(dir.path()) else {
        return;
    };
    // Populate channel 0 with a deck, then save the channel as a preset.
    let ch = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch.clone(),
            color: [0.0, 1.0, 0.0, 1.0],
        },
    );
    let r = send_cmd(
        &mut app,
        EngineCommand::SaveChannelPreset {
            channel_uuid: ch,
            name: "green".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    let channels_before = app.build_engine_state().mixer.channels.len();
    // Load with no target → a new channel is appended carrying the preset's deck.
    let r = send_cmd(
        &mut app,
        EngineCommand::LoadChannelPreset {
            target_channel_uuid: None,
            preset_name: "green".into(),
        },
    );
    assert!(matches!(r, CommandResult::Ok));
    let state = app.build_engine_state();
    assert_eq!(state.mixer.channels.len(), channels_before + 1);
    assert_eq!(state.mixer.channels[channels_before].decks.len(), 1);
}

// ── Headless Render Smoke ───────────────────────────────────────────

#[test]
fn headless_render_smoke() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [0.0, 1.0, 0.0, 1.0],
        },
    );
    for _ in 0..10 {
        app.update_frame_timing();
        app.render_mixer_frame();
    }
    let state = app.build_engine_state();
    assert!(state.fps >= 0.0);
    // frame_count is incremented during render_outputs, not render_mixer_frame in headless
    // Just verify no crash and FPS is valid
}

// ── Production Bus Commands ─────────────────────────────────────────

use varda::bus::BusInputRef;

/// Add a bus and return its UUID.
fn add_bus(app: &mut VardaApp, name: &str, width: u32, height: u32) -> String {
    new_uuid(send_cmd(
        app,
        EngineCommand::AddBus {
            name: name.to_string(),
            width,
            height,
        },
    ))
}

#[test]
fn add_bus_creates_a_bus_with_its_own_canvas() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let led = add_bus(&mut app, "LED", 1920, 480);
    let bus = app.production_buses().get(&led).expect("bus should exist");
    assert_eq!(bus.name, "LED");
    assert_eq!(bus.canvas().width, 1920);
    assert_eq!(bus.canvas().height, 480);
}

#[test]
fn a_bus_command_naming_one_bus_never_switches_another() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let stream = add_bus(&mut app, "STREAM", 1920, 1080);
    let led = add_bus(&mut app, "LED", 1920, 480);
    let tv = add_bus(&mut app, "TV", 1920, 1080);
    let channel = channel_uuid(&mut app, 0);

    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: led.clone(),
            input: BusInputRef::Channel(channel.clone()),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: led.clone() });

    let buses = app.production_buses();
    assert_eq!(
        buses.get(&led).unwrap().program(),
        &BusInputRef::Channel(channel),
        "LED should be on air"
    );
    for (name, uuid) in [("STREAM", &stream), ("TV", &tv)] {
        assert_eq!(
            buses.get(uuid).unwrap().program(),
            &BusInputRef::Master,
            "{name} must not follow LED: still on Main, as made"
        );
    }
}

#[test]
fn resizing_one_bus_canvas_leaves_the_others_alone() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let led = add_bus(&mut app, "LED", 1920, 1080);
    let tv = add_bus(&mut app, "TV", 1920, 1080);

    send_cmd(
        &mut app,
        EngineCommand::SetBusCanvas {
            uuid: led.clone(),
            width: 1408,
            height: 768,
        },
    );

    assert_eq!(
        app.production_buses().get(&led).unwrap().canvas().width,
        1408
    );
    assert_eq!(
        app.production_buses().get(&tv).unwrap().canvas().height,
        1080,
        "TV canvas must be untouched"
    );
}

#[test]
fn resizing_a_placed_bus_region_keeps_its_placement() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let led = add_bus(&mut app, "LED", 1920, 1080);
    send_cmd(
        &mut app,
        EngineCommand::SetBusCanvasRegion {
            uuid: led.clone(),
            width: 1536,
            height: 432,
            origin_x: 192,
            origin_y: 40,
        },
    );

    send_cmd(
        &mut app,
        EngineCommand::SetBusCanvas {
            uuid: led.clone(),
            width: 1280,
            height: 400,
        },
    );

    let canvas = app.production_buses().get(&led).unwrap().canvas();
    assert_eq!((canvas.width, canvas.height), (1280, 400));
    assert_eq!(
        (canvas.origin_x, canvas.origin_y),
        (192, 40),
        "a resize must not move the region back to the master's corner"
    );
}

#[test]
fn bus_auto_starts_a_transition_that_does_not_settle_immediately() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "STREAM", 1920, 1080);
    let channel = channel_uuid(&mut app, 0);

    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel),
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::BusAuto {
            uuid: bus.clone(),
            duration_secs: Some(10.0),
        },
    );

    let live = app.production_buses().get(&bus).unwrap();
    assert!(live.is_transitioning());
    assert_eq!(
        live.program(),
        &BusInputRef::Master,
        "program stays outgoing until the transition completes"
    );
}

#[test]
fn a_bus_mid_auto_feeds_a_dissolve_between_its_two_ends() {
    // AUTO used to hold the old picture and then cut. On air it must fade: the
    // feed names both ends and how far between them the bus is.
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "TV", 1920, 1080);
    let ch_a = channel_uuid(&mut app, 0);
    let ch_b = channel_uuid(&mut app, 1);
    for input in [ch_a, ch_b] {
        send_cmd(
            &mut app,
            EngineCommand::SetBusPreview {
                uuid: bus.clone(),
                input: BusInputRef::Channel(input),
            },
        );
        send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    }
    // Program is B, and after the flip-flop A is back on Preview.
    send_cmd(
        &mut app,
        EngineCommand::BusAuto {
            uuid: bus.clone(),
            duration_secs: Some(60.0),
        },
    );

    let feeds = app.bus_feeds();
    match feeds.get(&bus).map(|f| &f.picture) {
        Some(varda::app::FeedPicture::Dissolve { from, to, mix }) => {
            assert_eq!(
                from,
                &Some(varda::engine::OutputSource::Channel(1)),
                "fading out of B"
            );
            assert_eq!(
                to,
                &Some(varda::engine::OutputSource::Channel(0)),
                "fading into A"
            );
            assert!(
                (0.0..0.1).contains(mix),
                "a minute-long AUTO has barely started, got mix {mix}"
            );
        }
        other => panic!("a bus mid-AUTO must feed a dissolve, got {other:?}"),
    }
}

fn surfaces_following(app: &VardaApp, bus: &str) -> Vec<Vec<[f32; 2]>> {
    app.build_engine_state()
        .outputs
        .surfaces
        .into_iter()
        .filter(|s| s.source == varda::engine::OutputSource::BusProgram(bus.to_string()))
        .map(|s| s.vertices)
        .collect()
}

#[test]
fn showing_a_bus_on_a_screen_makes_one_full_frame_output_and_then_moves_it() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "TV", 1920, 1080);
    let show = |monitor: Option<&str>| EngineCommand::ShowBusOnScreen {
        uuid: bus.clone(),
        monitor_name: monitor.map(str::to_string),
    };

    match send_cmd(&mut app, show(Some("HDMI-A-1"))) {
        CommandResult::OkWithId { .. } => {}
        other => panic!("showing a bus should open its output: {other:?}"),
    }
    assert_eq!(app.bus_screen(&bus).as_deref(), Some("HDMI-A-1"));
    let following = surfaces_following(&app, &bus);
    assert_eq!(following.len(), 1, "one surface carries the bus");
    assert_eq!(
        following[0],
        vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
        "and it fills the whole screen"
    );

    send_cmd(&mut app, show(Some("HDMI-A-2")));
    assert_eq!(
        app.bus_screen(&bus).as_deref(),
        Some("HDMI-A-2"),
        "a second call moves the bus"
    );
    assert_eq!(
        surfaces_following(&app, &bus).len(),
        1,
        "rather than putting it on a second screen"
    );

    send_cmd(&mut app, show(None));
    assert_eq!(app.bus_screen(&bus), None, "and None takes it off screen");
}

#[test]
fn commands_naming_an_unknown_bus_report_not_found() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let missing = "nosuchid".to_string();
    let commands = vec![
        EngineCommand::BusCut {
            uuid: missing.clone(),
        },
        EngineCommand::BusAuto {
            uuid: missing.clone(),
            duration_secs: None,
        },
        EngineCommand::ShowBusOnScreen {
            uuid: missing.clone(),
            monitor_name: Some("HDMI-A-1".into()),
        },
        EngineCommand::RemoveBus {
            uuid: missing.clone(),
        },
        EngineCommand::RenameBus {
            uuid: missing.clone(),
            name: "x".into(),
        },
        EngineCommand::SetBusPreview {
            uuid: missing.clone(),
            input: BusInputRef::Black,
        },
        EngineCommand::SetBusCanvas {
            uuid: missing.clone(),
            width: 1920,
            height: 1080,
        },
        EngineCommand::SetBusAutoDuration {
            uuid: missing.clone(),
            duration_secs: 2.0,
        },
    ];
    for cmd in commands {
        let label = format!("{cmd:?}");
        match send_cmd(&mut app, cmd) {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                ..
            } => {}
            other => panic!("{label} should be NotFound, got {other:?}"),
        }
    }
}

#[test]
fn a_non_positive_auto_duration_is_rejected() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "STREAM", 1920, 1080);
    for duration in [0.0, -1.0, f32::NAN] {
        match send_cmd(
            &mut app,
            EngineCommand::SetBusAutoDuration {
                uuid: bus.clone(),
                duration_secs: duration,
            },
        ) {
            CommandResult::Err {
                code: ErrorCode::InvalidInput,
                ..
            } => {}
            other => panic!("duration {duration} should be rejected, got {other:?}"),
        }
    }
}

#[test]
fn removing_a_bus_leaves_the_remaining_buses_addressable() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let a = add_bus(&mut app, "A", 1920, 1080);
    let b = add_bus(&mut app, "B", 1920, 1080);

    send_cmd(&mut app, EngineCommand::RemoveBus { uuid: a.clone() });

    assert!(app.production_buses().get(&a).is_none());
    assert!(
        app.production_buses().get(&b).is_some(),
        "removing A must not disturb B"
    );
}

#[test]
fn a_second_auto_during_a_take_is_refused() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "STREAM", 1920, 1080);
    let channel = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel),
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::BusAuto {
            uuid: bus.clone(),
            duration_secs: Some(30.0),
        },
    );
    match send_cmd(
        &mut app,
        EngineCommand::BusAuto {
            uuid: bus,
            duration_secs: Some(30.0),
        },
    ) {
        CommandResult::Err {
            code: ErrorCode::Unavailable,
            ..
        } => {}
        other => panic!("a second AUTO should be refused, got {other:?}"),
    }
}

#[test]
fn an_auto_with_a_non_positive_duration_is_rejected_not_cut() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "STREAM", 1920, 1080);
    let channel = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel),
        },
    );
    match send_cmd(
        &mut app,
        EngineCommand::BusAuto {
            uuid: bus.clone(),
            duration_secs: Some(0.0),
        },
    ) {
        CommandResult::Err {
            code: ErrorCode::InvalidInput,
            ..
        } => {}
        other => panic!("a zero-duration AUTO should be rejected, got {other:?}"),
    }
    assert_eq!(
        app.production_buses().get(&bus).unwrap().program(),
        &BusInputRef::Master,
        "a rejected take must not silently put the source on air"
    );
}

/// A bus with Black taken to air — the state a bus feeds nothing in.
fn black_bus(app: &mut VardaApp, name: &str, width: u32, height: u32) -> String {
    let bus = add_bus(app, name, width, height);
    send_cmd(
        app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Black,
        },
    );
    send_cmd(app, EngineCommand::BusCut { uuid: bus.clone() });
    assert_eq!(app.production_buses().get(&bus).unwrap().program(), &BusInputRef::Black);
    bus
}

/// A new bus shows Main — on Preview and Program — until something else is
/// taken to it: sent to a screen, it shows the show, not black.
#[test]
fn a_new_bus_shows_main_until_something_else_is_taken() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "LED", 1920, 480);
    let made = app.production_buses().get(&bus).unwrap().clone();
    assert_eq!(made.program(), &BusInputRef::Master);
    assert_eq!(made.preview(), &BusInputRef::Master);
}

// ── Stage 2: destinations follow buses ──────────────────────────────

#[test]
fn a_bus_feed_resolves_to_whatever_that_bus_has_on_air() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = black_bus(&mut app, "LED", 1920, 480);
    let channel = channel_uuid(&mut app, 0);

    // Black on air: the bus contributes no feed, so a destination
    // following it reads black rather than some other source.
    assert!(
        !app.bus_feeds().contains_key(&bus),
        "a black bus must not resolve to any source"
    );

    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel.clone()),
        },
    );
    assert!(
        !app.bus_feeds().contains_key(&bus),
        "staging alone must not change what the bus feeds — only PGM is on air"
    );

    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    let feeds = app.bus_feeds();
    assert!(
        matches!(
            feeds.get(&bus).map(|f| &f.picture),
            Some(varda::app::FeedPicture::Source(
                varda::engine::OutputSource::Channel(0)
            ))
        ),
        "after the cut the bus feeds its program channel"
    );
}

#[test]
fn a_bus_feed_follows_its_bus_and_no_other() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let led = add_bus(&mut app, "LED", 1920, 480);
    let tv = black_bus(&mut app, "TV", 1920, 1080);
    let channel = channel_uuid(&mut app, 0);

    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: led.clone(),
            input: BusInputRef::Channel(channel),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: led.clone() });

    let feeds = app.bus_feeds();
    assert!(feeds.contains_key(&led), "LED is on air");
    assert!(
        !feeds.contains_key(&tv),
        "cutting LED must not give TV a feed"
    );
}

#[test]
fn a_bus_feed_disappears_when_its_source_stops_resolving() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "LED", 1920, 480);
    // A third channel, because the engine keeps a minimum of two and would
    // refuse to remove either of the defaults.
    let channel = new_uuid(send_cmd(&mut app, EngineCommand::AddChannel));

    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel.clone()),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    assert!(app.bus_feeds().contains_key(&bus), "on air to begin with");

    match send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: channel,
        },
    ) {
        CommandResult::Ok => {}
        other => panic!("removal should succeed: {other:?}"),
    }

    assert!(
        !app.bus_feeds().contains_key(&bus),
        "a route whose source is gone must feed nothing — never whatever now \
         occupies that index"
    );
    assert_eq!(
        app.production_buses()
            .get(&bus)
            .unwrap()
            .effective_program(),
        &BusInputRef::Black,
        "and the bus reads black"
    );
}

#[test]
fn a_bus_keeps_its_channel_drawn_when_the_crossfader_is_away() {
    // The mixer skips a channel the crossfader has faded out. A bus shows its
    // channel whatever the crossfader says, so that channel must still be
    // drawn — on real hardware the destination read black.
    let Some(mut app) = headless_app() else {
        return;
    };
    let ch_b = channel_uuid(&mut app, 1);
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch_b.clone(),
            color: [0.0, 1.0, 0.0, 1.0],
        },
    );
    app.render_mixer_frame();
    let state = app.build_engine_state();
    assert!(
        state.mixer.crossfader < 0.001 && state.mixer.channels[1].active_deck_count == 0,
        "precondition: the crossfader is on A and faded-out channel B is skipped \
         (crossfader {}, B drew {} decks)",
        state.mixer.crossfader,
        state.mixer.channels[1].active_deck_count
    );

    let tv = add_bus(&mut app, "TV", 1920, 1080);
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: tv.clone(),
            input: BusInputRef::Channel(ch_b),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: tv });
    app.render_mixer_frame();

    assert!(
        app.build_engine_state().mixer.channels[1].active_deck_count > 0,
        "channel B is on air on the TV bus, so it must be drawn"
    );
}

#[test]
fn a_channel_on_a_bus_gets_the_same_tonemap_as_the_main_output() {
    // Destinations sourcing a channel directly read its tonemapped copy; one
    // following a bus showed the raw composite, so the same channel looked
    // different on the bus's screen than in the main output.
    let Some(mut app) = headless_app() else {
        return;
    };
    // Off by default; a curve has to be chosen for there to be a copy at all.
    send_cmd(
        &mut app,
        EngineCommand::SetTonemapMode(varda::renderer::tonemap::TonemapMode::Aces),
    );
    let bus = add_bus(&mut app, "TV", 1920, 1080);
    let ch_b = channel_uuid(&mut app, 1);
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(ch_b),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus });

    app.render_mixer_frame();
    app.render_outputs();

    assert!(
        app.mixer_ref().get_tonemapped_channel_view(1).is_some(),
        "channel B is on air on a bus, so it must be tonemapped for it"
    );
}

#[test]
fn a_full_frame_bus_stays_full_frame_when_the_resolution_changes() {
    // Buses are regions of the master in pixels. Going to 4K left a 1920x1080
    // bus showing only the top-left quarter of the new picture.
    let Some(mut app) = headless_app() else {
        return;
    };
    let tv = add_bus(&mut app, "TV", 1920, 1080);
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 3840,
            height: 2160,
        },
    );
    let canvas = app.production_buses().get(&tv).unwrap().canvas();
    assert_eq!((canvas.width, canvas.height), (3840, 2160));
    assert_eq!(canvas.uv(3840, 2160), ([1.0, 1.0], [0.0, 0.0]));
}

#[test]
fn picking_a_resolution_turns_auto_off() {
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolutionAuto { enabled: true },
    );
    assert!(app.render_auto());
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 1920,
            height: 1080,
        },
    );
    assert!(
        !app.render_auto(),
        "a picked size is not followed away from"
    );
}

#[test]
fn auto_resolution_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let Some(mut app) = headless_app_in(dir.path()) else {
            return;
        };
        send_cmd(
            &mut app,
            EngineCommand::SetRenderResolutionAuto { enabled: true },
        );
        assert!(matches!(
            send_cmd(&mut app, EngineCommand::SaveWorkspace),
            CommandResult::Ok
        ));
    }
    // As the runner does on startup.
    let mut app = headless_app_in(dir.path()).expect("second app");
    assert!(
        !app.render_auto(),
        "precondition: a fresh app starts on a fixed size"
    );
    let _ = app.load_workspace();
    assert!(app.render_auto(), "Auto must come back after a restart");
}

/// The latency setting is the show's, like its resolution.
#[test]
fn latency_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    {
        let Some(mut app) = headless_app_in(dir.path()) else {
            return;
        };
        send_cmd(&mut app, EngineCommand::SetFramesInFlight { frames: 2 });
        assert!(matches!(
            send_cmd(&mut app, EngineCommand::SaveWorkspace),
            CommandResult::Ok
        ));
    }
    let mut app = headless_app_in(dir.path()).expect("second app");
    assert_eq!(app.frames_in_flight(), 1, "precondition: a fresh app starts on Low");
    let _ = app.load_workspace();
    assert_eq!(app.frames_in_flight(), 2, "Normal must come back after a restart");
}

#[test]
fn a_bus_region_selects_its_band_of_the_master_raster() {
    // The point of the region model: everything still composites once at the
    // master resolution, and the bus says which rectangle of it is its picture.
    // A 1920x480 band of a 1920x1080 master is those pixels 1:1 — no resampling.
    let Some(mut app) = headless_app() else {
        return;
    };
    let bus = add_bus(&mut app, "LED", 1920, 480);
    let channel = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });

    let feeds = app.bus_feeds();
    let feed = feeds.get(&bus).expect("bus is on air");

    // Full width, 480/1080 of the height, anchored top-left by default.
    assert!((feed.uv_scale[0] - 1.0).abs() < 1e-4, "full master width");
    assert!(
        (feed.uv_scale[1] - 480.0 / 1080.0).abs() < 1e-4,
        "the band's share of the height, got {}",
        feed.uv_scale[1]
    );
    assert!(
        feed.uv_offset[0].abs() < 1e-4 && feed.uv_offset[1].abs() < 1e-4,
        "anchored at the origin, got {:?}",
        feed.uv_offset
    );

    // Move the band down the master; the offset follows.
    send_cmd(
        &mut app,
        EngineCommand::SetBusCanvasRegion {
            uuid: bus.clone(),
            width: 1920,
            height: 480,
            origin_x: 0,
            origin_y: 300,
        },
    );
    let feeds = app.bus_feeds();
    let feed = feeds.get(&bus).expect("still on air");
    assert!(
        (feed.uv_offset[1] - 300.0 / 1080.0).abs() < 1e-4,
        "the band moved down the master, got {}",
        feed.uv_offset[1]
    );
}

// ── Routing: Master on a bus, hidden layers, per-output source ──────

/// Read back an 8-bit sRGB texture — what a headless output sends — as the
/// code values it holds.
fn read_rgba8(ctx: &varda::renderer::context::GpuContext, tex: &wgpu::Texture) -> Vec<[u8; 4]> {
    assert_eq!(
        tex.format(),
        wgpu::TextureFormat::Rgba8UnormSrgb,
        "precondition: an 8-bit output"
    );
    let (w, h) = (tex.width(), tex.height());
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = (w * 4).div_ceil(align) * align;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("output readback"),
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    ctx.submit(Some(encoder.finish()));
    let (tx, rx) = std::sync::mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |r| drop(tx.send(r)));
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().expect("map channel").expect("map ok");
    let data = buffer.slice(..).get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let p = (y * padded + x * 4) as usize;
            out.push([data[p], data[p + 1], data[p + 2], data[p + 3]]);
        }
    }
    out
}

/// A started headless output (NDI: no subprocess to spawn), returning its UUID.
fn started_output(app: &mut VardaApp, name: &str) -> String {
    assert!(matches!(
        send_cmd(
            app,
            EngineCommand::CreateHeadlessOutput {
                target: varda::renderer::context::OutputTarget::NdiSend {
                    sender_name: name.into(),
                },
            },
        ),
        CommandResult::Ok
    ));
    let uuid = app
        .build_engine_state()
        .outputs
        .windows
        .last()
        .expect("output created")
        .uuid
        .clone();
    assert!(matches!(
        send_cmd(
            app,
            EngineCommand::StartOutput {
                output_uuid: uuid.clone()
            }
        ),
        CommandResult::Ok
    ));
    uuid
}

/// Point a headless output at one bus's program.
fn route(app: &mut VardaApp, output: &str, bus: &str) {
    let r = send_cmd(
        app,
        EngineCommand::SetOutputSource {
            output_uuid: output.into(),
            source: varda::engine::OutputSource::BusProgram(bus.into()),
        },
    );
    assert!(matches!(r, CommandResult::Ok), "routing refused: {r:?}");
}

/// Take `input` straight to air on a bus.
fn take(app: &mut VardaApp, bus: &str, input: BusInputRef) {
    send_cmd(
        app,
        EngineCommand::SetBusPreview {
            uuid: bus.into(),
            input,
        },
    );
    send_cmd(app, EngineCommand::BusCut { uuid: bus.into() });
}

/// The pixels a headless output sent this frame.
fn output_pixels(app: &VardaApp, uuid: &str) -> Vec<[u8; 4]> {
    let tex = app
        .outputs_ref()
        .iter()
        .find_map(|o| match o {
            varda::renderer::context::UnifiedOutput::Headless(h) if h.uuid == uuid => {
                Some(&h.texture)
            }
            _ => None,
        })
        .expect("headless output");
    read_rgba8(app.gpu_context(), tex)
}

/// Whether an output pixel is `want`, give or take the output's dither.
fn near(px: [u8; 4], want: [u8; 3]) -> bool {
    (0..3).all(|c| px[c].abs_diff(want[c]) <= 2)
}

fn render_frames(app: &mut VardaApp, n: usize) {
    for _ in 0..n {
        app.render_mixer_frame();
        app.render_outputs();
    }
}

#[test]
fn a_stream_and_a_recording_can_differ_by_one_layer() {
    // vMix's example: the stream carries the lower third, the recording is
    // clean. Both buses take Master; the recording's bus leaves the overlay
    // layer out, and each output follows its own bus.
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 320,
            height: 180,
        },
    );
    // A third channel, so every layer keeps its own opacity (two channels are
    // an A/B crossfade).
    let base = channel_uuid(&mut app, 0);
    let overlay = new_uuid(send_cmd(&mut app, EngineCommand::AddChannel));
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: base,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: overlay.clone(),
            color: [0.0, 1.0, 0.0, 1.0],
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::SetChannelOpacity {
            channel_uuid: overlay.clone(),
            opacity: 0.5,
        },
    );

    let stream = add_bus(&mut app, "STREAM", 320, 180);
    let rec = add_bus(&mut app, "REC", 320, 180);
    take(&mut app, &stream, BusInputRef::Master);
    take(&mut app, &rec, BusInputRef::Master);
    let r = send_cmd(
        &mut app,
        EngineCommand::SetBusLayerHidden {
            uuid: rec.clone(),
            channel_uuid: overlay,
            hidden: true,
        },
    );
    assert!(matches!(r, CommandResult::Ok), "hiding refused: {r:?}");

    let stream_out = started_output(&mut app, "Stream");
    let rec_out = started_output(&mut app, "Rec");
    route(&mut app, &stream_out, &stream);
    route(&mut app, &rec_out, &rec);
    render_frames(&mut app, 3);

    let centre = 90 * 320 + 160;
    let master = read_rgba16f(app.gpu_context(), app.mixer_ref().composite_texture())[centre];
    let master = [srgb8(master[0]), srgb8(master[1]), srgb8(master[2])];
    assert!(
        master[0] > 100 && master[1] > 100,
        "precondition: the master shows red with the green overlay over it, got {master:?}"
    );

    let streamed = output_pixels(&app, &stream_out)[centre];
    let recorded = output_pixels(&app, &rec_out)[centre];
    assert!(
        near(streamed, master),
        "the stream shows Master as it is, overlay and all: stream {streamed:?}, \
         master {master:?}"
    );
    assert!(
        near(recorded, [255, 0, 0]),
        "the recording leaves the overlay out and keeps the rest of Master, got {recorded:?}"
    );
}

#[test]
fn hiding_a_layer_can_show_one_the_crossfader_faded_out() {
    // Two channels are an A/B crossfade. With the fader on B the master never
    // draws A, but a bus showing Master without B shows A — so A has to be
    // drawn for it, or the bus reads a stale or black A.
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 320,
            height: 180,
        },
    );
    let (a, b) = (channel_uuid(&mut app, 0), channel_uuid(&mut app, 1));
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: a,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: b.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    );
    send_cmd(&mut app, EngineCommand::SetCrossfader(1.0));

    let rec = add_bus(&mut app, "REC", 320, 180);
    take(&mut app, &rec, BusInputRef::Master);
    send_cmd(
        &mut app,
        EngineCommand::SetBusLayerHidden {
            uuid: rec.clone(),
            channel_uuid: b,
            hidden: true,
        },
    );
    let out = started_output(&mut app, "Rec");
    route(&mut app, &out, &rec);
    render_frames(&mut app, 3);

    let master = read_rgba16f(app.gpu_context(), app.mixer_ref().composite_texture())[0];
    assert!(
        master[2] > 0.9 && master[0] < 0.1,
        "precondition: the master is all B, got {master:?}"
    );
    let shown = output_pixels(&app, &out)[90 * 320 + 160];
    assert!(
        near(shown, [255, 0, 0]),
        "the bus shows A, which only the bus needs drawn, got {shown:?}"
    );
}

#[test]
fn a_recording_that_follows_a_bus_sends_only_that_bus_region() {
    // A screen following a bus shows the bus's rectangle of the master; a
    // recording or stream following it has to send the same rectangle, not
    // the whole master.
    let Some(mut app) = headless_app() else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 320,
            height: 180,
        },
    );
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("halves.png");
    image::RgbaImage::from_fn(320, 180, |_, y| {
        if y < 90 {
            image::Rgba([255, 0, 0, 255])
        } else {
            image::Rgba([0, 0, 255, 255])
        }
    })
    .save(&path)
    .unwrap();
    let ch = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: ch,
            path,
        },
    );
    // The top half of the master only.
    let band = add_bus(&mut app, "LED", 320, 90);
    take(&mut app, &band, BusInputRef::Master);
    let out = started_output(&mut app, "LED feed");
    route(&mut app, &out, &band);
    render_frames(&mut app, 3);

    let px = output_pixels(&app, &out);
    assert!(near(px[0], [255, 0, 0]), "top of the band, got {:?}", px[0]);
    let low = px[170 * 320 + 160];
    assert!(
        near(low, [255, 0, 0]),
        "the band fills the output — the blue lower half of the master is not sent, got {low:?}"
    );
}

#[test]
fn an_output_is_routed_only_from_master_or_a_bus_that_exists() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let out = started_output(&mut app, "Stream");
    let set = |app: &mut VardaApp, source| {
        send_cmd(
            app,
            EngineCommand::SetOutputSource {
                output_uuid: out.clone(),
                source,
            },
        )
    };
    assert!(matches!(
        set(&mut app, varda::engine::OutputSource::Channel(0)),
        CommandResult::Err {
            code: ErrorCode::InvalidInput,
            ..
        }
    ));
    assert!(matches!(
        set(
            &mut app,
            varda::engine::OutputSource::BusProgram("nosuchbus".into())
        ),
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
    assert!(matches!(
        set(&mut app, varda::engine::OutputSource::Master),
        CommandResult::Ok
    ));
    let rec = add_bus(&mut app, "REC", 1920, 1080);
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetBusLayerHidden {
                uuid: rec,
                channel_uuid: "nosuchch".into(),
                hidden: true,
            }
        ),
        CommandResult::Err {
            code: ErrorCode::NotFound,
            ..
        }
    ));
}

#[test]
fn output_routes_and_hidden_layers_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (rec, overlay, out) = {
        let Some(mut app) = headless_app_in(dir.path()) else {
            return;
        };
        let overlay = new_uuid(send_cmd(&mut app, EngineCommand::AddChannel));
        let rec = add_bus(&mut app, "REC", 1920, 1080);
        take(&mut app, &rec, BusInputRef::Master);
        send_cmd(
            &mut app,
            EngineCommand::SetBusLayerHidden {
                uuid: rec.clone(),
                channel_uuid: overlay.clone(),
                hidden: true,
            },
        );
        let out = started_output(&mut app, "Rec");
        route(&mut app, &out, &rec);
        assert!(matches!(
            send_cmd(&mut app, EngineCommand::SaveWorkspace),
            CommandResult::Ok
        ));
        (rec, overlay, out)
    };
    // As the runner does on startup.
    let mut app = headless_app_in(dir.path()).expect("second app");
    let _ = app.load_workspace();
    let state = app.build_engine_state();
    let bus = state
        .buses
        .iter()
        .find(|b| b.uuid == rec)
        .expect("bus restored");
    assert_eq!(bus.program, BusInputRef::Master);
    assert_eq!(bus.hidden_layers, [overlay]);
    // Saved outputs open on the event loop's next turn, from these configs.
    let output = app
        .pending_outputs()
        .iter()
        .find(|o| o.uuid == out)
        .expect("output restored");
    assert_eq!(
        output.source,
        varda::engine::OutputSource::BusProgram(rec),
        "the recording still follows its bus"
    );
}

#[test]
fn dropped_files_load_in_the_order_they_were_dropped() {
    // A heavy image first, then light ones: with a thread per file the light
    // ones finish first, and twenty speaker slides would land shuffled.
    use varda::app::{DeckFile, DeckLoadToken};
    let Some(app) = headless_app() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut files = Vec::new();
    for (i, size) in [3000u32, 16, 16, 16, 16].into_iter().enumerate() {
        let path = dir.path().join(format!("slide {i}.png"));
        image::RgbaImage::from_fn(size, size, |x, y| image::Rgba([(x ^ y) as u8, 0, 0, 255]))
            .save(&path)
            .unwrap();
        files.push((DeckLoadToken(i), DeckFile::Image(path)));
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let pending = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    VardaApp::spawn_ordered_deck_loads(&tx, app.gpu_context(), &pending, (320, 180), files);
    let order: Vec<usize> = (0..5)
        .map(|_| {
            let result = rx
                .recv_timeout(std::time::Duration::from_secs(30))
                .expect("every file loads");
            assert!(result.deck.is_ok(), "{} failed to load", result.name);
            result.token.0
        })
        .collect();
    assert_eq!(order, [0, 1, 2, 3, 4]);
    assert_eq!(pending.load(std::sync::atomic::Ordering::Relaxed), 0);
}
