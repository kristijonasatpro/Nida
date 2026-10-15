//! Recording end to end: the REC button's engine side writes a real file in
//! each format, with picture and sound, readable while it is being written.
//!
//! Files are checked with the `ffprobe` tool; without ffmpeg these skip.

use std::path::Path;
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand};
use varda::record::{RecordCodec, RecordFormat, RecordOptions};

mod common;

fn ffmpeg_available() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok()
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

fn app() -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    Some(VardaApp::new(gpu, &varda::testing::headless_config()).unwrap())
}

fn run(app: &mut VardaApp, secs: f32) {
    let end = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < end {
        app.render_mixer_frame();
        app.render_outputs();
        std::thread::sleep(Duration::from_millis(15));
    }
}

/// `codec_name` of each stream and the duration, once ffprobe can read the
/// whole file (the finish happens on a background thread).
fn probe(path: &Path) -> (String, f64) {
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let out = std::process::Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_name:format=duration",
            ])
            .args(["-of", "default=nw=1:nk=1"])
            .arg(path)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let lines: Vec<&str> = text.lines().collect();
        if out.status.success() && lines.len() >= 3 {
            let duration = lines.last().unwrap().parse().unwrap_or(0.0);
            return (lines[..lines.len() - 1].join(" "), duration);
        }
        assert!(
            Instant::now() < until,
            "{} never became readable",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn record_with(codec: RecordCodec, format: RecordFormat) -> Option<(String, f64)> {
    if !ffmpeg_available() {
        return None;
    }
    let mut app = app()?;
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        folder: dir.path().join("takes"),
        codec,
        format,
        size: Some((640, 360)),
        ..RecordOptions::default()
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions { options },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    run(&mut app, 3.0);
    let status = app.record_status();
    assert!(status.recording && status.bytes > 0, "{status:?}");
    let file = std::path::PathBuf::from(status.file.clone().unwrap());
    assert!(
        file.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("Nida 20"),
        "named by date: {}",
        file.display()
    );
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
    assert_eq!(
        app.record_status().last_file,
        status.file,
        "remembered for Show in Finder"
    );
    let result = probe(&file);
    drop(dir);
    Some(result)
}

#[test]
fn h264_mp4_carries_picture_and_sound() {
    let Some((streams, secs)) = record_with(RecordCodec::H264, RecordFormat::Mp4) else {
        return;
    };
    assert_eq!(streams, "h264 aac");
    assert!(secs > 2.0, "about three seconds: {secs}");
}

#[test]
fn hevc_and_mkv() {
    let Some((streams, secs)) = record_with(RecordCodec::Hevc, RecordFormat::Mkv) else {
        return;
    };
    assert_eq!(streams, "hevc aac");
    assert!(secs > 2.0, "{secs}");
}

#[test]
fn prores_goes_in_mov_with_uncompressed_sound() {
    // MP4 asked for: ProRes is always written as MOV.
    let Some((streams, secs)) = record_with(RecordCodec::ProRes, RecordFormat::Mp4) else {
        return;
    };
    assert_eq!(streams, "prores pcm_s16le");
    assert!(secs > 2.0, "{secs}");
}

#[test]
fn an_mp4_can_be_read_while_it_is_still_recording() {
    if !ffmpeg_available() {
        return;
    }
    let Some(mut app) = app() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        folder: dir.path().to_path_buf(),
        size: Some((640, 360)),
        ..RecordOptions::default()
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions { options },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    run(&mut app, 3.0);
    // Not stopped: this is what is on disk if Varda crashes now.
    let file = app.record_status().file.unwrap();
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-count_frames", "-select_streams", "v"])
        .args(["-show_entries", "stream=nb_read_frames", "-of", "csv=p=0"])
        .arg(&file)
        .output()
        .unwrap();
    let frames: u32 = String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    assert!(
        frames > 20,
        "the frames so far play without the file being finished: {frames}"
    );
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
}

#[test]
fn only_the_source_changes_while_recording() {
    if !ffmpeg_available() {
        return;
    }
    let Some(mut app) = app() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        folder: dir.path().to_path_buf(),
        size: Some((320, 180)),
        ..RecordOptions::default()
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions {
            options: options.clone(),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    let refused = send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions {
            options: RecordOptions {
                codec: RecordCodec::Hevc,
                ..options.clone()
            },
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
    let bus = app.build_engine_state().buses[0].uuid.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions {
            options: RecordOptions {
                source: varda::engine::value::render::OutputSource::BusProgram(bus),
                ..options
            },
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
}

#[test]
fn a_crashed_encoder_leaves_a_playable_file_and_recording_goes_on_in_a_new_one() {
    if !ffmpeg_available() {
        return;
    }
    let Some(mut app) = app() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        folder: dir.path().to_path_buf(),
        size: Some((640, 360)),
        ..RecordOptions::default()
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions { options },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    run(&mut app, 2.5);
    app.kill_record_encoder_for_test();
    run(&mut app, 3.5);
    let status = app.record_status();
    assert!(status.recording, "still recording");
    assert_eq!(status.files.len(), 2, "{status:?}");
    assert!(
        status
            .problem
            .as_deref()
            .is_some_and(|p| p.contains("new file")),
        "and says so: {status:?}"
    );
    assert!(status.files[1].ends_with("(2).mp4"), "{:?}", status.files);
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
    let (first, secs) = probe(Path::new(&status.files[0]));
    assert!(
        first.starts_with("h264"),
        "the killed file still plays: {first}"
    );
    assert!(
        secs > 1.0,
        "with what was recorded before the crash: {secs}"
    );
    let (second, _) = probe(Path::new(&status.files[1]));
    assert!(second.starts_with("h264"), "{second}");
}

#[test]
fn the_recording_settings_are_kept_with_the_stage() {
    use clap::Parser as _;
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let config = || {
        varda::app::AppConfig::try_parse_from([
            "varda",
            "--headless",
            "--no-osc",
            "--no-ndi",
            "--no-syphon",
            "--no-camera",
            "--workspace",
            &ws,
        ])
        .unwrap()
    };
    let options = RecordOptions {
        folder: dir.path().join("takes"),
        codec: RecordCodec::Hevc,
        format: RecordFormat::Mkv,
        video_kbps: Some(30_000),
        size: Some((1920, 1080)),
        fps: Some(25),
        ..RecordOptions::default()
    };
    {
        let Some(gpu) = common::headless_gpu() else {
            return;
        };
        let mut app = VardaApp::new(gpu, &config()).unwrap();
        ok(send_cmd(
            &mut app,
            EngineCommand::SetRecordOptions {
                options: options.clone(),
            },
        ));
        ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
    }
    let mut app = VardaApp::new(common::headless_gpu().expect("gpu"), &config()).unwrap();
    let _ = app.load_workspace();
    assert_eq!(app.record_settings().options, options);
}

/// The status is read every frame, so it must not ask the OS anything:
/// looking up free space per read once cost the renderer a third of its
/// frame rate.
#[test]
fn reading_the_status_every_frame_costs_nothing() {
    if !ffmpeg_available() {
        return;
    }
    let Some(mut app) = app() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let options = RecordOptions {
        folder: dir.path().to_path_buf(),
        size: Some((320, 180)),
        ..RecordOptions::default()
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions { options },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    run(&mut app, 0.5);
    let start = Instant::now();
    for _ in 0..300 {
        std::hint::black_box(app.record_status());
    }
    let per_read = start.elapsed() / 300;
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
    assert!(
        per_read < Duration::from_micros(200),
        "a status read took {per_read:?} — it must not touch the disk"
    );
}

/// Frames reach the encoder as NV12 made on the GPU, and the colours come
/// out of the file as they went in: a colour on Main read back from the
/// recording's middle within the encoder's rounding.
#[test]
fn a_recording_keeps_its_colours_through_the_gpu_conversion() {
    if !ffmpeg_available() {
        return;
    }
    let Some(mut app) = app() else {
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
    let colour = [0.8f32, 0.3, 0.1, 1.0];
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: ch.clone(),
                color: colour,
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
    let dir = tempfile::tempdir().unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRecordOptions {
            options: RecordOptions {
                folder: dir.path().to_path_buf(),
                size: Some((640, 360)),
                ..RecordOptions::default()
            },
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    run(&mut app, 0.3);
    assert!(
        app.record_encoder_takes_nv12(),
        "the encoder is fed NV12 from the GPU"
    );
    run(&mut app, 1.5);
    let file = app.record_status().file.unwrap();
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
    let _ = probe(Path::new(&file));

    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-ss", "1", "-i", &file, "-frames:v", "1"])
        .args([
            "-vf",
            "crop=2:2:319:179",
            "-pix_fmt",
            "rgb24",
            "-f",
            "rawvideo",
            "-",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let srgb = |c: f32| {
        let c = if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        };
        (c * 255.0).round() as i32
    };
    let expected: Vec<i32> = colour[..3].iter().map(|&c| srgb(c)).collect();
    let got: Vec<i32> = out.stdout[..3].iter().map(|&b| i32::from(b)).collect();
    for (g, e) in got.iter().zip(&expected) {
        assert!(
            (g - e).abs() <= 4,
            "the colour came out {got:?}, went in {expected:?}"
        );
    }
}
