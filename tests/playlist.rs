//! Playlists, as vMix's List input: pictures held their time and clips
//! played to their end, one after another, with a cut, fade or slide
//! between — and the list edited, stopped and picked from while it plays.
//!
//! Picture timing runs on a made-up clock, a frame at a time; opening a
//! file happens on a thread, so the tests wait (in real time) for that.

use std::path::Path;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use varda::deck::playlist::Playlist;
use varda::engine::value::playlist::{
    PlaylistItem, PlaylistSettings, PlaylistSnapshot, PlaylistTransition,
};
use varda::renderer::context::GpuContext;
use varda::renderer::playlist_mix::{Mix, PlaylistMixPipeline};

mod common;

fn picture(dir: &Path, name: &str, rgb: [u8; 3]) -> PlaylistItem {
    let path = dir.join(name);
    image::RgbaImage::from_pixel(16, 9, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
        .save(&path)
        .unwrap();
    PlaylistItem::new(path.to_string_lossy())
}

/// Red, green and blue pictures.
fn three(dir: &Path) -> Vec<PlaylistItem> {
    vec![
        picture(dir, "red.png", [255, 0, 0]),
        picture(dir, "green.png", [0, 255, 0]),
        picture(dir, "blue.png", [0, 0, 255]),
    ]
}

/// A made-up clock for pictures' time.
struct Clock(Instant);

impl Clock {
    fn new() -> Self {
        Self(Instant::now())
    }

    /// `secs` of play, a frame at a time.
    fn run(&mut self, list: &mut Playlist, secs: f64) {
        let frames = (secs * 60.0).ceil() as usize;
        for _ in 0..frames {
            self.0 += Duration::from_secs_f64(1.0 / 60.0);
            list.step(self.0);
        }
    }

    /// Look again and again, no time passing, until `done` — files opening
    /// on their threads meanwhile.
    fn until(&self, list: &mut Playlist, done: impl Fn(&PlaylistSnapshot) -> bool) {
        let start = Instant::now();
        loop {
            list.step(self.0);
            if done(&list.snapshot()) {
                return;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "never came: {:?}",
                list.snapshot()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

fn settings(transition: PlaylistTransition, picture_secs: f64) -> PlaylistSettings {
    PlaylistSettings {
        transition,
        picture_secs,
        ..PlaylistSettings::default()
    }
}

fn new_list(gpu: &GpuContext, items: Vec<PlaylistItem>, settings: PlaylistSettings) -> Playlist {
    Playlist::new(gpu, items, settings, (64, 36))
}

#[test]
fn a_picture_is_held_its_time_then_the_next_comes_and_the_list_loops() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path())[..2].to_vec();
    let mut list = new_list(&gpu, items, settings(PlaylistTransition::Cut, 1.0));
    let mut clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    clock.run(&mut list, 2.0);
    assert_eq!(list.snapshot().shown, Some(0), "stopped, it stays");

    list.set_playing(true);
    clock.until(&mut list, |s| s.next_ready == Some(1));
    clock.run(&mut list, 0.9);
    assert_eq!(list.snapshot().shown, Some(0), "not yet its time");
    clock.run(&mut list, 0.2);
    assert_eq!(list.snapshot().shown, Some(1));

    clock.until(&mut list, |s| s.next_ready == Some(0));
    clock.run(&mut list, 1.1);
    assert_eq!(list.snapshot().shown, Some(0), "round again");
    assert!(list.is_playing());
}

#[test]
fn a_list_that_does_not_loop_stops_at_its_end_and_plays_again_from_the_top() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path())[..2].to_vec();
    let mut list = new_list(
        &gpu,
        items,
        PlaylistSettings {
            loop_list: false,
            ..settings(PlaylistTransition::Cut, 0.5)
        },
    );
    let mut clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    list.set_playing(true);
    clock.until(&mut list, |s| s.next_ready == Some(1));
    clock.run(&mut list, 0.6);
    assert_eq!(list.snapshot().shown, Some(1));
    clock.run(&mut list, 0.6);
    let end = list.snapshot();
    assert_eq!(end.shown, Some(1), "the last item stays up");
    assert!(!end.playing, "and the list stops");

    list.set_playing(true);
    clock.until(&mut list, |s| s.shown == Some(0));
    assert!(list.is_playing());
}

#[test]
fn a_fade_runs_its_time_between_two_pictures() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path())[..2].to_vec();
    let mut list = new_list(
        &gpu,
        items,
        PlaylistSettings {
            transition_secs: 1.0,
            ..settings(PlaylistTransition::Fade, 0.5)
        },
    );
    let mut clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    list.set_playing(true);
    clock.until(&mut list, |s| s.next_ready == Some(1));
    clock.run(&mut list, 0.6);
    let mid = list.snapshot();
    assert_eq!((mid.shown, mid.coming), (Some(0), Some(1)), "fading");
    clock.run(&mut list, 0.5);
    assert_eq!(list.snapshot().coming, Some(1), "still fading half way");
    clock.run(&mut list, 0.6);
    let after = list.snapshot();
    assert_eq!((after.shown, after.coming), (Some(1), None));
}

#[test]
fn a_file_that_will_not_open_is_skipped() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut items = vec![PlaylistItem::new(
        dir.path().join("gone.png").to_string_lossy(),
    )];
    items.push(picture(dir.path(), "red.png", [255, 0, 0]));
    let mut list = new_list(&gpu, items, settings(PlaylistTransition::Cut, 1.0));
    let clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(1));
    assert_eq!(list.snapshot().failed, vec![0]);
}

#[test]
fn editing_the_list_keeps_what_is_on_screen() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path());
    let (red, green, blue) = (items[0].clone(), items[1].clone(), items[2].clone());
    let mut list = new_list(&gpu, items, settings(PlaylistTransition::Cut, 1.0));
    let mut clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    list.set_playing(true);
    clock.until(&mut list, |s| s.next_ready == Some(1));
    clock.run(&mut list, 1.1);
    assert_eq!(list.snapshot().shown, Some(1), "green up");

    // Moved: still green, at its new place; blue comes next.
    list.set_items(vec![green.clone(), red.clone(), blue.clone()]);
    assert_eq!(list.snapshot().shown, Some(0));
    clock.until(&mut list, |s| s.next_ready == Some(1));

    // Taken out: green stays up until its time is done, and the item that
    // took its place comes after it.
    list.set_items(vec![red, blue]);
    assert_eq!(list.snapshot().shown, None, "no longer in the list");
    clock.until(&mut list, |s| s.next_ready == Some(0));
    clock.run(&mut list, 1.1);
    assert_eq!(list.snapshot().shown, Some(0));

    list.set_items(Vec::new());
    list.step(clock.0);
    assert_eq!(list.snapshot().shown, None, "an empty list shows nothing");
}

#[test]
fn an_item_picked_while_stopped_shows_at_once() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let mut list = new_list(
        &gpu,
        three(dir.path()),
        settings(PlaylistTransition::Fade, 5.0),
    );
    let clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    list.show_item(2);
    clock.until(&mut list, |s| s.shown == Some(2));
    assert!(!list.is_playing());
}

/// A green clip `secs` long, or `None` without the ffmpeg tool.
fn clip(dir: &Path, secs: f64) -> Option<PlaylistItem> {
    let path = dir.join("green.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i"])
        .arg(format!("color=c=green:s=64x36:r=25:d={secs}"))
        .args(["-c:v", "mpeg4", "-q:v", "2"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if !made {
        eprintln!("ffmpeg not available — skipping");
        return None;
    }
    Some(PlaylistItem::new(path.to_string_lossy()))
}

#[test]
fn a_clip_plays_to_its_end_before_the_next_item() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let Some(green) = clip(dir.path(), 1.0) else {
        return;
    };
    let red = picture(dir.path(), "red.png", [255, 0, 0]);
    let mut list = new_list(
        &gpu,
        vec![green, red],
        settings(PlaylistTransition::Cut, 5.0),
    );
    let wait = |list: &mut Playlist, done: &dyn Fn(&PlaylistSnapshot) -> bool| {
        let start = Instant::now();
        while !done(&list.snapshot()) {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "{:?}",
                list.snapshot()
            );
            std::thread::sleep(Duration::from_millis(5));
            list.step(Instant::now());
        }
        start.elapsed()
    };
    wait(&mut list, &|s| s.shown == Some(0));
    list.set_playing(true);
    let played = wait(&mut list, &|s| s.shown == Some(1));
    assert!(
        played >= Duration::from_millis(800),
        "moved on after {played:?}, before the clip's end"
    );
}

// ── The picture ─────────────────────────────────────────────────────

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// A 4×4 texture of one straight-alpha colour.
fn filled(gpu: &GpuContext, rgba: [f32; 4]) -> wgpu::TextureView {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let texel: Vec<u8> = rgba
        .iter()
        .flat_map(|c| half::f16::from_f32(*c).to_le_bytes())
        .collect();
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &texel.repeat(16),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(32),
            rows_per_image: Some(4),
        },
        texture.size(),
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// Row 1 of a 4×4 picture of `mix` from red to blue, left to right.
fn mixed(gpu: &GpuContext, mix: Mix, blue: [f32; 4]) -> [[f32; 4]; 4] {
    let red = filled(gpu, [1.0, 0.0, 0.0, 1.0]);
    let blue = filled(gpu, blue);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d {
            width: 4,
            height: 4,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let scope = gpu.errors.scope("playlist mix test");
    let pipeline = PlaylistMixPipeline::new(&gpu.device, FORMAT);
    let mut buffers = Vec::new();
    pipeline.draw(
        &gpu.device,
        &gpu.queue,
        &mut buffers,
        mix,
        Some(&red),
        Some(&blue),
        &view,
        (4, 4),
    );
    let padded = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(padded * 4),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        target.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(4),
            },
        },
        target.size(),
    );
    buffers.push(encoder.finish());
    gpu.submit(buffers);
    let faults: Vec<String> = gpu
        .errors
        .take_faults()
        .into_iter()
        .map(|f| f.message)
        .collect();
    assert!(faults.is_empty(), "GPU errors: {faults:#?}");
    drop(scope);
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
    std::array::from_fn(|x| {
        let at = (padded + 8 * x as u32) as usize;
        std::array::from_fn(|c| {
            half::f16::from_le_bytes([data[at + c * 2], data[at + c * 2 + 1]]).to_f32()
        })
    })
}

fn near(a: [f32; 4], b: [f32; 4]) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.02)
}

#[test]
fn half_way_through_a_fade_both_pictures_show_equally() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let row = mixed(&gpu, Mix::Fade(0.5), [0.0, 0.0, 1.0, 1.0]);
    assert!(near(row[0], [0.5, 0.0, 0.5, 1.0]), "{:?}", row[0]);
    // Into a clear picture (bars round a fitted one), red thins out rather
    // than darkening: straight colour stays red, half as solid.
    let row = mixed(&gpu, Mix::Fade(0.5), [0.0, 0.0, 0.0, 0.0]);
    assert!(near(row[0], [1.0, 0.0, 0.0, 0.5]), "{:?}", row[0]);
}

#[test]
fn half_way_through_a_slide_each_picture_has_half_the_frame() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let from_right = varda::engine::value::playlist::SlideFrom::Right.offset();
    let row = mixed(&gpu, Mix::Slide(0.5, from_right, 0.0), [0.0, 0.0, 1.0, 1.0]);
    for (x, pixel) in row.iter().enumerate() {
        let want = if x < 2 {
            [1.0, 0.0, 0.0, 1.0]
        } else {
            [0.0, 0.0, 1.0, 1.0]
        };
        assert!(near(*pixel, want), "pixel {x}: {pixel:?}");
    }
    let row = mixed(&gpu, Mix::Shown, [0.0, 0.0, 1.0, 1.0]);
    assert!(row.iter().all(|p| near(*p, [1.0, 0.0, 0.0, 1.0])));
}

// ── In the show ─────────────────────────────────────────────────────

fn send_cmd(
    app: &mut varda::app::VardaApp,
    cmd: varda::engine::EngineCommand,
) -> varda::engine::CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn app_in(gpu: GpuContext, workspace: &Path) -> varda::app::VardaApp {
    use clap::Parser as _;
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
    varda::app::VardaApp::new(gpu, &config).expect("VardaApp::new")
}

/// The playlist layer of the show's last channel: its UUID and state.
fn last_playlist(app: &varda::app::VardaApp) -> (String, PlaylistSnapshot) {
    let state = app.build_engine_state();
    let deck = state.mixer.channels.last().unwrap().decks[0].clone();
    (deck.uuid, deck.playlist.expect("a playlist layer"))
}

/// A playlist input is a channel of its own holding the list, named after
/// it; it plays on command, and its list and settings come back with the
/// show.
#[test]
fn a_playlist_input_plays_and_comes_back_with_the_show() {
    use varda::engine::{CommandResult, EngineCommand, InputSource};
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path());
    let workspace = dir.path().join("show");
    std::fs::create_dir_all(&workspace).unwrap();
    let slide = PlaylistSettings {
        transition: PlaylistTransition::Slide,
        picture_secs: 8.0,
        loop_list: false,
        ..PlaylistSettings::default()
    };
    {
        let mut app = app_in(gpu.clone(), &workspace);
        let added = send_cmd(
            &mut app,
            EngineCommand::AddInput {
                source: InputSource::Playlist {
                    items: items.clone(),
                },
                name: None,
                audio: None,
            },
        );
        assert!(matches!(added, CommandResult::OkWithId { .. }), "{added:?}");
        let state = app.build_engine_state();
        assert_eq!(state.mixer.channels.last().unwrap().name, "Playlist");
        let (deck, list) = last_playlist(&app);
        assert_eq!(list.items, items);
        assert!(!list.playing, "it starts stopped");

        send_cmd(
            &mut app,
            EngineCommand::SetPlaylistSettings {
                deck_uuid: deck.clone(),
                settings: slide,
            },
        );
        send_cmd(
            &mut app,
            EngineCommand::SetPlaylistPlaying {
                deck_uuid: deck.clone(),
                playing: true,
            },
        );
        assert!(last_playlist(&app).1.playing);
        let colour = match send_cmd(
            &mut app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: state.mixer.channels[0].uuid.clone(),
                color: [1.0, 0.0, 0.0, 1.0],
            },
        ) {
            CommandResult::OkWithId { uuid } => uuid,
            other => panic!("{other:?}"),
        };
        let not_a_list = send_cmd(
            &mut app,
            EngineCommand::SetPlaylistPlaying {
                deck_uuid: colour,
                playing: true,
            },
        );
        assert!(matches!(not_a_list, CommandResult::Err { .. }));
        app.save_workspace(&varda::usecases::ui::UILayoutState::default())
            .expect("save");
    }
    let mut app = app_in(gpu, &workspace);
    let load = app.load_workspace();
    assert!(load.is_ok(), "{:?}", load.error_message());
    let (_, list) = last_playlist(&app);
    assert_eq!(list.items, items);
    assert_eq!(list.settings, slide);
}

/// A slide is blurred along its way: across the join, the two pictures
/// run into each other; with no blur the join is sharp.
#[test]
fn a_slide_is_blurred_along_its_way() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let from_right = varda::engine::value::playlist::SlideFrom::Right.offset();
    let sharp = mixed(&gpu, Mix::Slide(0.5, from_right, 0.0), [0.0, 0.0, 1.0, 1.0]);
    let blurred = mixed(&gpu, Mix::Slide(0.5, from_right, 1.0), [0.0, 0.0, 1.0, 1.0]);
    assert!(near(sharp[1], [1.0, 0.0, 0.0, 1.0]), "{:?}", sharp[1]);
    let [r, _, b, a] = blurred[1];
    assert!(
        r > 0.2 && b > 0.2,
        "red and blue run together: {:?}",
        blurred[1]
    );
    assert!((a - 1.0).abs() < 0.02, "and stay solid: {a}");
}

/// Frames drawn at uneven moments — a timer a few milliseconds early, then
/// late — still move a transition on by the same step each: the step the
/// screen shows them at.
#[test]
fn a_transition_moves_on_evenly_however_unevenly_frames_are_drawn() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path())[..2].to_vec();
    let settings = PlaylistSettings {
        speed_ramp: false,
        transition_secs: 1.0,
        ..settings(PlaylistTransition::Slide, 0.5)
    };
    let steps = |fps: u32| -> Vec<f32> {
        let mut list = new_list(&gpu, items.clone(), settings);
        list.set_output_fps(fps);
        let mut clock = Clock::new();
        clock.until(&mut list, |s| s.shown == Some(0));
        list.set_playing(true);
        clock.until(&mut list, |s| s.next_ready == Some(1));
        while list.transition_progress().is_none() {
            clock.run(&mut list, 1.0 / 60.0);
        }
        let mut seen = Vec::new();
        for i in 0..20 {
            // 12 ms, then 21 ms: a 60 Hz timer's wobble.
            clock.0 += Duration::from_millis(if i % 2 == 0 { 12 } else { 21 });
            list.step(clock.0);
            seen.push(list.transition_progress().unwrap());
        }
        seen.windows(2).map(|w| w[1] - w[0]).collect()
    };
    let even = steps(60);
    for step in &even {
        assert!(
            (step - 1.0 / 60.0).abs() < 1e-4,
            "every step one frame: {even:?}"
        );
    }
    let wall = steps(0);
    assert!(
        wall.iter().any(|s| (s - 1.0 / 60.0).abs() > 1e-3),
        "by the wall clock alone they wobble: {wall:?}"
    );
}

/// A frame the screen still shows at its own refresh, but started late —
/// held up by the frame before, more than half a frame — moves the slide
/// one frame on like any other: stepped by the clock of the moment it
/// started, it went two frames on and then none, a hitch on screen though
/// no refresh was missed.
#[test]
fn a_frame_started_late_but_shown_on_time_moves_the_slide_one_frame() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path())[..2].to_vec();
    let settings = PlaylistSettings {
        speed_ramp: false,
        transition_secs: 5.0,
        ..settings(PlaylistTransition::Slide, 0.5)
    };
    let mut list = new_list(&gpu, items, settings);
    list.set_output_fps(60);
    let mut clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    list.set_playing(true);
    clock.until(&mut list, |s| s.next_ready == Some(1));
    while list.transition_progress().is_none() {
        clock.run(&mut list, 1.0 / 60.0);
    }
    // Refreshes every 1/60 s; each frame starts a millisecond after its
    // refresh, but the tenth 10 ms after.
    let refresh = Duration::from_secs_f64(1.0 / 60.0);
    let first = clock.0;
    let mut seen = Vec::new();
    for i in 1..=20_u32 {
        let late = Duration::from_millis(if i == 10 { 10 } else { 1 });
        let refreshed_at = first + refresh * i;
        list.frame_refresh_at(Some(refreshed_at));
        list.step(refreshed_at + late);
        seen.push(list.transition_progress().unwrap());
    }
    let steps: Vec<f32> = seen.windows(2).map(|w| w[1] - w[0]).collect();
    for step in &steps {
        assert!(
            (step - 1.0 / 300.0).abs() < 1e-4,
            "every step one frame of the five-second slide: {steps:?}"
        );
    }
}

/// Each frame held on screen a frame's time — macOS showing every frame
/// once, in turn, whenever it was started — the slide moves a frame per
/// frame however unevenly frames start, as on a variable-refresh screen whose
/// refresh signal wanders: 4 ms after the one before, then 29.
#[test]
fn frames_held_on_screen_move_the_slide_a_frame_each_however_they_start() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let items = three(dir.path())[..2].to_vec();
    let settings = PlaylistSettings {
        speed_ramp: false,
        transition_secs: 5.0,
        ..settings(PlaylistTransition::Slide, 0.5)
    };
    let mut list = new_list(&gpu, items, settings);
    list.set_output_fps(60);
    let mut clock = Clock::new();
    clock.until(&mut list, |s| s.shown == Some(0));
    list.set_playing(true);
    clock.until(&mut list, |s| s.next_ready == Some(1));
    while list.transition_progress().is_none() {
        clock.run(&mut list, 1.0 / 60.0);
    }
    let mut at = clock.0;
    let mut seen = Vec::new();
    for i in 0..20_u32 {
        at += Duration::from_micros(if i % 2 == 0 { 4_300 } else { 29_033 });
        list.frame_held(true);
        list.step(at);
        seen.push(list.transition_progress().unwrap());
    }
    let steps: Vec<f32> = seen.windows(2).map(|w| w[1] - w[0]).collect();
    for step in &steps {
        assert!(
            (step - 1.0 / 300.0).abs() < 1e-4,
            "every step one frame: {steps:?}"
        );
    }
}
