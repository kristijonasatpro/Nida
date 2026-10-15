//! Where frames' time goes: each part of the frame loop timed, a slow frame
//! kept part by part, and how many frames the GPU may have in flight — all in
//! the performance figures.

use std::time::Duration;

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand};

mod common;

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

#[test]
fn frames_in_flight_are_set_within_one_to_three() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    assert_eq!(app.build_engine_state().pacing.frames_in_flight, 1);
    for (asked, got) in [(2, 2), (9, 3), (0, 1)] {
        let result = send_cmd(&mut app, EngineCommand::SetFramesInFlight { frames: asked });
        assert!(matches!(result, CommandResult::Ok), "{result:?}");
        assert_eq!(app.build_engine_state().pacing.frames_in_flight, got);
    }
}

#[test]
fn a_frames_parts_and_a_slow_frame_reach_the_figures() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    for i in 0..3_u64 {
        app.frame_begin(Some(Duration::from_micros(300)));
        app.render_mixer_frame();
        app.frame_part("ui", Duration::from_millis(3));
        // The last frame waits a long time on the GPU.
        app.frame_part("gpu wait", Duration::from_millis(if i == 2 { 30 } else { 1 }));
        std::thread::sleep(Duration::from_millis(if i == 2 { 30 } else { 1 }));
        app.frame_end();
    }
    let pacing = app.build_engine_state().pacing;
    let names: Vec<&str> = pacing.parts.iter().map(|p| p.name.as_str()).collect();
    for part in ["ui", "gpu wait", "start delay"] {
        assert!(names.contains(&part), "{part} in {names:?}");
    }
    let gpu = pacing.parts.iter().find(|p| p.name == "gpu wait").unwrap();
    assert!((gpu.max_ms - 30.0).abs() < 0.5, "{gpu:?}");
    let slow = pacing.slow_frames.last().expect("the long frame kept");
    assert!(slow.busy_ms >= 30.0, "{slow:?}");
    assert!(
        slow.parts.iter().any(|p| p.name == "gpu wait" && p.ms >= 29.0),
        "{slow:?}"
    );
}

#[test]
fn frames_can_be_drawn_ahead_and_back_on_the_refresh() {
    use varda::engine::value::render::FramePacing;
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    assert_eq!(app.build_engine_state().pacing.frame_pacing, FramePacing::Refresh);
    // As the API sends it.
    let ahead: EngineCommand =
        serde_json::from_str(r#"{"SetFramePacing":{"pacing":"ahead"}}"#).unwrap();
    let result = send_cmd(&mut app, ahead);
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
    assert_eq!(app.build_engine_state().pacing.frame_pacing, FramePacing::Ahead);
    send_cmd(
        &mut app,
        EngineCommand::SetFramePacing {
            pacing: FramePacing::Refresh,
        },
    );
    assert_eq!(app.build_engine_state().pacing.frame_pacing, FramePacing::Refresh);
}

#[test]
fn frames_can_be_asked_for_by_refresh_timestamps() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    assert!(!app.build_engine_state().pacing.clock_timestamps, "off to start");
    let on: EngineCommand = serde_json::from_str(r#"{"SetClockTimestamps":{"on":true}}"#).unwrap();
    let result = send_cmd(&mut app, on);
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
    assert!(app.build_engine_state().pacing.clock_timestamps);
}

#[test]
fn frames_can_be_held_on_screen_a_frames_time() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    assert_eq!(
        app.build_engine_state().pacing.frame_hold,
        cfg!(target_os = "macos"),
        "on where there is a hold to give"
    );
    let off: EngineCommand = serde_json::from_str(r#"{"SetFrameHold":{"on":false}}"#).unwrap();
    let result = send_cmd(&mut app, off);
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
    assert!(!app.build_engine_state().pacing.frame_hold);
    send_cmd(&mut app, EngineCommand::SetFrameHold { on: true });
    assert_eq!(app.frame_hold_secs(), None, "not on the timer");
    app.set_frame_clock("display 1 at 60.00 Hz, every refresh".into(), Some(1));
    assert_eq!(app.frame_hold_secs(), Some(1.0 / 60.0), "a frame's time on a screen's refresh");
}

#[test]
fn the_control_window_can_present_in_step_with_its_screen() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    assert!(!app.build_engine_state().pacing.control_vsync, "off to start");
    let on: EngineCommand = serde_json::from_str(r#"{"SetControlVsync":{"on":true}}"#).unwrap();
    let result = send_cmd(&mut app, on);
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
    assert!(app.build_engine_state().pacing.control_vsync);
}
