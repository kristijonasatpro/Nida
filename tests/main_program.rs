//! Main, the vMix way, through the engine: commands, saving, and undo
//! leaving what is on air alone.

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand, ErrorCode};
use varda::mixer::{MainSource, MixMode};

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

fn channels(app: &VardaApp) -> Vec<String> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|c| c.uuid.clone())
        .collect()
}

fn main_state(app: &VardaApp) -> varda::mixer::MainSnapshot {
    app.build_engine_state().mixer.main
}

#[test]
fn main_takes_channels_and_switches_overlays() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let ch = channels(&app);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch[0].clone()),
            fade: false,
        },
    ));
    assert_eq!(main_state(&app).program, MainSource::Channel(ch[0].clone()));

    // The second channel becomes an overlay: taking it switches it on
    // rather than replacing what is on Main.
    ok(send_cmd(
        &mut app,
        EngineCommand::SetChannelOverlay {
            channel_uuid: ch[1].clone(),
            overlay: true,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch[1].clone()),
            fade: false,
        },
    ));
    let main = main_state(&app);
    assert_eq!(
        main.program,
        MainSource::Channel(ch[0].clone()),
        "still on Main"
    );
    assert_eq!(main.overlays_on, [ch[1].clone()]);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetOverlayOn {
            channel_uuid: ch[1].clone(),
            on: false,
            fade: false,
        },
    ));
    assert!(main_state(&app).overlays_on.is_empty());

    // Wrong addresses are refused with a reason.
    match send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel("nosuchch".into()),
            fade: false,
        },
    ) {
        CommandResult::Err { code, .. } => assert_eq!(code, ErrorCode::NotFound),
        other => panic!("{other:?}"),
    }
    match send_cmd(
        &mut app,
        EngineCommand::SetOverlayOn {
            channel_uuid: ch[0].clone(),
            on: true,
            fade: false,
        },
    ) {
        CommandResult::Err { code, .. } => assert_eq!(code, ErrorCode::InvalidInput),
        other => panic!("{other:?}"),
    }
}

#[test]
fn switching_to_main_keeps_the_picture_the_mix_was_showing() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let ch = channels(&app);
    // A deck in the first channel only: that is what the mix shows.
    let added = send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch[0].clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    );
    assert!(matches!(added, CommandResult::OkWithId { .. }), "{added:?}");
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    ));
    assert_eq!(main_state(&app).program, MainSource::Channel(ch[0].clone()));
}

#[test]
fn undoing_an_edit_leaves_main_on_air() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let ch = channels(&app);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch[0].clone()),
            fade: false,
        },
    ));
    // An authoring edit, then the next speaker goes to Main.
    ok(send_cmd(
        &mut app,
        EngineCommand::SetChannelBlendMode {
            channel_uuid: ch[1].clone(),
            mode: varda::BlendMode::Add,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch[1].clone()),
            fade: false,
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::Undo));
    let state = app.build_engine_state();
    assert_eq!(
        state.mixer.channels[1].blend_mode,
        varda::BlendMode::Normal,
        "the edit is undone"
    );
    assert_eq!(
        state.mixer.main.program,
        MainSource::Channel(ch[1].clone()),
        "Main is not"
    );
    assert_eq!(state.mixer.main.mode, MixMode::Program);
}

#[test]
fn main_is_kept_with_the_scene() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let ch = {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let ch = channels(&app);
        for cmd in [
            EngineCommand::SetMixMode {
                mode: MixMode::Program,
            },
            EngineCommand::SetChannelOverlay {
                channel_uuid: ch[1].clone(),
                overlay: true,
            },
            EngineCommand::TakeToMain {
                source: MainSource::Channel(ch[0].clone()),
                fade: false,
            },
            EngineCommand::SetOverlayOn {
                channel_uuid: ch[1].clone(),
                on: true,
                fade: false,
            },
            EngineCommand::SetMainFadeTime { secs: 2.5 },
            EngineCommand::SetMainTransition {
                transition: varda::mixer::MainTransition::Merge,
            },
            EngineCommand::SaveWorkspace,
        ] {
            ok(send_cmd(&mut app, cmd));
        }
        ch
    };
    let mut app = app_in(&ws).expect("gpu");
    let _ = app.load_workspace();
    let state = app.build_engine_state();
    let main = &state.mixer.main;
    assert_eq!(main.mode, MixMode::Program);
    assert_eq!(main.program, MainSource::Channel(ch[0].clone()));
    assert_eq!(main.overlays_on, [ch[1].clone()]);
    assert!((main.fade_secs - 2.5).abs() < 1e-6);
    assert_eq!(main.transition, varda::mixer::MainTransition::Merge);
    assert!(state.mixer.channels[1].overlay, "the overlay role too");
}
