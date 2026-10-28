//! An input's colour and the buttons a bus's switcher row has, through the
//! engine: set, kept with the show, undone, and let go of with a deleted
//! input.

use varda::app::VardaApp;
use varda::bus::BusInputRef;
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
    assert!(
        matches!(result, CommandResult::Ok | CommandResult::OkWithId { .. }),
        "{result:?}"
    );
}

fn refused(result: &CommandResult) -> bool {
    matches!(result, CommandResult::Err { .. })
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

fn channel(app: &VardaApp, idx: usize) -> String {
    app.build_engine_state().mixer.channels[idx].uuid.clone()
}

fn colour(app: &VardaApp, uuid: &str) -> Option<[u8; 3]> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == uuid)
        .expect("the channel")
        .color
}

fn add_bus(app: &mut VardaApp, name: &str) -> String {
    match send_cmd(
        app,
        EngineCommand::AddBus {
            name: name.into(),
            width: 1920,
            height: 1080,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected an id, got {other:?}"),
    }
}

fn hidden(app: &VardaApp, bus: &str) -> Vec<BusInputRef> {
    app.build_engine_state()
        .buses
        .iter()
        .find(|b| b.uuid == bus)
        .expect("the bus")
        .hidden_sources
        .clone()
}

fn save(app: &mut VardaApp) {
    app.save_workspace(&varda::usecases::ui::UILayoutState::default())
        .expect("save workspace");
}

/// A colour given to an input is shown, kept with the show, and taken
/// back by Undo; an input that is not there is refused.
#[test]
fn an_inputs_colour_is_kept_with_the_show_and_undone() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_str().unwrap().to_string();
    let uuid = {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let uuid = channel(&app, 1);
        assert_eq!(colour(&app, &uuid), None);
        ok(send_cmd(
            &mut app,
            EngineCommand::SetChannelColor {
                channel_uuid: uuid.clone(),
                color: Some([55, 115, 220]),
            },
        ));
        assert_eq!(colour(&app, &uuid), Some([55, 115, 220]));
        assert!(refused(&send_cmd(
            &mut app,
            EngineCommand::SetChannelColor {
                channel_uuid: "nosuchch".into(),
                color: Some([1, 2, 3]),
            },
        )));
        save(&mut app);
        uuid
    };
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let load = app.load_workspace();
    assert!(load.is_ok(), "{:?}", load.error_message());
    assert_eq!(colour(&app, &uuid), Some([55, 115, 220]), "kept with the show");

    ok(send_cmd(
        &mut app,
        EngineCommand::SetChannelColor {
            channel_uuid: uuid.clone(),
            color: None,
        },
    ));
    assert_eq!(colour(&app, &uuid), None, "back to the colour of its place");
    ok(send_cmd(&mut app, EngineCommand::Undo));
    assert_eq!(colour(&app, &uuid), Some([55, 115, 220]), "undone");
}

/// The buttons taken off a bus's row stay off with the show; Undo puts one
/// back; a layer, or an input that is not there, is refused; and a deleted
/// input's button is let go of.
#[test]
fn a_bus_rows_buttons_are_kept_with_the_show() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_str().unwrap().to_string();
    let (led, first) = {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let led = add_bus(&mut app, "LED");
        let first = channel(&app, 0);
        for input in [BusInputRef::Black, BusInputRef::Channel(first.clone())] {
            ok(send_cmd(
                &mut app,
                EngineCommand::SetBusSourceHidden {
                    uuid: led.clone(),
                    input,
                    hidden: true,
                },
            ));
        }
        assert_eq!(
            hidden(&app, &led),
            vec![BusInputRef::Black, BusInputRef::Channel(first.clone())]
        );
        let deck_uuid = app.build_engine_state().mixer.channels[0]
            .decks
            .first()
            .map_or_else(|| "anydeck1".to_string(), |d| d.uuid.clone());
        for input in [
            BusInputRef::Deck(deck_uuid),
            BusInputRef::Channel("nosuchch".into()),
        ] {
            assert!(refused(&send_cmd(
                &mut app,
                EngineCommand::SetBusSourceHidden {
                    uuid: led.clone(),
                    input,
                    hidden: true,
                },
            )));
        }
        assert!(refused(&send_cmd(
            &mut app,
            EngineCommand::SetBusSourceHidden {
                uuid: "nosuchbus".into(),
                input: BusInputRef::Master,
                hidden: true,
            },
        )));
        save(&mut app);
        (led, first)
    };
    let Some(mut app) = app_in(&ws) else {
        return;
    };
    let load = app.load_workspace();
    assert!(load.is_ok(), "{:?}", load.error_message());
    assert_eq!(
        hidden(&app, &led),
        vec![BusInputRef::Black, BusInputRef::Channel(first.clone())],
        "kept with the show"
    );

    ok(send_cmd(
        &mut app,
        EngineCommand::SetBusSourceHidden {
            uuid: led.clone(),
            input: BusInputRef::Black,
            hidden: false,
        },
    ));
    assert_eq!(hidden(&app, &led), vec![BusInputRef::Channel(first.clone())]);
    ok(send_cmd(&mut app, EngineCommand::Undo));
    assert_eq!(
        hidden(&app, &led),
        vec![BusInputRef::Black, BusInputRef::Channel(first.clone())],
        "undone"
    );

    ok(send_cmd(&mut app, EngineCommand::AddChannel));
    let third = channel(&app, 2);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetBusSourceHidden {
            uuid: led.clone(),
            input: BusInputRef::Channel(third.clone()),
            hidden: true,
        },
    ));
    assert_eq!(hidden(&app, &led).len(), 3);
    ok(send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: third,
        },
    ));
    assert_eq!(
        hidden(&app, &led),
        vec![BusInputRef::Black, BusInputRef::Channel(first)],
        "a deleted input's button is let go of"
    );
}
