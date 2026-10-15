//! Persistence integration tests — save/load roundtrips with tempdir workspaces.

use varda::app::{AppConfig, VardaApp};
use varda::engine::{BlendMode, CommandResult, EffectTarget, EngineCommand};
use varda::modulation::LFOWaveform;
use varda::timebase::Timebase;
use varda::usecases::ui::UILayoutState;

use clap::Parser;
use tempfile::TempDir;

mod common;

fn parse_args(args: &[&str]) -> AppConfig {
    AppConfig::parse_from(std::iter::once("varda").chain(args.iter().copied()))
}

fn headless_app_in(workspace: &std::path::Path) -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    let ws = workspace.to_str().unwrap();
    let config = parse_args(&[
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        // See `varda::testing::headless_config`: enumeration raises the OS
        // camera prompt on every construction, and this suite constructs many.
        "--no-camera",
        "--workspace",
        ws,
    ]);
    // Once a GPU exists, a construction failure is a bug, not a reason to skip.
    Some(VardaApp::new(gpu, &config).expect("VardaApp::new"))
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn fire(app: &mut VardaApp, cmd: EngineCommand) {
    app.command_sender().send((cmd, None)).unwrap();
    app.process_commands();
}

/// UUID of the channel currently at `idx`.
fn channel_uuid(app: &mut VardaApp, idx: usize) -> String {
    app.build_engine_state().mixer.channels[idx].uuid.clone()
}

// ── Tests ──────────────────────────────────────────────────────────

#[test]
fn save_load_empty_workspace() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    // Reload
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert_eq!(state.mixer.channels.len(), 2);
}

#[test]
fn save_load_with_decks() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert!(
        !state.mixer.channels[0].decks.is_empty(),
        "deck should survive roundtrip"
    );
}

#[test]
fn save_load_crossfader_position() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    fire(&mut app, EngineCommand::SetCrossfader(0.75));
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert!((state.mixer.crossfader - 0.75).abs() < 1e-4);
}

#[test]
fn save_load_modulation_sources() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::AddLfo {
            waveform: LFOWaveform::Sine,
            frequency: 2.0,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert!(
        !state.modulation.sources.is_empty(),
        "LFO should survive roundtrip"
    );
}

/// A beat-locked modulator must still be beat-locked after a reload, otherwise
/// a saved show silently reverts to wall time. See /spec/timebase.md.
#[test]
fn save_load_modulation_timebase() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    send_cmd(
        &mut app,
        EngineCommand::AddLfo {
            waveform: LFOWaveform::Sine,
            frequency: 2.0,
        },
    );
    let uuid = app.build_engine_state().modulation.sources[0].uuid.clone();
    fire(
        &mut app,
        EngineCommand::UpdateModulationTimebase {
            uuid,
            timebase: Timebase::Beat,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert_eq!(
        state.modulation.sources[0].timebase,
        Timebase::Beat,
        "timebase should survive a save/load roundtrip"
    );
}

/// An automation curve is only worth drawing if it survives the save. This also
/// covers `AssignmentMode::Absolute` persisting, since a curve that reloaded as
/// additive would ride on the fader instead of setting it.
/// See /spec/automation.md § Persistence.
#[test]
fn save_load_automation_envelope() {
    use varda::modulation::{Breakpoint, CurveKind};

    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck_uuid = match send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    };
    let target = format!("deck_{deck_uuid}:opacity");

    let uuid = match send_cmd(
        &mut app,
        EngineCommand::AddAutomationLane {
            target: target.clone(),
            timebase: Timebase::Transport,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected the new envelope's uuid, got {other:?}"),
    };

    let drawn = vec![
        Breakpoint::new(0.0, 0.1),
        Breakpoint::new(8.5, 0.9).with_curve(CurveKind::Smooth),
    ];
    fire(
        &mut app,
        EngineCommand::SetEnvelopeBreakpoints {
            uuid: uuid.clone(),
            breakpoints: drawn.clone(),
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();

    let entry = state
        .modulation
        .sources
        .iter()
        .find(|s| s.uuid == uuid)
        .expect("envelope should survive the reload");
    assert_eq!(entry.timebase, Timebase::Transport);
    let varda::engine::types::ModulationSourceSnapshot::Envelope { breakpoints } = &entry.source
    else {
        panic!("expected the reloaded source to still be an envelope");
    };
    assert_eq!(breakpoints, &drawn, "the drawn curve should round-trip");

    let assigned = state
        .modulation
        .assignments
        .get(&target)
        .expect("the assignment should survive the reload");
    assert!(assigned.iter().any(|a| a.source_id == uuid));
}

/// Everything a timeline edit produces has to survive the file, or the show
/// authored on Friday is not the show that opens on Saturday.
/// See /spec/arrangement.md § Storage.
#[test]
fn save_load_arrangement_edits() {
    use varda::arrangement::RegionConfig;

    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck_uuid = match send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    };

    // Author as the panel does: drop a region, then drag it into shape.
    fire(
        &mut app,
        EngineCommand::AddRegion {
            deck_uuid: deck_uuid.clone(),
            region: RegionConfig::new(4.0, 8.0),
        },
    );
    let shaped = RegionConfig::new(4.5, 12.25).with_fades(0.5, 1.5);
    fire(
        &mut app,
        EngineCommand::UpdateRegion {
            deck_uuid: deck_uuid.clone(),
            index: 0,
            region: shaped,
        },
    );
    fire(
        &mut app,
        EngineCommand::SetLaneCollapsed {
            deck_uuid: deck_uuid.clone(),
            collapsed: true,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();

    let arrangement = state
        .arrangement
        .expect("the arrangement should survive the reload");
    let lane = arrangement
        .config
        .lane(&deck_uuid)
        .expect("the lane should still name its deck");
    assert_eq!(lane.regions, vec![shaped], "the edited span should reload");
    assert!(lane.collapsed, "a folded lane should reload folded");

    // The region compiles back to an opacity curve on load, so the deck is
    // driven without anyone having to touch it again.
    let key = varda::arrangement::opacity_param_key(&deck_uuid);
    assert!(
        state.modulation.assignments.contains_key(&key),
        "the reloaded region should drive the deck's opacity"
    );
}

/// Cues are how a show is navigated, so they belong to the scene rather than to
/// the session that dropped them. See /spec/arrangement.md § Cue points.
#[test]
fn save_load_cue_points() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    fire(
        &mut app,
        EngineCommand::AddCue {
            at: 64.5,
            name: "Drop".to_string(),
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let cues = app2
        .build_engine_state()
        .arrangement
        .expect("an arrangement holding only cues should still reload")
        .config
        .cues;

    assert_eq!(cues.len(), 1);
    assert_eq!(cues[0].name, "Drop");
    assert!((cues[0].at - 64.5).abs() < 1e-9);
}

#[test]
fn save_load_render_resolution() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    fire(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 1280,
            height: 720,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    assert_eq!(app2.render_width(), 1280);
    assert_eq!(app2.render_height(), 720);
}

/// Which signal a rig follows belongs to the venue, so it comes back with the
/// stage rather than with the show.
#[test]
fn save_load_timecode_preference() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    fire(
        &mut app,
        EngineCommand::SetTimecodePreference {
            preference: varda::timecode::TimecodePreference::Off,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    assert_eq!(
        app2.build_engine_state().timecode.preference,
        varda::timecode::TimecodePreference::Off,
        "ignoring timecode is a decision, so it must survive a restart"
    );
}

/// The patch is written down as the name of a box, never the slot it enumerated
/// in: ids are handed out at scan time and move whenever the rig changes between
/// load-ins, so a saved id would point at whatever interface came up in that slot
/// tonight and the show would chase silence.
#[test]
fn save_load_ltc_patch_by_interface_name() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let Some(device) = app.build_engine_state().audio.devices.last().cloned() else {
        return;
    };
    let patched = varda::timecode::LtcInput {
        source_id: device.id,
        channel: 1,
        rate: None,
    };
    fire(
        &mut app,
        EngineCommand::SetLtcInput {
            input: Some(patched),
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let stage: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join(".varda").join("stage.json"))
            .expect("stage.json should exist"),
    )
    .expect("stage.json should be valid JSON");
    let saved = &stage["timecode"]["ltc_input"];
    assert_eq!(saved["device"], device.name);
    assert_eq!(saved["channel"], 1);
    assert!(
        saved.get("source_id").is_none(),
        "an id in the file would point at whatever enumerates in that slot next time"
    );

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    assert_eq!(
        app2.build_engine_state().timecode.ltc_input,
        Some(patched),
        "the name resolves to the id that interface holds now"
    );
}

/// A stage naming an interface the rig no longer has must leave the patch unset
/// rather than reading timecode off whichever cable took its number.
#[test]
fn load_ltc_patch_for_a_missing_interface_leaves_it_unset() {
    let tmp = TempDir::new().unwrap();
    let varda_dir = tmp.path().join(".varda");
    std::fs::create_dir_all(&varda_dir).unwrap();
    std::fs::write(
        varda_dir.join("stage.json"),
        r#"{"timecode":{"preference":"ForceLtc",
            "ltc_input":{"device":"Scarlett 2i2 That Stayed Home","channel":1}}}"#,
    )
    .unwrap();

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app.load_workspace();

    let timecode = app.build_engine_state().timecode;
    assert_eq!(timecode.ltc_input, None);
    assert_eq!(
        timecode.preference,
        varda::timecode::TimecodePreference::ForceLtc,
        "the decision to follow LTC is kept, so the popover reads as waiting on an input"
    );
}

#[test]
fn save_load_domemaster_resolution() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    fire(
        &mut app,
        EngineCommand::SetDomemasterResolution {
            resolution: varda::renderer::dome::DomemasterResolution::R4K,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    assert_eq!(
        app2.domemaster_resolution(),
        varda::renderer::dome::DomemasterResolution::R4K,
        "the dome belongs to the venue, so its size must come back with the stage"
    );
}

#[test]
fn save_load_multiple_channels() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    fire(&mut app, EngineCommand::AddChannel);
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert_eq!(state.mixer.channels.len(), 3);
}

#[test]
fn save_load_svg_image_deck() {
    // SVG rides the existing image source config — no new scene.json shape —
    // so the roundtrip has to prove the restored deck comes back rasterized
    // rather than failing the way an unknown format would.
    let tmp = TempDir::new().unwrap();
    let art = tmp.path().join("logo.svg");
    std::fs::write(
        &art,
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 200 50"
            width="200" height="50"><rect width="200" height="50" fill="#20c0a0"/></svg>"##,
    )
    .unwrap();

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let result = send_cmd(
        &mut app,
        EngineCommand::AddImageDeck {
            channel_uuid: ch,
            path: art.clone(),
        },
    );
    assert!(
        matches!(result, CommandResult::OkWithId { .. }),
        "adding an SVG deck should succeed, got {result:?}"
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert_eq!(
        state.mixer.channels[0].decks.len(),
        1,
        "the SVG deck must survive the roundtrip"
    );
}

#[test]
fn load_missing_assets_graceful() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    // Add a video deck with a non-existent path
    let ch = channel_uuid(&mut app, 0);
    let _ = send_cmd(
        &mut app,
        EngineCommand::AddVideoDeck {
            channel_uuid: ch,
            path: std::path::PathBuf::from("/nonexistent/path/video.mp4"),
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    // Reload — should not crash
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
}

#[test]
fn save_creates_varda_directory() {
    let tmp = TempDir::new().unwrap();
    let varda_dir = tmp.path().join(".varda");
    assert!(!varda_dir.exists());
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    assert!(varda_dir.exists());
}

#[test]
fn scene_json_valid_format() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let scene_path = tmp.path().join(".varda").join("scene.json");
    let content = std::fs::read_to_string(scene_path).expect("scene.json should exist");
    let parsed: serde_json::Value = serde_json::from_str(&content).expect("should be valid JSON");
    assert!(parsed.is_object());
    assert!(parsed.get("channels").is_some());
}

#[test]
fn save_load_deck_fidelity_opacity_transparent_blend() {
    // A solid-color deck's opacity, transparent flag, and blend mode must all
    // survive the real snapshot_scene -> disk -> restore_scene path, not just
    // the deck's existence.
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [0.25, 0.5, 0.75, 1.0],
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    };
    fire(
        &mut app,
        EngineCommand::SetDeckOpacity {
            deck_uuid: deck.clone(),
            opacity: 0.42,
        },
    );
    fire(
        &mut app,
        EngineCommand::SetDeckTransparent {
            deck_uuid: deck.clone(),
            transparent: true,
        },
    );
    fire(
        &mut app,
        EngineCommand::SetDeckBlendMode {
            deck_uuid: deck,
            mode: BlendMode::Multiply,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    let restored = &state.mixer.channels[0].decks[0];
    assert!(
        (restored.opacity - 0.42).abs() < 1e-4,
        "opacity should survive: {}",
        restored.opacity
    );
    assert!(restored.transparent, "transparent flag should survive");
    assert_eq!(
        restored.blend_mode,
        BlendMode::Multiply,
        "blend mode should survive"
    );
}

#[test]
fn save_load_deck_effect_survives() {
    // A deck effect (ISF filter) and its enabled state must survive the
    // roundtrip. If the effect shader isn't available in this build the add
    // fails gracefully and the assertion is skipped (mirrors engine tests).
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    };
    let effect_name = match send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: EffectTarget::Deck(deck),
            shader_name: "invert".to_string(),
        },
    ) {
        CommandResult::OkWithId { .. } => "invert",
        // Effect shader unavailable in this build — nothing to assert.
        _ => return,
    };
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    let restored = &state.mixer.channels[0].decks[0];
    let names: Vec<&str> = restored.effects.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["crop", "transform", effect_name],
        "the saved chain comes back as it was — a load must not add the defaults again"
    );
    assert!(
        restored.effects[2].enabled,
        "effect should be enabled after roundtrip"
    );
}

#[test]
fn an_effects_integer_choice_survives_save_and_load() {
    // JSON has one kind of number: Mirror's `"mode": 3`, written through the
    // API or read back from a saved show, arrives as a float, and kept so
    // the shader's `int mode` matched no mode after every reload.
    let tmp = TempDir::new().unwrap();
    {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let ch = channel_uuid(&mut app, 0);
        let deck = match send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: ch,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ) {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("expected OkWithId, got {other:?}"),
        };
        let mirror = match send_cmd(
            &mut app,
            EngineCommand::AddEffect {
                target: EffectTarget::Deck(deck),
                shader_name: "mirror".to_string(),
            },
        ) {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("the mirror effect should be in the shaders folder: {other:?}"),
        };
        let command: EngineCommand = serde_json::from_str(&format!(
            r#"{{"SetEffectParam": {{"effect_uuid": "{mirror}", "name": "mode", "value": 3}}}}"#
        ))
        .expect("the API body parses");
        assert!(matches!(send_cmd(&mut app, command), CommandResult::Ok));
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
    }

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app.load_workspace();
    let state = app.build_engine_state();
    let mirror = state.mixer.channels[0].decks[0]
        .effects
        .iter()
        .find(|e| e.name == "mirror")
        .expect("the mirror comes back");
    let mode = mirror
        .params
        .params
        .iter()
        .find(|p| p.name == "mode")
        .expect("its mode");
    assert!(
        matches!(mode.value, varda::params::ParamValue::Long(3)),
        "still the integer 3: {:?}",
        mode.value
    );
}

#[test]
fn save_load_channel_opacity() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    fire(
        &mut app,
        EngineCommand::SetChannelOpacity {
            channel_uuid: ch,
            opacity: 0.5,
        },
    );
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    assert!((state.mixer.channels[0].opacity - 0.5).abs() < 1e-4);
}

// ── Production buses ────────────────────────────────────────────────

use varda::bus::BusInputRef;

fn add_bus(app: &mut VardaApp, name: &str, width: u32, height: u32) -> String {
    match send_cmd(
        app,
        EngineCommand::AddBus {
            name: name.to_string(),
            width,
            height,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected OkWithId, got {other:?}"),
    }
}

#[test]
fn buses_survive_a_save_and_reload_with_their_canvases() {
    let tmp = TempDir::new().unwrap();
    let (led, tv, channel) = {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let channel = channel_uuid(&mut app, 0);
        let _stream = add_bus(&mut app, "STREAM", 1920, 1080);
        let led = add_bus(&mut app, "LED", 1920, 480);
        let tv = add_bus(&mut app, "TV", 1920, 1080);

        // Put a real source on LED so routing has something to restore.
        fire(
            &mut app,
            EngineCommand::SetBusPreview {
                uuid: led.clone(),
                input: BusInputRef::Channel(channel.clone()),
            },
        );
        fire(&mut app, EngineCommand::BusCut { uuid: led.clone() });

        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        (led, tv, channel)
    };

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();

    let buses = app2.production_buses();
    assert_eq!(buses.len(), 4, "legacy PGM bus plus the three added");

    let led_bus = buses.get(&led).expect("LED bus should be restored by uuid");
    assert_eq!(led_bus.name, "LED");
    assert_eq!(
        led_bus.canvas(),
        varda::bus::BusCanvas::new(1920, 480),
        "a non-standard LED canvas must survive the round trip"
    );
    assert_eq!(
        led_bus.program(),
        &BusInputRef::Channel(channel),
        "LED routing should be restored"
    );
    assert!(
        led_bus.program_resolved(),
        "and it must actually resolve against the reloaded scene"
    );
    assert_eq!(
        led_bus.effective_program(),
        led_bus.program(),
        "so the effective (on-air) program is the configured one, not black"
    );

    let tv_bus = buses.get(&tv).expect("TV bus should be restored");
    assert_eq!(tv_bus.canvas(), varda::bus::BusCanvas::new(1920, 1080));
    assert_eq!(
        tv_bus.program(),
        &BusInputRef::Master,
        "TV was never taken, so it must come back as made: on Main"
    );
}

#[test]
fn restored_buses_keep_distinct_audio_mixes() {
    let tmp = TempDir::new().unwrap();
    let (a, b) = {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let a = add_bus(&mut app, "A", 1920, 1080);
        let b = add_bus(&mut app, "B", 1920, 1080);
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        (a, b)
    };

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let buses = app2.production_buses();
    assert_ne!(
        buses.get(&a).unwrap().audio_mix(),
        buses.get(&b).unwrap().audio_mix()
    );
}

#[test]
fn routing_a_bus_at_a_source_that_does_not_exist_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let led = add_bus(&mut app, "LED", 1920, 480);

    // Unresolvable UUIDs are a NotFound, never a silent write — otherwise a
    // show can be authored against sources that do not exist and only fails
    // on air. See src/app/resolve.rs and /spec/api-addressing.md.
    match send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: led.clone(),
            input: BusInputRef::Channel("deadbeef".into()),
        },
    ) {
        CommandResult::Err { code, .. } => assert!(
            matches!(code, varda::engine::ErrorCode::NotFound),
            "expected NotFound, got {code:?}"
        ),
        other => panic!("routing at a nonexistent channel should fail: {other:?}"),
    }
    assert_eq!(
        app.production_buses().get(&led).unwrap().preview(),
        &BusInputRef::Master,
        "the rejected write must not have landed"
    );
}

#[test]
fn a_channel_reference_naming_a_deck_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let channel = channel_uuid(&mut app, 0);
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddDeck {
            channel_uuid: channel,
            shader_name: "solid_color".into(),
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected a deck uuid, got {other:?}"),
    };
    let led = add_bus(&mut app, "LED", 1920, 480);

    // The UUID exists, but as a deck. A flat UUID set would accept this.
    match send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: led,
            input: BusInputRef::Channel(deck.clone()),
        },
    ) {
        CommandResult::Err { .. } => {}
        other => panic!("Channel(deck_uuid) should be rejected: {other:?}"),
    }
}

#[test]
fn a_stage_file_routing_at_a_lost_source_loads_black_but_keeps_the_route() {
    // Hand-written stage data is the only way to reach this state now that the
    // command layer rejects unresolvable sources — but a show whose scene was
    // swapped underneath it can still arrive here.
    let tmp = TempDir::new().unwrap();
    let varda_dir = tmp.path().join(".varda");
    std::fs::create_dir_all(&varda_dir).unwrap();
    std::fs::write(
        varda_dir.join("stage.json"),
        r#"{
            "buses": [{
                "uuid": "led00001",
                "name": "LED",
                "canvas": {"width": 1920, "height": 480},
                "preview": "Black",
                "program": {"Channel": "vanished"},
                "audio_mix": "led00001",
                "auto_duration": 1.0,
                "easing": "EaseInOut"
            }]
        }"#,
    )
    .unwrap();

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app.load_workspace();

    let bus = app
        .production_buses()
        .get("led00001")
        .expect("bus should load");
    assert!(
        !bus.program_resolved(),
        "a source absent from the scene must be marked unresolved"
    );
    assert_eq!(
        bus.effective_program(),
        &BusInputRef::Black,
        "it must put black on air, never a different source"
    );
    assert_eq!(
        bus.program(),
        &BusInputRef::Channel("vanished".into()),
        "the configured route is retained so the right scene recovers it"
    );
}

#[test]
fn a_stage_from_before_buses_gets_a_pgm_covering_the_whole_show() {
    // A stage without buses (one from before them, or a new show's) gets the
    // one PGM the single program was. Made at 1080p, in a 4K show it showed
    // a quarter of Main, so it must take the size the scene loads.
    let tmp = TempDir::new().unwrap();
    {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        fire(
            &mut app,
            EngineCommand::SetRenderResolution {
                width: 3840,
                height: 2160,
            },
        );
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
    }
    let stage_path = tmp.path().join(".varda").join("stage.json");
    let mut stage: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&stage_path).unwrap()).unwrap();
    assert!(
        stage.as_object_mut().unwrap().remove("buses").is_some(),
        "the saved stage had buses to take out"
    );
    std::fs::write(&stage_path, serde_json::to_string(&stage).unwrap()).unwrap();

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let load = app.load_workspace();
    assert!(load.error_message().is_none(), "{:?}", load.error_message());
    assert_eq!((app.render_width(), app.render_height()), (3840, 2160));
    let buses: Vec<_> = app.production_buses().iter().collect();
    assert_eq!(buses.len(), 1, "the one PGM");
    assert_eq!(buses[0].canvas(), varda::bus::BusCanvas::new(3840, 2160));
    assert_eq!(
        buses[0].canvas().uv(3840, 2160),
        ([1.0, 1.0], [0.0, 0.0]),
        "all of Main, not its top-left quarter"
    );
    assert_eq!(buses[0].effective_program(), &BusInputRef::Master);
}

#[test]
fn a_stage_file_with_no_buses_reloads_with_none() {
    let tmp = TempDir::new().unwrap();
    {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        // Remove every bus, including the default one.
        let uuids: Vec<String> = app
            .production_buses()
            .iter()
            .map(|b| b.uuid().to_string())
            .collect();
        for uuid in uuids {
            fire(&mut app, EngineCommand::RemoveBus { uuid });
        }
        assert_eq!(app.production_buses().len(), 0);
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
    }

    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    assert_eq!(
        app2.production_buses().len(),
        0,
        "an intentionally empty bank must not silently regain the legacy bus"
    );
}

#[test]
fn bus_routing_fails_closed_when_the_scene_it_refers_to_does_not_load() {
    // The scene is produced by a real save and then given a duplicate channel
    // identity, so it parses but is rejected on identity grounds. Hand-writing
    // it made an earlier version of this test pass for the wrong reason — a
    // parse failure — which is why the rejection reason is asserted below.
    let tmp = TempDir::new().unwrap();
    let (bus, channel) = {
        let Some(mut seed) = headless_app_in(tmp.path()) else {
            return;
        };
        let channel = channel_uuid(&mut seed, 0);
        let bus = match send_cmd(
            &mut seed,
            EngineCommand::AddBus {
                name: "LED".into(),
                width: 1920,
                height: 480,
            },
        ) {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("expected a bus uuid, got {other:?}"),
        };
        fire(
            &mut seed,
            EngineCommand::SetBusPreview {
                uuid: bus.clone(),
                input: BusInputRef::Channel(channel.clone()),
            },
        );
        fire(&mut seed, EngineCommand::BusCut { uuid: bus.clone() });
        seed.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        (bus, channel)
    };

    // Duplicate the first channel verbatim: still valid JSON, but two entities
    // now claim one identity, which the loader must refuse.
    let scene_path = tmp.path().join(".varda").join("scene.json");
    let mut scene: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&scene_path).unwrap()).unwrap();
    let first = scene["channels"][0].clone();
    scene["channels"]
        .as_array_mut()
        .expect("channels array")
        .push(first);
    std::fs::write(&scene_path, serde_json::to_string(&scene).unwrap()).unwrap();

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let load = app.load_workspace();

    let live = app.production_buses().get(&bus).expect("bus loads");
    assert!(
        !live.program_resolved(),
        "routing must not resolve against a scene that was rejected"
    );
    assert_eq!(live.effective_program(), &BusInputRef::Black);
    assert_eq!(
        live.program(),
        &BusInputRef::Channel(channel),
        "the configured route is retained"
    );

    let reported = load.error_message().unwrap_or_default();
    assert!(
        reported.contains("held black"),
        "the operator must be told why: {reported}"
    );
    assert!(
        reported.contains("unusable entity identities"),
        "and the scene must have been rejected for its duplicate identities, \
         not for some unrelated parse failure: {reported}"
    );
}

#[test]
fn stale_bus_routing_does_not_resolve_against_a_freshly_loaded_scene() {
    // Buses live in stage.json. If the stage is missing or unparseable the
    // previous bank stays live, while the scene may still be replaced beneath
    // it — so a retained route naming a UUID the new scene also uses would
    // resolve to different content. Routing must fail closed.
    let tmp = TempDir::new().unwrap();
    {
        let Some(mut seed) = headless_app_in(tmp.path()) else {
            return;
        };
        seed.save_workspace(&UILayoutState::default())
            .expect("save workspace");
    }

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    // First load: the mixer now holds the saved scene's channels.
    let _ = app.load_workspace();
    let channel = channel_uuid(&mut app, 0);

    // Route a bus at one of them, and confirm it really is routed.
    let bus = match send_cmd(
        &mut app,
        EngineCommand::AddBus {
            name: "LED".into(),
            width: 1920,
            height: 480,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected a bus uuid, got {other:?}"),
    };
    match send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel.clone()),
        },
    ) {
        CommandResult::Ok => {}
        other => panic!("routing should be accepted: {other:?}"),
    }
    fire(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    assert_eq!(
        app.production_buses().get(&bus).unwrap().program(),
        &BusInputRef::Channel(channel.clone()),
        "precondition: the bus really is routed (Black would resolve trivially)"
    );

    // Break only the stage, then reload: the bank cannot come from this load,
    // but the scene — carrying that same channel uuid — will.
    std::fs::write(tmp.path().join(".varda").join("stage.json"), "{ not json").unwrap();
    let load = app.load_workspace();

    let reported = load.error_message().unwrap_or_default();
    assert!(
        !reported.contains("scene:"),
        "the scene should still load cleanly; this test is about the stage: {reported}"
    );
    assert!(
        app.build_engine_state()
            .mixer
            .channels
            .iter()
            .any(|c| c.uuid == channel),
        "precondition: the reloaded scene reuses that channel uuid"
    );

    let live = app.production_buses().get(&bus).expect("stale bus remains");
    assert!(
        !live.program_resolved(),
        "a bank from a previous workspace must not resolve against a new scene"
    );
    assert_eq!(
        live.effective_program(),
        &BusInputRef::Black,
        "it must hold black rather than latch onto the reused uuid"
    );
    assert_eq!(
        live.program(),
        &BusInputRef::Channel(channel),
        "the configured route is retained"
    );
    assert!(
        reported.contains("previous workspace"),
        "the operator must be told why: {reported}"
    );

    // The quarantine must outlive ordinary reconciliation in this direction
    // too: a failed removal changes no sources and must not re-arm the route.
    fire(
        &mut app,
        EngineCommand::RemoveDeck {
            deck_uuid: "nosuchdeck".into(),
        },
    );
    assert!(
        !app.production_buses().get(&bus).unwrap().program_resolved(),
        "a failed removal must not re-arm quarantined routing"
    );
    assert_eq!(
        app.production_buses()
            .get(&bus)
            .unwrap()
            .effective_program(),
        &BusInputRef::Black
    );

    // Staging is the other door into the quarantine: it validates against the
    // stale mixer and marks the endpoint resolved, and a CUT would then swap
    // that onto Program.
    let stale_channel = channel_uuid(&mut app, 0);
    match send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(stale_channel),
        },
    ) {
        CommandResult::Err { .. } => {}
        other => panic!("staging while quarantined must be refused, got {other:?}"),
    }
    fire(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    assert_eq!(
        app.production_buses()
            .get(&bus)
            .unwrap()
            .effective_program(),
        &BusInputRef::Black,
        "a refused stage followed by a cut must not put stale content on air"
    );

    // Clearing to black stays allowed: it cannot resolve to anything.
    match send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Black,
        },
    ) {
        CommandResult::Ok => {}
        other => panic!("staging black must remain allowed, got {other:?}"),
    }

    // A new bus starts on Main, which here is the stale mixer: it is held
    // black with the rest, keeping Main to show once a show loads whole.
    let added = add_bus(&mut app, "TV", 1920, 1080);
    let added = app.production_buses().get(&added).unwrap();
    assert_eq!(
        added.effective_program(),
        &BusInputRef::Black,
        "a bus added while routing is held black must not show the stale Main"
    );
    assert_eq!(added.program(), &BusInputRef::Master);
}

#[test]
fn stage_only_load_does_not_resolve_routes_against_a_stale_mixer() {
    // The mirror of the stale-bank case: a workspace with a stage but no scene.
    // The mixer still holds whatever was there before, which is exactly the
    // provenance routing must not trust.
    //
    // The same app is kept alive across the reload on purpose. A fresh app
    // would mint new channel UUIDs, so the route could not match anything even
    // with the fix reverted — the test would pass while proving nothing.
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let channel = channel_uuid(&mut app, 0);
    let bus = match send_cmd(
        &mut app,
        EngineCommand::AddBus {
            name: "LED".into(),
            width: 1920,
            height: 480,
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected a bus uuid, got {other:?}"),
    };
    match send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel.clone()),
        },
    ) {
        CommandResult::Ok => {}
        other => panic!("routing should be accepted: {other:?}"),
    }
    fire(&mut app, EngineCommand::BusCut { uuid: bus.clone() });
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    // Remove the scene, keeping the stage that routes at a channel which is
    // still live in *this* app's mixer — the stale content the fix must refuse.
    std::fs::remove_file(tmp.path().join(".varda").join("scene.json")).unwrap();
    let load = app.load_workspace();

    assert!(
        app.build_engine_state()
            .mixer
            .channels
            .iter()
            .any(|c| c.uuid == channel),
        "precondition: the stale mixer still contains the routed uuid, so a \
         reverted fix really would resolve to it"
    );

    let live = app
        .production_buses()
        .get(&bus)
        .expect("bus loads from stage");
    assert_eq!(
        live.program(),
        &BusInputRef::Channel(channel),
        "the configured route is retained"
    );
    assert!(
        !live.program_resolved(),
        "with no scene of its own, routing must not resolve against the live mixer"
    );
    assert_eq!(live.effective_program(), &BusInputRef::Black);
    let reported = load.error_message().unwrap_or_default();
    assert!(
        reported.contains("held black"),
        "the operator must be told why: {reported}"
    );

    // The quarantine must outlive ordinary reconciliation. A failed removal
    // changes no sources, and an undo restores authored state — neither may
    // re-arm a route the load refused to trust.
    fire(
        &mut app,
        EngineCommand::RemoveDeck {
            deck_uuid: "nosuchdeck".into(),
        },
    );
    assert!(
        !app.production_buses().get(&bus).unwrap().program_resolved(),
        "a failed removal must not re-arm quarantined routing"
    );

    send_cmd(&mut app, EngineCommand::AddChannel);
    fire(&mut app, EngineCommand::Undo);
    assert!(
        !app.production_buses().get(&bus).unwrap().program_resolved(),
        "undo must not re-arm quarantined routing"
    );
    assert_eq!(
        app.production_buses()
            .get(&bus)
            .unwrap()
            .effective_program(),
        &BusInputRef::Black
    );
}

/// A camera that is not there when the show opens — unplugged, or not
/// allowed yet — keeps its layer, waiting for it, rather than the layer being
/// dropped and lost with the next save.
#[test]
fn a_missing_camera_keeps_its_layer_through_load_and_save() {
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let CommandResult::OkWithId { uuid } = send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ) else {
        panic!("no deck");
    };
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");

    // Make that layer a capture card the show saw last time.
    let scene_path = tmp.path().join(".varda").join("scene.json");
    let mut scene: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&scene_path).unwrap()).unwrap();
    let deck = scene["channels"][0]["decks"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|d| d["uuid"] == uuid.as_str())
        .expect("the saved deck");
    deck["source"] = serde_json::json!({
        "type": "Camera",
        "name": "Blackmagic Design",
        "mode": {"width": 1920, "height": 1080, "fps": 59}
    });
    std::fs::write(&scene_path, serde_json::to_string_pretty(&scene).unwrap()).unwrap();

    // Cameras are off here, so it cannot be opened.
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    let layer = state.mixer.channels[0]
        .decks
        .iter()
        .find(|d| d.uuid == uuid)
        .expect("the camera's layer is kept, waiting");
    assert!(layer.name.contains("Blackmagic Design"), "{}", layer.name);

    // Saved again, it is still the camera, at its format.
    app2.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let saved = std::fs::read_to_string(&scene_path).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let deck = saved["channels"][0]["decks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["uuid"] == uuid.as_str())
        .expect("still saved");
    assert_eq!(deck["source"]["type"], "Camera");
    assert_eq!(deck["source"]["name"], "Blackmagic Design");
    assert_eq!(deck["source"]["mode"]["fps"], 59);
}

/// A mask on an effect is kept with the show; one with no area is refused.
#[test]
fn an_effects_mask_is_kept_through_save_and_load() {
    use varda::engine::value::mask::{EffectMask, MaskShape};
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 1.0, 1.0, 1.0],
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("{other:?}"),
    };
    let effect = match send_cmd(
        &mut app,
        EngineCommand::AddEffect {
            target: varda::engine::EffectTarget::Deck(deck),
            shader_name: "invert".into(),
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("{other:?}"),
    };
    let mask = EffectMask {
        shape: MaskShape::Path {
            points: vec![[0.2, 0.2], [0.8, 0.3], [0.5, 0.9]],
        },
        feather: 40.0,
        invert: true,
    };
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetEffectMask {
                effect_uuid: effect.clone(),
                mask: Some(mask.clone()),
            },
        ),
        CommandResult::Ok
    ));
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetEffectMask {
                effect_uuid: effect.clone(),
                mask: Some(EffectMask {
                    shape: MaskShape::Rectangle {
                        x: 0.5,
                        y: 0.5,
                        width: 0.0,
                        height: 0.2,
                    },
                    feather: 0.0,
                    invert: false,
                }),
            },
        ),
        CommandResult::Err { .. }
    ));
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let kept = app2
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| c.decks.iter().flat_map(|d| d.effects.clone()))
        .find(|e| e.uuid == effect)
        .and_then(|e| e.mask);
    assert_eq!(kept, Some(mask));
}

/// A layer's own mask is kept with the show, its effect still last.
#[test]
fn a_layers_mask_is_kept_through_save_and_load() {
    use varda::engine::value::mask::{EffectMask, MaskShape, LAYER_MASK_EFFECT};
    let tmp = TempDir::new().unwrap();
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let ch = channel_uuid(&mut app, 0);
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: ch,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("{other:?}"),
    };
    let mask = EffectMask {
        shape: MaskShape::Path {
            points: vec![[0.2, 0.2], [0.8, 0.3], [0.5, 0.9]],
        },
        feather: 25.0,
        invert: true,
    };
    assert!(matches!(
        send_cmd(
            &mut app,
            EngineCommand::SetLayerMask {
                deck_uuid: deck.clone(),
                mask: Some(mask.clone()),
            },
        ),
        CommandResult::OkWithId { .. }
    ));
    app.save_workspace(&UILayoutState::default())
        .expect("save workspace");
    let Some(mut app2) = headless_app_in(tmp.path()) else {
        return;
    };
    let _ = app2.load_workspace();
    let state = app2.build_engine_state();
    let effects = &state
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .expect("the layer")
        .effects;
    let last = effects.last().expect("effects");
    assert_eq!(last.name, LAYER_MASK_EFFECT);
    assert_eq!(last.mask, Some(mask));
}

/// A channel deleted while buses point at it — on Preview, on Program, left
/// out of one's Master — is let go by all of them: saved and loaded, the show
/// reports nothing missing. Kept, those links were saved and every load
/// after said the workspace failed to load.
#[test]
fn a_deleted_channel_leaves_no_bus_pointing_at_it() {
    let tmp = TempDir::new().unwrap();
    let (stream, tv, gone) = {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        fire(&mut app, EngineCommand::AddChannel);
        let gone = channel_uuid(&mut app, 2);
        let stream = add_bus(&mut app, "STREAM", 1920, 1080);
        let tv = add_bus(&mut app, "TV", 1920, 1080);
        fire(
            &mut app,
            EngineCommand::SetBusPreview {
                uuid: stream.clone(),
                input: BusInputRef::Channel(gone.clone()),
            },
        );
        fire(
            &mut app,
            EngineCommand::BusCut {
                uuid: stream.clone(),
            },
        );
        fire(
            &mut app,
            EngineCommand::SetBusPreview {
                uuid: stream.clone(),
                input: BusInputRef::Channel(gone.clone()),
            },
        );
        fire(
            &mut app,
            EngineCommand::SetBusLayerHidden {
                uuid: tv.clone(),
                channel_uuid: gone.clone(),
                hidden: true,
            },
        );
        fire(
            &mut app,
            EngineCommand::RemoveChannel {
                channel_uuid: gone.clone(),
            },
        );
        let buses = app.production_buses();
        let bus = buses.get(&stream).unwrap();
        assert_eq!(bus.preview(), &BusInputRef::Black, "Preview let go");
        assert_eq!(bus.program(), &BusInputRef::Black, "Program let go");
        assert!(buses.get(&tv).unwrap().hidden_layers().is_empty());
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        (stream, tv, gone)
    };
    let saved = std::fs::read_to_string(tmp.path().join(".varda").join("stage.json")).unwrap();
    assert!(!saved.contains(&gone), "the stage keeps no link to it");

    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let load = app.load_workspace();
    assert!(load.is_ok(), "{:?}", load.error_message());
    assert!(app.production_buses().get(&stream).is_some());
    assert!(app.production_buses().get(&tv).is_some());
}

/// A layer showing another input keeps that input's name through a reload —
/// it came back named after the channel's id.
#[test]
fn a_layer_showing_another_input_keeps_its_name_through_a_reload() {
    let tmp = TempDir::new().unwrap();
    let shown_name = {
        let Some(mut app) = headless_app_in(tmp.path()) else {
            return;
        };
        let (first, second) = (channel_uuid(&mut app, 0), channel_uuid(&mut app, 1));
        send_cmd(
            &mut app,
            EngineCommand::AddTapDeck {
                channel_uuid: first,
                source: varda::scene::TapSourceConfig::Channel { uuid: second },
            },
        );
        app.save_workspace(&UILayoutState::default())
            .expect("save workspace");
        app.build_engine_state().mixer.channels[1].name.clone()
    };
    let Some(mut app) = headless_app_in(tmp.path()) else {
        return;
    };
    let load = app.load_workspace();
    assert!(load.is_ok(), "{:?}", load.error_message());
    let names: Vec<String> = app.build_engine_state().mixer.channels[0]
        .decks
        .iter()
        .map(|d| d.name.clone())
        .collect();
    assert!(
        names.iter().any(|n| n.ends_with(&shown_name)),
        "named after {shown_name}: {names:?}"
    );
}
