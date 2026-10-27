//! PowerPoint inputs: a deck played by PowerPoint in a window Nida captures,
//! moved on from Nida — the input added, its show opened and stepped (the one
//! on air, or the one moved last, when no deck is named), its notes read
//! from the file, kept with the show at its slide, and closed with the input.
//!
//! PowerPoint is a stand-in (`varda::presentation::fake`): nothing here
//! opens or drives the real one, or asks macOS to.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use clap::Parser;
use varda::app::{AppConfig, VardaApp};
use varda::engine::value::presentation::{
    ClocksAt, Go, PlaysOn, PresentationSnapshot, PresentationStatus, PresenterLayout, Screen,
    VideoControl, VideoState,
};
use varda::engine::{CommandResult, EngineCommand, InputSource};
use varda::presentation::fake::FakePowerPoint;
use varda::presentation::manager::PresentationManager;
use varda::presentation::screens::ScreenSpot;
use varda::usecases::ui::UILayoutState;

mod common;

/// A headless app on `workspace`, never driving PowerPoint: `ppt` stands
/// in for it, its show's window a made-up capture.
fn app_in(workspace: &Path, ppt: &FakePowerPoint) -> Option<VardaApp> {
    let gpu = common::headless_gpu()?;
    let config = AppConfig::parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--no-powerpoint",
        "--workspace",
        workspace.to_str().unwrap(),
    ]);
    let mut app = VardaApp::new(gpu, &config).expect("VardaApp::new");
    app.set_presentation_manager(PresentationManager::start(ppt.clone(), ppt.windows(), 1.0));
    // A 1080p screen at 1×, whatever this machine has.
    app.set_presentation_screens(vec![screen(1, 0, 1.0, true)]);
    Some(app)
}

/// A 1920 × 1080-point screen at `left`, `scale` pixels to a point.
fn screen(display: u32, left: i32, scale: f64, main: bool) -> ScreenSpot {
    ScreenSpot {
        display,
        left,
        top: 0,
        width: 1920,
        height: 1080,
        scale,
        main,
    }
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

/// A .pptx of three slides: "Welcome" with notes, a hidden one, "Results".
fn write_deck(path: &Path) -> PathBuf {
    const P: &str = "http://schemas.openxmlformats.org/presentationml/2006/main";
    const A: &str = "http://schemas.openxmlformats.org/drawingml/2006/main";
    const R: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    const RELS: &str = "http://schemas.openxmlformats.org/package/2006/relationships";
    let shape = |kind: &str, text: &str| {
        format!(
            r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="x"/><p:cNvSpPr/><p:nvPr><p:ph type="{kind}"/></p:nvPr></p:nvSpPr><p:txBody><a:bodyPr/><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"#
        )
    };
    let slide = |show: &str, shapes: String| {
        format!(
            r#"<p:sld xmlns:p="{P}" xmlns:a="{A}" xmlns:r="{R}"{show}><p:cSld><p:spTree>{shapes}</p:spTree></p:cSld></p:sld>"#
        )
    };
    let rels = |entries: &[(&str, &str, &str)]| {
        let mut list = String::new();
        for (id, kind, target) in entries {
            let _ = write!(
                list,
                r#"<Relationship Id="{id}" Type="{R}/{kind}" Target="{target}"/>"#
            );
        }
        format!(r#"<Relationships xmlns="{RELS}">{list}</Relationships>"#)
    };
    let parts = [
        (
            "ppt/presentation.xml",
            format!(
                r#"<p:presentation xmlns:p="{P}" xmlns:r="{R}"><p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId3"/></p:sldIdLst><p:sldSz cx="12192000" cy="6858000"/></p:presentation>"#
            ),
        ),
        (
            "ppt/_rels/presentation.xml.rels",
            rels(&[
                ("rId1", "slide", "slides/slide1.xml"),
                ("rId2", "slide", "slides/slide2.xml"),
                ("rId3", "slide", "slides/slide3.xml"),
            ]),
        ),
        (
            "ppt/slides/slide1.xml",
            slide("", shape("title", "Welcome")),
        ),
        (
            "ppt/slides/_rels/slide1.xml.rels",
            rels(&[("rId1", "notesSlide", "../notesSlides/notesSlide1.xml")]),
        ),
        (
            "ppt/notesSlides/notesSlide1.xml",
            format!(
                r#"<p:notes xmlns:p="{P}" xmlns:a="{A}"><p:cSld><p:spTree>{}</p:spTree></p:cSld></p:notes>"#,
                shape("body", "Thank the organisers.")
            ),
        ),
        (
            "ppt/slides/slide2.xml",
            slide(r#" show="0""#, String::new()),
        ),
        (
            "ppt/slides/slide3.xml",
            slide("", shape("title", "Results")),
        ),
    ];
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, text) in parts {
        zip.start_file(name, options).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    zip.finish().unwrap();
    std::fs::canonicalize(path).unwrap()
}

fn add_input(app: &mut VardaApp, path: &Path) {
    let result = send_cmd(
        app,
        EngineCommand::AddInput {
            source: InputSource::PowerPoint {
                path: path.to_string_lossy().into_owned(),
            },
            name: None,
            audio: None,
        },
    );
    assert!(
        matches!(result, CommandResult::OkWithId { .. }),
        "{result:?}"
    );
}

/// The PowerPoint input's layer and its show, as `GET /api/state` has it.
fn presentation(app: &VardaApp) -> Option<(String, String, PresentationSnapshot)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| ch.decks.iter().map(move |d| (ch, d)))
        .find_map(|(ch, d)| Some((ch.uuid.clone(), d.uuid.clone(), d.presentation.clone()?)))
}

/// Whether the PowerPoint layer shows its show's window.
fn captured(app: &VardaApp) -> bool {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .filter(|d| d.presentation.is_some())
        .any(|d| d.screen_capture.as_ref().is_some_and(|s| s.bound))
}

/// Frames until the show is as `done` wants it (the worker answers on its
/// own thread).
fn until(app: &mut VardaApp, done: impl Fn(&PresentationSnapshot) -> bool) -> PresentationSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let shown = presentation(app).map(|(_, _, p)| p);
        if let Some(p) = &shown {
            if done(p) {
                return p.clone();
            }
        }
        assert!(Instant::now() < deadline, "never came: {shown:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_show_plays_on_a_spare_screen_until_told_this_macs() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    // A spare 4K screen at 2× beside this Mac's 1080p one: sharper there.
    app.set_presentation_screens(vec![screen(1, 0, 1.0, true), screen(2, 1920, 2.0, false)]);
    add_input(&mut app, &deck);
    let shown = until(&mut app, |p| p.status == PresentationStatus::Showing);
    assert_eq!(ppt.window_place(), (1920, 0, 960, 572));
    assert_eq!(shown.plays_on, PlaysOn::Auto);
    assert_eq!(shown.playing_on.as_deref(), Some("screen 2"));
    assert_eq!(shown.slide_pixels, Some([1920, 1080]));

    let (_, layer, _) = presentation(&app).expect("a PowerPoint layer");
    let set = send_cmd(
        &mut app,
        EngineCommand::SetPresentationPlaysOn {
            deck_uuid: layer.clone(),
            plays_on: PlaysOn::Main,
        },
    );
    assert!(matches!(set, CommandResult::Ok), "{set:?}");
    let moved = until(&mut app, |p| {
        p.playing_on.as_deref() == Some("this Mac's screen")
    });
    assert_eq!(ppt.window_place(), (0, 0, 1920, 1112));
    assert_eq!(moved.plays_on, PlaysOn::Main);
    assert_eq!(moved.slide_pixels, Some([1920, 1080]));

    // Kept with the show.
    app.save_workspace(&UILayoutState::default()).unwrap();
    app.flush_saves();
    let saved = std::fs::read_to_string(dir.path().join("show/.varda/scene.json")).unwrap();
    let scene: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let kept = scene["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|ch| ch["decks"].as_array().into_iter().flatten())
        .find_map(|d| d["source"].get("presentation").cloned())
        .expect("the deck file kept with the show");
    assert_eq!(kept["plays_on"], "main");

    // Undone: Automatic again, and back on the spare screen.
    let undone = send_cmd(&mut app, EngineCommand::Undo);
    assert!(matches!(undone, CommandResult::Ok), "{undone:?}");
    let back = until(&mut app, |p| p.playing_on.as_deref() == Some("screen 2"));
    assert_eq!(back.plays_on, PlaysOn::Auto);
    assert_eq!(ppt.window_place(), (1920, 0, 960, 572));

    // Only a PowerPoint input has a screen to play on.
    let refused = send_cmd(
        &mut app,
        EngineCommand::SetPresentationPlaysOn {
            deck_uuid: "nope".into(),
            plays_on: PlaysOn::Main,
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
}

#[test]
fn two_inputs_of_one_file_share_its_show_where_the_one_left_asks() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    app.set_presentation_screens(vec![screen(1, 0, 1.0, true), screen(2, 1920, 2.0, false)]);
    add_input(&mut app, &deck);
    until(&mut app, |p| p.playing_on.as_deref() == Some("screen 2"));
    add_input(&mut app, &deck);
    let layers: Vec<String> = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .filter(|d| d.presentation.is_some())
        .map(|d| d.uuid.clone())
        .collect();
    assert_eq!(layers.len(), 2);
    // The second asks for this Mac's screen: the file's one show goes there.
    let set = send_cmd(
        &mut app,
        EngineCommand::SetPresentationPlaysOn {
            deck_uuid: layers[1].clone(),
            plays_on: PlaysOn::Main,
        },
    );
    assert!(matches!(set, CommandResult::Ok), "{set:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ppt.window_place().0 != 0 {
        assert!(
            Instant::now() < deadline,
            "never moved: {:?}",
            ppt.window_place()
        );
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
    // It goes: the one left has its show back on the spare screen.
    let removed = send_cmd(
        &mut app,
        EngineCommand::RemoveDeck {
            deck_uuid: layers[1].clone(),
        },
    );
    assert!(matches!(removed, CommandResult::Ok), "{removed:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while ppt.window_place().0 != 1920 {
        assert!(
            Instant::now() < deadline,
            "left where the other asked: {:?}",
            ppt.window_place()
        );
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Frames until the show's window is put at `left`.
#[track_caller]
fn until_placed(app: &mut VardaApp, ppt: &FakePowerPoint, left: i32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while ppt.window_place().0 != left {
        assert!(
            Instant::now() < deadline,
            "never put at {left}: {:?}",
            ppt.window_place()
        );
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_screen_chosen_takes_the_files_show_back_from_another_input() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    app.set_presentation_screens(vec![screen(1, 0, 1.0, true), screen(2, 1920, 2.0, false)]);
    add_input(&mut app, &deck);
    until(&mut app, |p| p.playing_on.as_deref() == Some("screen 2"));
    add_input(&mut app, &deck);
    let layers: Vec<String> = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .filter(|d| d.presentation.is_some())
        .map(|d| d.uuid.clone())
        .collect();
    let set = |app: &mut VardaApp, layer: &str, plays_on: PlaysOn| {
        let result = send_cmd(
            app,
            EngineCommand::SetPresentationPlaysOn {
                deck_uuid: layer.to_string(),
                plays_on,
            },
        );
        assert!(matches!(result, CommandResult::Ok), "{result:?}");
    };
    // The second takes the file's show to this Mac's screen.
    set(&mut app, &layers[1], PlaysOn::Main);
    until_placed(&mut app, &ppt, 0);
    // The first is told a screen — one not connected here, so as Automatic:
    // the spare one, where it was already sent. Told, it takes it back.
    set(&mut app, &layers[0], PlaysOn::Screen("not-here".into()));
    until_placed(&mut app, &ppt, 1920);
    // The second asks again (Automatic, seen at a frame, then this Mac's),
    // and an undo of that, back to Automatic, is a choice too.
    set(&mut app, &layers[1], PlaysOn::Auto);
    app.render_mixer_frame();
    set(&mut app, &layers[1], PlaysOn::Main);
    until_placed(&mut app, &ppt, 0);
    let undone = send_cmd(&mut app, EngineCommand::Undo);
    assert!(matches!(undone, CommandResult::Ok), "{undone:?}");
    until_placed(&mut app, &ppt, 1920);
}

#[test]
fn a_powerpoint_input_plays_its_deck_with_its_notes_and_steps_go_to_it() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    // No PowerPoint input yet: a clicker's step has nowhere to go.
    let refused = send_cmd(
        &mut app,
        EngineCommand::PresentationGo {
            deck_uuid: None,
            to: Go::Next,
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");

    add_input(&mut app, &deck);
    let (channel, _, opening) = presentation(&app).expect("a PowerPoint layer");
    assert_eq!(opening.status, PresentationStatus::Opening);
    assert_eq!(opening.path, deck.to_string_lossy());
    let name = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == channel)
        .map(|c| c.name.clone());
    assert_eq!(name.as_deref(), Some("talk"), "named after its file");

    let shown = until(&mut app, |p| p.status == PresentationStatus::Showing);
    assert_eq!(ppt.opened_files(), vec![deck.clone()]);
    // The show's window, captured on the worker's thread, taken by the layer.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !captured(&app) {
        assert!(
            Instant::now() < deadline,
            "the show's window was never captured"
        );
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(
        (shown.slide, shown.slides, shown.position),
        (Some(1), Some(3), Some(1))
    );
    assert_eq!(shown.title.as_deref(), Some("Welcome"));
    assert_eq!(shown.notes, "Thank the organisers.");
    // Slide 2 is hidden: the show plays two, and slide 3 comes next.
    assert_eq!(shown.shown, Some(2));
    assert_eq!(
        (shown.next_slide, shown.next_title.as_deref()),
        (Some(3), Some("Results"))
    );

    // A step with no deck named goes to the only show there is.
    let moved = send_cmd(
        &mut app,
        EngineCommand::PresentationGo {
            deck_uuid: None,
            to: Go::Next,
        },
    );
    assert!(matches!(moved, CommandResult::Ok), "{moved:?}");
    let next = until(&mut app, |p| p.slide == Some(2));
    assert_eq!(next.notes, "", "slide 2 has no notes");
    assert_eq!(ppt.steps(), vec!["next"]);

    send_cmd(
        &mut app,
        EngineCommand::SetPresentationScreen {
            deck_uuid: None,
            screen: Screen::Black,
        },
    );
    until(&mut app, |p| p.screen == Screen::Black);
}

/// The video on the slide on screen: in the input's state, and played,
/// moved and paused by the API's `PresentationVideo` — the show on air's
/// without a `deck_uuid`.
#[test]
fn a_slides_video_is_played_moved_and_paused_by_the_api() {
    let dir = tempfile::tempdir().unwrap();
    let deck_path = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(5);
    ppt.with_video(2, 8);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    add_input(&mut app, &deck_path);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    assert_eq!(
        presentation(&app).unwrap().2.video,
        None,
        "slide 1 has none"
    );

    send_cmd(
        &mut app,
        EngineCommand::PresentationGo {
            deck_uuid: None,
            to: Go::Next,
        },
    );
    let p = until(&mut app, |p| p.video.is_some());
    assert_eq!(p.slide, Some(2));
    assert_eq!(p.video.map(|v| v.state), Some(VideoState::NotReady));

    let control = |app: &mut VardaApp, control| {
        let done = send_cmd(
            app,
            EngineCommand::PresentationVideo {
                deck_uuid: None,
                control,
            },
        );
        assert!(matches!(done, CommandResult::Ok), "{done:?}");
    };
    control(&mut app, VideoControl::Toggle);
    until(&mut app, |p| {
        p.video.map(|v| v.state) == Some(VideoState::Playing)
    });
    control(&mut app, VideoControl::Skip(10));
    until(&mut app, |p| p.video.map(|v| v.position_ms) == Some(10_000));
    control(&mut app, VideoControl::Pause);
    let p = until(&mut app, |p| {
        p.video.map(|v| v.state) == Some(VideoState::Paused)
    });
    assert_eq!(p.video.map(|v| v.position_ms), Some(10_000));

    send_cmd(
        &mut app,
        EngineCommand::PresentationGo {
            deck_uuid: None,
            to: Go::Next,
        },
    );
    until(&mut app, |p| p.slide == Some(3) && p.video.is_none());
}

#[test]
fn a_powerpoint_input_comes_back_with_the_show_at_its_slide() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let workspace = dir.path().join("show");
    let layout = UILayoutState::default();
    {
        let ppt = FakePowerPoint::with_slides(3);
        let Some(mut app) = app_in(&workspace, &ppt) else {
            return;
        };
        add_input(&mut app, &deck);
        let (_, deck_uuid, _) = presentation(&app).unwrap();
        until(&mut app, |p| p.status == PresentationStatus::Showing);
        send_cmd(
            &mut app,
            EngineCommand::AddInput {
                source: InputSource::PresenterView {
                    deck_uuid: deck_uuid.clone(),
                },
                name: None,
                audio: None,
            },
        );
        send_cmd(
            &mut app,
            EngineCommand::PresentationGo {
                deck_uuid: Some(deck_uuid),
                to: Go::Next,
            },
        );
        until(&mut app, |p| p.slide == Some(2));
        // A frame takes the slide into the show to be saved.
        app.render_mixer_frame();
        app.save_workspace(&layout).unwrap();
        app.flush_saves();
    }
    let saved = std::fs::read_to_string(workspace.join(".varda/scene.json")).unwrap();
    let scene: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let kept = scene["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|ch| ch["decks"].as_array().into_iter().flatten())
        .find_map(|d| d["source"].get("presentation").cloned())
        .expect("the deck file kept with the show");
    assert_eq!(kept["path"], deck.to_string_lossy().as_ref());
    assert_eq!(kept["slide"], 2);

    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&workspace, &ppt) else {
        return;
    };
    let _ = app.load_workspace();
    let back = until(&mut app, |p| p.status == PresentationStatus::Showing);
    assert_eq!(back.slide, Some(2), "back where it was");
    assert_eq!(ppt.slide(), 2);
    // Its presenter view came back with it, showing it.
    let (_, powerpoint_layer, _) = presentation(&app).unwrap();
    let follows = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .find_map(|d| d.presenter_of.clone());
    assert_eq!(follows, Some(powerpoint_layer));
}

#[test]
fn removing_a_powerpoint_input_closes_the_deck_nida_opened() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    add_input(&mut app, &deck);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    let (channel, _, _) = presentation(&app).unwrap();
    let removed = send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: channel,
        },
    );
    assert!(matches!(removed, CommandResult::Ok), "{removed:?}");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ppt.closed() {
        assert!(Instant::now() < deadline, "the deck was never closed");
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!ppt.showing());
}

#[test]
fn a_powerpoint_input_needs_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    let result = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::PowerPoint {
                path: dir.path().join("gone.pptx").to_string_lossy().into_owned(),
            },
            name: None,
            audio: None,
        },
    );
    assert!(matches!(result, CommandResult::Err { .. }), "{result:?}");
    assert!(presentation(&app).is_none());
}

#[test]
fn after_an_undo_the_powerpoint_layer_captures_its_show_again() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    add_input(&mut app, &deck);
    let (channel, deck_uuid, _) = presentation(&app).unwrap();
    // Renamed before its window is captured: the history keeps the layer as
    // it was then, with no window of its own.
    send_cmd(
        &mut app,
        EngineCommand::RenameChannel {
            channel_uuid: channel,
            name: "Keynote".into(),
        },
    );
    let wait_captured = |app: &mut VardaApp| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !captured(app) {
            assert!(
                Instant::now() < deadline,
                "the show's window was never captured"
            );
            app.render_mixer_frame();
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait_captured(&mut app);
    // Undone, the layer is made again from that history — without its
    // capture — and must win its window back, its show still running.
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(presentation(&app).map(|(_, d, _)| d), Some(deck_uuid));
    assert!(!captured(&app), "made again, with no capture yet");
    wait_captured(&mut app);
    assert_eq!(ppt.opened_files().len(), 1, "the deck was not opened again");
}

fn f16(bits: u16) -> f32 {
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f32::from(bits & 0x3ff);
    let mag = if exp == 0 {
        frac * 2f32.powi(-24)
    } else {
        (1.0 + frac / 1024.0) * 2f32.powi(exp - 15)
    };
    if bits >> 15 == 1 {
        -mag
    } else {
        mag
    }
}

/// Every pixel of an `Rgba16Float` texture, row by row.
fn frame_pixels(
    gpu: &varda::renderer::context::GpuContext,
    texture: &wgpu::Texture,
) -> Vec<[f32; 4]> {
    let (w, h) = (texture.width(), texture.height());
    let row =
        (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        texture.size(),
    );
    gpu.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let at = (y * row + x * 8) as usize;
            out.push(std::array::from_fn(|c| {
                f16(u16::from_le_bytes([data[at + c * 2], data[at + c * 2 + 1]]))
            }));
        }
    }
    out
}

/// The presenter view's layer: its channel, its own UUID.
fn presenter(app: &VardaApp) -> Option<(String, String)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .find_map(|ch| {
            let d = ch.decks.iter().find(|d| d.presenter_of.is_some())?;
            Some((ch.uuid.clone(), d.uuid.clone()))
        })
}

/// What the presenter view's words say.
fn presenter_words(app: &VardaApp) -> Option<varda::deck::presenter::Words> {
    app.mixer_ref()
        .channels()
        .iter()
        .flat_map(|ch| &ch.decks)
        .find_map(|slot| slot.deck.presenter().map(|p| p.words().clone()))
}

#[test]
fn a_presenter_view_shows_the_slide_live_beside_its_notes() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    add_input(&mut app, &deck);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    let (_, deck_uuid, _) = presentation(&app).unwrap();
    let added = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::PresenterView {
                deck_uuid: deck_uuid.clone(),
            },
            name: None,
            audio: None,
        },
    );
    assert!(matches!(added, CommandResult::OkWithId { .. }), "{added:?}");
    let (channel, _) = presenter(&app).expect("a presenter view layer");
    let state = app.build_engine_state();
    let name = state
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == channel)
        .map(|c| c.name.clone());
    assert_eq!(name.as_deref(), Some("Presenter — talk"));

    // Its words follow the show: slide 1 of the 2 the show plays, its notes.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let words = presenter_words(&app).unwrap_or_default();
        if words.position == "1 / 2" {
            assert_eq!(words.notes, "Thank the organisers.");
            assert_eq!(words.next, "Next: 3 · Results");
            break;
        }
        assert!(Instant::now() < deadline, "words never came: {words:?}");
        std::thread::sleep(Duration::from_millis(5));
    }

    // Drawn: the live slide (the stand-in's coloured capture) in its place,
    // the panel's dark round it.
    let index = app
        .mixer_ref()
        .channels()
        .iter()
        .position(|c| c.uuid() == channel)
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        app.render_mixer_frame();
        let texture = &app.mixer_ref().channels()[index].composite_texture;
        let (w, h) = (texture.width(), texture.height());
        let pixels = frame_pixels(app.gpu_context(), texture);
        let at = varda::deck::presenter::layout((w, h), 16.0 / 9.0, &PresenterLayout::default());
        let px = |x: f32, y: f32| pixels[(y as u32 * w + x as u32) as usize];
        let slide = px(at.slide.x + at.slide.w / 2.0, at.slide.y + at.slide.h / 2.0);
        // Between the slide and the notes, below the bar: the panel.
        let panel = px(at.slide.x + at.slide.w + 2.0, at.slide.y + at.slide.h / 2.0);
        let lit = slide[1] > 0.1 && slide[2] > 0.1;
        let dark = panel[0] < 0.05 && panel[1] < 0.05 && panel[2] < 0.05 && panel[3] > 0.9;
        if lit && dark {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "slide {slide:?}, panel {panel:?} in a {w}×{h} picture"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    // Blacked out: BLACK across the slide's foot, over the live slide.
    send_cmd(
        &mut app,
        EngineCommand::SetPresentationScreen {
            deck_uuid: None,
            screen: Screen::Black,
        },
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        app.render_mixer_frame();
        let texture = &app.mixer_ref().channels()[index].composite_texture;
        let (w, h) = (texture.width(), texture.height());
        let pixels = frame_pixels(app.gpu_context(), texture);
        let at = varda::deck::presenter::layout((w, h), 16.0 / 9.0, &PresenterLayout::default());
        let px = |x: f32, y: f32| pixels[(y as u32 * w + x as u32) as usize];
        let x = at.slide.x + at.slide.w * 0.1;
        let strip = px(x, at.slide.y + at.slide.h - 0.045 * h as f32 * 0.9);
        let above = px(x, at.slide.y + at.slide.h * 0.5);
        if strip[2] < 0.05 && above[2] > 0.1 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "banner strip {strip:?}, slide above it {above:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// PowerPoint's sound is one, whichever PowerPoint input shows: its strip
/// is heard while any PowerPoint input is on air, quiet while none is, and
/// kept for the input still there when the one it is tied to goes.
#[test]
fn powerpoints_sound_is_heard_with_any_powerpoint_input_on_air() {
    use varda::mixer::{MainSource, MixMode};
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    add_input(&mut app, &deck);
    add_input(&mut app, &deck);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    let state = app.build_engine_state();
    let powerpoint: Vec<(String, String)> = state
        .mixer
        .channels
        .iter()
        .flat_map(|c| {
            c.decks
                .iter()
                .filter(|d| d.presentation.is_some())
                .map(move |d| (c.uuid.clone(), d.uuid.clone()))
        })
        .collect();
    assert_eq!(powerpoint.len(), 2, "two PowerPoint inputs");
    let other = state
        .mixer
        .channels
        .iter()
        .find(|c| c.decks.iter().all(|d| d.presentation.is_none()))
        .map(|c| c.uuid.clone())
        .expect("a channel of something else");
    let CommandResult::OkWithId { uuid: strip } = send_cmd(
        &mut app,
        EngineCommand::AddAppSound {
            app: "com.microsoft.Powerpoint".into(),
            name: "PowerPoint".into(),
            deck_uuid: Some(powerpoint[0].1.clone()),
        },
    ) else {
        panic!("no strip");
    };
    let again = send_cmd(
        &mut app,
        EngineCommand::AddAppSound {
            app: "com.microsoft.Powerpoint".into(),
            name: "PowerPoint".into(),
            deck_uuid: Some(powerpoint[1].1.clone()),
        },
    );
    assert!(
        matches!(&again, CommandResult::OkWithId { uuid } if *uuid == strip),
        "the other PowerPoint input's sound is the same strip: {again:?}"
    );
    let muted = |app: &mut VardaApp| {
        for _ in 0..3 {
            app.render_mixer_frame();
        }
        app.audio_mix()
            .config()
            .strips
            .iter()
            .find(|s| s.uuid == strip)
            .map(|s| s.muted)
    };
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    );
    let take = |app: &mut VardaApp, channel: &str| {
        send_cmd(
            app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channel.to_string()),
                fade: false,
            },
        );
    };
    take(&mut app, &other);
    assert_eq!(muted(&mut app), Some(true), "no PowerPoint input on air");
    take(&mut app, &powerpoint[1].0);
    assert_eq!(
        muted(&mut app),
        Some(false),
        "heard with the PowerPoint input it is not tied to"
    );

    send_cmd(
        &mut app,
        EngineCommand::RemoveDeck {
            deck_uuid: powerpoint[0].1.clone(),
        },
    );
    let tied = app
        .audio_mix()
        .config()
        .inputs
        .iter()
        .find(|i| i.uuid == strip)
        .map(|i| i.deck.clone());
    assert_eq!(
        tied,
        Some(Some(powerpoint[1].1.clone())),
        "kept for the input still there"
    );
}

/// An undo that takes away the PowerPoint input PowerPoint's sound is tied
/// to leaves the sound heard with the PowerPoint input still there.
#[test]
fn powerpoints_sound_outlasts_an_undone_input() {
    use varda::mixer::{MainSource, MixMode};
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    let inputs = |app: &VardaApp| -> Vec<(String, String)> {
        app.build_engine_state()
            .mixer
            .channels
            .iter()
            .flat_map(|c| {
                c.decks
                    .iter()
                    .filter(|d| d.presentation.is_some())
                    .map(move |d| (c.uuid.clone(), d.uuid.clone()))
            })
            .collect()
    };
    add_input(&mut app, &deck);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    let first = inputs(&app)[0].clone();
    add_input(&mut app, &deck);
    let second = inputs(&app)
        .into_iter()
        .find(|i| *i != first)
        .expect("a second PowerPoint input");
    let CommandResult::OkWithId { uuid: strip } = send_cmd(
        &mut app,
        EngineCommand::AddAppSound {
            app: "com.microsoft.Powerpoint".into(),
            name: "PowerPoint".into(),
            deck_uuid: Some(second.1.clone()),
        },
    ) else {
        panic!("no strip");
    };
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(inputs(&app), vec![first.clone()], "the second input undone");
    send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: MixMode::Program,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(first.0.clone()),
            fade: false,
        },
    );
    for _ in 0..3 {
        app.render_mixer_frame();
    }
    let muted = app
        .audio_mix()
        .config()
        .strips
        .iter()
        .find(|s| s.uuid == strip)
        .map(|s| s.muted);
    assert_eq!(muted, Some(false), "heard with the input still there");
}

/// A presenter view laid out — its timer off, its clocks at the foot, its
/// slide wider — keeps that in the show, and an undo takes it back.
#[test]
fn a_presenter_views_layout_is_kept_with_the_show_and_undone() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let show = dir.path().join("show");
    let Some(mut app) = app_in(&show, &ppt) else {
        return;
    };
    add_input(&mut app, &deck);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    let (_, deck_uuid, _) = presentation(&app).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::PresenterView { deck_uuid },
            name: None,
            audio: None,
        },
    );
    let (_, view) = presenter(&app).expect("a presenter view");
    let layout_of = |app: &VardaApp| {
        app.build_engine_state()
            .mixer
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.presenter_of.is_some())
            .and_then(|d| d.presenter_layout)
    };
    assert_eq!(layout_of(&app), Some(PresenterLayout::default()));
    let laid_out = PresenterLayout {
        timer: false,
        clocks_at: ClocksAt::Bottom,
        slide_share: 0.7,
        ..PresenterLayout::default()
    };
    let set = send_cmd(
        &mut app,
        EngineCommand::SetPresenterLayout {
            deck_uuid: view,
            layout: laid_out,
        },
    );
    assert!(matches!(set, CommandResult::Ok), "{set:?}");
    assert_eq!(layout_of(&app), Some(laid_out));
    app.save_workspace(&varda::usecases::ui::UILayoutState::default())
        .expect("save");
    drop(app);

    let Some(mut app) = app_in(&show, &ppt) else {
        return;
    };
    assert!(app.load_workspace().is_ok());
    assert_eq!(layout_of(&app), Some(laid_out), "kept with the show");

    // Undone in the same session: back as it was.
    let (_, view) = presenter(&app).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::SetPresenterLayout {
            deck_uuid: view,
            layout: PresenterLayout::default(),
        },
    );
    assert_eq!(layout_of(&app), Some(PresenterLayout::default()));
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(layout_of(&app), Some(laid_out), "undone");
}

#[test]
fn a_presenter_view_says_when_its_powerpoint_input_is_gone() {
    let dir = tempfile::tempdir().unwrap();
    let deck = write_deck(&dir.path().join("talk.pptx"));
    let ppt = FakePowerPoint::with_slides(3);
    let Some(mut app) = app_in(&dir.path().join("show"), &ppt) else {
        return;
    };
    add_input(&mut app, &deck);
    until(&mut app, |p| p.status == PresentationStatus::Showing);
    let (powerpoint, deck_uuid, _) = presentation(&app).unwrap();
    send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::PresenterView { deck_uuid },
            name: None,
            audio: None,
        },
    );
    send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: powerpoint,
        },
    );
    app.render_mixer_frame();
    assert!(presenter(&app).is_some(), "the presenter view stays");
    assert_eq!(
        presenter_words(&app).map(|w| w.banner),
        Some("The PowerPoint input is gone".to_string())
    );
    // A presenter view of no PowerPoint input is refused.
    let refused = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::PresenterView {
                deck_uuid: "nothing".into(),
            },
            name: None,
            audio: None,
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
}
