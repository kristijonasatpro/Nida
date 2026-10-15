//! The audio mixer end to end: a clip's soundtrack decoded, gated by what is
//! on air, mixed onto a bus and read the way a recording reads it.
//!
//! Test clips are made with the `ffmpeg` command-line tool; without it these
//! tests skip, like the recording tests.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::bus::BusInputRef;
use varda::engine::{CommandResult, EngineCommand};

mod common;

fn headless_app() -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    let config = varda::testing::headless_config();
    Some(VardaApp::new(gpu, &config).expect("VardaApp::new"))
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn new_uuid(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    }
}

#[allow(clippy::needless_pass_by_value)] // Takes the reply straight from `send_cmd`.
fn ok(result: CommandResult) {
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
}

/// A 64x36 clip with a 1 kHz tone, shaped by `volume` (an ffmpeg expression
/// in `t`), or `None` without the ffmpeg tool.
fn clip(dir: &Path, name: &str, volume: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=red:s=64x36:r=25:d=6")
        .args(["-f", "lavfi", "-i"])
        .arg(format!(
            "sine=frequency=1000:sample_rate=44100:duration=6,volume='{volume}':eval=frame"
        ))
        .args(["-c:v", "mpeg4", "-c:a", "aac", "-b:a", "192k", "-shortest"])
        .arg(&path)
        .status();
    match status {
        Ok(s) if s.success() => Some(path),
        _ => {
            eprintln!("ffmpeg not available — skipping");
            None
        }
    }
}

/// The first video bus, a clip deck on the first channel, and the Master
/// audio bus.
struct Rig {
    app: VardaApp,
    pgm: String,
    deck: String,
    master: String,
}

fn rig(path: &Path) -> Option<Rig> {
    let mut app = headless_app()?;
    // Main in the vMix way, with nothing on it, so nothing is on air until
    // a test takes it there. A fresh engine has one video bus, PGM.
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));
    let pgm = app.build_engine_state().buses[0].uuid.clone();
    let ch = app.build_engine_state().mixer.channels[0].uuid.clone();
    let deck = new_uuid(send_cmd(
        &mut app,
        EngineCommand::AddVideoDeck {
            channel_uuid: ch,
            path: path.to_path_buf(),
        },
    ));
    app.render_mixer_frame();
    let master = app.build_engine_state().audio_mix.buses[0]
        .config
        .uuid
        .clone();
    Some(Rig {
        app,
        pgm,
        deck,
        master,
    })
}

/// Cut `input` to Program on `bus`.
fn cut(app: &mut VardaApp, bus: &str, input: BusInputRef) {
    ok(send_cmd(
        app,
        EngineCommand::SetBusPreview {
            uuid: bus.to_string(),
            input,
        },
    ));
    ok(send_cmd(
        app,
        EngineCommand::BusCut {
            uuid: bus.to_string(),
        },
    ));
}

/// Everything a bus tap delivers over `secs` of running frames, with the
/// moment each chunk arrived.
fn listen(
    app: &mut VardaApp,
    tap: &varda::audio_mix::BusTap,
    secs: f64,
) -> Vec<(Instant, Vec<f32>)> {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    let mut got = Vec::new();
    while Instant::now() < end {
        app.render_mixer_frame();
        while let Ok(chunk) = tap.receiver.try_recv() {
            got.push((Instant::now(), chunk.samples));
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    got
}

fn peak(chunks: &[(Instant, Vec<f32>)]) -> f32 {
    chunks
        .iter()
        .flat_map(|(_, s)| s.iter())
        .fold(0.0f32, |m, s| m.max(s.abs()))
}

/// The last `secs` of what a tap delivered.
fn tail(chunks: &[(Instant, Vec<f32>)], secs: f64) -> &[(Instant, Vec<f32>)] {
    let n = (secs * 100.0) as usize; // 100 chunks a second
    &chunks[chunks.len().saturating_sub(n)..]
}

#[test]
fn a_clip_is_heard_only_while_it_is_on_air() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "tone.mp4", "1") else {
        return;
    };
    let Some(Rig {
        mut app,
        pgm,
        deck,
        master,
    }) = rig(&path)
    else {
        return;
    };

    // A new clip has Audio Auto, is sent to Master, and is off until it
    // goes on air.
    let source = |app: &VardaApp| {
        app.build_engine_state()
            .audio_mix
            .sources
            .into_iter()
            .find(|s| s.uuid == deck)
            .expect("the clip has a strip")
    };
    let strip = source(&app);
    assert!(strip.auto, "Audio Auto is on for a new clip");
    assert_eq!(strip.follow, None, "on air anywhere");
    assert_eq!(strip.sends, std::slice::from_ref(&master));
    assert!(strip.muted, "off air, its On button is out");

    let tap = app.audio_mix().tap(&master).expect("Master exists");
    let off_air = listen(&mut app, &tap, 1.0);
    assert!(
        off_air.len() > 80,
        "the mix runs every 10 ms: {} chunks",
        off_air.len()
    );
    assert!(peak(&off_air) < 1e-4, "off air, silent: {}", peak(&off_air));

    cut(&mut app, &pgm, BusInputRef::Deck(deck.clone()));
    let on_air = listen(&mut app, &tap, 1.5);
    assert!(!source(&app).muted, "on air, its On button is pressed");
    let heard = tail(&on_air, 0.8);
    // lavfi's sine is 1/8 of full scale; AAC may shave a little.
    assert!(peak(heard) > 0.08, "on air, the tone: peak {}", peak(heard));
    // 1 kHz: about 2000 zero crossings a second, left channel.
    let left: Vec<f32> = heard
        .iter()
        .flat_map(|(_, s)| s.iter().step_by(2).copied())
        .collect();
    let crossings = left
        .windows(2)
        .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
        .count();
    let per_sec = crossings as f64 / (left.len() as f64 / 48_000.0);
    assert!(
        (per_sec - 1000.0).abs() < 30.0,
        "the tone is still 1 kHz after resampling to 48 kHz: {per_sec} Hz"
    );

    // The operator can still turn it off on air.
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioSourceMute {
            uuid: deck.clone(),
            muted: true,
        },
    ));
    let muted = listen(&mut app, &tap, 0.5);
    assert!(peak(tail(&muted, 0.3)) < 1e-4, "muted");
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioSourceMute {
            uuid: deck.clone(),
            muted: false,
        },
    ));

    // Off air again: the button comes out and the sound goes.
    cut(&mut app, &pgm, BusInputRef::Black);
    let gone = listen(&mut app, &tap, 0.6);
    assert!(source(&app).muted, "off air, its On button is out again");
    assert!(peak(tail(&gone, 0.3)) < 1e-4, "off air, silent");
}

#[test]
fn without_audio_auto_the_on_button_is_the_operators_alone() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "tone.mp4", "1") else {
        return;
    };
    let Some(Rig {
        mut app,
        pgm,
        deck,
        master,
    }) = rig(&path)
    else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioAuto {
            uuid: deck.clone(),
            auto: false,
        },
    ));
    // The clip keeps playing when it leaves air.
    ok(send_cmd(
        &mut app,
        EngineCommand::VideoSetAutoPlayback {
            deck_uuid: deck.clone(),
            auto: varda::engine::value::video::AutoPlayback {
                play: true,
                pause: false,
                restart: false,
                main_only: false,
            },
        },
    ));
    let tap = app.audio_mix().tap(&master).expect("Master exists");
    cut(&mut app, &pgm, BusInputRef::Deck(deck.clone()));
    let on_air = listen(&mut app, &tap, 1.0);
    assert!(
        peak(tail(&on_air, 0.5)) < 1e-4,
        "on air but not switched on: silent"
    );
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioSourceMute {
            uuid: deck.clone(),
            muted: false,
        },
    ));
    cut(&mut app, &pgm, BusInputRef::Black);
    let off_air = listen(&mut app, &tap, 1.2);
    assert!(
        peak(tail(&off_air, 0.6)) > 0.08,
        "switched on by hand, heard off air: {}",
        peak(tail(&off_air, 0.6))
    );
}

#[test]
fn a_fade_brings_the_sound_in_with_the_picture() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "tone.mp4", "1") else {
        return;
    };
    let Some(Rig {
        mut app, pgm, deck, ..
    }) = rig(&path)
    else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: pgm.clone(),
            input: BusInputRef::Deck(deck.clone()),
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::BusAuto {
            uuid: pgm.clone(),
            duration_secs: Some(1.0),
        },
    ));
    let mut halfway = None;
    let end = Instant::now() + Duration::from_secs(2);
    while Instant::now() < end {
        // The frame loop moves bus transitions on before rendering.
        app.advance_frame_state();
        app.render_mixer_frame();
        let strip = app
            .build_engine_state()
            .audio_mix
            .sources
            .into_iter()
            .find(|s| s.uuid == deck)
            .unwrap();
        if (0.3..0.7).contains(&strip.gate) {
            assert!(!strip.muted, "switched on as the fade starts");
            halfway = Some(strip.gate);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(halfway.is_some(), "the level passed through the middle");
    let strip = app
        .build_engine_state()
        .audio_mix
        .sources
        .into_iter()
        .find(|s| s.uuid == deck)
        .unwrap();
    assert!((strip.gate - 1.0).abs() < 1e-6 && !strip.muted, "fully in");
}

#[test]
fn a_clips_sound_stays_in_step_with_its_picture() {
    // Silence, then the tone from 2.0 s to 2.5 s. The tone must reach the bus
    // when the playhead reaches 2.0 s, not a noticeable moment before or
    // after.
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "beep.mp4", "if(between(t,2,2.5),1,0)") else {
        return;
    };
    let Some(Rig {
        mut app,
        pgm,
        deck,
        master,
    }) = rig(&path)
    else {
        return;
    };
    cut(&mut app, &pgm, BusInputRef::Deck(deck.clone()));
    let tap = app.audio_mix().tap(&master).expect("Master exists");
    // Let the sound get going, then jump the picture back to the top: the
    // soundtrack has to notice and follow, as it does on every loop.
    listen(&mut app, &tap, 0.6);
    send_cmd(
        &mut app,
        EngineCommand::VideoSeek {
            deck_uuid: deck.clone(),
            position_secs: 0.0,
        },
    );
    while tap.receiver.try_recv().is_ok() {}

    let playhead = |app: &VardaApp| {
        app.mixer_ref().channels()[0].decks[0]
            .deck
            .playback_snapshot()
            .map_or(0.0, |s| s.position_now())
    };
    let end = Instant::now() + Duration::from_secs(3);
    let mut crossed_at: Option<Instant> = None;
    let mut beep_at: Option<Instant> = None;
    let mut last_pos = playhead(&app);
    while Instant::now() < end && (crossed_at.is_none() || beep_at.is_none()) {
        app.render_mixer_frame();
        let now = Instant::now();
        let pos = playhead(&app);
        if crossed_at.is_none() && last_pos < 2.0 && (2.0..2.3).contains(&pos) {
            crossed_at = Some(now);
        }
        last_pos = pos;
        while let Ok(chunk) = tap.receiver.try_recv() {
            if beep_at.is_none() {
                if let Some(i) = chunk.samples.iter().position(|s| s.abs() > 0.02) {
                    let frames_after = (chunk.samples.len() - i) / 2;
                    beep_at = Instant::now()
                        .checked_sub(Duration::from_secs_f64(frames_after as f64 / 48_000.0));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let crossed_at = crossed_at.expect("the playhead reached 2 s");
    let beep_at = beep_at.expect("the beep was heard");
    let offset = if beep_at > crossed_at {
        (beep_at - crossed_at).as_secs_f64()
    } else {
        -(crossed_at - beep_at).as_secs_f64()
    };
    assert!(
        offset.abs() < 0.06,
        "sound {:+.0} ms from the picture (more than 60 ms is visible)",
        offset * 1000.0
    );
}

#[test]
fn audio_buses_route_and_save_with_the_stage() {
    use clap::Parser as _;
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_str().unwrap().to_string();
    let config = |ws: &str| {
        varda::app::AppConfig::try_parse_from([
            "varda",
            "--headless",
            "--no-osc",
            "--no-ndi",
            "--no-syphon",
            "--no-camera",
            "--workspace",
            ws,
        ])
        .unwrap()
    };
    let (stream, mic) = {
        let Some(gpu) = common::headless_gpu() else {
            return;
        };
        let mut app = VardaApp::new(gpu, &config(&ws)).unwrap();
        let stream = new_uuid(send_cmd(
            &mut app,
            EngineCommand::AddAudioBus {
                name: "STREAM".into(),
            },
        ));
        ok(send_cmd(
            &mut app,
            EngineCommand::SetAudioBusGain {
                uuid: stream.clone(),
                gain_db: -3.0,
            },
        ));
        ok(send_cmd(
            &mut app,
            EngineCommand::SetAudioBusDelay {
                uuid: stream.clone(),
                delay_ms: 80.0,
            },
        ));
        // A microphone that is not connected here still keeps its strip.
        let mic = new_uuid(send_cmd(
            &mut app,
            EngineCommand::AddAudioInput {
                name: "Lectern".into(),
                device: "No Such Interface".into(),
                left: 1,
                right: 1,
            },
        ));
        ok(send_cmd(
            &mut app,
            EngineCommand::SetAudioSend {
                uuid: mic.clone(),
                bus_uuid: stream.clone(),
                on: true,
            },
        ));
        assert!(matches!(
            send_cmd(
                &mut app,
                EngineCommand::SetAudioSend {
                    uuid: mic.clone(),
                    bus_uuid: "nosuchbus".into(),
                    on: true,
                },
            ),
            CommandResult::Err { .. }
        ));
        ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
        (stream, mic)
    };
    let gpu = common::headless_gpu().expect("gpu");
    let mut app = VardaApp::new(gpu, &config(&ws)).unwrap();
    let _ = app.load_workspace();
    let state = app.build_engine_state();
    let bus = state
        .audio_mix
        .buses
        .iter()
        .find(|b| b.config.uuid == stream)
        .expect("the bus came back");
    assert_eq!(bus.config.name, "STREAM");
    assert!((bus.config.gain_db + 3.0).abs() < 1e-6);
    assert!((bus.config.delay_ms - 80.0).abs() < 1e-6);
    let strip = state
        .audio_mix
        .sources
        .iter()
        .find(|s| s.uuid == mic)
        .expect("the microphone came back");
    assert!(!strip.live, "its device is not here");
    assert!(strip.sends.contains(&stream));
}

#[test]
fn a_recording_carries_the_master_bus_by_default() {
    // Like vMix: a new recording takes the Master mix, so a clip that is
    // heard is also in the file.
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "tone.mp4", "1") else {
        return;
    };
    let Some(Rig {
        mut app,
        pgm,
        deck,
        master,
    }) = rig(&path)
    else {
        return;
    };
    cut(&mut app, &pgm, BusInputRef::Deck(deck.clone()));
    let file = dir.path().join("rec.mp4");
    ok(send_cmd(
        &mut app,
        EngineCommand::CreateHeadlessOutput {
            target: varda::renderer::context::OutputTarget::Recording {
                path: file.to_string_lossy().into_owned(),
                codec: varda::renderer::context::RecordingCodec::H264,
                audio_device: None,
            },
        },
    ));
    let out = app
        .build_engine_state()
        .outputs
        .windows
        .last()
        .expect("recording created")
        .uuid
        .clone();
    assert_eq!(
        app.outputs_ref().iter().find_map(|o| match o {
            varda::renderer::context::UnifiedOutput::Headless(h) if h.uuid == out => {
                h.audio_bus.clone()
            }
            _ => None,
        }),
        Some(master),
        "a new recording carries the first audio bus"
    );
    ok(send_cmd(
        &mut app,
        EngineCommand::StartOutput {
            output_uuid: out.clone(),
        },
    ));
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        app.render_mixer_frame();
        app.render_outputs();
        std::thread::sleep(Duration::from_millis(15));
    }
    ok(send_cmd(
        &mut app,
        EngineCommand::StopOutput { output_uuid: out },
    ));

    // Stopping finishes the file on a background thread; wait for it.
    let finished = Instant::now() + Duration::from_secs(20);
    loop {
        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=codec_type"])
            .arg(&file)
            .output()
            .expect("ffprobe runs");
        if probe.status.success() {
            let streams = String::from_utf8_lossy(&probe.stdout).to_string();
            assert!(
                streams.contains("codec_type=audio"),
                "an audio stream: {streams}"
            );
            break;
        }
        assert!(
            Instant::now() < finished,
            "the recording was never finished"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    // Decode the recording's sound and look for the tone.
    let pcm = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(&file)
        .args(["-vn", "-f", "f32le", "-ac", "1", "-ar", "48000", "-"])
        .output()
        .expect("ffmpeg decodes the recording");
    let samples: Vec<f32> = pcm
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    assert!(
        samples.len() > 48_000,
        "the file has at least a second of sound: {} samples",
        samples.len()
    );
    let loud = samples[samples.len() / 2..]
        .iter()
        .fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        loud > 0.05,
        "the clip's tone is in the recording: peak {loud}"
    );
}

#[test]
fn pfl_lets_the_operator_hear_a_clip_before_it_goes_on_air() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = clip(dir.path(), "tone.mp4", "1") else {
        return;
    };
    let Some(Rig {
        mut app,
        deck,
        master,
        ..
    }) = rig(&path)
    else {
        return;
    };
    // Off air, switched off; played by hand to check it.
    ok(send_cmd(
        &mut app,
        EngineCommand::VideoTogglePlay {
            deck_uuid: deck.clone(),
        },
    ));
    let master_tap = app.audio_mix().tap(&master).expect("Master exists");
    let phones = app.audio_mix().tap_monitor().expect("the monitor");
    let before = listen(&mut app, &phones, 0.6);
    assert!(peak(&before) < 1e-4, "nothing in the headphones yet");

    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioSolo {
            uuid: deck.clone(),
            solo: true,
        },
    ));
    let soloed = listen(&mut app, &phones, 1.2);
    assert!(
        peak(tail(&soloed, 0.6)) > 0.08,
        "soloed, the clip is in the headphones though off air: {}",
        peak(tail(&soloed, 0.6))
    );
    let master_chunks: Vec<_> = master_tap
        .receiver
        .try_iter()
        .map(|c| (Instant::now(), c.samples))
        .collect();
    assert!(
        peak(&master_chunks) < 1e-4,
        "and nothing reaches Master: {}",
        peak(&master_chunks)
    );
    let state = app.build_engine_state();
    assert!(state.audio_mix.monitor.soloing);
    assert!(state
        .audio_mix
        .sources
        .iter()
        .any(|s| s.uuid == deck && s.solo));
}

/// A clip that flashes white and beeps at the same moments — a fifth of a
/// second every two seconds, far enough apart that a sound half a second out
/// is not taken for the next flash's — or `None` without the ffmpeg tool.
/// The sound is PCM, so no encoder delay moves it off the picture.
fn flash_clip(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("flash.mov");
    let status = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(
            "color=c=black:s=320x180:r=30:d=20,\
             drawbox=x=0:y=0:w=iw:h=ih:color=white:t=fill:enable='lt(mod(t,2),0.2)'",
        )
        .args(["-f", "lavfi", "-i"])
        .arg("aevalsrc=exprs='if(lt(mod(t,2),0.2),0.7*sin(2*PI*1000*t),0)':s=48000:d=20")
        .args(["-c:v", "mpeg4", "-q:v", "2", "-c:a", "pcm_s16le", "-shortest"])
        .arg(&path)
        .status();
    match status {
        Ok(s) if s.success() => Some(path),
        _ => {
            eprintln!("ffmpeg not available — skipping");
            None
        }
    }
}

/// When each flash starts in a recording, and each beep, in seconds of the
/// file.
fn flashes_and_beeps(file: &Path) -> (Vec<f64>, Vec<f64>) {
    let until = Instant::now() + Duration::from_secs(30);
    let pictures = loop {
        let out = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(file)
            .args(["-map", "0:v:0", "-vf"])
            .arg("signalstats,metadata=print:key=lavfi.signalstats.YAVG:file=-")
            .args(["-f", "null", "-"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if out.status.success() && text.contains("YAVG") {
            break text;
        }
        assert!(Instant::now() < until, "{} never became readable", file.display());
        std::thread::sleep(Duration::from_millis(200));
    };
    // Each frame's time and mean brightness, on whatever scale its bit
    // depth gives: a flash is a frame past halfway from the darkest to the
    // brightest after one that was not.
    let mut levels = Vec::new();
    let mut at = 0.0;
    for line in pictures.lines() {
        if let Some(time) = line.split("pts_time:").nth(1) {
            at = time.trim().parse().unwrap_or(at);
        } else if let Some(level) = line.split("YAVG=").nth(1) {
            levels.push((at, level.trim().parse::<f64>().unwrap_or(0.0)));
        }
    }
    let darkest = levels.iter().map(|l| l.1).fold(f64::INFINITY, f64::min);
    let brightest = levels.iter().map(|l| l.1).fold(0.0, f64::max);
    let halfway = f64::midpoint(darkest, brightest);
    let mut flashes = Vec::new();
    let mut bright = true;
    for &(at, level) in &levels {
        if !bright && level > halfway {
            flashes.push(at);
        }
        bright = level > halfway;
    }
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file)
        .args(["-map", "0:a:0", "-ac", "1", "-ar", "48000", "-f", "f32le", "-"])
        .output()
        .unwrap();
    let samples: Vec<f32> = out
        .stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect();
    let mut beeps = Vec::new();
    let mut quiet = 0usize;
    for (i, s) in samples.iter().enumerate() {
        if s.abs() > 0.2 {
            if quiet > 14_400 {
                beeps.push(i as f64 / 48_000.0);
            }
            quiet = 0;
        } else {
            quiet += 1;
        }
    }
    (flashes, beeps)
}

/// How far each beep in a take lands from its flash, in seconds — positive
/// when the sound is late. `stall` holds the frames back for that long right
/// after REC is pressed, as a slow encoder start did.
fn recorded_offsets(
    codec: varda::record::RecordCodec,
    format: varda::record::RecordFormat,
    stall: Duration,
) -> Option<Vec<f64>> {
    let dir = tempfile::tempdir().unwrap();
    let path = flash_clip(dir.path())?;
    let Rig {
        mut app, pgm, deck, ..
    } = rig(&path)?;
    cut(&mut app, &pgm, BusInputRef::Deck(deck));
    let source = varda::engine::OutputSource::BusProgram(pgm);
    let run = |app: &mut VardaApp, secs: f64| {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end {
            app.render_mixer_frame();
            app.render_outputs();
            std::thread::sleep(Duration::from_millis(12));
        }
    };
    run(&mut app, 1.0);
    let options = varda::record::RecordOptions {
        folder: dir.path().join("takes"),
        source,
        codec,
        format,
        size: Some((320, 180)),
        ..varda::record::RecordOptions::default()
    };
    ok(send_cmd(&mut app, EngineCommand::SetRecordOptions { options }));
    ok(send_cmd(&mut app, EngineCommand::StartRecording));
    std::thread::sleep(stall);
    run(&mut app, 9.0);
    let file = PathBuf::from(app.record_status().file.expect("recording"));
    ok(send_cmd(&mut app, EngineCommand::StopRecording));
    let (flashes, beeps) = flashes_and_beeps(&file);
    let offsets: Vec<f64> = flashes
        .iter()
        .filter_map(|&flash| {
            beeps
                .iter()
                .map(|&beep| beep - flash)
                .filter(|off| off.abs() < 0.9)
                .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        })
        .collect();
    assert!(
        offsets.len() >= 3,
        "flashes {flashes:?}, beeps {beeps:?}: too few pairs"
    );
    Some(offsets)
}

fn assert_together(offsets: &[f64], what: &str) {
    let ms: Vec<i64> = offsets.iter().map(|o| (o * 1000.0).round() as i64).collect();
    assert!(
        offsets.iter().all(|o| o.abs() < 0.05),
        "{what}: the sound lands {ms:?} ms from the picture (more than 50 ms is visible)"
    );
}

/// A frame drawn for a screen's refresh goes to the recording timed by that
/// refresh, not by when it was read back after the mixer had drawn it: the
/// mixer takes the refresh, and the recording once timed every frame by the
/// moment after, a few milliseconds later each time.
#[test]
fn a_frame_is_recorded_at_the_refresh_it_was_drawn_for() {
    let Some(mut app) = headless_app() else {
        return;
    };
    let refresh = Instant::now()
        .checked_sub(Duration::from_millis(30))
        .unwrap();
    app.set_frame_refresh_at(Some(refresh));
    app.render_mixer_frame();
    assert_eq!(app.frame_drawn_at(), Some(refresh));
}

#[test]
fn a_recording_keeps_the_sound_with_the_picture() {
    let Some(offsets) = recorded_offsets(
        varda::record::RecordCodec::H264,
        varda::record::RecordFormat::Mp4,
        Duration::ZERO,
    ) else {
        return;
    };
    assert_together(&offsets, "H.264 in MP4");
}

#[test]
fn a_recording_started_while_the_frames_stall_keeps_the_sound_with_the_picture() {
    // The first REC after a launch tried the media engine first, and the
    // frames stopped for half a second while it did.
    let Some(offsets) = recorded_offsets(
        varda::record::RecordCodec::H264,
        varda::record::RecordFormat::Mp4,
        Duration::from_millis(500),
    ) else {
        return;
    };
    assert_together(&offsets, "after a half-second stall");
}

#[test]
fn a_prores_recording_keeps_the_sound_with_the_picture() {
    let Some(offsets) = recorded_offsets(
        varda::record::RecordCodec::ProRes,
        varda::record::RecordFormat::Mov,
        Duration::from_millis(500),
    ) else {
        return;
    };
    assert_together(&offsets, "ProRes in MOV");
}
