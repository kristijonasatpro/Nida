//! The stream end to end: frames into the encoder, out through the fan-out to
//! RTMP servers, with each destination's health read the way the STREAM
//! button reads it.
//!
//! The servers are the `ffmpeg` command-line tool listening for one RTMP
//! publisher; without ffmpeg these tests skip, like the recording tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::engine::value::render::{OutputSource, PresentationRequest};
use varda::engine::{CommandResult, EngineCommand};
use varda::renderer::FfmpegSubprocess;
use varda::streaming::{DestinationState, StreamDestination, StreamEncoder, StreamSettings};

mod common;

fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An RTMP server taking one publisher on `port`, app `live`, stream `key`,
/// writing what it receives to `out`.
fn rtmp_server(port: u16, key: &str, out: &Path) -> Child {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-listen", "1"])
        .args(["-timeout", "30", "-f", "flv", "-i"])
        .arg(format!("rtmp://127.0.0.1:{port}/live/{key}"))
        .args(["-c", "copy"])
        .arg(out)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the test RTMP server")
}

fn settings(destinations: Vec<StreamDestination>) -> StreamSettings {
    let mut s = StreamSettings::default();
    s.options.width = 320;
    s.options.height = 180;
    s.options.video_kbps = 800;
    s.options.encoder = StreamEncoder::preferred();
    s.destinations = destinations;
    s.normalized()
}

fn dest(port: u16, key: &str) -> StreamDestination {
    StreamDestination {
        enabled: true,
        name: format!("port {port}"),
        url: format!("rtmp://127.0.0.1:{port}/live"),
        key: key.to_string(),
        ..StreamDestination::default()
    }
}

/// Feed frames in real time for `secs`, calling `each` after every frame.
fn feed(enc: &mut FfmpegSubprocess, secs: f32, mut each: impl FnMut(&FfmpegSubprocess) -> bool) {
    // Noise, new every frame, so the encoder spends its bit rate.
    let mut frame = vec![0u8; 320 * 180 * 4];
    let mut seed = 0x2545_f491_u32;
    let start = Instant::now();
    let mut n = 0u32;
    while start.elapsed().as_secs_f32() < secs {
        for px in frame.as_chunks_mut::<4>().0 {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            px.copy_from_slice(&seed.to_le_bytes());
            px[3] = 255;
        }
        assert!(enc.feed_frame(&frame), "the encoder is running");
        n += 1;
        if each(enc) {
            return;
        }
        let next = Duration::from_millis(40) * n;
        if let Some(wait) = next.checked_sub(start.elapsed()) {
            std::thread::sleep(wait);
        }
    }
}

fn state(enc: &FfmpegSubprocess, i: usize) -> DestinationState {
    enc.fanout().unwrap().status()[i].state
}

fn streams_in(path: &Path) -> String {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "csv=p=0",
        ])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).replace('\n', " ")
}

fn wait_exit(child: &mut Child) {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
}

fn temp(name: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    (dir, path)
}

#[test]
fn one_encode_reaches_two_servers_with_picture_and_sound() {
    if !ffmpeg_available() {
        return;
    }
    let (_d1, out1) = temp("one.flv");
    let (_d2, out2) = temp("two.flv");
    let (p1, p2) = (free_port(), free_port());
    let mut s1 = rtmp_server(p1, "alpha", &out1);
    let mut s2 = rtmp_server(p2, "beta?token=x", &out2);
    std::thread::sleep(Duration::from_millis(500));

    let mut enc = FfmpegSubprocess::spawn_stream(
        &settings(vec![dest(p1, "alpha"), dest(p2, "beta?token=x")]),
        PresentationRequest::default(),
        (320, 180),
        25,
        None,
    )
    .unwrap();
    let mut both_live = false;
    feed(&mut enc, 12.0, |enc| {
        both_live =
            state(enc, 0) == DestinationState::Live && state(enc, 1) == DestinationState::Live;
        both_live
    });
    assert!(both_live, "{:?}", enc.fanout().unwrap().status());
    feed(&mut enc, 3.0, |_| false);
    let status = enc.fanout().unwrap().status();
    assert!(
        status[..2]
            .iter()
            .all(|d| d.state == DestinationState::Live && d.kbps > 100.0),
        "both stay live and a bit rate is measured: {status:?}"
    );
    assert_eq!(
        status[2].state,
        DestinationState::Off,
        "blank slots stay off"
    );
    enc.stop();

    wait_exit(&mut s1);
    wait_exit(&mut s2);
    for out in [&out1, &out2] {
        let streams = streams_in(out);
        assert!(
            streams.contains("h264") && streams.contains("aac"),
            "{}: {streams} — a silent track goes even with no audio",
            out.display()
        );
    }
}

#[test]
fn a_destination_that_is_down_reconnects_on_its_own_when_it_comes_back() {
    if !ffmpeg_available() {
        return;
    }
    let (_d, out) = temp("late.flv");
    let port = free_port();
    let mut enc = FfmpegSubprocess::spawn_stream(
        &settings(vec![dest(port, "late")]),
        PresentationRequest::default(),
        (320, 180),
        25,
        None,
    )
    .unwrap();
    let mut refused = None;
    feed(&mut enc, 8.0, |enc| {
        let s = &enc.fanout().unwrap().status()[0];
        if s.state == DestinationState::Reconnecting {
            refused = s.message.clone();
        }
        refused.is_some()
    });
    let refused = refused.expect("nobody listening reads as reconnecting");
    assert!(refused.contains("refused"), "{refused}");

    let mut server = rtmp_server(port, "late", &out);
    let mut live = false;
    feed(&mut enc, 20.0, |enc| {
        live = state(enc, 0) == DestinationState::Live;
        live
    });
    assert!(live, "{:?}", enc.fanout().unwrap().status());
    assert!(enc.fanout().unwrap().status()[0].reconnects >= 1);
    feed(&mut enc, 1.5, |_| false);
    enc.stop();
    wait_exit(&mut server);
    assert!(
        streams_in(&out).contains("h264"),
        "the late server got the picture"
    );
}

#[test]
fn switching_a_destination_off_while_live_leaves_the_others_sending() {
    if !ffmpeg_available() {
        return;
    }
    let (_d1, out1) = temp("stay.flv");
    let (_d2, out2) = temp("go.flv");
    let (p1, p2) = (free_port(), free_port());
    let mut s1 = rtmp_server(p1, "stay", &out1);
    let mut s2 = rtmp_server(p2, "go", &out2);
    std::thread::sleep(Duration::from_millis(500));
    let (d1, mut d2) = (dest(p1, "stay"), dest(p2, "go"));
    let mut enc = FfmpegSubprocess::spawn_stream(
        &settings(vec![d1, d2.clone()]),
        PresentationRequest::default(),
        (320, 180),
        25,
        None,
    )
    .unwrap();
    feed(&mut enc, 12.0, |enc| {
        state(enc, 0) == DestinationState::Live && state(enc, 1) == DestinationState::Live
    });
    d2.enabled = false;
    enc.fanout().unwrap().set_destination(1, &d2);
    wait_exit(&mut s2);
    assert!(
        s2.try_wait().unwrap().is_some(),
        "the switched-off server saw the stream end"
    );
    feed(&mut enc, 2.0, |_| false);
    assert_eq!(
        state(&enc, 0),
        DestinationState::Live,
        "the other kept going"
    );
    assert_eq!(state(&enc, 1), DestinationState::Off);
    enc.stop();
    wait_exit(&mut s1);
    assert!(
        streams_in(&out2).contains("h264"),
        "what it got before is whole"
    );
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

#[allow(clippy::needless_pass_by_value)] // Takes the reply straight from `send_cmd`.
fn ok(result: CommandResult) {
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
}

fn app_in(workspace: &str) -> Option<VardaApp> {
    use clap::Parser as _;
    let gpu = common::headless_gpu()?;
    let config = varda::app::AppConfig::try_parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        workspace,
    ])
    .unwrap();
    Some(VardaApp::new(gpu, &config).unwrap())
}

/// Render frames until `done` or `secs` pass.
fn run(app: &mut VardaApp, secs: f32, mut done: impl FnMut(&VardaApp) -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < end {
        app.render_mixer_frame();
        app.render_outputs();
        if done(app) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    false
}

#[test]
fn the_engine_streams_its_picture_and_keeps_the_destinations_with_the_stage() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("engine.flv");
    let port = free_port();
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 1500;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions {
            options: options.clone(),
        },
    ));
    assert!(
        matches!(
            send_cmd(&mut app, EngineCommand::StartStream),
            CommandResult::Err { .. }
        ),
        "nothing to send to yet"
    );
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Local".into(),
            url: format!("rtmp://127.0.0.1:{port}/live"),
            key: Some("showkey42".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    let mut server = rtmp_server(port, "showkey42", &out);
    std::thread::sleep(Duration::from_millis(500));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let live = run(&mut app, 15.0, |app| {
        app.stream_status().destinations[0].state == DestinationState::Live
    });
    assert!(live, "{:?}", app.stream_status());
    assert!(app.stream_status().is_healthy());

    // Live, the size needs a new encoder; the video bit rate changes only
    // where the stream is encoded in Nida.
    let bus = app.build_engine_state().buses[0].uuid.clone();
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetStreamOptions {
                options: varda::streaming::StreamOptions {
                    width: 1280,
                    height: 720,
                    ..options.clone()
                }
            }
        ),
        CommandResult::Err { .. }
    ));
    let rate = send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions {
            options: varda::streaming::StreamOptions {
                video_kbps: 3000,
                ..options.clone()
            },
        },
    );
    assert_eq!(
        matches!(rate, CommandResult::Ok),
        app.stream_status().rate_live,
        "{rate:?}"
    );
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions {
            options: varda::streaming::StreamOptions {
                source: OutputSource::BusProgram(bus.clone()),
                ..options.clone()
            },
        },
    ));
    let state = serde_json::to_string(&app.build_engine_state().stream).unwrap();
    assert!(
        !state.contains("showkey42"),
        "the key stays in Varda: {state}"
    );
    run(&mut app, 2.0, |_| false);
    assert_eq!(
        app.stream_status().destinations[0].state,
        DestinationState::Live
    );

    ok(send_cmd(&mut app, EngineCommand::StopStream));
    assert!(!app.stream_status().live);
    wait_exit(&mut server);
    let streams = streams_in(&out);
    assert!(
        streams.contains("h264") && streams.contains("aac"),
        "{streams}"
    );

    ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
    drop(app);
    let mut again = app_in(&ws).expect("gpu");
    let _ = again.load_workspace();
    let saved = again.stream_settings();
    assert_eq!(saved.destinations[0].key, "showkey42");
    assert_eq!(
        saved.destinations[0].url,
        format!("rtmp://127.0.0.1:{port}/live")
    );
    assert_eq!(saved.options.source, OutputSource::BusProgram(bus));
    assert_eq!((saved.options.width, saved.options.video_kbps), (640, 1500));
    assert!(
        !again.stream_status().live,
        "a loaded workspace never starts streaming by itself"
    );
}

#[test]
fn hevc_at_a_lower_frame_rate_reaches_the_server_as_chosen() {
    if !ffmpeg_available() {
        return;
    }
    let (_d, out) = temp("hevc.flv");
    let port = free_port();
    let mut server = rtmp_server(port, "h", &out);
    std::thread::sleep(Duration::from_millis(500));
    let mut s = settings(vec![dest(port, "h")]);
    s.options.codec = varda::streaming::VideoCodec::Hevc;
    s.options.fps = Some(10);
    let mut enc =
        FfmpegSubprocess::spawn_stream(&s, PresentationRequest::default(), (320, 180), 25, None)
            .unwrap();
    feed(&mut enc, 12.0, |enc| {
        state(enc, 0) == DestinationState::Live
    });
    feed(&mut enc, 3.0, |_| false);
    enc.stop();
    wait_exit(&mut server);
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v", "-show_entries"])
        .args(["stream=codec_name,r_frame_rate", "-of", "csv=p=0"])
        .arg(&out)
        .output()
        .unwrap();
    let video = String::from_utf8_lossy(&probe.stdout).trim().to_string();
    assert_eq!(
        video, "hevc,10/1",
        "HEVC over Enhanced RTMP, at 10 fps from a 25 fps feed"
    );
}

/// The stream's frames reach its encoder as NV12 made on the GPU, and the
/// colour on Main is the colour the server receives — through ffmpeg's
/// encoder (x264), its rate as set.
#[test]
fn the_stream_keeps_its_colours_through_the_gpu_conversion() {
    colour_through(false);
}

/// And through the encoder in Nida, whose rate changes while live — on a
/// Mac, where the media engine encodes in Nida.
#[cfg(target_os = "macos")]
#[test]
fn the_stream_keeps_its_colours_through_the_encoder_in_nida() {
    colour_through(true);
}

fn colour_through(in_nida: bool) {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("colour.flv");
    let port = free_port();
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));
    let state = app.build_engine_state();
    let ch = state.mixer.channels[0].uuid.clone();
    for deck in &state.mixer.channels[0].decks {
        send_cmd(
            &mut app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    // Linear light; on screen, sRGB (231, 149, 89).
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: ch.clone(),
                color: [0.8, 0.3, 0.1, 1.0],
            },
        ),
        CommandResult::OkWithId { .. }
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: varda::mixer::MainSource::Channel(ch),
            fade: false,
        },
    ));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 2000;
    options.encoder = if in_nida {
        StreamEncoder::Hardware
    } else {
        StreamEncoder::Software
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions { options },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Local".into(),
            url: format!("rtmp://127.0.0.1:{port}/live"),
            key: Some("colour".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    let mut server = rtmp_server(port, "colour", &out);
    std::thread::sleep(Duration::from_millis(500));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let live = run(&mut app, 15.0, |app| {
        app.stream_status().destinations[0].state == DestinationState::Live
    });
    assert!(live, "{:?}", app.stream_status());
    assert!(app.stream_encoder_takes_nv12(), "NV12 from the GPU");
    let encoded_in_nida = app.stream_status().rate_live;
    if in_nida && !encoded_in_nida && std::env::var_os("VARDA_REQUIRE_GPU").is_none() {
        eprintln!("no media engine to encode in Nida here; skipping");
        ok(send_cmd(&mut app, EngineCommand::StopStream));
        let _ = server.kill();
        let _ = server.wait();
        return;
    }
    assert_eq!(encoded_in_nida, in_nida, "encoded where asked");
    run(&mut app, 2.0, |_| false);
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    wait_exit(&mut server);

    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-sseof", "-1", "-i"])
        .arg(&out)
        .args(["-frames:v", "1", "-vf", "crop=2:2:319:179"])
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success() && decoded.stdout.len() >= 3);
    let got: Vec<i32> = decoded.stdout[..3].iter().map(|&b| i32::from(b)).collect();
    for (g, e) in got.iter().zip([231, 149, 89]) {
        assert!((g - e).abs() <= 4, "the colour came out {got:?}");
    }
}

/// The operator's case: the upload looks slow, so the video bit rate is
/// lowered from 4000 to 1500 while live, and it goes out at that within
/// seconds, the destination still connected and nothing lost; raised again,
/// it follows. With the box ticked the rate set is the most it sends, and
/// the box can be switched while live too.
#[test]
fn the_bit_rate_changes_while_live() {
    for adaptive in [false, true] {
        if !bit_rate_changes_live(adaptive) {
            return;
        }
    }
}

/// See [`the_bit_rate_changes_while_live`]; false where the stream cannot be
/// encoded in Nida here (no media engine).
fn bit_rate_changes_live(adaptive: bool) -> bool {
    if !ffmpeg_available() {
        return false;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("rate.flv");
    let Some(mut app) = app_in(&ws) else {
        return false;
    };
    let port = free_port();
    let mut server = rtmp_server(port, "rate", &out);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 4000;
    options.adaptive = adaptive;
    ok(send_cmd(&mut app, EngineCommand::SetStreamOptions { options: options.clone() }));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Local".into(),
            url: format!("rtmp://127.0.0.1:{port}/live"),
            key: Some("rate".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let live = run(&mut app, 15.0, |app| {
        app.stream_status().destinations[0].state == DestinationState::Live
    });
    assert!(live, "{:?}", app.stream_status());
    if !app.stream_status().rate_live && std::env::var_os("VARDA_REQUIRE_GPU").is_none() {
        eprintln!("no encoder in Nida here (no media engine); skipping");
        ok(send_cmd(&mut app, EngineCommand::StopStream));
        let _ = server.kill();
        let _ = server.wait();
        return false;
    }
    assert!(app.stream_status().rate_live, "encoded in Nida, whatever the box says");
    let sending = |app: &VardaApp| app.stream_status().destinations[0].kbps;
    assert!(run(&mut app, 10.0, |app| sending(app) > 3500.0), "at the rate set: {}", sending(&app));
    let generation = app.stream_status().destinations[0].generation;

    let set = |app: &mut VardaApp, video_kbps: u32, adaptive: bool| {
        ok(send_cmd(
            app,
            EngineCommand::SetStreamOptions {
                options: varda::streaming::StreamOptions {
                    video_kbps,
                    adaptive,
                    ..options.clone()
                },
            },
        ));
    };
    set(&mut app, 1500, adaptive);
    let lowered = run(&mut app, 10.0, |app| sending(app) < 2000.0);
    assert!(lowered, "adaptive {adaptive}: lowered while live: {} kbps", sending(&app));
    // What the stream is at now, sound and picture, as it comes out.
    let audio = options.audio_kbps;
    let at_new_rate = run(&mut app, 6.0, |app| {
        app.stream_loss_reckoned_kbps()
            .is_some_and(|k| (f64::from(k) - f64::from(1500 + audio)).abs() < f64::from(1500 + audio) * 0.15)
    });
    assert!(
        at_new_rate,
        "a lost piece is worth its time at the rate lowered to: {:?}",
        app.stream_loss_reckoned_kbps()
    );
    if adaptive {
        let rate = app.stream_status().adaptive.expect("following the upload");
        assert_eq!((rate.asked_kbps, rate.kbps), (1500, 1500), "the rate set is the most");
    }
    set(&mut app, 4000, !adaptive);
    let raised = run(&mut app, 10.0, |app| sending(app) > 3500.0);
    assert!(raised, "adaptive {adaptive}: raised while live: {} kbps", sending(&app));
    assert_eq!(
        app.stream_status().adaptive.is_some(),
        !adaptive,
        "the box switched while live"
    );
    let status = app.stream_status();
    let dest = &status.destinations[0];
    assert_eq!(dest.generation, generation, "the same connection throughout");
    assert_eq!((dest.reconnects, dest.dropped), (0, 0), "nothing lost: {dest:?}");
    assert_eq!(app.stream_settings().options.video_kbps, 4000, "the rate set is kept");

    ok(send_cmd(&mut app, EngineCommand::StopStream));
    wait_exit(&mut server);
    let streams = streams_in(&out);
    assert!(streams.contains("h264") && streams.contains("aac"), "{streams}");
    true
}

/// Variable on the media engine: Constant's picture without its padding.
/// A still picture at 4000 kbps goes out at the rate set held Constant —
/// padded — and at a small part of it Variable, switched while live on the
/// same connection; Average, encoded apart, waits for a stop.
#[test]
fn variable_drops_the_padding_of_a_still_picture() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("still.flv");
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));
    let state = app.build_engine_state();
    let ch = state.mixer.channels[0].uuid.clone();
    for deck in &state.mixer.channels[0].decks {
        send_cmd(&mut app, EngineCommand::RemoveDeck { deck_uuid: deck.uuid.clone() });
    }
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: ch.clone(),
                color: [0.2, 0.4, 0.6, 1.0],
            },
        ),
        CommandResult::OkWithId { .. }
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: varda::mixer::MainSource::Channel(ch),
            fade: false,
        },
    ));
    let port = free_port();
    let mut server = rtmp_server(port, "still", &out);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 4000;
    options.adaptive = false;
    options.encoder = StreamEncoder::Hardware;
    options.rate_mode = varda::streaming::RateMode::Constant;
    ok(send_cmd(&mut app, EngineCommand::SetStreamOptions { options: options.clone() }));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Local".into(),
            url: format!("rtmp://127.0.0.1:{port}/live"),
            key: Some("still".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let live = run(&mut app, 15.0, |app| {
        app.stream_status().destinations[0].state == DestinationState::Live
    });
    assert!(live, "{:?}", app.stream_status());
    if !app.stream_status().rate_live && std::env::var_os("VARDA_REQUIRE_GPU").is_none() {
        eprintln!("no encoder in Nida here (no media engine); skipping");
        ok(send_cmd(&mut app, EngineCommand::StopStream));
        let _ = server.kill();
        let _ = server.wait();
        return;
    }
    let sending = |app: &VardaApp| app.stream_status().destinations[0].kbps;
    assert!(run(&mut app, 10.0, |app| sending(app) > 3500.0), "Constant pads: {}", sending(&app));
    let generation = app.stream_status().destinations[0].generation;
    let set = |app: &mut VardaApp, rate_mode| {
        send_cmd(
            app,
            EngineCommand::SetStreamOptions {
                options: varda::streaming::StreamOptions {
                    rate_mode,
                    ..options.clone()
                },
            },
        )
    };
    let measured = app.stream_status().stream_kbps.expect("measured");
    assert!(measured > 3500, "the stream at the rate set, padded: {measured}");
    ok(set(&mut app, varda::streaming::RateMode::Variable));
    let lean = run(&mut app, 10.0, |app| sending(app) < 1500.0);
    assert!(lean, "Variable drops the padding: {} kbps", sending(&app));
    assert_eq!(app.stream_status().rate_mode, Some(varda::streaming::RateMode::Variable));
    let lean = run(&mut app, 6.0, |app| app.stream_status().stream_kbps.is_some_and(|k| k < 1500));
    assert!(lean, "and measured so: {:?}", app.stream_status().stream_kbps);
    match set(&mut app, varda::streaming::RateMode::Average) {
        CommandResult::Err { message, .. } => assert!(message.contains("Average"), "{message}"),
        other => panic!("Average taken while live: {other:?}"),
    }
    ok(set(&mut app, varda::streaming::RateMode::Constant));
    assert!(run(&mut app, 10.0, |app| sending(app) > 3500.0), "padded again: {}", sending(&app));
    let dest = &app.stream_status().destinations[0];
    assert_eq!(dest.generation, generation, "the same connection throughout");
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    wait_exit(&mut server);
    let streams = streams_in(&out);
    assert!(streams.contains("h264") && streams.contains("aac"), "{streams}");
}

/// With the software encoder the bit rate waits for a stop, and a change
/// while live says so rather than doing nothing.
#[test]
fn a_software_stream_says_its_bit_rate_waits_for_a_stop() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("software.flv");
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let port = free_port();
    let mut server = rtmp_server(port, "soft", &out);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 320;
    options.height = 180;
    options.video_kbps = 800;
    options.encoder = StreamEncoder::Software;
    ok(send_cmd(&mut app, EngineCommand::SetStreamOptions { options: options.clone() }));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Local".into(),
            url: format!("rtmp://127.0.0.1:{port}/live"),
            key: Some("soft".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    assert!(!app.stream_status().rate_live, "x264 runs in ffmpeg");
    let refused = send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions {
            options: varda::streaming::StreamOptions {
                video_kbps: 500,
                ..options.clone()
            },
        },
    );
    match refused {
        CommandResult::Err { message, .. } => {
            assert!(message.contains("Hardware encoder"), "{message}");
        }
        other => panic!("taken while live: {other:?}"),
    }
    assert_eq!(app.stream_settings().options.video_kbps, 800, "unchanged");
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    let _ = server.kill();
    let _ = server.wait();
}

/// A TCP relay from its `port` to the server on `to`, carrying what the
/// client sends at no more than `limit` bytes a second (0: as it comes) — a
/// venue's upload — and what the server sends back as it comes. It reads
/// only as fast as it may send, so its own buffer holds a few kilobytes.
/// `cut` ends the way every connection made so far goes: nothing more gets
/// through either way, and nothing says so — the sockets stay open — as
/// when Wi-Fi moves to another network; `to` is where new connections go.
struct Throttle {
    port: u16,
    limit: Arc<AtomicU64>,
    cut: Arc<AtomicU64>,
    to: Arc<AtomicU16>,
}

impl Throttle {
    /// The way every connection made so far goes ends (see [`Throttle`]).
    fn cut(&self) {
        self.cut.fetch_add(1, Ordering::Relaxed);
    }
}

fn throttle(to: u16) -> Throttle {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Its connections take in a few kilobytes of what the client sends
    // before they read it, as a way over a network holds: left to the
    // system, a connection on this machine takes megabytes, so one whose
    // way has been cut seems to send on for seconds.
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        let bytes: libc::c_int = 16 * 1024;
        // SAFETY: setting an option on a socket this owns, with its size.
        let set = unsafe {
            libc::setsockopt(
                listener.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                (&raw const bytes).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        assert_eq!(set, 0, "the relay's receive buffer");
    }
    let limit = Arc::new(AtomicU64::new(0));
    let cut = Arc::new(AtomicU64::new(0));
    let to = Arc::new(AtomicU16::new(to));
    let (shared, cuts, target) = (limit.clone(), cut.clone(), to.clone());
    std::thread::spawn(move || {
        for client in listener.incoming() {
            let Ok(client) = client else { return };
            let Ok(server) = TcpStream::connect(("127.0.0.1", target.load(Ordering::Relaxed))) else {
                continue;
            };
            // Made at this count of cuts: a later one ends its way.
            let born = cuts.load(Ordering::Relaxed);
            let dead = {
                let cuts = cuts.clone();
                move || cuts.load(Ordering::Relaxed) > born
            };
            let (mut back_in, mut back_out) = (server.try_clone().unwrap(), client.try_clone().unwrap());
            let back_dead = dead.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                loop {
                    let n = match back_in.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    if back_dead() {
                        // Held open, passing nothing on.
                        loop {
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                    if back_out.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
            let (mut from, mut onto, limit) = (client, server, shared.clone());
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                // Paced from when the limit last changed.
                let (mut since, mut sent, mut rate) = (Instant::now(), 0u64, 0u64);
                loop {
                    if dead() {
                        // Nothing more is read, so the client's writes back
                        // up, and nothing is closed.
                        loop {
                            std::thread::sleep(Duration::from_secs(1));
                        }
                    }
                    let n = match from.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let now_rate = limit.load(Ordering::Relaxed);
                    if now_rate != rate {
                        (since, sent, rate) = (Instant::now(), 0, now_rate);
                    }
                    sent += n as u64;
                    if rate > 0 {
                        let due = since + Duration::from_secs_f64(sent as f64 / rate as f64);
                        if let Some(wait) = due.checked_duration_since(Instant::now()) {
                            std::thread::sleep(wait);
                        }
                    }
                    if onto.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
            });
        }
    });
    Throttle { port, limit, cut, to }
}

/// The operator's case: Wi-Fi switched to another network mid-stream. The
/// connection made over the old one was dead though its socket said
/// nothing, and the stream blinked for half a minute until its sender gave
/// up on it: a write stuck for ten seconds, or nothing out for twenty. Once
/// the network is seen to change, a destination that has stopped sending
/// is connected again at once, the stream it missed meanwhile is counted,
/// and the rate stands where it stood.
#[test]
fn a_change_of_network_reconnects_at_once() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, first) = temp("first.flv");
    let (_e, second) = temp("second.flv");
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let old_port = free_port();
    let mut old_server = rtmp_server(old_port, "wifi", &first);
    let way = throttle(old_port);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 2000;
    ok(send_cmd(&mut app, EngineCommand::SetStreamOptions { options }));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Venue".into(),
            url: format!("rtmp://127.0.0.1:{}/live", way.port),
            key: Some("wifi".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let dest = |app: &VardaApp| app.stream_status().destinations[0].clone();
    assert!(run(&mut app, 15.0, |app| dest(app).state == DestinationState::Live), "{:?}", dest(&app));
    run(&mut app, 3.0, |_| false);
    let before = dest(&app);

    // The Wi-Fi moves: the old way dies without a word, a new one opens.
    // Left to the sender's own timeouts, it hangs on the dead connection.
    let new_port = free_port();
    let mut new_server = rtmp_server(new_port, "wifi", &second);
    std::thread::sleep(Duration::from_millis(500));
    way.to.store(new_port, Ordering::Relaxed);
    way.cut();
    run(&mut app, 7.0, |_| false);
    assert_eq!(dest(&app).reconnects, before.reconnects, "still on the dead connection: {:?}", dest(&app));
    // The network is seen to change: the quiet connection goes at once.
    varda::streaming::network::change_for_test(way.port);
    let moved = Instant::now();
    let gone = run(&mut app, 10.0, |app| dest(app).reconnects > before.reconnects);
    assert!(gone, "{:?}", dest(&app));
    assert!(
        moved.elapsed() < Duration::from_secs(3),
        "at once, not after the sender's timeouts: {:?}",
        moved.elapsed()
    );
    let back = run(&mut app, 15.0, |app| {
        let d = dest(app);
        d.generation != before.generation && d.state == DestinationState::Live
    });
    let after = dest(&app);
    assert!(back, "connected again the new way: {after:?}");
    assert_eq!(after.reconnects, before.reconnects + 1, "{after:?}");
    assert!(after.dropped_secs > before.dropped_secs, "the stream it missed is counted: {after:?}");
    if let Some(rate) = app.stream_status().adaptive {
        let restored = run(&mut app, 5.0, |app| {
            app.stream_status().adaptive.is_some_and(|a| a.kbps == a.asked_kbps)
        });
        assert!(restored, "the rate stands where it stood: {rate:?}");
    }
    run(&mut app, 3.0, |_| false);
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    wait_exit(&mut new_server);
    let _ = old_server.kill();
    let _ = old_server.wait();
    let streams = streams_in(&second);
    assert!(streams.contains("h264") && streams.contains("aac"), "the new way carries it: {streams}");
}

/// The way dies just as the network is seen to change — the usual order
/// when Wi-Fi moves. What the sender writes still goes into its socket for a
/// moment, so it seems to send on; it must not seem so for long enough to
/// be trusted (six seconds), or it falls back to its own timeouts.
#[test]
fn a_way_dying_as_the_network_changes_is_caught() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, first) = temp("first.flv");
    let (_e, second) = temp("second.flv");
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let old_port = free_port();
    let mut old_server = rtmp_server(old_port, "roam", &first);
    let way = throttle(old_port);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 2000;
    ok(send_cmd(&mut app, EngineCommand::SetStreamOptions { options }));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Venue".into(),
            url: format!("rtmp://127.0.0.1:{}/live", way.port),
            key: Some("roam".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let dest = |app: &VardaApp| app.stream_status().destinations[0].clone();
    assert!(run(&mut app, 15.0, |app| dest(app).state == DestinationState::Live), "{:?}", dest(&app));
    run(&mut app, 3.0, |_| false);
    let before = dest(&app);
    let new_port = free_port();
    let mut new_server = rtmp_server(new_port, "roam", &second);
    std::thread::sleep(Duration::from_millis(500));
    way.to.store(new_port, Ordering::Relaxed);
    way.cut();
    varda::streaming::network::change_for_test(way.port);
    let moved = Instant::now();
    let gone = run(&mut app, 15.0, |app| dest(app).reconnects > before.reconnects);
    assert!(gone, "{:?}", dest(&app));
    assert!(
        moved.elapsed() < Duration::from_secs(6),
        "caught before it could be trusted: {:?}",
        moved.elapsed()
    );
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    let _ = new_server.kill();
    let _ = new_server.wait();
    let _ = old_server.kill();
    let _ = old_server.wait();
}

/// A destination that cannot connect waits longer between attempts, up to
/// ten seconds; a change of network while it waits — Wi-Fi back, another
/// network — is a reason to try again at once rather than wait on.
#[test]
fn a_change_of_network_cuts_the_wait_between_attempts_short() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    // Nothing listens there.
    let port = free_port();
    let mut options = app.stream_settings().options.clone();
    options.width = 320;
    options.height = 180;
    options.video_kbps = 800;
    ok(send_cmd(&mut app, EngineCommand::SetStreamOptions { options }));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Down".into(),
            url: format!("rtmp://127.0.0.1:{port}/live"),
            key: Some("down".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let tries = |app: &VardaApp| app.stream_status().destinations[0].reconnects;
    // Each attempt reads three seconds of the stream before it dials; after
    // the third the wait is four seconds.
    assert!(run(&mut app, 30.0, |app| tries(app) >= 3), "{:?}", app.stream_status().destinations[0]);
    let before = tries(&app);
    varda::streaming::network::change_for_test(port);
    let changed = Instant::now();
    assert!(run(&mut app, 15.0, |app| tries(app) > before), "tried again");
    assert!(
        changed.elapsed() < Duration::from_millis(5500),
        "at once — the attempt's own three seconds — not after the four seconds' wait too: {:?}",
        changed.elapsed()
    );
    ok(send_cmd(&mut app, EngineCommand::StopStream));
}

/// The operator's case: an upload that falls to less than the stream needs.
/// The stream's video rate comes down under what the upload carries, so its
/// backlog stops growing and nothing is lost; with the upload back, the rate
/// goes back up.
#[test]
fn a_stream_follows_what_its_upload_carries() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("slow.flv");
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let server_port = free_port();
    let mut server = rtmp_server(server_port, "slow", &out);
    let upload = throttle(server_port);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 4000;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions { options },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Venue".into(),
            url: format!("rtmp://127.0.0.1:{}/live", upload.port),
            key: Some("slow".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let live = run(&mut app, 15.0, |app| {
        app.stream_status().destinations[0].state == DestinationState::Live
    });
    assert!(live, "{:?}", app.stream_status());
    let Some(adaptive) = app.stream_status().adaptive else {
        eprintln!("no encoder in Nida here (no media engine); skipping");
        ok(send_cmd(&mut app, EngineCommand::StopStream));
        let _ = server.kill();
        let _ = server.wait();
        return;
    };
    assert_eq!(adaptive.kbps, 4000, "at the rate set");
    run(&mut app, 5.0, |_| false);

    // The upload falls to 1.5 Mbit/s.
    upload.limit.store(1_500_000 / 8, Ordering::Relaxed);
    // Cut under the upload; whether that is enough is what follows says:
    // nothing lost, and what waits draining.
    let cut = run(&mut app, 45.0, |app| {
        app.stream_status().adaptive.is_some_and(|a| a.kbps < 1500)
    });
    let status = app.stream_status();
    assert!(cut, "cut under the upload: {:?}", status.adaptive);
    let (lost, waiting) = (status.destinations[0].dropped, status.destinations[0].backlog_bytes);
    run(&mut app, 8.0, |_| false);
    let status = app.stream_status();
    assert_eq!(status.destinations[0].dropped, lost, "nothing more lost once under it");
    assert!(lost == 0, "nothing lost at all: {lost} pieces");
    let rate = status.adaptive.unwrap().kbps;
    // What waits is draining: less than when the rate was cut, or next to
    // nothing — half a second of stream at the rate now.
    let now_waiting = status.destinations[0].backlog_bytes;
    let half_second = u64::from(rate + 160) * 1000 / 16;
    assert!(
        now_waiting < waiting.max(half_second),
        "draining: {waiting} bytes waiting at the cut, {now_waiting} now"
    );

    // The upload comes back: the rate goes back up.
    upload.limit.store(0, Ordering::Relaxed);
    let raised = run(&mut app, 30.0, |app| {
        app.stream_status().adaptive.is_some_and(|a| a.kbps > rate)
    });
    assert!(raised, "raised again: {:?}", app.stream_status().adaptive);
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    wait_exit(&mut server);
    let streams = streams_in(&out);
    assert!(streams.contains("h264") && streams.contains("aac"), "{streams}");
}

/// An upload too slow even at the lowest rate: the destination falls as far
/// behind as it may, and what it then sends resumes at a keyframe — a jump,
/// where losing pieces of the stream at random broke the picture up until
/// the next one. What the server gets decodes cleanly throughout.
#[test]
fn an_upload_too_slow_for_the_lowest_rate_jumps_ahead_whole() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let (_d, out) = temp("too-slow.flv");
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let server_port = free_port();
    let mut server = rtmp_server(server_port, "slow", &out);
    let upload = throttle(server_port);
    std::thread::sleep(Duration::from_millis(500));
    let mut options = app.stream_settings().options.clone();
    options.width = 640;
    options.height = 360;
    options.video_kbps = 1500;
    options.buffer_secs = 1;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamOptions { options },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetStreamDestination {
            index: 0,
            enabled: true,
            name: "Venue".into(),
            url: format!("rtmp://127.0.0.1:{}/live", upload.port),
            key: Some("slow".into()),
            username: String::new(),
            password: Some(String::new()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartStream));
    let live = run(&mut app, 15.0, |app| {
        app.stream_status().destinations[0].state == DestinationState::Live
    });
    assert!(live, "{:?}", app.stream_status());
    if app.stream_status().adaptive.is_none() {
        eprintln!("no encoder in Nida here (no media engine); skipping");
        ok(send_cmd(&mut app, EngineCommand::StopStream));
        let _ = server.kill();
        let _ = server.wait();
        return;
    }
    run(&mut app, 5.0, |_| false);
    // 100 kbit/s: far less than the sound and the least picture together.
    upload.limit.store(100_000 / 8, Ordering::Relaxed);
    let lost = run(&mut app, 60.0, |app| {
        let s = app.stream_status();
        s.destinations[0].dropped > 0 && s.adaptive.is_some_and(|a| a.at_floor())
    });
    assert!(lost, "at the floor, and losing: {:?}", app.stream_status());
    // Back to as fast as it comes, and stopped once all that waited has
    // gone: stopped with stream still on its way, the last frame is cut off
    // in the server's file, which is not what is being looked for here.
    upload.limit.store(0, Ordering::Relaxed);
    let drained = run(&mut app, 20.0, |app| {
        app.stream_status().destinations[0].backlog_bytes == 0
    });
    assert!(drained, "{:?}", app.stream_status().destinations[0]);
    run(&mut app, 2.0, |_| false);
    ok(send_cmd(&mut app, EngineCommand::StopStream));
    wait_exit(&mut server);
    // A stream stopped ends with its encoder stopped mid-frame: the last tag
    // is cut off, as it always was. Everything before it decodes cleanly.
    let whole = dir.path().join("whole.flv");
    std::fs::write(&whole, without_last_tag(&std::fs::read(&out).unwrap())).unwrap();
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&whole)
        .args(["-map", "0:v", "-f", "null", "-"])
        .output()
        .unwrap();
    let errors = String::from_utf8_lossy(&decoded.stderr);
    assert!(
        decoded.status.success() && errors.trim().is_empty(),
        "the picture decodes whole: {errors}"
    );
}

/// An FLV file without its last tag.
fn without_last_tag(flv: &[u8]) -> Vec<u8> {
    let mut at = u32::from_be_bytes(flv[5..9].try_into().unwrap()) as usize + 4;
    let mut last = at;
    while at + 11 <= flv.len() {
        let size = u32::from_be_bytes([0, flv[at + 1], flv[at + 2], flv[at + 3]]) as usize;
        if at + 11 + size + 4 > flv.len() {
            break;
        }
        last = at;
        at += 11 + size + 4;
    }
    flv[..last].to_vec()
}
