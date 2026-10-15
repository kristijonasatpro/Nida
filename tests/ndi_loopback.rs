//! NDI out and back in, on this computer, with the NDI runtime: Main goes out
//! over an NDI output, picture and Master's sound, and comes back as an NDI
//! input — its picture on the input, its sound on the input's strip.
//!
//! Skipped without the NDI runtime or the `ffmpeg` tool (for the test clip).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand, InputSource};
use varda::mixer::MainSource;
use varda::renderer::context::OutputTarget;

mod common;

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

#[allow(clippy::needless_pass_by_value)] // Takes the reply straight from `send_cmd`.
fn ok(result: CommandResult) {
    assert!(
        matches!(result, CommandResult::Ok | CommandResult::OkWithId { .. }),
        "{result:?}"
    );
}

fn id(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected an id, got {other:?}"),
    }
}

/// A red clip with a steady tone, or `None` without the ffmpeg tool.
fn red_clip(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("red.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=red:s=64x36:r=25:d=20")
        .args(["-f", "lavfi", "-i", "sine=frequency=1000:duration=20"])
        .args(["-c:v", "mpeg4", "-c:a", "aac", "-shortest"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    made.then_some(path)
}

/// An app with NDI on (the suite's usual config turns it off).
fn ndi_app(workspace: &Path) -> Option<VardaApp> {
    use clap::Parser as _;
    let gpu = common::headless_gpu()?;
    let config = varda::app::AppConfig::try_parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        workspace.to_str().unwrap(),
    ])
    .unwrap();
    let app = VardaApp::new(gpu, &config).unwrap();
    if app.build_engine_state().ndi_available {
        Some(app)
    } else {
        eprintln!("NDI runtime not installed — skipping");
        None
    }
}

/// Render frames until `done` or `secs` pass; true if it came about.
fn run_until(app: &mut VardaApp, secs: f64, mut done: impl FnMut(&mut VardaApp) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        app.render_mixer_frame();
        app.render_outputs();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

/// The middle pixel of the master, linear RGBA.
fn middle_of_main(app: &VardaApp) -> [f32; 4] {
    let ctx = app.gpu_context();
    let tex = app.mixer_ref().composite_texture();
    let (w, h) = (tex.width(), tex.height());
    let padded =
        (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("ndi loopback readback"),
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
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
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().expect("map channel").expect("map ok");
    let data = slice.get_mapped_range().expect("mapped");
    let at = (h / 2 * padded + w / 2 * 8) as usize;
    let half = |i: usize| {
        half::f16::from_bits(u16::from_le_bytes([data[at + i * 2], data[at + i * 2 + 1]])).to_f32()
    };
    [half(0), half(1), half(2), half(3)]
}

fn is_red(px: [f32; 4]) -> bool {
    px[0] > 0.5 && px[1] < 0.2 && px[2] < 0.2
}

#[test]
fn main_goes_out_over_ndi_and_comes_back_in_with_its_sound() {
    let dir = tempfile::tempdir().unwrap();
    let Some(clip) = red_clip(dir.path()) else {
        eprintln!("ffmpeg not available — skipping");
        return;
    };
    let Some(mut app) = ndi_app(&dir.path().join("ws")) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));

    // Main: a red clip with a tone.
    let red = app.build_engine_state().mixer.channels[0].uuid.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::AddVideoDeck {
            channel_uuid: red.clone(),
            path: clip,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(red),
            fade: false,
        },
    ));

    // Out: an NDI sender carrying Main and Master's sound.
    let sender = format!("Varda loop {}", std::process::id());
    ok(send_cmd(
        &mut app,
        EngineCommand::CreateHeadlessOutput {
            target: OutputTarget::NdiSend {
                sender_name: sender.clone(),
            },
        },
    ));
    let output = app
        .build_engine_state()
        .outputs
        .windows
        .last()
        .unwrap()
        .uuid
        .clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::StartOutput {
            output_uuid: output,
        },
    ));

    // Found on the network, as "COMPUTER (Varda loop …)".
    let mut source = None;
    let found = run_until(&mut app, 15.0, |app| {
        send_cmd(app, EngineCommand::RescanNdi);
        source = app
            .build_engine_state()
            .ndi_sources
            .into_iter()
            .find(|s| s.ends_with(&format!("({sender})")));
        source.is_some()
    });
    assert!(found, "our NDI output never showed up on the network");
    let source = source.unwrap();

    // In: a new input from it.
    let input = id(send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::Ndi {
                source_name: source.clone(),
            },
            name: None,
            audio: None,
        },
    ));
    let state = app.build_engine_state();
    let channel = state
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == input)
        .unwrap();
    assert_eq!(channel.name, source, "named after the NDI source");
    let deck = channel.decks[0].uuid.clone();

    // Its sound gets a strip. Heard here with Audio Auto off, sent nowhere,
    // so it does not come back round.
    let strip_up = run_until(&mut app, 10.0, |app| {
        app.build_engine_state()
            .audio_mix
            .sources
            .iter()
            .any(|s| s.uuid == deck)
    });
    assert!(strip_up, "no strip for the NDI source's sound");
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioAuto {
            uuid: deck.clone(),
            auto: false,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioSourceMute {
            uuid: deck.clone(),
            muted: false,
        },
    ));
    let master = app.build_engine_state().audio_mix.buses[0]
        .config
        .uuid
        .clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioSend {
            uuid: deck.clone(),
            bus_uuid: master,
            on: false,
        },
    ));
    let mut loudest = f32::NEG_INFINITY;
    let heard = run_until(&mut app, 10.0, |app| {
        let meter = app
            .build_engine_state()
            .audio_mix
            .sources
            .iter()
            .find(|s| s.uuid == deck)
            .map_or(-96.0, |s| s.meter_db[0].max(s.meter_db[1]));
        loudest = loudest.max(meter);
        meter > -30.0
    });
    assert!(
        heard,
        "the tone never arrived over NDI: loudest {loudest} dB"
    );

    // Its picture: Main now shows the NDI input, which shows what Main
    // showed — red, kept up only by the loop.
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(input),
            fade: false,
        },
    ));
    let mut px = [0.0; 4];
    let red_through_ndi = run_until(&mut app, 3.0, |app| {
        px = middle_of_main(app);
        false
    }) || is_red(px);
    assert!(red_through_ndi, "Main through NDI is not red: {px:?}");
}
