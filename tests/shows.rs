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

/// Whose Crop an effect added to a new layer is: Nida's own (its settings),
/// a `--shader-dir`'s (Posterize's, standing in), or the show's own (one
/// without settings).
fn added_crop(app: &mut VardaApp) -> &'static str {
    let deck = add_colour(app);
    id(send_cmd(
        app,
        EngineCommand::AddEffect {
            target: varda::engine::EffectTarget::Deck(deck.clone()),
            shader_name: "crop".to_string(),
        },
    ));
    let state = app.build_engine_state();
    let layer = state
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .expect("the layer");
    let crop = layer.effects.last().expect("the crop");
    let has = |name: &str| crop.params.params.iter().any(|p| p.name == name);
    if has("crop_left") {
        "Nida's"
    } else if has("color_levels") {
        "--shader-dir's"
    } else {
        "the show's"
    }
}

/// A "crop" of its own in `folder`: `like`, a shader from Nida's library.
fn own_crop(folder: &Path, like: &str) {
    std::fs::create_dir_all(folder).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("shaders")
            .join(like),
        folder.join("crop.fs"),
    )
    .unwrap();
}

/// A show in `folder`, made elsewhere, with no shaders of its own.
fn plain_show(folder: &Path) {
    std::fs::create_dir_all(folder).unwrap();
    let Some(mut other) = app_in(folder) else {
        return;
    };
    other
        .save_workspace(&UILayoutState::default())
        .expect("save");
}

/// A show's own shaders go when another show opens: left in the library,
/// its crop was the next show's — an effect added there, or one standing in
/// for a shader file gone.
#[test]
fn a_shows_own_shaders_go_when_another_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("Day 1"), dir.path().join("Day 2"));
    own_crop(&a.join(".varda/shaders"), "invert.fs");
    plain_show(&b);
    let Some(mut app) = app_in(&a) else {
        return;
    };
    let layout = UILayoutState::default();
    assert_eq!(added_crop(&mut app), "the show's");
    let loaded = app.open_show(&b, &layout).expect("open");
    assert!(loaded.is_ok(), "{:?}", loaded.error_message());
    assert_eq!(added_crop(&mut app), "Nida's", "Day 1's went with it");
    let loaded = app.open_show(&a, &layout).expect("open");
    assert!(loaded.is_ok(), "{:?}", loaded.error_message());
    assert_eq!(added_crop(&mut app), "the show's", "back with Day 1");

    // Saved as another show: its copy's shaders are the show's, and they
    // go when the next show opens — not the shaders of the show it was.
    let copy = dir.path().join("Day 1 again");
    app.save_show_as(&copy, &layout).expect("save as");
    app.flush_saves();
    assert_eq!(app.show_name(), "Day 1 again");
    assert_eq!(added_crop(&mut app), "the show's", "the copy's own");
    app.save_workspace(&layout).unwrap();
    let scene = std::fs::read_to_string(copy.join(".varda/scene.json")).unwrap();
    assert!(
        scene.contains("Day 1 again/.varda/shaders/crop.fs"),
        "the copy's file, not Day 1's: {scene}"
    );
    let loaded = app.open_show(&b, &layout).expect("open");
    assert!(loaded.is_ok(), "{:?}", loaded.error_message());
    assert_eq!(added_crop(&mut app), "Nida's", "neither copy's left");
}

/// A `--shader-dir` wins a name over the show's own shaders — as Nida
/// starts, and still on coming back to that show.
#[test]
fn a_shader_dir_wins_over_the_shows_own_shaders_coming_back_too() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("Day 1"), dir.path().join("Day 2"));
    own_crop(&a.join(".varda/shaders"), "invert.fs");
    plain_show(&b);
    let extra = dir.path().join("desk shaders");
    own_crop(&extra, "posterize.fs");
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let config = AppConfig::parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        a.to_str().unwrap(),
        "--shader-dir",
        extra.to_str().unwrap(),
    ]);
    let mut app = VardaApp::new(gpu, &config).expect("VardaApp::new");
    let layout = UILayoutState::default();
    assert_eq!(added_crop(&mut app), "--shader-dir's");
    app.open_show(&b, &layout).expect("open");
    assert_eq!(added_crop(&mut app), "--shader-dir's");
    app.open_show(&a, &layout).expect("open");
    assert_eq!(added_crop(&mut app), "--shader-dir's", "back in Day 1");
}

/// A show's own folder asked for as a `--shader-dir` is a library of its
/// own: it stays when another show opens.
#[test]
fn a_shader_dir_that_is_the_shows_own_folder_stays_when_another_opens() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("Day 1"), dir.path().join("Day 2"));
    let own = a.join(".varda/shaders");
    own_crop(&own, "posterize.fs");
    plain_show(&b);
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let config = AppConfig::parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        a.to_str().unwrap(),
        "--shader-dir",
        own.to_str().unwrap(),
    ]);
    let mut app = VardaApp::new(gpu, &config).expect("VardaApp::new");
    let layout = UILayoutState::default();
    assert_eq!(added_crop(&mut app), "--shader-dir's");
    app.open_show(&b, &layout).expect("open");
    assert_eq!(added_crop(&mut app), "--shader-dir's", "asked for, kept");
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

/// A show whose scene or stage cannot be read — saved by a later Nida, or
/// damaged — is not opened, and the show open stays: opened, Nida still held
/// the show before, and the next save wrote that over the files that would
/// not load.
#[test]
fn a_show_that_cannot_be_read_is_not_opened_nor_written_over() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("Day 1"), dir.path().join("Day 2"));
    plain_show(&a);
    plain_show(&b);
    let Some(mut app) = app_in(&a) else {
        return;
    };
    let layout = UILayoutState::default();
    for file in ["scene.json", "stage.json"] {
        let path = b.join(".varda").join(file);
        let good = std::fs::read(&path).unwrap();
        std::fs::write(&path, "not valid json {{{").unwrap();
        assert!(app.open_show(&b, &layout).is_err(), "{file}: opened");
        assert_eq!(app.show_name(), "Day 1", "{file}: the show open stays");
        app.save_workspace(&layout).unwrap();
        app.flush_saves();
        let now = std::fs::read_to_string(&path).unwrap();
        assert_eq!(now, "not valid json {{{", "{file}: written over");
        std::fs::write(&path, good).unwrap();
    }
    let loaded = app.open_show(&b, &layout).expect("read again, it opens");
    assert!(loaded.is_ok(), "{:?}", loaded.error_message());
    assert_eq!(app.show_name(), "Day 2");
}

/// A file of the show open that did not load — the scene of the show Nida
/// starts in, say — is kept beside it before a save writes over it.
#[test]
fn a_file_that_did_not_load_is_kept_before_a_save_writes_over_it() {
    let dir = tempfile::tempdir().unwrap();
    let show = dir.path().join("Day 1");
    plain_show(&show);
    let varda = show.join(".varda");
    std::fs::write(varda.join("scene.json"), "not valid json {{{").unwrap();
    let Some(mut app) = app_in(&show) else {
        return;
    };
    let message = app.load_workspace().error_message().expect("not loaded");
    assert!(message.contains("kept as scene.not-loaded-"), "{message}");
    app.save_workspace(&UILayoutState::default()).unwrap();
    app.flush_saves();
    let kept: Vec<String> = std::fs::read_dir(&varda)
        .unwrap()
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("scene.not-loaded-"))
        .collect();
    assert_eq!(kept.len(), 1, "{kept:?}");
    let copy = std::fs::read_to_string(varda.join(&kept[0])).unwrap();
    assert_eq!(copy, "not valid json {{{", "the file as it was");
    let saved = std::fs::read_to_string(varda.join("scene.json")).unwrap();
    assert_ne!(saved, "not valid json {{{", "the save landed");
}

/// Two loads within a second of a file that will not load keep two copies,
/// neither written over the other.
#[test]
fn copies_of_a_file_that_did_not_load_never_overwrite_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let show = dir.path().join("Day 1");
    plain_show(&show);
    let varda = show.join(".varda");
    let Some(mut app) = app_in(&show) else {
        return;
    };
    for text in ["first {{{", "second {{{"] {
        std::fs::write(varda.join("scene.json"), text).unwrap();
        assert!(app.load_workspace().error_message().is_some());
    }
    let mut copies: Vec<String> = std::fs::read_dir(&varda)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            name.to_string_lossy().starts_with("scene.not-loaded-")
        })
        .map(|entry| std::fs::read_to_string(entry.path()).unwrap())
        .collect();
    copies.sort();
    assert_eq!(copies, ["first {{{", "second {{{"]);
}

/// A file that will not load and cannot even be copied — unreadable — is
/// not saved over: no copy of it would be left.
#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_kept_is_not_saved_over() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let show = dir.path().join("Day 1");
    plain_show(&show);
    let scene = show.join(".varda/scene.json");
    std::fs::write(&scene, "not valid json {{{").unwrap();
    let mode = std::fs::Permissions::from_mode;
    std::fs::set_permissions(&scene, mode(0o000)).unwrap();
    let readable = std::fs::read(&scene).is_ok();
    let Some(mut app) = app_in(&show).filter(|_| !readable) else {
        std::fs::set_permissions(&scene, mode(0o644)).unwrap();
        return;
    };
    let message = app.load_workspace().error_message().expect("not loaded");
    assert!(message.contains("not saved over"), "{message}");
    app.save_workspace(&UILayoutState::default()).unwrap();
    app.flush_saves();
    std::fs::set_permissions(&scene, mode(0o644)).unwrap();
    let now = std::fs::read_to_string(&scene).unwrap();
    assert_eq!(now, "not valid json {{{", "saved over");
}

/// Nor through a link to its folder: Save As there is the same folder, and
/// the file is the same file, whatever the path says.
#[cfg(unix)]
#[test]
fn a_file_that_cannot_be_kept_is_not_saved_over_through_a_link_either() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let show = dir.path().join("Day 1");
    plain_show(&show);
    let link = dir.path().join("Day 1 link");
    std::os::unix::fs::symlink(&show, &link).unwrap();
    let scene = show.join(".varda/scene.json");
    std::fs::write(&scene, "not valid json {{{").unwrap();
    let mode = std::fs::Permissions::from_mode;
    std::fs::set_permissions(&scene, mode(0o000)).unwrap();
    let readable = std::fs::read(&scene).is_ok();
    let Some(mut app) = app_in(&show).filter(|_| !readable) else {
        std::fs::set_permissions(&scene, mode(0o644)).unwrap();
        return;
    };
    let layout = UILayoutState::default();
    assert!(app.load_workspace().error_message().is_some());
    let saved = app.save_show_as(&link, &layout);
    app.flush_saves();
    app.save_workspace(&layout).ok();
    app.flush_saves();
    std::fs::set_permissions(&scene, mode(0o644)).unwrap();
    let now = std::fs::read_to_string(&scene).unwrap();
    assert_eq!(now, "not valid json {{{", "saved over (save as: {saved:?})");
}
