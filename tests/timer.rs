//! Timers: a count down, a count to a time, a count up or the time of day,
//! drawn as text over clear in one layer — started, paused, reset and
//! adjusted on command, placed where its look puts it, and back with the
//! show paused where it was.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::engine::value::timer::{
    Anchor, TimeFormat, TimerAction, TimerMode, TimerSettings, TimerSnapshot, TimerState,
};
use varda::engine::{CommandResult, EngineCommand, InputSource};

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

#[allow(clippy::needless_pass_by_value)]
fn ok(result: CommandResult) {
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
}

fn app_in(workspace: &Path) -> Option<VardaApp> {
    use clap::Parser as _;
    let gpu = common::headless_gpu()?;
    let config = varda::app::AppConfig::parse_from([
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

/// The timer layer `deck`'s state.
fn timer(app: &VardaApp, deck: &str) -> TimerSnapshot {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .find(|d| d.uuid == deck)
        .and_then(|d| d.timer.clone())
        .expect("a timer layer")
}

fn control(app: &mut VardaApp, deck: &str, action: TimerAction) {
    ok(send_cmd(
        app,
        EngineCommand::ControlTimer {
            deck_uuid: deck.to_string(),
            action,
        },
    ));
}

fn set(app: &mut VardaApp, deck: &str, settings: TimerSettings) {
    ok(send_cmd(
        app,
        EngineCommand::SetTimerSettings {
            deck_uuid: deck.to_string(),
            settings,
        },
    ));
}

/// A timer input's layer: its channel's last deck.
fn add_timer_input(app: &mut VardaApp, settings: TimerSettings) -> String {
    let before = app.build_engine_state().mixer.channels.len();
    id(send_cmd(
        app,
        EngineCommand::AddInput {
            source: InputSource::Timer {
                settings: Some(settings),
            },
            name: None,
            audio: None,
        },
    ));
    let state = app.build_engine_state();
    assert_eq!(state.mixer.channels.len(), before + 1, "an input of its own");
    let channel = state.mixer.channels.last().unwrap();
    channel.decks.last().unwrap().uuid.clone()
}

#[test]
fn a_count_down_starts_pauses_adjusts_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path()) else {
        return;
    };
    let deck = add_timer_input(
        &mut app,
        TimerSettings {
            duration_secs: 90,
            ..TimerSettings::default()
        },
    );
    let state = app.build_engine_state();
    assert_eq!(state.mixer.channels.last().unwrap().name, "Count down");
    let t = timer(&app, &deck);
    assert_eq!((t.text.as_str(), t.running), ("1:30", false), "paused at its top");

    control(&mut app, &deck, TimerAction::Start);
    assert!(timer(&app, &deck).running);
    control(&mut app, &deck, TimerAction::Pause);
    assert!(!timer(&app, &deck).running);
    control(&mut app, &deck, TimerAction::Adjust { secs: 60 });
    assert_eq!(timer(&app, &deck).text, "2:30", "a minute more left");
    control(&mut app, &deck, TimerAction::Reset);
    let t = timer(&app, &deck);
    assert_eq!((t.text.as_str(), t.running), ("1:30", false));

    // Only a timer takes timer commands.
    let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
    let colour = id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: channel,
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    let refused = send_cmd(
        &mut app,
        EngineCommand::ControlTimer {
            deck_uuid: colour,
            action: TimerAction::Start,
        },
    );
    assert!(matches!(refused, CommandResult::Err { .. }), "{refused:?}");
}

#[test]
fn a_count_down_stops_on_zero_and_a_new_length_starts_from_its_top() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path()) else {
        return;
    };
    let one_second = TimerSettings {
        duration_secs: 1,
        ..TimerSettings::default()
    };
    let deck = add_timer_input(&mut app, one_second.clone());
    control(&mut app, &deck, TimerAction::Start);
    std::thread::sleep(Duration::from_millis(1100));
    app.render_mixer_frame();
    let t = timer(&app, &deck);
    assert_eq!(
        (t.text.as_str(), t.state, t.running),
        ("0:00", TimerState::Over, false),
        "stopped on zero"
    );
    set(
        &mut app,
        &deck,
        TimerSettings {
            duration_secs: 600,
            ..one_second
        },
    );
    assert_eq!(timer(&app, &deck).text, "10:00", "not 9:59");
}

#[test]
fn a_count_up_comes_back_with_the_show_paused_where_it_was() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = dir.path().join("show");
    std::fs::create_dir_all(&workspace).unwrap();
    let up = TimerSettings {
        mode: TimerMode::CountUp,
        ..TimerSettings::default()
    };
    let deck;
    {
        let Some(mut app) = app_in(&workspace) else {
            return;
        };
        deck = add_timer_input(&mut app, up.clone());
        assert_eq!(timer(&app, &deck).text, "0:00");
        control(&mut app, &deck, TimerAction::Adjust { secs: 65 });
        control(&mut app, &deck, TimerAction::Start);
        assert_eq!(timer(&app, &deck).text, "1:05");

        // A change of look is undone without losing the time run.
        set(
            &mut app,
            &deck,
            TimerSettings {
                format: TimeFormat::Hours,
                ..up.clone()
            },
        );
        assert_eq!(timer(&app, &deck).text, "00:01:05");
        ok(send_cmd(&mut app, EngineCommand::Undo));
        let t = timer(&app, &deck);
        assert_eq!(t.settings.format, TimeFormat::Auto);
        assert!(t.elapsed >= 65.0 && t.running, "{t:?}");

        app.save_workspace(&varda::usecases::ui::UILayoutState::default())
            .expect("save");
    }
    let Some(mut app) = app_in(&workspace) else {
        return;
    };
    let load = app.load_workspace();
    assert!(load.is_ok(), "{:?}", load.error_message());
    let t = timer(&app, &deck);
    assert_eq!(t.settings, up);
    assert!(!t.running, "it comes back paused");
    assert!((65.0..67.0).contains(&t.elapsed), "{}", t.elapsed);
    assert_eq!(t.panel, varda::engine::value::timer::panel_of(&deck));
}

fn f16(bits: u16) -> f32 {
    half::f16::from_bits(bits).to_f32()
}

/// Every pixel of an `Rgba16Float` texture, row by row.
fn pixels(gpu: &varda::renderer::context::GpuContext, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
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

/// Where the white of the numbers is on channel `ch`'s picture, as the box
/// round it in fractions of the frame, once it has been drawn.
fn white_box(app: &mut VardaApp, ch: usize) -> Option<[f32; 4]> {
    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        app.render_mixer_frame();
        let mixer = app.mixer_ref();
        let texture = &mixer.channels()[ch].composite_texture;
        let (w, h) = (texture.width(), texture.height());
        let all = pixels(app.gpu_context(), texture);
        let white: Vec<(u32, u32)> = all
            .iter()
            .enumerate()
            .filter(|(_, p)| p[0] > 0.8 && p[1] > 0.8 && p[2] > 0.8)
            .map(|(i, _)| (i as u32 % w, i as u32 / w))
            .collect();
        if !white.is_empty() {
            let (x0, x1) = (white.iter().map(|p| p.0).min()?, white.iter().map(|p| p.0).max()?);
            let (y0, y1) = (white.iter().map(|p| p.1).min()?, white.iter().map(|p| p.1).max()?);
            return Some([
                x0 as f32 / w as f32,
                y0 as f32 / h as f32,
                x1 as f32 / w as f32,
                y1 as f32 / h as f32,
            ]);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

/// Drawn over the layer below, white numbers where the look puts them, and
/// moved when it puts them elsewhere.
#[test]
fn the_numbers_are_drawn_where_the_look_puts_them_over_the_layer_below() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let state = app.build_engine_state();
    let channel = state.mixer.channels[0].uuid.clone();
    for deck in &state.mixer.channels[0].decks {
        send_cmd(
            &mut app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    id(send_cmd(
        &mut app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: channel.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    let mut settings = TimerSettings::default();
    settings.look.anchor = Anchor::BottomRight;
    settings.look.size = 0.2;
    let deck = id(send_cmd(
        &mut app,
        EngineCommand::AddTimerDeck {
            channel_uuid: channel,
            settings: Some(settings.clone()),
        },
    ));

    let [x0, y0, x1, y1] = white_box(&mut app, 0).expect("the numbers drawn");
    assert!(x0 > 0.5 && y0 > 0.5, "bottom right: {x0},{y0} – {x1},{y1}");
    assert!(x1 < 0.99 && y1 < 0.99, "in from the edges: {x1},{y1}");
    let height = y1 - y0;
    assert!((0.17..0.23).contains(&height), "a fifth of the frame high: {height}");
    let mixer = app.mixer_ref();
    let corner = pixels(app.gpu_context(), &mixer.channels()[0].composite_texture)[0];
    assert!(corner[2] > 0.9 && corner[0] < 0.1, "the layer below shows: {corner:?}");

    settings.look.anchor = Anchor::TopLeft;
    set(&mut app, &deck, settings);
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let [x0, y0, x1, y1] = white_box(&mut app, 0).expect("still drawn");
        if x1 < 0.5 && y1 < 0.5 {
            assert!(x0 > 0.01 && y0 > 0.01, "in from the edges: {x0},{y0}");
            break;
        }
        assert!(Instant::now() < until, "never moved: {x0},{y0} – {x1},{y1}");
    }
}
