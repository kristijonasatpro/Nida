//! E2E snapshot tests for visual regression.
//!
//! Render the UI (or specific panels) and compare against reference images.
//! Reference images are stored in `tests/snapshots/` and tracked in git.
//! `.diff.png`, `.new.png` and `.old.png` files are git-ignored.
//!
//! These require wgpu — they will be skipped if no GPU/software renderer is available.
//!
//! **These do not run in CI.** The reference PNGs are generated on a developer's
//! GPU (Metal), while CI renders on lavapipe, and the two disagree by a couple of
//! 8-bit steps on a handful of pixels. Reconciling that needs either a per-pixel
//! tolerance wide enough to hide real regressions, or lavapipe-generated goldens
//! that can no longer be regenerated locally. Neither is worth it, so CI sets
//! `VARDA_SKIP_GOLDEN_SNAPSHOTS` and these stay a local-only check. Every other
//! GPU suite does run in CI on lavapipe.

use std::rc::Rc;

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use varda::usecases::ui::panels::render_ui;
use varda::usecases::ui::{UIActions, UIData};

/// Logical-point size of the simulated window. `pixels_per_point` is 1.0, so
/// this is both the point size and the PNG's pixel size.
///
/// 1920×1080 rather than a smaller box because the panels are sized in points:
/// at 1280×720 the top bar overlapped its own tonemap label, the bottom panel
/// clipped, and "Drag effects here" wrapped to one letter per line. None of that
/// reproduces at a realistic maximized-window size, so the smaller box was
/// pinning layout defects that no user would ever see.
const SIZE: egui::Vec2 = egui::vec2(1920.0, 1080.0);

/// Build a sized harness, or `None` when golden comparison is disabled.
///
/// The opt-out lives here rather than in each test so a snapshot test cannot be
/// added that bypasses it — they all have to come through this constructor.
fn snapshot_harness(data: UIData) -> Option<Harness<'static, UIActions>> {
    let mut harness = unsettled_snapshot_harness(data)?;
    harness.run();
    Some(harness)
}

/// [`snapshot_harness`] for a UI that never settles — a live stream's clock
/// repaints it every half second — so the caller steps it instead.
fn unsettled_snapshot_harness(data: UIData) -> Option<Harness<'static, UIActions>> {
    if std::env::var_os("VARDA_SKIP_GOLDEN_SNAPSHOTS").is_some() {
        eprintln!("VARDA_SKIP_GOLDEN_SNAPSHOTS set — skipping golden comparison");
        return None;
    }
    let data = Rc::new(data);
    Some(Harness::builder().with_size(SIZE).build_ui_state(
        move |ui, actions: &mut UIActions| {
            *actions = render_ui(ui, &data);
        },
        UIActions::new(),
    ))
}

// ── Full UI layout ──────────────────────────────────────────────────

#[test]
fn snapshot_full_ui_default() {
    let Some(mut harness) = snapshot_harness(UIData::test_fixture()) else {
        return;
    };
    harness.snapshot("full_ui_default");
}

/// A notice shows whole, wrapped, its toast as tall as it needs: cut to one
/// line, what happened and what to do about it reached only the log.
#[test]
fn snapshot_toasts_show_the_whole_message() {
    let mut data = UIData::test_fixture();
    data.notifications = vec![
        varda::usecases::ui::NotificationUI {
            id: 1,
            level: varda::notifications::NotificationLevel::Warning,
            message: "Output 'LED' is on a 50 Hz screen: at 60 fps its frames cannot all stay up \
                      equally long, so motion judders. Set that screen to 60 Hz or a multiple \
                      (System Settings → Displays → Refresh Rate), or — where the screen's rate \
                      is fixed, as an LED processor's often is — run the show at 50 fps (Target \
                      FPS, in the top bar)."
                .to_string(),
            progress: 0.2,
        },
        varda::usecases::ui::NotificationUI {
            id: 2,
            level: varda::notifications::NotificationLevel::Info,
            message: "Saved".to_string(),
            progress: 0.5,
        },
    ];
    // Toasts repaint every frame for their countdown: step, don't settle.
    let Some(mut harness) = unsettled_snapshot_harness(data) else {
        return;
    };
    harness.run_steps(3);
    harness.snapshot("toasts_show_the_whole_message");
}

/// The Broadcast layout: Preview and Program across the top with the take
/// buttons between them, a tile per channel below, the audio mixer beside.
#[test]
fn snapshot_full_ui_broadcast_layout() {
    let mut data = UIData::test_fixture();
    data.screen = varda::usecases::ui::screen_layout::ScreenLayout::builtin(
        varda::usecases::ui::screen_layout::Builtin::Broadcast,
    );
    data.main.mode = varda::mixer::MixMode::Program;
    data.main.program = varda::mixer::MainSource::Channel(data.channels[0].uuid.clone());
    data.selected_deck = None;
    data.selected_channel = Some(1);
    // A third channel folded to a strip by right-clicking its name bar.
    let mut folded = data.channels[1].clone();
    folded.ch_idx = 2;
    folded.uuid = "cc000003".into();
    folded.name = "Lower third".into();
    data.collapsed_channels = vec![folded.uuid.clone()];
    data.channels.push(folded);
    data.channel_count = 3;
    data.audio_mix = broadcast_audio_mix(&data);
    // Preview's input holds a clip, 20 s into a minute, trimmed at 50 s:
    // its transport shows under the picture.
    data.channels[1].decks[0].video_playback = Some(varda::usecases::ui::VideoPlaybackUI {
        playing: true,
        position: 20.0,
        duration: 60.0,
        speed: 1.0,
        effective_speed: 1.0,
        position_offset: 0.0,
        loop_mode: varda::video::LoopMode::Loop,
        in_point: 4.0,
        out_point: 50.0,
        frame_rate: 25.0,
        transport_sync: varda::video::DeckTransportSync::default(),
        hardware_decode: true,
        auto: varda::engine::value::video::AutoPlayback::default(),
        sound_only: false,
    });
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("full_ui_broadcast_layout");
}

/// Add Input from the Broadcast layout's Inputs toolbar, on Camera: a capture
/// card chosen, at its best format, with its own sound.
#[test]
fn snapshot_add_input_window() {
    use varda::engine::value::camera::CameraMode;
    let mut data = UIData::test_fixture();
    data.screen = varda::usecases::ui::screen_layout::ScreenLayout::builtin(
        varda::usecases::ui::screen_layout::Builtin::Broadcast,
    );
    data.main.mode = varda::mixer::MixMode::Program;
    data.cameras = vec![
        ("FaceTime HD Camera".into(), 0),
        ("Blackmagic Design".into(), 1),
    ];
    let mode = |width, height, fps| CameraMode { width, height, fps };
    data.camera_modes = vec![
        (0, vec![mode(1920, 1080, 30), mode(1280, 720, 30)]),
        (
            1,
            vec![
                mode(1920, 1080, 60),
                mode(1920, 1080, 50),
                mode(1920, 1080, 30),
                mode(1280, 720, 60),
            ],
        ),
    ];
    data.audio_mix.input_devices = vec![
        varda::audio_mix::AudioInputDevice {
            name: "Scarlett Solo 4th Gen".into(),
            channels: 2,
        },
        varda::audio_mix::AudioInputDevice {
            name: "Blackmagic Design".into(),
            channels: 2,
        },
    ];
    data.ndi_available = true;
    data.ndi_sources = vec!["LAPTOP (Screen 1)".into()];
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.get_by_label("Add Input").click();
    harness.run();
    harness.get_by_label("Camera").click();
    harness.run();
    harness.get_by_label("Blackmagic Design").click();
    harness.run();
    harness.hover_at(egui::pos2(5.0, 5.0));
    harness.run();
    harness.snapshot("add_input_window");
}

/// A playlist on Ch B's tile, playing, and its window open.
#[test]
fn snapshot_playlist_window() {
    use varda::engine::value::playlist::{
        PlaylistItem, PlaylistSettings, PlaylistSnapshot, PlaylistTransition,
    };
    let mut data = UIData::test_fixture();
    data.screen = varda::usecases::ui::screen_layout::ScreenLayout::builtin(
        varda::usecases::ui::screen_layout::Builtin::Broadcast,
    );
    data.main.mode = varda::mixer::MixMode::Program;
    data.channels[1].name = "Slides".into();
    data.channels[1].decks[0].playlist = Some(PlaylistSnapshot {
        items: [
            "/Show/intro.mp4",
            "/Show/01 welcome.png",
            "/Show/02 agenda.png",
            "/Show/03 speakers.jpg",
            "/Show/moved away.png",
            "/Show/walk-in music.mp3",
        ]
        .into_iter()
        .map(PlaylistItem::new)
        .collect(),
        settings: PlaylistSettings {
            transition: PlaylistTransition::Slide,
            ..PlaylistSettings::default()
        },
        playing: true,
        shown: Some(2),
        coming: None,
        next_ready: Some(3),
        failed: vec![4],
        remaining: Some(3.4),
    });
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness
        .get_all_by_label("☰")
        .next()
        .expect("the tile's list button")
        .click();
    harness.run();
    harness.hover_at(egui::pos2(5.0, 5.0));
    harness.run();
    harness.snapshot("playlist_window");
}

/// A count down on Ch B's tile, in its warning seconds and running, its
/// control panel in the dock and its settings window open.
#[test]
fn snapshot_timer_panel_and_window() {
    use varda::engine::value::timer::{TimerSettings, TimerSnapshot, TimerState};
    use varda::usecases::ui::screen_layout::{add_pane, Builtin, Pane, ScreenLayout};
    let mut data = UIData::test_fixture();
    data.screen = ScreenLayout::builtin(Builtin::Broadcast);
    add_pane(&mut data.screen.dock, Pane::Timer(7));
    data.main.mode = varda::mixer::MixMode::Program;
    data.channels[1].name = "Break".into();
    let mut settings = TimerSettings {
        duration_secs: 15 * 60,
        ..TimerSettings::default()
    };
    settings.look.caption = "Back in".into();
    settings.look.box_opacity = 0.6;
    data.channels[1].decks[0].timer = Some(TimerSnapshot {
        text: "0:48".into(),
        state: TimerState::Warn,
        remaining: Some(47.3),
        elapsed: 852.7,
        running: true,
        panel: 7,
        settings,
    });
    // The font list is read on a thread; the window waits for it.
    let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while varda::deck::svg::font_families().is_none() {
        assert!(std::time::Instant::now() < until, "fonts never listed");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness
        .get_all_by_label("⚙")
        .next()
        .expect("the tile's settings button")
        .click();
    harness.run();
    harness.hover_at(egui::pos2(5.0, 5.0));
    harness.run();
    harness.snapshot("timer_panel_and_window");
}

/// The Rundown panel in the Broadcast layout — a step taken, the next going on by
/// itself, one that cannot be taken — and a step's window open over it.
#[test]
fn snapshot_rundown_panel_and_step_window() {
    use varda::bus::BusInputRef;
    use varda::engine::{RundownSnapshot, RundownStepSnapshot, RundownWaitSnapshot};
    use varda::mixer::MainSource;
    use varda::rundown::{OverlaySet, PlayAction, PlaySet, ScreenSet, Step};
    use varda::usecases::ui::screen_layout::{Builtin, Pane, ScreenLayout};
    let mut data = UIData::test_fixture();
    data.screen = ScreenLayout::builtin(Builtin::Broadcast);
    // The Rundown is a tab behind the Audio Mixer: brought to the front.
    let tab = data.screen.dock.find_tab(&Pane::Rundown).expect("the tab");
    data.screen.dock.set_active_tab(tab).expect("brought to the front");
    data.main.mode = varda::mixer::MixMode::Program;
    data.channels[0].name = "Camera".into();
    data.channels[1].name = "Slides".into();
    // A wall in three parts, and a title over Main.
    let tv = data.buses[0].clone();
    data.buses = ["Left", "Middle", "Right"]
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let mut bus = tv.clone();
            bus.bus.uuid = format!("bus0000{}", i + 1);
            bus.bus.name = name.into();
            bus
        })
        .collect();
    let mut title = data.channels[1].clone();
    title.uuid = "cc000001".into();
    title.name = "Title".into();
    title.overlay = true;
    title.decks.clear();
    data.channels.push(title);
    let (camera, slides) = (data.channels[0].uuid.clone(), data.channels[1].uuid.clone());
    let sting = data.channels[0].decks[0].uuid.clone();
    let wall = |inputs: [&str; 3]| -> Vec<ScreenSet> {
        inputs
            .into_iter()
            .enumerate()
            .map(|(i, input)| ScreenSet {
                bus: format!("bus0000{}", i + 1),
                input: BusInputRef::Channel(input.to_string()),
            })
            .collect()
    };
    let step = |uuid: &str, name: &str, sets: Step| RundownStepSnapshot {
        step: Step {
            uuid: uuid.into(),
            name: name.into(),
            ..sets
        },
        missing: vec![],
        fading: vec![],
        shown: false,
    };
    let mut gone = step(
        "s4",
        "D Panel",
        Step {
            screens: wall([&camera, "nosuchid", &camera]),
            ..Step::new("")
        },
    );
    gone.missing = vec!["Middle's input".into()];
    data.rundown = RundownSnapshot {
        fade_secs: 1.5,
        steps: vec![
            step(
                "s1",
                "A Opening",
                Step {
                    screens: wall([&slides, &slides, &slides]),
                    ..Step::new("")
                },
            ),
            step(
                "s2",
                "B Walk-on",
                Step {
                    screens: wall([&slides, &camera, &slides]),
                    overlays: vec![OverlaySet {
                        channel: "cc000001".into(),
                        on: true,
                    }],
                    plays: vec![PlaySet {
                        deck: sting,
                        action: PlayAction::Start,
                    }],
                    then_secs: Some(4.0),
                    ..Step::new("")
                },
            ),
            step(
                "s3",
                "C Talk",
                Step {
                    main: Some(MainSource::Channel(camera.clone())),
                    ..Step::new("")
                },
            ),
            gone,
            step("s5", "E", Step::new("")),
        ],
        last: Some("s2".into()),
        next: Some("s3".into()),
        wait: Some(RundownWaitSnapshot {
            from: "s2".into(),
            to: "s3".into(),
            secs_left: 2.6,
            fade: true,
        }),
    };
    // The wait repaints the panel, so it never settles: stepped instead.
    let Some(mut harness) = unsettled_snapshot_harness(data) else {
        return;
    };
    harness.run_steps(4);
    harness.get_by_label("B Walk-on").click_secondary();
    harness.run_steps(4);
    harness.get_by_label("Edit…").click();
    harness.run_steps(4);
    harness.hover_at(egui::pos2(5.0, 5.0));
    harness.run_steps(4);
    harness.snapshot("rundown_panel_and_step_window");
}

/// Master and Bus A; a clip on air, a clip waiting, a microphone.
fn broadcast_audio_mix(data: &UIData) -> varda::audio_mix::AudioMixSnapshot {
    use varda::audio_mix::{
        AudioBusConfig, AudioBusSnapshot, AudioMixSnapshot, AudioSourceKind, AudioSourceSnapshot,
    };
    let bus = |uuid: &str, name: &str, gain_db: f32, meter: f32| AudioBusSnapshot {
        dynamics: varda::audio_mix::DynamicsSnapshot::default(),
        config: AudioBusConfig {
            compressor: varda::audio_mix::CompressorConfig::default(),
            uuid: uuid.into(),
            name: name.into(),
            gain_db,
            muted: false,
            delay_ms: 0.0,
            device: Some("BMD HDMI".into()),
            first_channel: 0,
        },
        device_ok: Some(true),
        meter_db: [meter, meter - 2.0],
        peak_db: [meter,meter-2.0],
    };
    let clip =
        |deck: &str, name: &str, on_air: f32, gain_db: f32, meter: f32| AudioSourceSnapshot {
            dynamics: varda::audio_mix::DynamicsSnapshot::default(),
            uuid: deck.into(),
            name: name.into(),
            kind: AudioSourceKind::Clip {
                deck_uuid: deck.into(),
            },
            gain_db,
            muted: on_air == 0.0,
            sends: vec!["amaster1".into()],
            auto: true,
            follow: None,
            balance: 0.0,
            delay_ms: 0.0,
            on_air,
            gate: 1.0,
            solo: false,
            live: true,
            meter_db: [meter, meter - 1.5],
            peak_db: [meter,meter-1.5],
            refused: None,
        };
    let deck = |ch: usize| data.channels[ch].decks[0].uuid.clone();
    let mut mic = clip("mic00001", "Lectern", 0.0, -4.0, -14.0);
    mic.kind = AudioSourceKind::Input {
        device: "Scarlett Solo 4th Gen".into(),
        left: 0,
        right: 0,
        deck: None,
        app: None,
    };
    mic.auto = false;
    mic.muted = false;
    mic.solo = true;
    mic.balance = -0.3;
    mic.sends = vec!["amaster1".into(), "abusa001".into()];
    AudioMixSnapshot {
        buses: vec![
            bus("amaster1", "Master", 0.0, -9.0),
            bus("abusa001", "Bus A", -6.0, -20.0),
        ],
        sources: vec![
            clip(&deck(0), "intro.mp4", 1.0, 0.0, -12.0),
            clip(&deck(1), "walk-in.mp3", 0.0, -10.0, -96.0),
            mic,
        ],
        monitor: varda::audio_mix::AudioMonitorSnapshot {
            soloing: true,
            ..Default::default()
        },
        output_devices: vec!["BMD HDMI".into()],
        input_devices: vec![],
        apps: vec![],
    }
}

/// The VJ layout: a row per layer with the top layer on top, the output
/// top right, properties and the browser below.
#[test]
fn snapshot_full_ui_vj_layout() {
    let mut data = UIData::test_fixture();
    data.screen = varda::usecases::ui::screen_layout::ScreenLayout::builtin(
        varda::usecases::ui::screen_layout::Builtin::Vj,
    );
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("full_ui_vj_layout");
}

/// Input settings, as vMix opens them on a double-click: the channel's result
/// and its layers down the left, the picked one's preview and tabs on the
/// right.
#[test]
fn snapshot_input_settings_window() {
    if std::env::var_os("VARDA_SKIP_GOLDEN_SNAPSHOTS").is_some() {
        return;
    }
    let mut data = UIData::test_fixture();
    data.screen = varda::usecases::ui::screen_layout::ScreenLayout::builtin(
        varda::usecases::ui::screen_layout::Builtin::Broadcast,
    );
    data.selected_deck = None;
    let data = Rc::new(data);
    let mut harness = Harness::builder()
        .with_size(SIZE)
        .with_step_dt(1.0 / 60.0)
        .build_ui_state(
            move |ui, actions: &mut UIActions| {
                *actions = render_ui(ui, &data);
            },
            UIActions::new(),
        );
    harness.run();
    let at = harness.get_by_label("Ch A tile").rect().center();
    harness.hover_at(at);
    harness.run();
    for _ in 0..2 {
        harness.drag_at(at);
        harness.run();
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
    }
    // Away from the window, so nothing in it shows hovered.
    harness.hover_at(egui::pos2(5.0, 5.0));
    harness.run();
    harness.snapshot("input_settings_window");
}

/// Arrangement mode swaps the channel view and nothing else: the library,
/// properties and the right-hand panels must survive the mode switch intact. That is the
/// whole claim of /spec/arrangement.md § UI, and a picture is the only way to
/// check it.
#[test]
fn snapshot_full_ui_arrangement_mode() {
    let mut data = UIData::test_fixture();
    let deck_uuid = data.channels[0].decks[0].uuid.clone();
    let mut lane = varda::arrangement::LaneConfig::new(&deck_uuid);
    lane.regions.push(varda::arrangement::RegionConfig {
        start: 2.0,
        end: 14.0,
        fade_in: 1.5,
        fade_out: 3.0,
    });
    let config = varda::arrangement::ArrangementConfig {
        lanes: vec![lane],
        // A cue in the picture, because "yellow dot on the ruler, dashed line
        // down the lanes" is a claim only a picture can check.
        cues: vec![varda::arrangement::Cue {
            uuid: "cue00001".to_string(),
            name: "Drop".to_string(),
            at: 10.0,
        }],
        ..Default::default()
    };
    data.arrangement = Some(varda::engine::types::ArrangementSnapshot {
        duration: config.duration(),
        config,
        engaged: true,
        overridden_params: vec![],
    });
    data.arrangement_mode_open = true;
    data.transport.has_run = true;
    data.transport.position = 6.0;

    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("full_ui_arrangement_mode");
}

/// The same show back at the desk. "Two buttons wide, under the mixer and the
/// macros, without crowding either" is a claim only a picture can check.
#[test]
fn snapshot_full_ui_cue_bank() {
    let mut data = UIData::test_fixture();
    let config = varda::arrangement::ArrangementConfig {
        cues: vec![
            varda::arrangement::Cue {
                uuid: "cue00001".to_string(),
                name: "Intro".to_string(),
                at: 0.0,
            },
            varda::arrangement::Cue {
                uuid: "cue00002".to_string(),
                name: "Drop".to_string(),
                at: 10.0,
            },
            varda::arrangement::Cue {
                uuid: "cue00003".to_string(),
                name: "Breakdown".to_string(),
                at: 24.0,
            },
        ],
        ..Default::default()
    };
    data.arrangement = Some(varda::engine::types::ArrangementSnapshot {
        duration: config.duration(),
        config,
        engaged: false,
        overridden_params: vec![],
    });

    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("full_ui_cue_bank");
}

// ── Bottom bar contexts ─────────────────────────────────────────────

#[test]
fn snapshot_bottom_bar_deck_detail() {
    let mut data = UIData::test_fixture();
    data.selected_deck = Some((0, 0));
    data.selected_channel = None;
    data.selected_master = false;
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("bottom_bar_deck_detail");
}

/// The video playback column, with a modulator on the speed slider and another
/// carrying the playhead away from its anchor.
///
/// The shared fixture has no video deck, so without this the whole playback
/// column (and every `〰` dropdown and ghost on it) would render in no snapshot
/// at all. See /spec/video-playback-modulation.md § UI.
#[test]
fn snapshot_bottom_bar_video_playback() {
    let mut data = UIData::test_fixture();
    data.selected_deck = Some((0, 0));
    data.selected_channel = None;
    data.selected_master = false;

    let uuid = data.channels[0].decks[0].uuid.clone();
    data.channels[0].decks[0].video_playback = Some(varda::usecases::ui::VideoPlaybackUI {
        playing: true,
        position: 4.25,
        duration: 30.0,
        // Set point and live rate differ, which is the whole point of the ghost:
        // a golden with them equal would not show whether it is drawn.
        speed: 1.0,
        effective_speed: 2.4,
        // Non-zero so the scrub bar's ghost sits away from the handle, where a
        // golden can tell the two apart.
        position_offset: -3.0,
        loop_mode: varda::video::LoopMode::Loop,
        in_point: 0.0,
        out_point: 0.0,
        frame_rate: 30.0,
        transport_sync: varda::video::DeckTransportSync::default(),
        hardware_decode: true,
        auto: varda::engine::value::video::AutoPlayback::default(),
        sound_only: false,
    });
    for name in [
        varda::video::modulation::SPEED,
        varda::video::modulation::POSITION,
    ] {
        data.modulation_assignments.insert(
            format!("deck_{uuid}:{name}"),
            vec![varda::usecases::ui::ModAssignmentUI {
                source_id: "mod00001".to_string(),
                amount: 0.5,
            }],
        );
    }

    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("bottom_bar_video_playback");
}

#[test]
fn snapshot_bottom_bar_channel_fx() {
    let mut data = UIData::test_fixture();
    data.selected_deck = None;
    data.selected_channel = Some(0);
    data.selected_master = false;
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("bottom_bar_channel_fx");
}

#[test]
fn snapshot_bottom_bar_master_fx() {
    let mut data = UIData::test_fixture();
    data.selected_deck = None;
    data.selected_channel = None;
    data.selected_master = true;
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("bottom_bar_master_fx");
}

#[test]
fn snapshot_bottom_bar_nothing_selected() {
    let mut data = UIData::test_fixture();
    data.selected_deck = None;
    data.selected_channel = None;
    data.selected_master = false;
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("bottom_bar_nothing_selected");
}

// ── Stream ──────────────────────────────────────────────────────────

/// The STREAM window live, with one destination sending, one reconnecting
/// and one entered wrong: the three looks an operator has to tell apart at a
/// glance. The button above is mid-blink (yellow), since not all is well.
#[test]
fn snapshot_stream_window_live() {
    use egui_kittest::kittest::Queryable;
    use varda::streaming::{DestinationState, StreamDestination};
    let mut data = UIData::test_fixture();
    let dest = |name: &str, url: &str, key: &str| StreamDestination {
        enabled: true,
        name: name.into(),
        url: url.into(),
        key: key.into(),
        ..StreamDestination::default()
    };
    data.stream_settings.destinations[0] = dest(
        "Main stream",
        "rtmp://ingest.example.com/live2",
        "abcd-efgh-ijkl",
    );
    data.stream_settings.destinations[1] = StreamDestination {
        username: "operator".into(),
        password: "secret".into(),
        ..dest("Venue", "rtmp://stream.example.com/live", "event1")
    };
    data.stream_settings.destinations[2] = dest("CDN", "rtmps://live.example.com:443/live/", "");
    data.stream_status.live = true;
    data.stream_status.live_secs = 754.0;
    let d = &mut data.stream_status.destinations;
    d[0].state = DestinationState::Live;
    d[0].kbps = 5980.0;
    d[1].state = DestinationState::Reconnecting;
    d[1].message =
        Some("The server refused the login — check the stream key, username and password".into());
    d[1].reconnects = 3;
    d[2].state = DestinationState::Invalid;
    d[2].message = Some("Enter the stream key".into());
    let Some(mut harness) = unsettled_snapshot_harness(data) else {
        return;
    };
    harness.run_steps(2);
    harness.get_by_label_contains("STREAM").click();
    // Live, the button keeps repainting (clock, blink): step, don't settle.
    harness.run_steps(3);
    harness.snapshot("stream_window_live");
}

// ── Main desk ───────────────────────────────────────────────────────

/// Main mode: the toolbar, channels in one wide row, Ch B on Main and Ch A
/// next (its deck is selected).
#[test]
fn snapshot_main_desk() {
    let mut data = UIData::test_fixture();
    data.main.mode = varda::mixer::MixMode::Program;
    data.main.fade_secs = 1.0;
    data.main.program = varda::mixer::MainSource::Channel("cb000001".into());
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    harness.snapshot("main_desk");
}

/// An output carrying an LED feed, for the LED Screens snapshot.
fn led_output(layout: varda::engine::value::led::LedLayout) -> varda::usecases::ui::OutputUI {
    varda::usecases::ui::OutputUI {
        uuid: "ledout01".into(),
        name: "LED".into(),
        target: varda::renderer::context::OutputTarget::Windowed,
        target_label: "Windowed".into(),
        is_windowed: true,
        is_active: true,
        active_duration: std::time::Duration::ZERO,
        surface_assignments: vec![],
        calibration_mode: varda::renderer::context::CalibrationMode::Off,
        edge_blend_mode: varda::renderer::edge_blend::EdgeBlendMode::default(),
        edge_blend: varda::renderer::edge_blend::EdgeBlendConfig::default(),
        rotation: varda::renderer::context::OutputRotation::default(),
        presentation_request: varda::engine::value::render::PresentationRequest::default(),
        resolved_presentation: varda::engine::value::render::ResolvedPresentation::default(),
        audio_passthrough: None,
        delivery: None,
        preview_width: 1920,
        preview_height: 1080,
        source: None,
        audio_bus: None,
        led: Some(layout),
    }
}

/// LED Screens, as Resolume's Advanced Output: a wall and two cubes on one
/// 1920×1080 feed to the processor — the list, the selected screen's
/// settings, and the feed map with each screen where the processor reads it.
#[test]
fn snapshot_led_screens_window() {
    use varda::engine::value::led::{LedFit, LedLayout, LedScreen};
    use varda::renderer::context::OutputSource;

    let mut data = UIData::test_fixture();
    let mut cubes = data.buses[0].clone();
    cubes.bus.uuid = "bus00002".into();
    cubes.bus.name = "CUBES".into();
    data.buses.push(cubes);
    let screen = |uuid: &str, name: &str, size, at, bus: &str, fit| {
        let mut s = LedScreen::new(name, size, at, OutputSource::BusProgram(bus.into()));
        s.uuid = uuid.into();
        s.fit = fit;
        s
    };
    data.outputs = vec![led_output(LedLayout {
        feed: [1920, 1080],
        screens: vec![
            screen(
                "wall0001",
                "Main wall",
                [1152, 648],
                [0, 0],
                "bus00001",
                LedFit::Fit,
            ),
            screen(
                "cubel001",
                "Cube L",
                [300, 300],
                [1152, 0],
                "bus00002",
                LedFit::Fill,
            ),
            screen(
                "cuber001",
                "Cube R",
                [300, 300],
                [1452, 0],
                "bus00002",
                LedFit::Fill,
            ),
        ],
        test_card: false,
    })];
    // Setup in front of Audio, where the Outputs section is.
    let setup = data
        .screen
        .dock
        .find_tab(&varda::usecases::ui::screen_layout::Pane::Setup)
        .expect("the Classic layout has Setup");
    let _ = data.screen.dock.set_active_tab(setup);
    let Some(mut harness) = snapshot_harness(data) else {
        return;
    };
    let outputs = harness.get_by_label("📺 Outputs");
    outputs.scroll_to_me();
    outputs.click();
    harness.run();
    let open = harness.get_by_label("LED screens…");
    open.scroll_to_me();
    open.click();
    harness.run();
    harness.hover_at(egui::pos2(5.0, 5.0));
    harness.run();
    harness.snapshot("led_screens_window");
}
