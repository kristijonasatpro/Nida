//! Slides inputs: a conference's presentations in one input, in running
//! order — talks added (a Slides input made for them when there is none),
//! started with one press, the talk on stepped by the clicker, what is on
//! kept on until the next talk's picture is there, PowerPoint talks opened
//! ahead and their shows ended when they go off, the talk on kept with the
//! show, and list edits undone without taking the talk on off.
//!
//! PowerPoint is a stand-in (`varda::presentation::fake::FakeDecks`):
//! nothing here opens or drives the real one.

use std::path::Path;
use std::time::{Duration, Instant};

use clap::Parser;
use varda::app::{AppConfig, VardaApp};
use varda::engine::value::presentation::{Go, Screen};
use varda::engine::value::slides::{SlidesSnapshot, Talk, TalkKind, TalkRef, TalkState};
use varda::engine::{CommandResult, EngineCommand};
use varda::mixer::MainSource;
use varda::presentation::fake::FakeDecks;
use varda::presentation::manager::PresentationManager;
use varda::presentation::screens::ScreenSpot;
use varda::usecases::ui::UILayoutState;

mod common;

/// A headless app on `workspace`, never driving PowerPoint: `ppt` stands
/// in for it when given.
fn app_in(workspace: &Path, ppt: Option<&FakeDecks>) -> Option<VardaApp> {
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
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));
    if let Some(ppt) = ppt {
        app.set_presentation_manager(PresentationManager::start(ppt.clone(), ppt.windows(), 1.0));
        app.set_presentation_screens(vec![ScreenSpot {
            display: 1,
            left: 0,
            top: 0,
            width: 1920,
            height: 1080,
            scale: 1.0,
            main: true,
        }]);
    }
    Some(app)
}

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn ok(result: CommandResult) -> CommandResult {
    assert!(
        matches!(result, CommandResult::Ok | CommandResult::OkWithId { .. }),
        "{result:?}"
    );
    result
}

/// A PDF of `pages` pages, each one colour of its own.
fn write_pdf(path: &Path, pages: u32) -> String {
    std::fs::write(path, varda::presentation::pdf::sample_pdf(pages)).unwrap();
    path.to_string_lossy().into_owned()
}

/// A picture all one colour.
fn write_picture(path: &Path, rgb: [u8; 3]) -> String {
    let image = image::RgbaImage::from_pixel(64, 36, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
    image.save(path).unwrap();
    path.to_string_lossy().into_owned()
}

/// A file PowerPoint's stand-in plays: it reads nothing in it.
fn write_deck(path: &Path) -> String {
    std::fs::write(path, b"stand-in deck").unwrap();
    path.to_string_lossy().into_owned()
}

/// The Slides input: its channel, its layer, and what it shows.
fn slides(app: &VardaApp) -> Option<(String, String, SlidesSnapshot)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| ch.decks.iter().map(move |d| (ch, d)))
        .find_map(|(ch, d)| Some((ch.uuid.clone(), d.uuid.clone(), d.slides.clone()?)))
}

/// Frames until the Slides input is as `done` wants it.
fn until(app: &mut VardaApp, done: impl Fn(&SlidesSnapshot) -> bool) -> SlidesSnapshot {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let now = slides(app).map(|(_, _, s)| s);
        if let Some(s) = &now {
            if done(s) {
                return s.clone();
            }
        }
        assert!(Instant::now() < deadline, "never came: {now:#?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The middle pixel of Main, linear RGB.
fn middle_of_main(app: &VardaApp) -> [f32; 3] {
    let tex = app.mixer_ref().composite_texture();
    pixel_of(app, tex, (tex.width() / 2, tex.height() / 2))
}

/// The pixel at `(x, y)` of `tex` — a layer's or the master's, half floats —
/// linear RGB.
fn pixel_of(app: &VardaApp, tex: &wgpu::Texture, (x, y): (u32, u32)) -> [f32; 3] {
    let ctx = app.gpu_context();
    let (w, h) = (tex.width(), tex.height());
    let padded =
        (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("slides readback"),
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    ctx.submit(Some(encoder.finish()));
    let slice = buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().expect("map channel").expect("map ok");
    let data = slice.get_mapped_range().expect("mapped");
    let at = (y.min(h - 1) * padded + x.min(w - 1) * 8) as usize;
    let half = |i: usize| {
        half::f16::from_bits(u16::from_le_bytes([data[at + i * 2], data[at + i * 2 + 1]])).to_f32()
    };
    [half(0), half(1), half(2)]
}

/// An sRGB colour as the master holds it: linear.
fn linear(rgb: [u8; 3]) -> [f32; 3] {
    rgb.map(|c| {
        let c = f32::from(c) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    })
}

fn near(a: [f32; 3], b: [f32; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.03)
}

/// Frames until the middle of Main is `rgb`.
fn until_main_is(app: &mut VardaApp, rgb: [u8; 3]) {
    let want = linear(rgb);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let now = middle_of_main(app);
        if near(now, want) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Main never showed {rgb:?}: {now:?} (want {want:?})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Page `n` of a sample PDF, as `sample_pdf` colours it.
fn page_colour(n: u32) -> [u8; 3] {
    let shade = u8::try_from((n * 40) % 256).unwrap();
    [shade, 255 - shade, 0]
}

fn start(app: &mut VardaApp, to: TalkRef) {
    ok(send_cmd(
        app,
        EngineCommand::StartTalk {
            deck_uuid: None,
            to,
        },
    ));
}

fn step(app: &mut VardaApp, to: Go) {
    ok(send_cmd(
        app,
        EngineCommand::PresentationGo {
            deck_uuid: None,
            to,
        },
    ));
}

#[test]
fn talks_are_started_in_order_and_the_clicker_steps_the_talk_on() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(&dir.path().join("results.pdf"), 3);
    let blue = write_picture(&dir.path().join("break.png"), [0, 0, 255]);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    // Dropped with no Slides input: one is made for them, called Slides.
    let added = ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![pdf.clone(), blue.clone()],
            at: None,
        },
    ));
    let (channel, layer, s) = slides(&app).expect("a Slides input");
    assert!(matches!(added, CommandResult::OkWithId { uuid } if uuid == layer));
    let name = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == channel)
        .map(|c| c.name.clone());
    assert_eq!(name.as_deref(), Some("Slides"));
    assert_eq!(s.talks.len(), 2);
    assert_eq!(s.talks[0].kind, Some(TalkKind::Pdf));
    assert_eq!(s.talks[0].name, "results");
    assert_eq!(s.talks[1].kind, Some(TalkKind::Picture));
    assert_eq!(
        (s.live, s.next),
        (None, Some(0)),
        "nothing on; the first comes next"
    );
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel.clone()),
            fade: false,
        },
    ));

    // Start next: the PDF, at its first page.
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| s.live == Some(0));
    assert_eq!((s.talks[0].position, s.talks[0].shown), (Some(1), Some(3)));
    assert_eq!(s.talks[0].state, TalkState::Live);
    assert_eq!(s.next, Some(1));
    until_main_is(&mut app, page_colour(1));

    // The clicker (no input named) steps the talk on: page 2.
    step(&mut app, Go::Next);
    until(&mut app, |s| s.talks[0].position == Some(2));
    until_main_is(&mut app, page_colour(2));
    // Past the last page it stays on the last.
    step(&mut app, Go::Last);
    step(&mut app, Go::Next);
    let s = until(&mut app, |s| s.talks[0].position == Some(3));
    assert_eq!(s.talks[0].position, Some(3));
    step(&mut app, Go::Slide(2));
    until(&mut app, |s| s.talks[0].position == Some(2));

    // Black over the talk, and the slide again.
    ok(send_cmd(
        &mut app,
        EngineCommand::TogglePresentationScreen {
            deck_uuid: None,
            screen: Screen::Black,
        },
    ));
    until_main_is(&mut app, [0, 0, 0]);
    ok(send_cmd(
        &mut app,
        EngineCommand::TogglePresentationScreen {
            deck_uuid: None,
            screen: Screen::Black,
        },
    ));
    until_main_is(&mut app, page_colour(2));

    // The next talk: the picture. The PDF is the one before it.
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| s.live == Some(1));
    assert_eq!(s.previous, Some(0));
    assert_eq!(s.next, None, "the last talk");
    until_main_is(&mut app, [0, 0, 255]);

    // Back to the PDF for questions: at the page it was left on.
    start(&mut app, TalkRef::Previous);
    let s = until(&mut app, |s| s.live == Some(0));
    assert_eq!(s.talks[0].position, Some(2));
    until_main_is(&mut app, page_colour(2));
}

#[test]
fn a_talk_is_kept_on_until_the_next_talks_picture_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let blue = write_picture(&dir.path().join("a.png"), [0, 0, 255]);
    let pdf = write_pdf(&dir.path().join("b.pdf"), 2);
    let gone = write_picture(&dir.path().join("gone.png"), [255, 0, 0]);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![blue, gone.clone(), pdf],
            at: None,
        },
    ));
    let (channel, layer, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    until_main_is(&mut app, [0, 0, 255]);

    // A talk whose file has gone: said, refused, and what is on stays on.
    std::fs::remove_file(&gone).unwrap();
    let s = until(&mut app, |s| s.talks[1].missing);
    assert_eq!(s.talks[1].state, TalkState::Failed);
    let refused = send_cmd(
        &mut app,
        EngineCommand::StartTalk {
            deck_uuid: Some(layer.clone()),
            to: TalkRef::Next,
        },
    );
    assert!(
        matches!(&refused, CommandResult::Err { message, .. } if message.contains("not there")),
        "{refused:?}"
    );
    for _ in 0..5 {
        app.render_mixer_frame();
    }
    assert!(near(middle_of_main(&app), linear([0, 0, 255])), "still on");
    let s = slides(&app).unwrap().2;
    assert_eq!(s.live, Some(0));

    // A file nothing plays, or not there, is not added.
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "x").unwrap();
    for path in [
        text.to_string_lossy().into_owned(),
        "/no/such/talk.pdf".into(),
    ] {
        let refused = send_cmd(
            &mut app,
            EngineCommand::AddTalks {
                deck_uuid: Some(layer.clone()),
                paths: vec![path],
                at: None,
            },
        );
        assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
    }
    assert_eq!(slides(&app).unwrap().2.talks.len(), 3);

    // Picked to come next by hand: the PDF, though the missing talk is
    // next in the list.
    let pdf_id = slides(&app).unwrap().2.talks[2].id.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::SetNextTalk {
            deck_uuid: layer.clone(),
            talk_id: Some(pdf_id),
        },
    ));
    let s = slides(&app).unwrap().2;
    assert_eq!((s.next, s.next_picked), (Some(2), true));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(2));
    until_main_is(&mut app, page_colour(1));
}

#[test]
fn powerpoint_talks_are_opened_ahead_and_their_shows_end_as_they_go_off() {
    let dir = tempfile::tempdir().unwrap();
    let one = write_deck(&dir.path().join("one.pptx"));
    let two = write_deck(&dir.path().join("two.pptx"));
    let three = write_deck(&dir.path().join("three.pptx"));
    let ppt = FakeDecks::with_slides(5);
    let Some(mut app) = app_in(&dir.path().join("show"), Some(&ppt)) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![one, two, three],
            at: None,
        },
    ));
    // Nothing on: the first talk, next, is opened ahead — its show not
    // played.
    let s = until(&mut app, |s| s.talks[0].state == TalkState::Ready);
    assert_eq!(s.talks[1].state, TalkState::Waiting);
    assert_eq!(ppt.decks(), vec![("one.pptx".into(), false, 0)]);

    // Started: its show plays, captured, on; the second opened ahead.
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| {
        s.live == Some(0) && s.talks[1].state == TalkState::Ready
    });
    assert_eq!(s.talks[0].state, TalkState::Live);
    assert!(ppt.decks().contains(&("one.pptx".into(), true, 1)));
    assert!(ppt.decks().contains(&("two.pptx".into(), false, 0)));

    // The clicker steps the talk on: PowerPoint's show of it.
    step(&mut app, Go::Next);
    until(&mut app, |s| {
        s.talks[0].presentation.as_ref().and_then(|p| p.slide) == Some(2)
    });
    assert!(ppt.log().contains(&"next one.pptx".to_string()));

    // The next talk: its show plays and takes over; the first's show ends,
    // its deck kept open for questions; the third opened ahead.
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| {
        s.live == Some(1)
            && s.talks[0].state == TalkState::Ready
            && s.talks[2].state == TalkState::Ready
    });
    assert_eq!(s.previous, Some(0));
    let decks = ppt.decks();
    assert!(decks.contains(&("one.pptx".into(), false, 2)), "{decks:?}");
    assert!(decks.contains(&("two.pptx".into(), true, 1)), "{decks:?}");
    let log = ppt.log();
    let show_two = log.iter().position(|l| l == "show two.pptx").unwrap();
    let end_one = log.iter().position(|l| l == "end one.pptx").unwrap();
    assert!(
        show_two < end_one,
        "the next show plays before the last ends: {log:?}"
    );

    // Back to the first for questions: played again at its slide.
    start(&mut app, TalkRef::Previous);
    let s = until(&mut app, |s| s.live == Some(0));
    assert_eq!(
        s.talks[0].presentation.as_ref().and_then(|p| p.slide),
        Some(2)
    );

    // A talk two away is closed: the third, then the first again ahead.
    start(&mut app, TalkRef::Id(s.talks[2].id.clone()));
    until(&mut app, |s| s.live == Some(2));
    let s = until(&mut app, |s| {
        s.previous == Some(0) && s.talks[1].state == TalkState::Waiting
    });
    assert_eq!(s.talks[1].state, TalkState::Waiting);
    until(&mut app, |_| {
        !ppt.decks().iter().any(|(f, ..)| f == "two.pptx")
    });

    // PowerPoint hidden by hand: shown again while a show plays.
    ppt.hide();
    let deadline = Instant::now() + Duration::from_secs(10);
    while ppt.hidden().0 {
        assert!(Instant::now() < deadline, "PowerPoint left hidden");
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_talk_on_is_kept_with_the_show_and_list_edits_undo_without_taking_it_off() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(&dir.path().join("results.pdf"), 3);
    let blue = write_picture(&dir.path().join("break.png"), [0, 0, 255]);
    let workspace = dir.path().join("show");
    {
        let Some(mut app) = app_in(&workspace, None) else {
            return;
        };
        ok(send_cmd(
            &mut app,
            EngineCommand::AddTalks {
                deck_uuid: None,
                paths: vec![blue.clone(), pdf.clone()],
                at: None,
            },
        ));
        let (_, layer, s) = slides(&app).unwrap();
        // Renamed and moved: the PDF first.
        let mut talks: Vec<Talk> = s
            .talks
            .iter()
            .map(|t| Talk {
                id: t.id.clone(),
                path: t.path.clone(),
                name: String::new(),
                slide: None,
            })
            .collect();
        talks.swap(0, 1);
        talks[0].name = "Ona — Results".into();
        ok(send_cmd(
            &mut app,
            EngineCommand::SetSlidesTalks {
                deck_uuid: layer.clone(),
                talks: talks.clone(),
            },
        ));
        let s = slides(&app).unwrap().2;
        assert_eq!(s.talks[0].name, "Ona — Results");
        start(&mut app, TalkRef::Next);
        until(&mut app, |s| s.live == Some(0));
        step(&mut app, Go::Next);
        until(&mut app, |s| s.talks[0].position == Some(2));

        // Taken out of the list while on: still on; undone, back in it.
        ok(send_cmd(
            &mut app,
            EngineCommand::SetSlidesTalks {
                deck_uuid: layer.clone(),
                talks: talks[1..].to_vec(),
            },
        ));
        let s = until(&mut app, |s| s.talks.len() == 1);
        assert_eq!(s.live, None, "on, though not in the list");
        assert_eq!(s.next, Some(0), "what came after it comes next");
        send_cmd(&mut app, EngineCommand::Undo);
        let s = until(&mut app, |s| s.talks.len() == 2);
        assert_eq!(s.live, Some(0), "the same talk on, never taken off");
        assert_eq!(s.talks[0].position, Some(2));

        app.render_mixer_frame();
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.flush_saves();
    }
    let saved = std::fs::read_to_string(workspace.join(".varda/scene.json")).unwrap();
    let scene: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let source = scene["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|ch| ch["decks"].as_array().into_iter().flatten())
        .map(|d| d["source"].clone())
        .find(|source| source["type"] == "Slides")
        .expect("the Slides input saved");
    assert_eq!(source["talks"][0]["name"], "Ona — Results");
    assert_eq!(source["talks"][0]["slide"], 2);
    assert_eq!(source["live"], source["talks"][0]["id"]);

    // Opened again: the talk on, on again where it was.
    let Some(mut app) = app_in(&workspace, None) else {
        return;
    };
    let _ = app.load_workspace();
    let s = until(&mut app, |s| s.live == Some(0));
    assert_eq!(s.talks[0].position, Some(2));
    assert_eq!(s.talks[0].name, "Ona — Results");
}

/// A six-second red clip, or `None` without the ffmpeg tool.
fn red_clip(dir: &Path) -> Option<String> {
    let path = dir.join("red.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg("color=c=red:s=64x36:r=25:d=6")
        .args(["-c:v", "mpeg4"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if made {
        Some(path.to_string_lossy().into_owned())
    } else {
        eprintln!("ffmpeg not available — skipping");
        None
    }
}

/// The talks as the engine keeps them, from the snapshot.
fn talks_of(s: &SlidesSnapshot) -> Vec<Talk> {
    s.talks
        .iter()
        .map(|t| Talk {
            id: t.id.clone(),
            path: t.path.clone(),
            name: t.given_name.clone(),
            slide: None,
        })
        .collect()
}

#[test]
fn a_clip_talk_goes_on_with_its_first_frame_never_an_empty_picture() {
    let dir = tempfile::tempdir().unwrap();
    let Some(clip) = red_clip(dir.path()) else {
        return;
    };
    let blue = write_picture(&dir.path().join("blue.png"), [0, 0, 255]);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![blue, clip],
            at: None,
        },
    ));
    let (channel, _, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    until_main_is(&mut app, [0, 0, 255]);
    // Cut to the clip: the first frame it is on, Main shows the clip's red
    // — until then the picture stays on.
    start(&mut app, TalkRef::Next);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let on = middle_of_main(&app);
        if slides(&app).unwrap().2.live == Some(1) {
            assert!(near(on, linear([255, 0, 0])), "the clip came on without its picture: {on:?}");
            break;
        }
        assert!(near(on, linear([0, 0, 255])), "what was on did not stay on: {on:?}");
        assert!(Instant::now() < deadline, "the clip never came on");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_pdf_left_further_on_than_it_now_goes_starts_at_its_last_page() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(&dir.path().join("short.pdf"), 3);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![pdf],
            at: None,
        },
    ));
    let (channel, layer, s) = slides(&app).unwrap();
    // As a show saved at page 20 of a file since cut to three.
    let mut talks = talks_of(&s);
    talks[0].id = "saved020".into();
    talks[0].slide = Some(20);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetSlidesTalks {
            deck_uuid: layer,
            talks,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| s.live == Some(0));
    assert_eq!(s.talks[0].position, Some(3));
    until_main_is(&mut app, page_colour(3));
}

#[test]
fn a_talk_on_out_of_the_list_is_kept_with_the_show_and_a_missing_one_is_not_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(&dir.path().join("results.pdf"), 3);
    let blue = write_picture(&dir.path().join("blue.png"), [0, 0, 255]);
    let workspace = dir.path().join("show");
    {
        let Some(mut app) = app_in(&workspace, None) else {
            return;
        };
        ok(send_cmd(
            &mut app,
            EngineCommand::AddTalks {
                deck_uuid: None,
                paths: vec![blue.clone(), pdf.clone()],
                at: None,
            },
        ));
        let (_, layer, s) = slides(&app).unwrap();
        start(&mut app, TalkRef::Id(s.talks[1].id.clone()));
        until(&mut app, |s| s.live == Some(1));
        // Taken out of the list while on, and moved on while out of it.
        let all = talks_of(&s);
        ok(send_cmd(
            &mut app,
            EngineCommand::SetSlidesTalks {
                deck_uuid: layer.clone(),
                talks: all[..1].to_vec(),
            },
        ));
        step(&mut app, Go::Next);
        step(&mut app, Go::Next);
        // Undone: back in the list where it has got to, page 3 — kept with
        // the show so.
        send_cmd(&mut app, EngineCommand::Undo);
        let s = until(&mut app, |s| s.talks.len() == 2 && s.live == Some(1));
        assert_eq!(s.talks[1].position, Some(3));
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.flush_saves();
        let saved = std::fs::read_to_string(workspace.join(".varda/scene.json")).unwrap();
        let scene: serde_json::Value = serde_json::from_str(&saved).unwrap();
        let listed = scene["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|ch| ch["decks"].as_array().into_iter().flatten())
            .map(|d| d["source"].clone())
            .find(|source| source["type"] == "Slides")
            .unwrap();
        assert_eq!(listed["talks"][1]["slide"], 3, "{listed}");
        // Out of the list again, and saved so.
        ok(send_cmd(
            &mut app,
            EngineCommand::SetSlidesTalks {
                deck_uuid: layer,
                talks: all[..1].to_vec(),
            },
        ));
        until(&mut app, |s| s.talks.len() == 1);
        app.render_mixer_frame();
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.flush_saves();
    }
    // Opened again: the PDF on again, at page 3, though not in the list.
    {
        let Some(mut app) = app_in(&workspace, None) else {
            return;
        };
        let _ = app.load_workspace();
        let (channel, _, _) = slides(&app).unwrap();
        ok(send_cmd(
            &mut app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channel),
                fade: false,
            },
        ));
        until_main_is(&mut app, page_colour(3));
        let s = slides(&app).unwrap().2;
        assert_eq!(s.talks.len(), 1);
        assert_eq!(s.live, None, "on, though not in the list");
        // Its file gone before the next opening: the show still remembers it.
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.flush_saves();
    }
    std::fs::rename(&pdf, dir.path().join("away.pdf")).unwrap();
    std::fs::rename(&blue, dir.path().join("away.png")).unwrap();
    {
        let Some(mut app) = app_in(&workspace, None) else {
            return;
        };
        let _ = app.load_workspace();
        for _ in 0..5 {
            app.render_mixer_frame();
        }
        // Another talk tried, its file gone too: nothing came on, so the
        // talk the show had on is still the one remembered.
        let id = slides(&app).unwrap().2.talks[0].id.clone();
        let refused = send_cmd(
            &mut app,
            EngineCommand::StartTalk {
                deck_uuid: None,
                to: TalkRef::Id(id),
            },
        );
        assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
        app.save_workspace(&UILayoutState::default()).unwrap();
        app.flush_saves();
    }
    let saved = std::fs::read_to_string(workspace.join(".varda/scene.json")).unwrap();
    let scene: serde_json::Value = serde_json::from_str(&saved).unwrap();
    let source = scene["channels"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|ch| ch["decks"].as_array().into_iter().flatten())
        .map(|d| d["source"].clone())
        .find(|source| source["type"] == "Slides")
        .unwrap();
    assert_eq!(source["live"], source["live_talk"]["id"], "{source}");
    assert_eq!(source["live_talk"]["slide"], 3);
}

#[test]
fn a_powerpoint_talk_given_another_file_plays_on_until_started_again() {
    let dir = tempfile::tempdir().unwrap();
    let one = write_deck(&dir.path().join("one.pptx"));
    let v2 = write_deck(&dir.path().join("one-v2.pptx"));
    let ppt = FakeDecks::with_slides(5);
    let Some(mut app) = app_in(&dir.path().join("show"), Some(&ppt)) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![one],
            at: None,
        },
    ));
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| s.live == Some(0));
    let (_, layer, _) = slides(&app).unwrap();
    // The speaker's new version, put in while the talk is on.
    let mut talks = talks_of(&s);
    talks[0].path = v2;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetSlidesTalks {
            deck_uuid: layer.clone(),
            talks,
        },
    ));
    for _ in 0..30 {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(5));
    }
    let log = ppt.log();
    assert!(!log.iter().any(|l| l == "end one.pptx" || l == "close one.pptx"), "{log:?}");
    assert!(!log.iter().any(|l| l == "show one-v2.pptx"), "{log:?}");
    assert_eq!(slides(&app).unwrap().2.live, Some(0), "still on");
    // Started again: the new file plays, then the old show ends.
    start(&mut app, TalkRef::Id(s.talks[0].id.clone()));
    until(&mut app, |_| ppt.log().iter().any(|l| l == "end one.pptx"));
    let log = ppt.log();
    let show_v2 = log.iter().position(|l| l == "show one-v2.pptx").expect("the new file plays");
    let end_old = log.iter().position(|l| l == "end one.pptx").unwrap();
    assert!(show_v2 < end_old, "{log:?}");
    let s = slides(&app).unwrap().2;
    assert_eq!((s.live, s.previous), (Some(0), None), "the same talk, not the one before it");
}

#[test]
fn two_slides_inputs_with_the_same_talks_play_them_each_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let one = write_deck(&dir.path().join("one.pptx"));
    let ppt = FakeDecks::with_slides(5);
    let Some(mut app) = app_in(&dir.path().join("show"), Some(&ppt)) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![one],
            at: None,
        },
    ));
    let (_, first, s) = slides(&app).unwrap();
    // A copy of the input, its talks with the same ids.
    let CommandResult::OkWithId { uuid: channel } = send_cmd(&mut app, EngineCommand::AddChannel)
    else {
        panic!("no channel");
    };
    let CommandResult::OkWithId { uuid: copy } = send_cmd(
        &mut app,
        EngineCommand::AddSlidesDeck {
            channel_uuid: channel,
            paths: Vec::new(),
            settings: None,
        },
    ) else {
        panic!("no copy");
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetSlidesTalks {
            deck_uuid: copy.clone(),
            talks: talks_of(&s),
        },
    ));
    // Started on the copy: it comes on there, while the first only has it
    // ready.
    ok(send_cmd(
        &mut app,
        EngineCommand::StartTalk {
            deck_uuid: Some(copy.clone()),
            to: TalkRef::Next,
        },
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let state = app.build_engine_state();
        let of = |uuid: &str| {
            state
                .mixer
                .channels
                .iter()
                .flat_map(|c| &c.decks)
                .find(|d| d.uuid == uuid)
                .and_then(|d| d.slides.clone())
                .unwrap()
        };
        if of(&copy).live == Some(0) {
            assert_eq!(of(&first).live, None);
            break;
        }
        assert!(Instant::now() < deadline, "the copy's talk never came on");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_web_talk_is_opened_ahead_played_stepped_by_its_keys_and_closed_two_talks_on() {
    use varda::presentation::web::FakeWeb;
    let dir = tempfile::tempdir().unwrap();
    let deck = dir.path().join("ai-deck.html");
    std::fs::write(&deck, "<!doctype html><title>deck</title>").unwrap();
    let deck = deck.to_string_lossy().into_owned();
    let blue = write_picture(&dir.path().join("blue.png"), [0, 0, 255]);
    let red = write_picture(&dir.path().join("red.png"), [255, 0, 0]);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![deck, blue, red],
            at: None,
        },
    ));
    // Next before anything is on: opened ahead, its media paused.
    let s = until(&mut app, |s| s.talks[0].state == TalkState::Ready);
    assert_eq!(s.talks[0].kind, Some(TalkKind::Web));
    assert_eq!(web.pages().len(), 1);
    assert!(!web.pages()[0].2, "not playing while only ready");
    // Started: played, its window captured, on; where it is, from the page.
    start(&mut app, TalkRef::Next);
    let s = until(&mut app, |s| s.live == Some(0) && s.talks[0].shown == Some(5));
    assert_eq!(s.talks[0].position, Some(1));
    // Its media plays from the frame after its picture took over.
    app.render_mixer_frame();
    assert!(web.pages()[0].2, "playing while on");
    // The clicker moves the page.
    step(&mut app, Go::Next);
    until(&mut app, |s| s.talks[0].position == Some(2));
    assert!(web.log().iter().any(|l| l.starts_with("Next ")));
    // The next talk: the page paused and kept, ready for questions.
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(1) && s.previous == Some(0));
    let s = until(&mut app, |_| web.pages().first().is_some_and(|p| !p.2));
    assert_eq!(s.talks[0].state, TalkState::Ready);
    // Two talks on: closed.
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(2));
    until(&mut app, |_| web.pages().is_empty());
    assert!(web.log().iter().any(|l| l.starts_with("unload ")));
    // Back to it by hand: opened again at the slide it was left on.
    let id = slides(&app).unwrap().2.talks[0].id.clone();
    start(&mut app, TalkRef::Id(id));
    let s = until(&mut app, |s| s.live == Some(0));
    assert_eq!(s.talks[0].kind, Some(TalkKind::Web));
}

#[test]
fn a_talk_is_renamed_moved_and_taken_out_by_its_id_and_each_undone() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_picture(&dir.path().join("a.png"), [255, 0, 0]);
    let b = write_picture(&dir.path().join("b.png"), [0, 255, 0]);
    let c = write_picture(&dir.path().join("c.png"), [0, 0, 255]);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![a, b, c],
            at: None,
        },
    ));
    let (_, layer, s) = slides(&app).unwrap();
    let ids: Vec<String> = s.talks.iter().map(|t| t.id.clone()).collect();
    let names = |app: &VardaApp| -> Vec<String> {
        slides(app).unwrap().2.talks.iter().map(|t| t.name.clone()).collect()
    };
    // A talk not there, or a file nothing plays: refused, nothing changed
    // (each still leaves an undo step, as any refused edit does: asked first).
    let refused = send_cmd(
        &mut app,
        EngineCommand::RemoveTalk {
            deck_uuid: layer.clone(),
            talk_id: "nosuch00".into(),
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
    let text = dir.path().join("notes.txt");
    std::fs::write(&text, "x").unwrap();
    let refused = send_cmd(
        &mut app,
        EngineCommand::EditTalk {
            deck_uuid: layer.clone(),
            talk_id: ids[0].clone(),
            name: None,
            path: Some(text.to_string_lossy().into_owned()),
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
    ok(send_cmd(
        &mut app,
        EngineCommand::MoveTalk {
            deck_uuid: layer.clone(),
            talk_id: ids[2].clone(),
            to: 0,
        },
    ));
    assert_eq!(names(&app), ["c", "a", "b"]);
    ok(send_cmd(
        &mut app,
        EngineCommand::EditTalk {
            deck_uuid: layer.clone(),
            talk_id: ids[0].clone(),
            name: Some("  Ona — opening ".into()),
            path: None,
        },
    ));
    assert_eq!(names(&app), ["c", "Ona — opening", "b"]);
    ok(send_cmd(
        &mut app,
        EngineCommand::RemoveTalk {
            deck_uuid: layer.clone(),
            talk_id: ids[1].clone(),
        },
    ));
    assert_eq!(names(&app), ["c", "Ona — opening"]);
    // Each one undone, in turn.
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(names(&app), ["c", "Ona — opening", "b"]);
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(names(&app), ["c", "a", "b"]);
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(names(&app), ["a", "b", "c"]);
}

#[test]
fn a_talk_given_another_file_keeps_each_files_place_apart() {
    let dir = tempfile::tempdir().unwrap();
    let old = write_pdf(&dir.path().join("old.pdf"), 3);
    let new = write_pdf(&dir.path().join("new.pdf"), 3);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![old],
            at: None,
        },
    ));
    let (channel, layer, s) = slides(&app).unwrap();
    let id = s.talks[0].id.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    // The new version put in; the old one, still on, stepped to page 3.
    ok(send_cmd(
        &mut app,
        EngineCommand::EditTalk {
            deck_uuid: layer,
            talk_id: id.clone(),
            name: None,
            path: Some(new.clone()),
        },
    ));
    step(&mut app, Go::Next);
    step(&mut app, Go::Next);
    let s = until(&mut app, |s| s.on.as_ref().and_then(|t| t.position) == Some(3));
    assert_eq!(s.talks[0].path, std::fs::canonicalize(&new).unwrap().to_string_lossy());
    // Started again: the new file, from its own first page — not page 3
    // of the old one.
    start(&mut app, TalkRef::Id(id));
    let s = until(&mut app, |s| {
        s.on.as_ref()
            .is_some_and(|t| t.path.ends_with("new.pdf") && t.position.is_some())
    });
    assert_eq!(s.on.unwrap().position, Some(1));
    until_main_is(&mut app, page_colour(1));
}

#[test]
fn the_manuals_examples_are_commands_nida_takes() {
    let manual = include_str!("../docs/26-presentations.md");
    let mut slides = 0;
    for line in manual.lines() {
        if let Some(json) = line.strip_prefix("POST /api/command") {
            let parsed = serde_json::from_str::<EngineCommand>(json.trim());
            assert!(parsed.is_ok(), "{line}: {parsed:?}");
            if matches!(
                parsed,
                Ok(EngineCommand::StartTalk { .. }
                    | EngineCommand::AddTalks { .. }
                    | EngineCommand::SetNextTalk { .. }
                    | EngineCommand::EditTalk { .. }
                    | EngineCommand::MoveTalk { .. }
                    | EngineCommand::RemoveTalk { .. }
                    | EngineCommand::SetSlidesSettings { .. }
                    | EngineCommand::CheckTalks { .. })
            ) {
                slides += 1;
            }
        } else if let Some(json) = line.strip_prefix("POST /api/inputs") {
            let body: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
            let source = serde_json::from_value::<varda::engine::InputSource>(body["source"].clone());
            assert!(source.is_ok(), "{line}: {source:?}");
        }
    }
    assert_eq!(slides, 11, "the manual shows the Slides commands");
}

#[test]
fn talks_are_looked_over_for_what_may_go_wrong_and_again_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("deck.html");
    std::fs::write(
        &page,
        r#"<script src="https://cdn.example.com/reveal.js"></script><img src="photo.jpg">"#,
    )
    .unwrap();
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![page.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let s = until(&mut app, |s| !s.talks[0].warnings.is_empty());
    assert_eq!(
        s.talks[0].warnings,
        [
            "needs the internet when shown — it loads from cdn.example.com",
            "files it shows are not beside it: photo.jpg",
        ]
    );
    // The picture copied beside the page changes no talk's file: seen once
    // the talks are looked over again.
    std::fs::write(dir.path().join("photo.jpg"), b"a photo").unwrap();
    for _ in 0..5 {
        app.render_mixer_frame();
    }
    assert_eq!(slides(&app).unwrap().2.talks[0].warnings.len(), 2);
    let layer = slides(&app).unwrap().1;
    ok(send_cmd(
        &mut app,
        EngineCommand::CheckTalks {
            deck_uuid: Some(layer),
        },
    ));
    let s = until(&mut app, |s| s.talks[0].warnings.len() == 1);
    assert_eq!(
        s.talks[0].warnings,
        ["needs the internet when shown — it loads from cdn.example.com"]
    );
    let refused = send_cmd(
        &mut app,
        EngineCommand::CheckTalks {
            deck_uuid: Some("nosuch".into()),
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
}

/// Moved on before its file is replaced: the new file still starts at its
/// own first page, not where the old one was left.
#[test]
fn a_talk_replaced_once_moved_on_starts_its_new_file_from_the_beginning() {
    let dir = tempfile::tempdir().unwrap();
    let old = write_pdf(&dir.path().join("old.pdf"), 3);
    let new = write_pdf(&dir.path().join("new.pdf"), 3);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![old],
            at: None,
        },
    ));
    let (channel, layer, s) = slides(&app).unwrap();
    let id = s.talks[0].id.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    step(&mut app, Go::Next);
    step(&mut app, Go::Next);
    until(&mut app, |s| s.on.as_ref().and_then(|t| t.position) == Some(3));
    ok(send_cmd(
        &mut app,
        EngineCommand::EditTalk {
            deck_uuid: layer,
            talk_id: id.clone(),
            name: None,
            path: Some(new),
        },
    ));
    start(&mut app, TalkRef::Id(id));
    let s = until(&mut app, |s| {
        s.on.as_ref()
            .is_some_and(|t| t.path.ends_with("new.pdf") && t.position.is_some())
    });
    assert_eq!(s.on.unwrap().position, Some(1));
    until_main_is(&mut app, page_colour(1));
}

/// A web page moved on, then the talk given a PDF: the PDF says its own
/// page, never the page's slide.
#[test]
fn a_web_talk_given_a_pdf_says_the_pdfs_page_not_the_pages_slide() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let pdf = write_pdf(&dir.path().join("handout.pdf"), 2);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![page.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let (channel, layer, s) = slides(&app).unwrap();
    let id = s.talks[0].id.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    step(&mut app, Go::Next);
    step(&mut app, Go::Next);
    until(&mut app, |s| s.on.as_ref().and_then(|t| t.position) == Some(3));
    ok(send_cmd(
        &mut app,
        EngineCommand::EditTalk {
            deck_uuid: layer,
            talk_id: id.clone(),
            name: None,
            path: Some(pdf),
        },
    ));
    start(&mut app, TalkRef::Id(id));
    let s = until(&mut app, |s| {
        s.on.as_ref()
            .is_some_and(|t| t.path.ends_with("handout.pdf") && t.position.is_some())
    });
    let on = s.on.unwrap();
    assert_eq!((on.position, on.shown), (Some(1), Some(2)));
}

/// The key of the web page open for the Slides layer `layer`.
fn page_of(web: &varda::presentation::web::FakeWeb, layer: &str) -> String {
    web.pages()
        .into_iter()
        .map(|(key, _, _)| key)
        .find(|key| key.starts_with(layer))
        .unwrap_or_else(|| panic!("no page open for {layer}: {:?}", web.pages()))
}

/// A page slow to set itself up is not shown half drawn: the talk on stays
/// on until the page is set up, and only then is its window captured. What
/// a capture or an opening since replaced says, late, is let go.
#[test]
fn a_web_talk_is_shown_only_once_its_page_is_set_up_and_late_answers_are_let_go() {
    use varda::presentation::web::WebEvent;
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(&dir.path().join("first.pdf"), 2);
    let page = dir.path().join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    web.hold_ready();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![pdf, page.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let (channel, layer, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    until_main_is(&mut app, page_colour(1));
    // The page opened ahead, not set up: started, it waits.
    start(&mut app, TalkRef::Next);
    for _ in 0..20 {
        app.render_mixer_frame();
    }
    let s = slides(&app).unwrap().2;
    assert_eq!(s.live, Some(0), "the talk on stays on");
    assert_eq!(s.talks[1].state, TalkState::Starting);
    assert!(!web.log().iter().any(|l| l.starts_with("capture ")), "{:?}", web.log());
    until_main_is(&mut app, page_colour(1));
    let key = page_of(&web, &layer);
    web.set_up(&key);
    until(&mut app, |s| s.live == Some(1));
    assert!(web.log().contains(&format!("capture {key}")));
    // Late answers of what was replaced: a capture asked before, an opening
    // before this one.
    let generation = web.generation_of(&key).unwrap();
    web.push(WebEvent::Failed {
        key: key.clone(),
        generation,
        attempt: Some(0),
        why: "an old capture".into(),
    });
    web.push(WebEvent::Failed {
        key: key.clone(),
        generation: generation - 1,
        attempt: None,
        why: "an old opening".into(),
    });
    web.push(WebEvent::Position {
        key: key.clone(),
        generation: generation - 1,
        at: 4,
        of: 5,
    });
    for _ in 0..10 {
        app.render_mixer_frame();
    }
    let s = slides(&app).unwrap().2;
    assert_eq!(s.live, Some(1));
    assert_eq!(s.talks[1].state, TalkState::Live);
    assert_eq!(s.talks[1].message, None);
    assert_eq!(s.talks[1].position, Some(1));
}

/// A web talk closed two talks on and started again goes back to the slide
/// it was left on.
#[test]
fn a_web_talk_opened_again_goes_back_to_where_it_was_left() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let a = write_picture(&dir.path().join("a.png"), [200, 0, 0]);
    let b = write_picture(&dir.path().join("b.png"), [0, 200, 0]);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![page.to_string_lossy().into_owned(), a, b],
            at: None,
        },
    ));
    let (channel, _, s) = slides(&app).unwrap();
    let id = s.talks[0].id.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    step(&mut app, Go::Next);
    step(&mut app, Go::Next);
    until(&mut app, |s| s.on.as_ref().and_then(|t| t.position) == Some(3));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(1));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(2));
    until(&mut app, |_| web.pages().is_empty());
    start(&mut app, TalkRef::Id(id));
    let s = until(&mut app, |s| s.live == Some(0) && s.on.as_ref().is_some_and(|t| t.position.is_some()));
    assert_eq!(s.on.unwrap().position, Some(3));
}

/// The picture made another size: the page is laid out again at it, where
/// it is, and captured again.
#[test]
fn a_web_page_is_laid_out_again_when_the_picture_changes_size() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![page.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let (channel, layer, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    let key = page_of(&web, &layer);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 1280,
            height: 720,
        },
    ));
    let resized = format!("resize {key} 1280x720");
    let captures_after = |log: &[String]| {
        log.iter()
            .position(|l| *l == resized)
            .is_some_and(|at| log[at..].iter().any(|l| *l == format!("capture {key}")))
    };
    until(&mut app, |_| captures_after(&web.log()));
    assert_eq!(web.log().iter().filter(|l| l.starts_with("load ")).count(), 1, "not opened again");
}

/// Web talks' sound is one strip, as PowerPoint's is, whichever Slides input
/// asks for it; and only the Slides input on air is heard in it — the
/// other's pages are silenced.
#[test]
fn web_talks_sound_is_one_strip_and_only_the_slides_input_on_air_is_heard() {
    let dir = tempfile::tempdir().unwrap();
    let pages: Vec<String> = ["a.html", "b.html"]
        .iter()
        .map(|name| {
            let path = dir.path().join(name);
            std::fs::write(&path, "<html><body>deck</body></html>").unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect();
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    for page in &pages {
        ok(send_cmd(
            &mut app,
            EngineCommand::AddInput {
                source: varda::engine::InputSource::Slides {
                    paths: vec![page.clone()],
                },
                name: None,
                audio: None,
            },
        ));
    }
    let inputs: Vec<(String, String)> = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| {
            ch.decks
                .iter()
                .filter(|d| d.slides.is_some())
                .map(move |d| (ch.uuid.clone(), d.uuid.clone()))
        })
        .collect();
    assert_eq!(inputs.len(), 2);
    let (a, b) = (&inputs[0], &inputs[1]);
    for (_, layer) in [a, b] {
        ok(send_cmd(
            &mut app,
            EngineCommand::StartTalk {
                deck_uuid: Some(layer.clone()),
                to: TalkRef::Next,
            },
        ));
    }
    let strip = |app: &mut VardaApp, layer: &str| match send_cmd(
        app,
        EngineCommand::AddAppSound {
            app: varda::audio::app_sound::WEB_TALKS.into(),
            name: "Web talks".into(),
            deck_uuid: Some(layer.to_string()),
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("{other:?}"),
    };
    let one = strip(&mut app, &a.1);
    assert_eq!(strip(&mut app, &b.1), one, "the strip there already is it");
    for _ in 0..10 {
        app.render_mixer_frame();
    }
    let (key_a, key_b) = (page_of(&web, &a.1), page_of(&web, &b.1));
    let last = |log: &[String], key: &str| {
        log.iter()
            .rev()
            .find(|l| **l == format!("mute {key}") || **l == format!("unmute {key}"))
            .cloned()
    };
    let take = |app: &mut VardaApp, channel: &str| {
        ok(send_cmd(
            app,
            EngineCommand::TakeToMain {
                source: MainSource::Channel(channel.to_string()),
                fade: false,
            },
        ));
        for _ in 0..5 {
            app.render_mixer_frame();
        }
    };
    take(&mut app, &a.0);
    assert_eq!(last(&web.log(), &key_b), Some(format!("mute {key_b}")));
    assert_ne!(last(&web.log(), &key_a), Some(format!("mute {key_a}")));
    take(&mut app, &b.0);
    assert_eq!(last(&web.log(), &key_a), Some(format!("mute {key_a}")));
    assert_eq!(last(&web.log(), &key_b), Some(format!("unmute {key_b}")));
}

/// A web talk started with its page set up, its capture asked: the talk
/// on and a page's id.
fn web_talk_starting(
    web: &varda::presentation::web::FakeWeb,
    dir: &Path,
) -> Option<(VardaApp, String, String)> {
    let pdf = write_pdf(&dir.join("first.pdf"), 2);
    let page = dir.join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let mut app = app_in(&dir.join("show"), None)?;
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![pdf, page.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let (channel, layer, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    start(&mut app, TalkRef::Next);
    let key = page_of(web, &layer);
    Some((app, layer, key))
}

/// Frames for `secs` seconds of wall time.
fn frames_for(app: &mut VardaApp, secs: f32) {
    let until = Instant::now() + Duration::from_secs_f32(secs);
    while Instant::now() < until {
        app.render_mixer_frame();
    }
}

/// A window slow to capture — longer than the three seconds a failed one
/// waits — is waited for, not asked again over it and its answer let go.
#[test]
fn a_page_slow_to_capture_is_waited_for() {
    let dir = tempfile::tempdir().unwrap();
    let web = varda::presentation::web::FakeWeb::default();
    web.hold_captures();
    let Some((mut app, _, key)) = web_talk_starting(&web, dir.path()) else {
        return;
    };
    frames_for(&mut app, 4.0);
    let asked = |web: &varda::presentation::web::FakeWeb| {
        web.log().iter().filter(|l| **l == format!("capture {key}")).count()
    };
    assert_eq!(asked(&web), 1, "{:?}", web.log());
    assert_eq!(slides(&app).unwrap().2.live, Some(0), "the talk on stays on meanwhile");
    web.answer_captures();
    until(&mut app, |s| s.live == Some(1));
    assert_eq!(asked(&web), 1);
}

/// A capture that opened and never sends a picture is asked for again: a
/// talk is never left starting for good.
#[test]
fn a_capture_with_no_picture_is_asked_for_again() {
    let dir = tempfile::tempdir().unwrap();
    let web = varda::presentation::web::FakeWeb::default();
    web.blind_captures(1);
    let Some((mut app, _, key)) = web_talk_starting(&web, dir.path()) else {
        return;
    };
    frames_for(&mut app, 2.0);
    assert_eq!(slides(&app).unwrap().2.live, Some(0), "no picture: not on");
    let s = until(&mut app, |s| s.live == Some(1));
    assert_eq!(s.talks[1].state, TalkState::Live);
    assert_eq!(
        web.log().iter().filter(|l| **l == format!("capture {key}")).count(),
        2,
        "{:?}",
        web.log()
    );
}

/// The talk on, its file replaced and the new one starting: the old file
/// failing does not stop the new one.
#[test]
fn an_old_files_failure_does_not_stop_its_replacement_starting() {
    use varda::presentation::web::WebEvent;
    let dir = tempfile::tempdir().unwrap();
    let old = dir.path().join("old.html");
    let new = dir.path().join("new.html");
    for page in [&old, &new] {
        std::fs::write(page, "<html><body>deck</body></html>").unwrap();
    }
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![old.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let (channel, layer, s) = slides(&app).unwrap();
    let id = s.talks[0].id.clone();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    let old_key = page_of(&web, &layer);
    web.hold_ready();
    ok(send_cmd(
        &mut app,
        EngineCommand::EditTalk {
            deck_uuid: layer.clone(),
            talk_id: id.clone(),
            name: None,
            path: Some(new.to_string_lossy().into_owned()),
        },
    ));
    start(&mut app, TalkRef::Id(id));
    until(&mut app, |s| s.starting == Some(0));
    let new_key = web
        .pages()
        .into_iter()
        .map(|(key, _, _)| key)
        .find(|key| key.starts_with(&layer) && *key != old_key)
        .unwrap();
    web.push(WebEvent::Failed {
        key: old_key.clone(),
        generation: web.generation_of(&old_key).unwrap(),
        attempt: None,
        why: "the old page stopped".into(),
    });
    for _ in 0..10 {
        app.render_mixer_frame();
    }
    let s = slides(&app).unwrap().2;
    assert_eq!(s.starting, Some(0), "the new file still starting");
    assert_eq!(s.talks[0].message, None);
    web.set_up(&new_key);
    let s = until(&mut app, |s| {
        s.live == Some(0) && s.on.as_ref().is_some_and(|t| t.path.ends_with("new.html"))
    });
    assert_eq!(s.talks[0].state, TalkState::Live);
}

/// A web talk brought back plays its video and sound only once its picture
/// shows — not while its window is still being captured, another talk on.
#[test]
fn a_web_talk_coming_is_not_heard_before_it_is_seen() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let pdf = write_pdf(&dir.path().join("between.pdf"), 2);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![page.to_string_lossy().into_owned(), pdf],
            at: None,
        },
    ));
    let (channel, layer, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    let playing = |web: &varda::presentation::web::FakeWeb| {
        web.pages().into_iter().find(|(k, _, _)| k.starts_with(&layer)).is_some_and(|p| p.2)
    };
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    app.render_mixer_frame();
    assert!(playing(&web), "on: heard");
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(1));
    app.render_mixer_frame();
    assert!(!playing(&web), "the talk before, ready for questions: quiet");
    web.hold_captures();
    start(&mut app, TalkRef::Previous);
    frames_for(&mut app, 1.0);
    assert_eq!(slides(&app).unwrap().2.live, Some(1), "the PDF still on");
    assert!(!playing(&web), "coming, its picture not there: not heard");
    web.answer_captures();
    until(&mut app, |s| s.live == Some(0));
    app.render_mixer_frame();
    assert!(playing(&web), "on again: heard");
}

/// The picture made another size and the new capture sending nothing: it
/// is asked for again — the old capture's last picture is not taken for
/// the new one's.
#[test]
fn a_page_resized_whose_new_capture_sends_nothing_is_captured_again() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("deck.html");
    std::fs::write(&page, "<html><body>deck</body></html>").unwrap();
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    let web = varda::presentation::web::FakeWeb::default();
    app.set_web_engine(Box::new(web.clone()));
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![page.to_string_lossy().into_owned()],
            at: None,
        },
    ));
    let (channel, layer, _) = slides(&app).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    let key = page_of(&web, &layer);
    web.blind_captures(1);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetRenderResolution {
            width: 1280,
            height: 720,
        },
    ));
    let resized = format!("resize {key} 1280x720");
    let captures_after = |log: &[String]| {
        log.iter().position(|l| *l == resized).map_or(0, |at| {
            log[at..].iter().filter(|l| **l == format!("capture {key}")).count()
        })
    };
    until(&mut app, |_| captures_after(&web.log()) >= 2);
}

/// PowerPoint talks' slides saved as pictures — asked with no PowerPoint
/// show running, refused with one — give the presenter view the next
/// slide's picture as the talk steps: the page of PowerPoint's next slide.
#[test]
fn a_powerpoint_talks_slide_pictures_give_the_presenter_view_its_next_slide() {
    let dir = tempfile::tempdir().unwrap();
    varda::presentation::pictures::use_folder(&dir.path().join("Slide pictures"));
    let deck = write_deck(&dir.path().join("keynote.pptx"));
    let ppt = FakeDecks::with_slides(5);
    let Some(mut app) = app_in(&dir.path().join("show"), Some(&ppt)) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![deck.clone()],
            at: None,
        },
    ));
    let (_, layer, _) = slides(&app).expect("a Slides input");
    ok(send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: varda::engine::InputSource::PresenterView {
                deck_uuid: layer.clone(),
            },
            name: None,
            audio: None,
        },
    ));
    // The worker runs once the talk is in: then the pictures are asked.
    until(&mut app, |s| s.talks[0].state != TalkState::Failed);
    ok(send_cmd(
        &mut app,
        EngineCommand::MakeSlidePictures {
            deck_uuid: Some(layer.clone()),
        },
    ));
    let made = until(&mut app, |s| s.talks[0].pictures && s.making_pictures == 0);
    assert!(made.talks[0].pictures);
    assert!(ppt.log().iter().any(|l| l.starts_with("save pdf")), "{:?}", ppt.log());

    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    let next_shown = |app: &mut VardaApp, want: Option<u32>| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.render_mixer_frame();
            let now = presenter(app).and_then(|(p, _)| p.next_picture());
            if now == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the next slide's picture never showed {want:?}: {now:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    next_shown(&mut app, Some(2));
    step(&mut app, Go::Next);
    next_shown(&mut app, Some(3));
    // The PDF written again, of 3 pages: counted again — of neither the
    // deck's slides nor those shown, so no page is the next slide's.
    let pdf = varda::presentation::pictures::saved_for(Path::new(&deck)).expect("made");
    write_pdf(&pdf, 3);
    next_shown(&mut app, None);
    // And stays so once the new PDF is drawn from.
    let until = Instant::now() + Duration::from_secs(1);
    while Instant::now() < until {
        app.render_mixer_frame();
        let now = presenter(&app).and_then(|(p, _)| p.next_picture());
        assert_eq!(now, None, "a page of the PDF as counted before");
        std::thread::sleep(Duration::from_millis(5));
    }
    // Its show running: asked again, refused — nothing asked of PowerPoint.
    let refused = send_cmd(
        &mut app,
        EngineCommand::MakeSlidePictures {
            deck_uuid: Some(layer),
        },
    );
    assert!(
        matches!(&refused, CommandResult::Err { message, .. } if message.contains("talk is on")),
        "{refused:?}"
    );
}

/// The presenter view following the Slides input: its layer's presenter.
fn presenter(app: &VardaApp) -> Option<(&varda::deck::presenter::Presenter, &wgpu::Texture)> {
    app.mixer_ref()
        .channels()
        .iter()
        .flat_map(|ch| &ch.decks)
        .find_map(|slot| Some((slot.deck.presenter()?, &slot.deck.texture)))
}

/// A presenter view of a Slides input shows the PDF talk on's next page as
/// the next slide's picture, in its place beside the slide — stepped with
/// the talk, none past its last page. A new one shows the talk's clock and
/// the next slide, not the time of day.
#[test]
fn a_presenter_view_shows_a_pdf_talks_next_page_beside_the_slide() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write_pdf(&dir.path().join("results.pdf"), 3);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![pdf],
            at: None,
        },
    ));
    let (_, layer, _) = slides(&app).expect("a Slides input");
    ok(send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: varda::engine::InputSource::PresenterView { deck_uuid: layer },
            name: None,
            audio: None,
        },
    ));
    let (channel, layout) = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .find_map(|ch| {
            let layout = ch.decks.iter().find_map(|d| d.presenter_layout)?;
            Some((ch.uuid.clone(), layout))
        })
        .expect("a presenter view");
    assert!(layout.timer && layout.next && !layout.clock, "{layout:?}");
    // On Main, where it is drawn.
    ok(send_cmd(
        &mut app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel),
            fade: false,
        },
    ));

    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    let next_shown = |app: &mut VardaApp, want: Option<u32>| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.render_mixer_frame();
            let now = presenter(app).and_then(|(p, _)| p.next_picture());
            if now == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the next slide's picture never showed {want:?}: {now:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    next_shown(&mut app, Some(2));
    // Its place, beside the slide, shows page 2.
    let want = linear(page_colour(2));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        app.render_mixer_frame();
        let (_, tex) = presenter(&app).expect("a presenter view");
        let frame = (tex.width(), tex.height());
        #[allow(clippy::cast_precision_loss)]
        let aspect = frame.0 as f32 / frame.1 as f32;
        let at = varda::deck::presenter::layout_for(frame, aspect, &layout, true)
            .next
            .expect("room for the picture");
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let middle = ((at.x + at.w / 2.0) as u32, (at.y + at.h / 2.0) as u32);
        let now = pixel_of(&app, tex, middle);
        if near(now, want) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the next slide's place never showed page 2: {now:?} (want {want:?})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    step(&mut app, Go::Next);
    until(&mut app, |s| s.talks[0].position == Some(2));
    next_shown(&mut app, Some(3));
    step(&mut app, Go::Next);
    until(&mut app, |s| s.talks[0].position == Some(3));
    next_shown(&mut app, None);
}

/// The presenter's next pictures are the file's as the talk on plays it: a
/// PDF put in the place of one shown before, started as another talk, is
/// opened afresh — its pages, not the old file's. Its Slides input taken
/// away, the presenter shows no next picture.
#[test]
fn a_presenter_views_next_pictures_follow_the_file_the_talk_plays() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("results.pdf");
    let pdf = write_pdf(&path, 3);
    let Some(mut app) = app_in(&dir.path().join("show"), None) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::AddTalks {
            deck_uuid: None,
            paths: vec![pdf.clone(), pdf.clone()],
            at: None,
        },
    ));
    let (slides_channel, layer, s) = slides(&app).expect("a Slides input");
    assert_eq!(s.talks.len(), 2);
    ok(send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: varda::engine::InputSource::PresenterView { deck_uuid: layer },
            name: None,
            audio: None,
        },
    ));
    let next_shown = |app: &mut VardaApp, want: Option<u32>| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            app.render_mixer_frame();
            let now = presenter(app).and_then(|(p, _)| p.next_picture());
            if now == want {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the next slide's picture never showed {want:?}: {now:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(0));
    next_shown(&mut app, Some(2));

    // Five pages now, in the same place; the second talk plays them.
    std::thread::sleep(Duration::from_millis(20));
    write_pdf(&path, 5);
    start(&mut app, TalkRef::Next);
    until(&mut app, |s| s.live == Some(1));
    step(&mut app, Go::Slide(4));
    until(&mut app, |s| s.talks[1].position == Some(4));
    next_shown(&mut app, Some(5));

    ok(send_cmd(
        &mut app,
        EngineCommand::RemoveChannel {
            channel_uuid: slides_channel,
        },
    ));
    next_shown(&mut app, None);
}
