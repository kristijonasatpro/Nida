//! Explicit end-to-end source qualification; never part of an ordinary test run.
use crate::{
    deck::playlist::Playlist,
    engine::value::playlist::{PlaylistItem, PlaylistSettings, PlaylistTransition},
    renderer::GpuContext,
    video::{hwdecode, VideoPlayer},
};
use std::{
    collections::{HashMap, HashSet},
    time::{Duration, Instant},
};

struct HardwarePreference(bool);
impl HardwarePreference {
    fn requested() -> Self {
        assert!(std::env::var_os("VARDA_REQUIRE_GPU_RESIDENT").is_some());
        assert!(std::env::var_os(hwdecode::SOFTWARE_ENV).is_none());
        let previous = hwdecode::windows_enabled();
        // This setter changes the in-process choice and returns serialization
        // for the settings writer. We never write that serialization to disk.
        let _ = hwdecode::set_windows_enabled(true).unwrap();
        Self(previous)
    }
}
impl Drop for HardwarePreference {
    fn drop(&mut self) {
        let _ = hwdecode::set_windows_enabled(self.0);
    }
}

#[test]
#[ignore = "explicit GPU qualification with NIDA_GPU_INTERLACED_CLIP"]
fn interlaced_uses_cpu_conversion_without_losing_the_picture() {
    let _preference = HardwarePreference::requested();
    let path = std::env::var_os("NIDA_GPU_INTERLACED_CLIP").expect("interlaced fixture required");
    let gpu = GpuContext::new_headless().unwrap();
    let mut player = VideoPlayer::new(path).unwrap();
    player.enable_gpu(&gpu);
    player.set_playing(false);
    player.seek_and_reset(0.0).unwrap();
    let expected = player.width() as usize * player.height() as usize * 4;
    assert_eq!(
        player
            .next_frame()
            .unwrap()
            .expect("fallback picture")
            .len(),
        expected
    );
    assert!(
        player.gpu.is_none(),
        "interlaced material must use established field processing"
    );
    assert!(player.delivery.snapshot().fallback_reason.is_some());
    assert!(gpu.errors.take_faults().is_empty());
}

#[test]
#[ignore = "explicit GPU playlist smoke/soak; NIDA_GPU_SOAK_SECONDS defaults to 30"]
fn playlist_playback_seek_pause_reload_and_soak() {
    let _preference = HardwarePreference::requested();
    let paths = std::env::var_os("NIDA_GPU_SOAK_CLIPS").expect("qualification clip paths required");
    let items: Vec<_> = std::env::split_paths(&paths)
        .map(|p| PlaylistItem::new(p.to_string_lossy()))
        .collect();
    assert!(items.len() >= 2);
    let seconds: u64 = std::env::var("NIDA_GPU_SOAK_SECONDS")
        .unwrap_or_else(|_| "30".into())
        .parse()
        .unwrap();
    assert!((20..=7200).contains(&seconds));
    let gpu = GpuContext::new_headless().unwrap();
    let size = (1920, 1080);
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("GPU playlist qualification output"),
        size: wgpu::Extent3d {
            width: size.0,
            height: size.1,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: gpu.compositing_format,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let view = target.create_view(&wgpu::TextureViewDescriptor::default());
    let mut settings = PlaylistSettings {
        transition_secs: 0.25,
        ..PlaylistSettings::default()
    };
    let mut list = Playlist::new(&gpu, items.clone(), settings, size);
    list.set_output_fps(60);
    list.set_playing(true);
    let audio = crate::audio::AudioData::default();
    let modulation = crate::modulation::ModulationEngine::default();
    let mut history = HashMap::new();
    let mut shown = HashSet::new();
    let mut parked_next_picture = false;
    let mut frames = 0_u64;
    let mut seeks = 0_u64;
    let mut reloads = 0_u64;
    let mut ticks = 0_u32;
    let mut last_progress = Instant::now();
    let started = Instant::now();
    let pid = sysinfo::get_current_pid().unwrap();
    let mut system = sysinfo::System::new();
    let mut steady_rss = None;
    let mut peak_rss = 0;
    while started.elapsed() < Duration::from_secs(seconds) {
        if ticks > 0 && ticks.is_multiple_of(900) {
            list.set_playing(false);
            if list.seek(0.25) {
                seeks += 1;
            }
        } else if ticks % 900 == 30 {
            list.set_playing(true);
        }
        if ticks > 0 && ticks.is_multiple_of(1800) {
            list.show_item((ticks as usize / 1800) % items.len());
        }
        if ticks > 0 && ticks.is_multiple_of(3600) {
            settings.transition = match (ticks / 3600) % 3 {
                0 => PlaylistTransition::Fade,
                1 => PlaylistTransition::Cut,
                _ => PlaylistTransition::Slide,
            };
            list.set_settings(settings);
        }
        if ticks > 0 && ticks.is_multiple_of(7200) {
            list = Playlist::new(&gpu, items.clone(), settings, size);
            list.set_output_fps(60);
            list.set_playing(true);
            reloads += 1;
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        list.tick_with_readiness(
            &mut encoder,
            &mut crate::video::FrameReadiness::new(60, false),
        );
        let mut commands = vec![encoder.finish()];
        list.render(&audio, &modulation, "qualification", &mut commands, &view);
        gpu.submit(commands);
        // The first native picture must arrive with CPU staging still unmapped.
        // Thereafter keep the app's normal post-submit lifecycle running too.
        if frames > 0 {
            list.for_each_deck_mut(crate::deck::Deck::request_video_remap);
        }
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        assert!(
            gpu.errors.take_faults().is_empty(),
            "GPU fault during playlist playback"
        );
        let snapshot = list.snapshot();
        assert!(snapshot.failed.is_empty(), "playlist source failed to open");
        if let Some(index) = snapshot.shown {
            shown.insert(index);
        }
        let mut deck_index = 0;
        list.for_each_deck(|deck| {
            if let Some(state) = deck.playback_snapshot() {
                let d = &state.delivery;
                if deck_index == 1 && !state.playing && d.gpu_frames > 0 {
                    parked_next_picture = true;
                }
                assert!(
                    d.fallback_reason.is_none(),
                    "unexpected fallback: {:?}",
                    d.fallback_reason
                );
                assert_eq!(d.downloaded_bytes, 0);
                assert_eq!(d.uploaded_bytes, 0);
                assert!(d.peak_slots <= 3);
                let previous = history.insert(deck.instance(), d.gpu_frames).unwrap_or(0);
                if d.gpu_frames > previous {
                    assert!(state.gpu_resident);
                    frames += d.gpu_frames - previous;
                    last_progress = Instant::now();
                }
            }
            deck_index += 1;
        });
        assert!(
            last_progress.elapsed() < Duration::from_secs(20),
            "playlist stopped delivering GPU pictures"
        );
        ticks += 1;
        if ticks.is_multiple_of(3600) {
            system.refresh_processes_specifics(
                sysinfo::ProcessesToUpdate::Some(&[pid]),
                true,
                sysinfo::ProcessRefreshKind::nothing().with_memory(),
            );
            let rss = system.process(pid).unwrap().memory();
            let baseline = *steady_rss.get_or_insert(rss);
            peak_rss = peak_rss.max(rss);
            assert!(
                rss < baseline + 512 * 1024 * 1024,
                "steady-state RSS grew by more than 512 MiB"
            );
            eprintln!(
                "GPU_PLAYLIST_SOAK {}",
                serde_json::json!({"elapsed_s": started.elapsed().as_secs_f64(), "frames": frames, "seeks": seeks, "reloads": reloads, "rss_bytes": rss, "peak_rss_bytes": peak_rss})
            );
        }
        let next = started + Duration::from_secs_f64(f64::from(ticks) / 60.0);
        if let Some(wait) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    assert!(shown.len() >= 2, "playlist did not move between clips");
    assert!(
        parked_next_picture,
        "preloaded paused clip did not retain its first GPU picture"
    );
    assert!(frames > seconds * 20, "insufficient GPU frame delivery");
    drop(list);
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert!(gpu.errors.take_faults().is_empty());
    eprintln!(
        "GPU_PLAYLIST_SOAK_COMPLETE {}",
        serde_json::json!({"wall_s": started.elapsed().as_secs_f64(), "frames": frames, "seeks": seeks, "reloads": reloads, "shown_items": shown.len(), "peak_rss_bytes": peak_rss})
    );
}
