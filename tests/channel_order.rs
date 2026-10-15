//! Renaming a channel and moving it along the row, through the engine: the
//! order and names are kept with the scene and undo takes a move back.

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

/// (uuid, name) of each channel, in order.
fn channels(app: &VardaApp) -> Vec<(String, String)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|c| (c.uuid.clone(), c.name.clone()))
        .collect()
}

fn uuids(app: &VardaApp) -> Vec<String> {
    channels(app).into_iter().map(|(u, _)| u).collect()
}

#[test]
fn a_channel_is_renamed() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let ch = uuids(&app)[0].clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::RenameChannel {
            channel_uuid: ch.clone(),
            name: "  Speaker cam  ".into(),
        },
    ));
    assert_eq!(channels(&app)[0].1, "Speaker cam", "trimmed");
    let refused = send_cmd(
        &mut app,
        EngineCommand::RenameChannel {
            channel_uuid: ch,
            name: "   ".into(),
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
    assert_eq!(channels(&app)[0].1, "Speaker cam", "unchanged");
}

#[test]
fn a_channel_moves_before_another_or_to_the_end() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    assert!(matches!(
        send_cmd(&mut app, EngineCommand::AddChannel),
        CommandResult::OkWithId { .. }
    ));
    let before = uuids(&app);
    let [a, b, c] = [before[0].clone(), before[1].clone(), before[2].clone()];
    ok(send_cmd(
        &mut app,
        EngineCommand::MoveChannel {
            channel_uuid: c.clone(),
            before_channel_uuid: Some(a.clone()),
        },
    ));
    assert_eq!(uuids(&app), vec![c.clone(), a.clone(), b.clone()]);
    ok(send_cmd(
        &mut app,
        EngineCommand::MoveChannel {
            channel_uuid: c.clone(),
            before_channel_uuid: None,
        },
    ));
    assert_eq!(
        uuids(&app),
        vec![a.clone(), b.clone(), c.clone()],
        "to the end"
    );
    let missing = send_cmd(
        &mut app,
        EngineCommand::MoveChannel {
            channel_uuid: "nosuchch".into(),
            before_channel_uuid: None,
        },
    );
    assert!(matches!(missing, CommandResult::Err { .. }));
}

#[test]
fn undo_takes_a_move_back() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let before = uuids(&app);
    ok(send_cmd(
        &mut app,
        EngineCommand::MoveChannel {
            channel_uuid: before[1].clone(),
            before_channel_uuid: Some(before[0].clone()),
        },
    ));
    assert_ne!(uuids(&app), before);
    ok(send_cmd(&mut app, EngineCommand::Undo));
    assert_eq!(uuids(&app), before);
}

#[test]
fn names_and_order_are_kept_with_the_scene() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_str().unwrap().to_string();
    let saved = {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let ids = uuids(&app);
        ok(send_cmd(
            &mut app,
            EngineCommand::RenameChannel {
                channel_uuid: ids[1].clone(),
                name: "Slides".into(),
            },
        ));
        ok(send_cmd(
            &mut app,
            EngineCommand::MoveChannel {
                channel_uuid: ids[1].clone(),
                before_channel_uuid: Some(ids[0].clone()),
            },
        ));
        ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
        channels(&app)
    };
    let mut app = app_in(&ws).unwrap();
    let _ = app.load_workspace();
    assert_eq!(channels(&app), saved);
    assert_eq!(channels(&app)[0].1, "Slides");
}
