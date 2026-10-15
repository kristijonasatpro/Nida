//! Shows, as vMix's presets: Save As carries on in the new folder and leaves
//! the old show as it was; a new show starts fresh — keeping only the desk's
//! own setup — and never overwrites the show before it; opening a show
//! brings it back.

use std::path::Path;

use varda::app::{AppConfig, VardaApp};
use varda::engine::{CommandResult, EngineCommand};
use varda::usecases::ui::UILayoutState;

use clap::Parser;

mod common;

fn app_in(workspace: &Path) -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    let config = AppConfig::parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    Some(VardaApp::new(gpu, &config).expect("VardaApp::new"))
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    // The engine's frames take in what the writer has landed: a Save As
    // moves the show then, and answers (see `VardaApp::save_show_as`).
    app.flush_saves();
    reply_rx.blocking_recv().unwrap()
}

fn id(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected an id, got {other:?}"),
    }
}

/// Layers in the show open.
fn layers(app: &VardaApp) -> usize {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .map(|ch| ch.decks.len())
        .sum()
}

/// Layers in the show saved in `folder`.
fn saved_layers(folder: &Path) -> usize {
    let scene = varda::scene::SceneConfig::load(folder.join(".varda/scene.json")).unwrap();
    scene.channels.iter().map(|ch| ch.decks.len()).sum()
}

fn add_colour(app: &mut VardaApp) -> String {
    let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
    id(send_cmd(
        app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: channel,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ))
}

#[test]
fn save_as_carries_on_in_the_new_folder_and_leaves_the_old_show() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("Day 1"), dir.path().join("Day 2"));
    std::fs::create_dir_all(a.join(".varda/presets/decks")).unwrap();
    std::fs::write(a.join(".varda/presets/decks/logo.json"), "{}").unwrap();
    let Some(mut app) = app_in(&a) else {
        return;
    };
    let layout = UILayoutState::default();
    let base = layers(&app);
    add_colour(&mut app);
    app.save_workspace(&layout).unwrap();

    app.save_show_as(&b, &layout).expect("save as");
    // Copied in the background; it moves once the copy has landed.
    app.flush_saves();
    assert_eq!(app.show_name(), "Day 2");
    assert_eq!(saved_layers(&b), base + 1);
    assert!(
        b.join(".varda/presets/decks/logo.json").is_file(),
        "its presets go with it"
    );

    add_colour(&mut app);
    app.save_workspace(&layout).unwrap();
    assert_eq!(saved_layers(&b), base + 2, "saves go to the new folder");
    assert_eq!(saved_layers(&a), base + 1, "the old show stays as it was");

    let taken = dir.path().join("Taxes");
    std::fs::create_dir_all(&taken).unwrap();
    std::fs::write(taken.join("2025.pdf"), b"").unwrap();
    app.save_show_as(&taken, &layout).expect("started");
    app.flush_saves();
    assert_eq!(app.show_name(), "Day 2", "refused, it stays where it was");
    assert!(!taken.join(".varda").exists());
}

/// A new show starts fresh, as a show never set up — the user found the
/// last show's microphone and screens still on in one: "new should reset all
/// to fresh" (2026-10-04). Only the desk's own setup comes along, and the show
/// before is saved first, as it was.
#[test]
fn a_new_show_starts_fresh_keeps_the_desk_and_saves_the_one_before() {
    let dir = tempfile::tempdir().unwrap();
    let (a, n) = (dir.path().join("Day 1"), dir.path().join("Untitled"));
    std::fs::create_dir_all(a.join(".varda/presets/decks")).unwrap();
    std::fs::write(a.join(".varda/presets/decks/logo.json"), "{}").unwrap();
    // A key of the operator's own: K cuts to Main.
    std::fs::write(
        a.join(".varda/keymap.json"),
        r#"{"version":1,"bindings":[{"key":"K","command":false,"shift":false,"alt":false,"target":{"Action":"CutToMain"}}]}"#,
    )
    .unwrap();
    let Some(mut app) = app_in(&a) else {
        return;
    };
    let _ = app.load_workspace();
    let layout = UILayoutState::default();
    let ok = |r: CommandResult| assert!(!matches!(r, CommandResult::Err { .. }), "{r:?}");
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 1280,
            height: 720,
        },
    ));
    add_colour(&mut app);
    id(send_cmd(
        &mut app,
        EngineCommand::AddBus {
            name: "TV".into(),
            width: 1280,
            height: 720,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::CreateHeadlessOutput {
            target: varda::renderer::context::OutputTarget::SyphonServer {
                server_name: "LED".into(),
            },
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddAudioInput {
            name: "Scarlett".into(),
            device: "Scarlett Solo 4th Gen".into(),
            left: 0,
            right: 1,
        },
    ));
    let master = app.audio_mix().config().buses[0].uuid.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::SetAudioBusDelay {
            uuid: master,
            delay_ms: 150.0,
        },
    ));
    let mut options = app.record_settings().options.clone();
    options.folder = dir.path().join("Takes");
    ok(send_cmd(&mut app, EngineCommand::SetRecordOptions { options }));
    let before = layers(&app);

    app.new_show(&n, &layout).expect("new show");
    assert_eq!(app.show_name(), "Untitled");
    assert_eq!(layers(&app), 0, "no layers");
    let state = app.build_engine_state();
    assert_eq!(state.mixer.channels.len(), 2, "two empty inputs");
    assert!(state.outputs.windows.is_empty(), "no outputs");
    let buses: Vec<String> = app
        .production_buses()
        .iter()
        .map(|b| b.name.clone())
        .collect();
    assert_eq!(buses, ["PGM"], "the one bus");
    let pgm = app.production_buses().iter().next().unwrap();
    assert_eq!(
        pgm.canvas(),
        varda::bus::BusCanvas::new(1280, 720),
        "showing all of Main, at the size the show is"
    );
    assert_eq!(pgm.effective_program(), &varda::bus::BusInputRef::Master);
    let mix = app.audio_mix().config();
    assert!(mix.inputs.is_empty(), "no sound inputs");
    assert_eq!(mix.buses.len(), 1, "Master alone");
    assert!(mix.buses[0].delay_ms.abs() < f32::EPSILON, "no delay");
    assert_eq!(
        app.record_settings().options.folder,
        varda::record::RecordOptions::default().folder,
        "recording as new"
    );
    assert_eq!(
        (app.render_width(), app.render_height()),
        (1280, 720),
        "the size as it was"
    );
    assert!(!app.history_can_undo(), "nothing to undo into");
    assert!(
        n.join(".varda/presets/decks/logo.json").is_file(),
        "the desk's presets come along"
    );
    let keys: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(n.join(".varda/keymap.json")).unwrap())
            .unwrap();
    assert!(
        keys["bindings"].as_array().unwrap().iter().any(|b| b["key"] == "K"),
        "and the operator's keys: {keys}"
    );

    assert_eq!(saved_layers(&a), before, "the show before was saved first");
    let old_stage = std::fs::read_to_string(a.join(".varda/stage.json")).unwrap();
    assert!(old_stage.contains("\"TV\"") && old_stage.contains("Scarlett"));
    assert_eq!(saved_layers(&n), 0);
    let new_stage = std::fs::read_to_string(n.join(".varda/stage.json")).unwrap();
    assert!(
        !new_stage.contains("\"TV\"") && !new_stage.contains("Scarlett"),
        "none of the show before in the new one"
    );
}

#[test]
fn opening_a_show_brings_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let (a, n) = (dir.path().join("Day 1"), dir.path().join("Untitled"));
    let Some(mut app) = app_in(&a) else {
        return;
    };
    let layout = UILayoutState::default();
    let deck = add_colour(&mut app);
    let with = layers(&app);
    app.new_show(&n, &layout).unwrap();
    assert_eq!(layers(&app), 0);

    let loaded = app.open_show(&a, &layout).expect("open");
    assert!(loaded.is_ok(), "{:?}", loaded.error_message());
    assert_eq!(app.show_name(), "Day 1");
    assert_eq!(layers(&app), with);
    assert!(app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .any(|d| d.uuid == deck));

    let nothing = dir.path().join("Holiday photos");
    std::fs::create_dir_all(&nothing).unwrap();
    assert!(app.open_show(&nothing, &layout).is_err());
    assert_eq!(app.show_name(), "Day 1", "still open");
}

#[test]
fn the_api_can_save_as_start_and_open_shows() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b, n) = (
        dir.path().join("A"),
        dir.path().join("B"),
        dir.path().join("N"),
    );
    let Some(mut app) = app_in(&a) else {
        return;
    };
    add_colour(&mut app);
    let ok = |r: CommandResult| assert!(matches!(r, CommandResult::Ok), "{r:?}");
    ok(send_cmd(
        &mut app,
        EngineCommand::SaveShowAs { folder: b.clone() },
    ));
    assert_eq!(app.show_name(), "B");
    ok(send_cmd(
        &mut app,
        EngineCommand::NewShow {
            folder: Some(n.clone()),
        },
    ));
    assert_eq!(app.show_name(), "N");
    ok(send_cmd(&mut app, EngineCommand::OpenShow { folder: b }));
    assert_eq!(app.show_name(), "B");
    assert!(layers(&app) > 0);
}
