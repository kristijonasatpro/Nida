//! Clips going on and off air, as vMix inputs do: a new clip waits on its
//! first frame, plays as it goes on air, pauses and goes back to its start as
//! it leaves — each switchable (Auto Play, Auto Pause, Auto Restart). A sound
//! file plays the same way, with nothing to see.
//!
//! Test clips are made with the `ffmpeg` command-line tool; without it these
//! tests skip.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::bus::BusInputRef;
use varda::engine::value::video::AutoPlayback;
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

#[allow(clippy::needless_pass_by_value)] // Takes the reply straight from `send_cmd`.
fn ok(result: CommandResult) {
    assert!(matches!(result, CommandResult::Ok), "{result:?}");
}

fn ffmpeg(args: &[&str]) -> bool {
    std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error"])
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

/// A six-second red clip with a tone, or `None` without the ffmpeg tool.
fn red_clip(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("red.mp4");
    let made = ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "color=c=red:s=64x36:r=25:d=6",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:duration=6",
        "-c:v",
        "mpeg4",
        "-c:a",
        "aac",
        "-shortest",
        path.to_str().unwrap(),
    ]);
    if made {
        Some(path)
    } else {
        eprintln!("ffmpeg not available — skipping");
        None
    }
}

/// An app in the vMix way with nothing on Main, and a clip on the first
/// channel: (app, first channel, clip deck, first video bus).
fn rig(path: &Path) -> Option<(VardaApp, String, String, String)> {
    let gpu = common::headless_gpu()?;
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).expect("VardaApp::new");
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    ));
    let state = app.build_engine_state();
    let ch = state.mixer.channels[0].uuid.clone();
    let bus = state.buses[0].uuid.clone();
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddVideoDeck {
            channel_uuid: ch.clone(),
            path: path.to_path_buf(),
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("the clip did not load: {other:?}"),
    };
    Some((app, ch, deck, bus))
}

/// Render frames for `secs`.
fn run(app: &mut VardaApp, secs: f64) {
    let end = Instant::now() + Duration::from_secs_f64(secs);
    while Instant::now() < end {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn playback(app: &VardaApp, deck: &str) -> varda::video::PlaybackSnapshot {
    let (ch, d) = app.mixer_ref().find_deck_by_uuid(deck).expect("the deck");
    app.mixer_ref().channels()[ch].decks[d]
        .deck
        .playback_snapshot()
        .expect("a clip")
}

fn take(app: &mut VardaApp, source: MainSource) {
    ok(send_cmd(
        app,
        EngineCommand::TakeToMain {
            source,
            fade: false,
        },
    ));
}

fn set_auto(app: &mut VardaApp, deck: &str, auto: AutoPlayback) {
    ok(send_cmd(
        app,
        EngineCommand::VideoSetAutoPlayback {
            deck_uuid: deck.to_string(),
            auto,
        },
    ));
}

fn playlist_rig(path: &Path) -> Option<(VardaApp, String, String, String)> {
    use varda::engine::value::playlist::{PlaylistItem, PlaylistSettings};
    let (mut app, channel, clip, bus) = rig(path)?;
    ok(send_cmd(
        &mut app,
        EngineCommand::RemoveDeck { deck_uuid: clip },
    ));
    let item = PlaylistItem::new(path.to_string_lossy());
    let deck = match send_cmd(
        &mut app,
        EngineCommand::AddPlaylistDeck {
            channel_uuid: channel.clone(),
            items: vec![item.clone(), item],
            settings: Some(PlaylistSettings {
                auto_next: false,
                ..PlaylistSettings::default()
            }),
        },
    ) {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("the playlist did not load: {other:?}"),
    };
    app.set_preview_channels(vec![0]);
    Some((app, channel, deck, bus))
}

fn playlist_state(app: &VardaApp, deck: &str) -> varda::engine::value::playlist::PlaylistSnapshot {
    let (ch, d) = app.mixer_ref().find_deck_by_uuid(deck).unwrap();
    app.mixer_ref().channels()[ch].decks[d]
        .deck
        .playlist()
        .unwrap()
        .snapshot()
}

#[test]
fn playlist_auto_play_starts_the_selected_item_on_cut_and_during_a_fade() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, channel, deck, _)) = playlist_rig(&path) else {
        return;
    };
    ok(send_cmd(
        &mut app,
        EngineCommand::SetMainFadeTime { secs: 2.0 },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::ShowPlaylistItem {
            deck_uuid: deck.clone(),
            index: 1,
        },
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while playlist_state(&app, &deck).shown != Some(1) {
        assert!(Instant::now() < deadline, "selected clip did not load");
        run(&mut app, 0.05);
    }
    for fade in [false, true] {
        for enabled in [false, true] {
            take(&mut app, MainSource::Black);
            run(&mut app, 0.1);
            ok(send_cmd(
                &mut app,
                EngineCommand::SetPlaylistPlaying {
                    deck_uuid: deck.clone(),
                    playing: false,
                },
            ));
            ok(send_cmd(
                &mut app,
                EngineCommand::VideoSeek {
                    deck_uuid: deck.clone(),
                    position_secs: 0.0,
                },
            ));
            let mut settings = playlist_state(&app, &deck).settings;
            settings.auto_play = enabled;
            ok(send_cmd(
                &mut app,
                EngineCommand::SetPlaylistSettings {
                    deck_uuid: deck.clone(),
                    settings,
                },
            ));
            run(&mut app, 0.15);
            assert!(
                !playlist_state(&app, &deck).playing,
                "Preview does not autoplay"
            );
            ok(send_cmd(
                &mut app,
                EngineCommand::TakeToMain {
                    source: MainSource::Channel(channel.clone()),
                    fade,
                },
            ));
            run(&mut app, 0.3);
            let list = playlist_state(&app, &deck);
            assert_eq!(list.playing, enabled, "Auto Play {enabled}, fade {fade}");
            assert_eq!(
                list.shown,
                Some(1),
                "start the selected item, not the first"
            );
            if fade {
                assert!(
                    app.mixer_ref().main().fade.is_some(),
                    "playback starts before the fade finishes"
                );
            }
            if enabled {
                let (ch, d) = app.mixer_ref().find_deck_by_uuid(&deck).unwrap();
                let snapshot = app.mixer_ref().channels()[ch].decks[d]
                    .deck
                    .transport_playback_snapshot()
                    .unwrap();
                assert!(snapshot.position > 0.0, "the video actually advances");
                ok(send_cmd(
                    &mut app,
                    EngineCommand::SetPlaylistPlaying {
                        deck_uuid: deck.clone(),
                        playing: false,
                    },
                ));
                run(&mut app, 0.15);
                assert!(
                    !playlist_state(&app, &deck).playing,
                    "a manual pause on Program stays paused"
                );
            }
        }
    }
}

#[test]
fn playlist_auto_play_starts_on_a_bus_program_but_not_its_preview() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, channel, deck, bus)) = playlist_rig(&path) else {
        return;
    };
    let mut settings = playlist_state(&app, &deck).settings;
    settings.auto_play = true;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetPlaylistSettings {
            deck_uuid: deck.clone(),
            settings,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(channel),
        },
    ));
    run(&mut app, 0.2);
    assert!(!playlist_state(&app, &deck).playing);
    ok(send_cmd(&mut app, EngineCommand::BusCut { uuid: bus }));
    run(&mut app, 0.3);
    assert!(playlist_state(&app, &deck).playing);
}

/// The middle pixel of the master, linear RGBA.
fn middle_of_main(app: &VardaApp) -> [f32; 4] {
    let ctx = app.gpu_context();
    let tex = app.mixer_ref().composite_texture();
    let (w, h) = (tex.width(), tex.height());
    let padded =
        (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("on-air readback"),
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
    let at = (h / 2 * padded + w / 2 * 8) as usize;
    let half = |i: usize| {
        half::f16::from_bits(u16::from_le_bytes([data[at + i * 2], data[at + i * 2 + 1]])).to_f32()
    };
    [half(0), half(1), half(2), half(3)]
}

#[test]
fn a_new_clip_waits_on_its_first_frame_until_it_goes_on_air() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, ch, deck, _)) = rig(&path) else {
        return;
    };
    run(&mut app, 0.5);
    let parked = playback(&app, &deck);
    assert!(!parked.playing, "a new clip does not start by itself");
    assert!(parked.position < 0.05, "at its start: {}", parked.position);

    // On air without Auto Play it stays paused — and shows its first frame.
    set_auto(
        &mut app,
        &deck,
        AutoPlayback {
            play: false,
            ..AutoPlayback::default()
        },
    );
    take(&mut app, MainSource::Channel(ch.clone()));
    run(&mut app, 0.4);
    assert!(!playback(&app, &deck).playing, "Auto Play off: paused");
    let px = middle_of_main(&app);
    assert!(
        px[0] > 0.5 && px[1] < 0.1 && px[2] < 0.1,
        "the paused clip's first frame is on Main: {px:?}"
    );

    // With Auto Play it plays as it goes on air…
    take(&mut app, MainSource::Black);
    run(&mut app, 0.1);
    set_auto(&mut app, &deck, AutoPlayback::default());
    take(&mut app, MainSource::Channel(ch.clone()));
    run(&mut app, 0.8);
    let playing = playback(&app, &deck);
    assert!(playing.playing, "Auto Play: playing on air");
    assert!(playing.position > 0.3, "moving: {}", playing.position);

    // …and pauses and goes back to its start as it leaves.
    take(&mut app, MainSource::Black);
    run(&mut app, 0.4);
    let left = playback(&app, &deck);
    assert!(!left.playing, "Auto Pause: paused off air");
    assert!(
        left.position < 0.1,
        "Auto Restart: at its start: {}",
        left.position
    );
}

#[test]
fn auto_pause_and_auto_restart_can_each_be_switched_off() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, ch, deck, _)) = rig(&path) else {
        return;
    };
    set_auto(
        &mut app,
        &deck,
        AutoPlayback {
            restart: false,
            ..AutoPlayback::default()
        },
    );
    take(&mut app, MainSource::Channel(ch.clone()));
    run(&mut app, 0.8);
    take(&mut app, MainSource::Black);
    run(&mut app, 0.4);
    let held = playback(&app, &deck);
    assert!(!held.playing, "paused");
    assert!(held.position > 0.3, "left where it was: {}", held.position);

    set_auto(
        &mut app,
        &deck,
        AutoPlayback {
            pause: false,
            restart: false,
            play: true,
            main_only: false,
        },
    );
    take(&mut app, MainSource::Channel(ch));
    run(&mut app, 0.3);
    take(&mut app, MainSource::Black);
    run(&mut app, 0.5);
    let still = playback(&app, &deck);
    assert!(still.playing, "Auto Pause off: it plays on off air");
}

#[test]
fn a_clip_plays_as_a_layer_of_a_channel_on_a_bus() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, ch, deck, bus)) = rig(&path) else {
        return;
    };
    run(&mut app, 0.2);
    assert!(!playback(&app, &deck).playing);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(ch),
        },
    ));
    run(&mut app, 0.2);
    assert!(
        !playback(&app, &deck).playing,
        "on the bus's Preview it is not on air"
    );
    ok(send_cmd(&mut app, EngineCommand::BusCut { uuid: bus }));
    run(&mut app, 0.3);
    assert!(
        playback(&app, &deck).playing,
        "on the bus's Program: playing"
    );
}

/// A clip's input shown as a layer of another input (another input as a
/// layer): that input going to Main plays the clip, leaving it pauses and
/// rewinds it — as if the clip's own input had gone.
#[test]
fn a_clip_plays_when_the_input_showing_its_input_goes_on_air() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, ch, deck, _)) = rig(&path) else {
        return;
    };
    let second = app.build_engine_state().mixer.channels[1].uuid.clone();
    let show = |app: &mut VardaApp, into: &str, shown: &str| match send_cmd(
        app,
        EngineCommand::AddTapDeck {
            channel_uuid: into.to_string(),
            source: varda::scene::TapSourceConfig::Channel {
                uuid: shown.to_string(),
            },
        },
    ) {
        CommandResult::OkWithId { .. } => {}
        other => panic!("the layer was not added: {other:?}"),
    };
    show(&mut app, &second, &ch);
    run(&mut app, 0.2);
    assert!(!playback(&app, &deck).playing);

    take(&mut app, MainSource::Channel(second.clone()));
    run(&mut app, 0.6);
    let on = playback(&app, &deck);
    assert!(on.playing, "on Main through the other input: playing");
    assert!(on.position > 0.1, "and moving: {}", on.position);

    take(&mut app, MainSource::Black);
    run(&mut app, 0.2);
    let off = playback(&app, &deck);
    assert!(!off.playing, "off air: paused");
    assert!(
        off.position < 0.05,
        "and back at its start: {}",
        off.position
    );

    // The two showing each other, round a loop: still worked out, at once.
    show(&mut app, &ch, &second);
    take(&mut app, MainSource::Channel(second));
    run(&mut app, 0.3);
    assert!(playback(&app, &deck).playing, "round a loop too");
}

#[test]
fn a_clip_set_to_main_only_ignores_the_buses() {
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = red_clip(dir.path()) else {
        return;
    };
    let Some((mut app, ch, deck, bus)) = rig(&path) else {
        return;
    };
    set_auto(
        &mut app,
        &deck,
        AutoPlayback {
            main_only: true,
            ..AutoPlayback::default()
        },
    );
    ok(send_cmd(
        &mut app,
        EngineCommand::SetBusPreview {
            uuid: bus.clone(),
            input: BusInputRef::Channel(ch.clone()),
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::BusCut { uuid: bus }));
    run(&mut app, 0.3);
    assert!(
        !playback(&app, &deck).playing,
        "on a bus's Program only: not started"
    );
    take(&mut app, MainSource::Channel(ch));
    run(&mut app, 0.3);
    assert!(playback(&app, &deck).playing, "on Main: playing");
    // Still on the bus, but off Main: paused.
    take(&mut app, MainSource::Black);
    run(&mut app, 0.3);
    assert!(
        !playback(&app, &deck).playing,
        "off Main, though still on the bus: paused"
    );
}

#[test]
fn a_sound_file_plays_like_a_clip_with_nothing_to_see() {
    let dir = tempfile::tempdir().unwrap();
    let wav = dir.path().join("jingle.wav");
    if !ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=1000:duration=4",
        wav.to_str().unwrap(),
    ]) {
        eprintln!("ffmpeg not available — skipping");
        return;
    }
    let Some((mut app, ch, deck, _)) = rig(&wav) else {
        return;
    };
    let master = app.build_engine_state().audio_mix.buses[0]
        .config
        .uuid
        .clone();
    run(&mut app, 0.2);
    let state = app.build_engine_state();
    let snapshot = state
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .and_then(|d| d.video_playback.clone())
        .expect("a sound file plays like a clip");
    assert!(snapshot.sound_only);
    assert!(!snapshot.playing, "waiting at its start");
    assert!(
        state.audio_mix.sources.iter().any(|s| s.uuid == deck),
        "it has a strip in the mixer"
    );

    let tap = app.audio_mix().tap(&master).expect("Master");
    take(&mut app, MainSource::Channel(ch));
    let end = Instant::now() + Duration::from_secs(1);
    let mut loud = 0.0f32;
    while Instant::now() < end {
        app.render_mixer_frame();
        while let Ok(chunk) = tap.receiver.try_recv() {
            loud = chunk.samples.iter().fold(loud, |m, s| m.max(s.abs()));
        }
        std::thread::sleep(Duration::from_millis(8));
    }
    assert!(playback(&app, &deck).playing, "on air: playing");
    assert!(loud > 0.08, "and heard on Master: {loud}");
    let px = middle_of_main(&app);
    assert!(
        px[0] < 0.01 && px[1] < 0.01 && px[2] < 0.01,
        "nothing to see: {px:?}"
    );
}
