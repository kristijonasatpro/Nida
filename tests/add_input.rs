//! Add Input, through the engine: a live source becomes a channel of its own
//! at the end of the row, named after it or as asked; one undo takes it
//! away; a source that fails leaves nothing behind.

use varda::app::VardaApp;
use varda::engine::{CommandResult, EngineCommand, InputAudio, InputSource};

mod common;

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
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

/// (uuid, name, layer names) of each channel, in order.
fn channels(app: &VardaApp) -> Vec<(String, String, Vec<String>)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|c| {
            (
                c.uuid.clone(),
                c.name.clone(),
                c.decks.iter().map(|d| d.name.clone()).collect(),
            )
        })
        .collect()
}

fn colour() -> InputSource {
    InputSource::Colour {
        color: [0.1, 0.2, 0.3, 1.0],
    }
}

#[test]
fn a_new_input_is_a_channel_at_the_end_named_after_its_source() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let before = channels(&app);
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: colour(),
            name: None,
            audio: None,
        },
    );
    let CommandResult::OkWithId { uuid } = reply else {
        panic!("expected the new input's UUID, got {reply:?}");
    };
    let after = channels(&app);
    assert_eq!(after.len(), before.len() + 1);
    let (last_uuid, name, layers) = after.last().unwrap();
    assert_eq!(last_uuid, &uuid, "the reply names the new channel");
    assert_eq!(name, "Colour");
    assert_eq!(layers.len(), 1, "one layer, the source: {layers:?}");

    // Named as asked.
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: colour(),
            name: Some("  Backdrop ".into()),
            audio: None,
        },
    );
    assert!(matches!(reply, CommandResult::OkWithId { .. }), "{reply:?}");
    assert_eq!(channels(&app).last().unwrap().1, "Backdrop");
}

#[test]
fn one_undo_takes_a_new_input_away() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let before = channels(&app);
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: colour(),
            name: None,
            audio: None,
        },
    );
    assert!(matches!(reply, CommandResult::OkWithId { .. }), "{reply:?}");
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(channels(&app), before);
}

#[test]
fn a_source_that_fails_leaves_no_channel_behind() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let before = channels(&app);
    // NDI is switched off in this app, so the source cannot start.
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::Ndi {
                source_name: "STUDIO (Camera 1)".into(),
            },
            name: None,
            audio: None,
        },
    );
    assert!(matches!(reply, CommandResult::Err { .. }), "{reply:?}");
    assert_eq!(channels(&app), before);
    // So does a camera that is not there.
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::Camera {
                camera_id: 7,
                mode: None,
            },
            name: None,
            audio: Some(capture_card_sound()),
        },
    );
    assert!(matches!(reply, CommandResult::Err { .. }), "{reply:?}");
    assert_eq!(channels(&app), before);
    assert!(
        app.audio_mix().config().inputs.is_empty(),
        "nor its sound: {:?}",
        app.audio_mix().config().inputs
    );
}

fn capture_card_sound() -> InputAudio {
    InputAudio {
        device: "Blackmagic Design".into(),
        left: 0,
        right: 1,
    }
}

/// The sound strip of the input's own sound: (On pressed, Audio Auto).
fn own_sound(app: &VardaApp) -> Option<(bool, bool)> {
    let input = app.audio_mix().config().inputs.first()?.uuid.clone();
    let strip = app
        .build_engine_state()
        .audio_mix
        .sources
        .into_iter()
        .find(|s| s.uuid == input)?;
    Some((!strip.muted, strip.auto))
}

#[test]
fn an_inputs_own_sound_goes_on_and_off_air_with_its_picture() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    // A colour stands in for the camera: the sound is linked the same way.
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: colour(),
            name: Some("Stage cam".into()),
            audio: Some(capture_card_sound()),
        },
    );
    let CommandResult::OkWithId { uuid: input } = reply else {
        panic!("expected the new input, got {reply:?}");
    };
    let deck = channels(&app).last().unwrap().2.len();
    assert_eq!(deck, 1);
    let config = app.audio_mix().config().clone();
    assert_eq!(config.inputs.len(), 1);
    let sound = &config.inputs[0];
    assert_eq!(sound.name, "Stage cam", "named as the input");
    assert_eq!(sound.device, "Blackmagic Design");
    let layer = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == input)
        .unwrap()
        .decks[0]
        .uuid
        .clone();
    assert_eq!(sound.deck.as_deref(), Some(layer.as_str()));

    app.update_audio_mix();
    assert_eq!(own_sound(&app), Some((false, true)), "off until on air");

    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: varda::mixer::MainSource::Channel(input.clone()),
            fade: false,
        },
    );
    app.update_audio_mix();
    assert_eq!(own_sound(&app), Some((true, true)), "on with its picture");

    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: varda::mixer::MainSource::Black,
            fade: false,
        },
    );
    app.update_audio_mix();
    assert_eq!(own_sound(&app), Some((false, true)), "off as it leaves");

    // Removing the input takes its sound with it.
    send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: input,
        },
    );
    assert!(app.audio_mix().config().inputs.is_empty());
}

/// A generator is an input of its own, named after it; another input can be
/// copied, the copy named after it and showing its picture.
#[test]
fn a_generator_and_a_copy_of_an_input_are_inputs_too() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let generator = match send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::Generator {
                shader_name: "checkerboard".into(),
            },
            name: None,
            audio: None,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("the generator was not added: {other:?}"),
    };
    let (_, name, layers) = channels(&app)
        .into_iter()
        .find(|c| c.0 == generator)
        .unwrap();
    assert_eq!(name, "checkerboard");
    assert_eq!(layers.len(), 1);

    let copy = match send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::Input {
                channel_uuid: generator.clone(),
            },
            name: None,
            audio: None,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("the copy was not added: {other:?}"),
    };
    let (_, name, layers) = channels(&app).into_iter().find(|c| c.0 == copy).unwrap();
    assert_eq!(name, "checkerboard (copy)");
    assert_eq!(layers, vec!["🔁 checkerboard".to_string()]);
}

/// A colour input's colour can be changed after it is added; a layer still
/// called after its colour follows it, one renamed keeps its name.
#[test]
fn a_colour_can_be_changed_after_it_is_added() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let channel = match send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: colour(),
            name: None,
            audio: None,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("the colour was not added: {other:?}"),
    };
    let deck = |app: &VardaApp| {
        app.build_engine_state()
            .mixer
            .channels
            .into_iter()
            .find(|c| c.uuid == channel)
            .unwrap()
            .decks
            .remove(0)
    };
    let layer = deck(&app);
    assert_eq!(layer.solid_color, Some([0.1, 0.2, 0.3, 1.0]));
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetDeckColor {
                deck_uuid: layer.uuid.clone(),
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ),
        CommandResult::Ok
    ));
    let changed = deck(&app);
    assert_eq!(changed.solid_color, Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(changed.name, "#FF0000", "named after its colour still");

    // Only a colour layer has a colour.
    let generator = match send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::Generator {
                shader_name: "checkerboard".into(),
            },
            name: None,
            audio: None,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("{other:?}"),
    };
    let generator_deck = app
        .build_engine_state()
        .mixer
        .channels
        .into_iter()
        .find(|c| c.uuid == generator)
        .unwrap()
        .decks[0]
        .uuid
        .clone();
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetDeckColor {
                deck_uuid: generator_deck,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ),
        CommandResult::Err { .. }
    ));
}
