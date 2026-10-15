//! A layer hidden under one that covers the whole frame is not rendered —
//! nothing of it shows — unless it is shown on its own somewhere: alone on a
//! bus, alone on Main, or selected for editing.

use varda::app::VardaApp;
use varda::bus::BusInputRef;
use varda::engine::{CommandResult, EngineCommand};
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

/// An app with a red layer under an opaque blue one on the first channel,
/// that channel on Main: (app, channel, the hidden layer, the first bus).
fn rig() -> Option<(VardaApp, String, String, String)> {
    let gpu = common::headless_gpu()?;
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let ch = state.mixer.channels[0].uuid.clone();
    let bus = state.buses[0].uuid.clone();
    for deck in &state.mixer.channels[0].decks {
        send_cmd(
            &mut app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    let hidden = id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch.clone()),
            fade: false,
        },
    );
    Some((app, ch, hidden, bus))
}

/// Layers the channel rendered in the last frame.
fn rendered(app: &mut VardaApp, ch: &str) -> u32 {
    app.render_mixer_frame();
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == ch)
        .unwrap()
        .active_deck_count
}

#[test]
fn a_covered_layer_is_rendered_only_while_shown_on_its_own() {
    let Some((mut app, ch, hidden, bus)) = rig() else {
        return;
    };
    assert_eq!(rendered(&mut app, &ch), 1, "the covered layer is skipped");

    // Alone on a bus's Program.
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Deck(hidden.clone()),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    assert_eq!(rendered(&mut app, &ch), 2, "a bus shows it alone");
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Black,
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    assert_eq!(
        rendered(&mut app, &ch),
        2,
        "the cut swapped it to the bus's Preview, which shows it"
    );
    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus,
            input: BusInputRef::Black,
        },
    );
    assert_eq!(rendered(&mut app, &ch), 1);

    // Alone on Main.
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Deck(hidden),
            fade: false,
        },
    );
    assert_eq!(rendered(&mut app, &ch), 2, "Main shows it alone");

    // Selected for editing.
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch.clone()),
            fade: false,
        },
    );
    assert_eq!(rendered(&mut app, &ch), 1);
    app.set_ui_selected_deck(Some((0, 0)));
    assert_eq!(rendered(&mut app, &ch), 2, "its settings show it");
}

/// A layer partly covered is drawn only where it shows, but whole while it
/// is shown on its own: on a bus, or fed by itself to a surface.
#[test]
fn a_partly_covered_layer_is_drawn_whole_while_shown_on_its_own() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let ch = state.mixer.channels[0].uuid.clone();
    let bus = state.buses[0].uuid.clone();
    for deck in &state.mixer.channels[0].decks {
        send_cmd(
            &mut app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    let pattern = id(send_cmd(
        &mut app,
        EngineCommand::AddDeck {
            channel_uuid: ch.clone(),
            shader_name: "checkerboard".into(),
        },
    ));
    let picture = id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    // The picture at half size in the middle.
    let state = app.build_engine_state();
    let transform = state.mixer.channels[0]
        .decks
        .iter()
        .find(|d| d.uuid == picture)
        .and_then(|d| d.effects.iter().find(|e| e.name == "transform"))
        .map(|e| e.uuid.clone());
    let transform = transform.unwrap_or_else(|| {
        id(send_cmd(
            &mut app,
            EngineCommand::AddEffect {
                target: varda::engine::EffectTarget::Deck(picture.clone()),
                shader_name: "transform".into(),
            },
        ))
    });
    send_cmd(
        &mut app,
        EngineCommand::SetEffectParam {
            effect_uuid: transform,
            name: "scale".into(),
            value: varda::params::ParamValue::Float(0.5),
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(ch.clone()),
            fade: false,
        },
    );
    // The share drawn once the change has reached a drawn frame: a frame the
    // layer sits out (under load) leaves the last one's share standing.
    let drawn = |app: &mut VardaApp| {
        let mut share = 0.0;
        for _ in 0..3 {
            app.render_mixer_frame();
            share = app.mixer_ref().channels()[0].decks[0].deck.drawn_share();
        }
        share
    };
    assert_eq!(app.mixer_ref().channels()[0].decks[0].deck.uuid(), pattern);
    assert!(drawn(&mut app) < 0.9, "drawn round the picture only");

    send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Deck(pattern),
        },
    );
    send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    assert!(drawn(&mut app) >= 1.0, "a bus shows it alone");
    for _ in 0..2 {
        send_cmd(
            &mut app,
            EngineCommand::SetBusPreview {
                uuid: bus.clone(),
                input: BusInputRef::Black,
            },
        );
        send_cmd(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    }
    assert!(drawn(&mut app) < 0.9, "off the bus, in parts again");

    send_cmd(
        &mut app,
        EngineCommand::AddSurface {
            name: "Just the pattern".into(),
            source: varda::engine::value::render::OutputSource::Deck(0, 0),
        },
    );
    assert!(drawn(&mut app) >= 1.0, "a surface shows it alone");
}
