use super::*;
use std::time::{Duration, Instant};

#[cfg(target_os = "windows")]
struct TimerPeriod;
#[cfg(target_os = "windows")]
#[link(name = "winmm")]
unsafe extern "system" {
    fn timeBeginPeriod(period: u32) -> u32;
    fn timeEndPeriod(period: u32) -> u32;
}
#[cfg(target_os = "windows")]
impl TimerPeriod {
    fn requested() -> Option<Self> {
        std::env::var_os("NIDA_VIDEO_BENCH_TIMER_1MS").map(|_| {
            assert_eq!(unsafe { timeBeginPeriod(1) }, 0);
            Self
        })
    }
}
#[cfg(target_os = "windows")]
impl Drop for TimerPeriod {
    fn drop(&mut self) {
        assert_eq!(unsafe { timeEndPeriod(1) }, 0);
    }
}

#[derive(Default)]
struct Audit {
    produced: std::sync::atomic::AtomicUsize,
    hardware: std::sync::atomic::AtomicUsize,
    failed: AtomicBool,
}

struct AuditedPlayer {
    player: VideoPlayer,
    require_hardware: bool,
    require_resident: bool,
    audit: Arc<Audit>,
}

impl FramePlayer for AuditedPlayer {
    fn next_picture_deadline(&self) -> Option<Instant> {
        self.player.next_picture_deadline()
    }
    fn retry_picture(&mut self) {
        self.player.retry_picture();
    }
    fn pingpong_cache_truncated(&self) -> bool {
        self.player.pingpong_cache_truncated()
    }
    fn next_decoded_frame(&mut self, pool: &Mutex<Vec<Vec<u8>>>) -> Result<Option<DecodedFrame>> {
        let result = self.player.next_decoded_frame(pool);
        let frame = match result {
            Ok(frame) => frame,
            Err(error) => {
                self.audit.failed.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        if frame.is_some() {
            #[cfg(target_os = "windows")]
            let resident = matches!(
                frame.as_ref().map(|f| &f.color_data),
                Some(DecodedPixels::Gpu(_))
            );
            #[cfg(not(target_os = "windows"))]
            let resident = false;
            let hardware = self.player.hardware_decode();
            if hardware != self.require_hardware || resident != self.require_resident {
                self.audit.failed.store(true, Ordering::Relaxed);
                anyhow::bail!("benchmark decoder/delivery path changed");
            }
            self.audit.produced.fetch_add(1, Ordering::Relaxed);
            if hardware {
                self.audit.hardware.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(frame)
    }
    fn delivery_counters(&self) -> Option<Arc<delivery::DeliveryCounters>> {
        self.player.delivery_counters()
    }
    fn gpu_resident(&self) -> bool {
        self.player.gpu_resident()
    }
    fn playback(&mut self) -> &mut PlaybackState {
        &mut self.player.playback
    }

    fn playback_state(&self) -> &PlaybackState {
        &self.player.playback
    }

    fn next_frame(&mut self) -> Result<Option<&[u8]>> {
        let ready = match self.player.next_frame() {
            Ok(frame) => frame.is_some(),
            Err(error) => {
                self.audit.failed.store(true, Ordering::Relaxed);
                return Err(error);
            }
        };
        if ready {
            let hardware = self.player.hardware_decode();
            if hardware != self.require_hardware {
                self.audit.failed.store(true, Ordering::Relaxed);
                anyhow::bail!("benchmark decoder changed backend");
            }
            self.audit.produced.fetch_add(1, Ordering::Relaxed);
            if hardware {
                self.audit.hardware.fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(ready.then_some(self.player.frame_data.as_slice()))
    }

    fn seek_and_reset(&mut self, time: f64) -> Result<()> {
        self.player.seek_and_reset(time)
    }

    fn width(&self) -> u32 {
        self.player.width()
    }
    fn height(&self) -> u32 {
        self.player.height()
    }
    fn hardware_decode(&self) -> bool {
        self.player.hardware_decode()
    }
}

#[test]
fn audited_player_preserves_deadline_and_retry_contract() {
    let path = Path::new("target/gpu-qualification/fixtures/continuous/h264-1080p60.mp4");
    if !path.exists() {
        return;
    }
    let mut player = VideoPlayer::open(path, false).unwrap();
    player.set_playing(false);
    player.seek_and_reset(0.0).unwrap();
    player.next_frame().unwrap().unwrap();
    player.set_playing(true);
    let expected = player
        .next_picture_deadline()
        .expect("prepared lookahead has a deadline");
    let mut audited = AuditedPlayer {
        player,
        require_hardware: false,
        require_resident: false,
        audit: Arc::default(),
    };
    assert_eq!(audited.next_picture_deadline(), Some(expected));
    audited.retry_picture();
    assert!(audited.player.retained_seek_frame);
    assert!(audited.player.playback.show_frame);
}

#[test]
#[ignore = "real-time multi-clip playback and offscreen rendering benchmark"]
fn realtime_measurement() {
    #[cfg(target_os = "windows")]
    let _timer_period = TimerPeriod::requested();
    let path = std::env::var_os("VARDA_VIDEO_BENCH_CLIP").expect("VARDA_VIDEO_BENCH_CLIP");
    let resident = std::env::var_os("VARDA_REQUIRE_GPU_RESIDENT").is_some();
    let hardware = resident || std::env::var_os("VARDA_REQUIRE_HW_DECODE").is_some();
    let count: usize = std::env::var("VARDA_VIDEO_BENCH_CLIPS")
        .unwrap_or_else(|_| "1".into())
        .parse()
        .unwrap();
    let seconds: f64 = std::env::var("VARDA_VIDEO_BENCH_SECONDS")
        .unwrap_or_else(|_| "10".into())
        .parse()
        .unwrap();
    assert!(count > 0 && seconds.is_finite() && seconds > 0.0);
    let gpu = crate::renderer::context::GpuContext::new_headless().unwrap();
    let pipeline =
        crate::renderer::blit::BlitPipeline::new(&gpu.device, wgpu::TextureFormat::Rgba8UnormSrgb)
            .unwrap();
    let mut handles = Vec::new();
    let mut audits = Vec::new();
    let mut textures = Vec::new();
    let mut groups = Vec::new();
    let mut extent = wgpu::Extent3d::default();
    let mut rate = 0.0;
    for _ in 0..count {
        let mut player = VideoPlayer::open(&path, hardware).unwrap();
        if resident {
            #[cfg(target_os = "windows")]
            player
                .try_enable_gpu(&gpu)
                .expect("GPU-resident playback required");
            #[cfg(not(target_os = "windows"))]
            panic!("GPU-resident benchmark currently requires Windows");
        }
        player.set_playing(false);
        player.seek_and_reset(0.0).unwrap();
        player.next_frame().unwrap().expect("initial decoded frame");
        assert_eq!(
            player.hardware_decode(),
            hardware,
            "hardware playback unavailable"
        );
        extent = wgpu::Extent3d {
            width: player.width(),
            height: player.height(),
            depth_or_array_layers: 1,
        };
        rate = player.frame_rate();
        player.set_playing(true);
        player.set_looping(true);
        let audit = Arc::new(Audit::default());
        let handle = VideoDecodeHandle::spawn_player(
            AuditedPlayer {
                player,
                require_hardware: hardware,
                require_resident: resident,
                audit: Arc::clone(&audit),
            },
            "video-benchmark",
        );
        audits.push(audit);
        handle.set_output_fps(rate.round() as u32);
        handles.push(handle);
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("playback source"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        groups.push(pipeline.create_bind_group(
            &gpu.device,
            &texture.create_view(&wgpu::TextureViewDescriptor::default()),
        ));
        textures.push(texture);
    }
    let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("playback output"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        view_formats: &[],
    });
    let target_view = target.create_view(&wgpu::TextureViewDescriptor::default());
    std::thread::sleep(Duration::from_secs(1));
    for handle in &handles {
        if let Some(frame) = handle.take_frame() {
            handle.recycle(frame);
        }
    }
    for audit in &audits {
        audit.produced.store(0, Ordering::Relaxed);
        audit.hardware.store(0, Ordering::Relaxed);
    }
    // Diagnostic controls affect this offscreen harness only. Production clocks
    // and the historical benchmark remain unchanged when unset.
    let phase_ms: f64 = std::env::var("NIDA_VIDEO_BENCH_PHASE_MS")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .unwrap();
    assert!((0.0..=100.0).contains(&phase_ms));
    std::thread::sleep(Duration::from_secs_f64(phase_ms / 1000.0));
    let poll_only = std::env::var_os("NIDA_VIDEO_BENCH_POLL_ONLY").is_some();
    let warmup_delivery: Vec<_> = handles
        .iter()
        .map(|h| {
            h.delivery
                .as_ref()
                .map(|c| c.snapshot())
                .unwrap_or_default()
        })
        .collect();
    let pid = sysinfo::get_current_pid().unwrap();
    let mut system = sysinfo::System::new();
    let refresh = |system: &mut sysinfo::System| {
        system.refresh_processes_specifics(
            sysinfo::ProcessesToUpdate::Some(&[pid]),
            true,
            sysinfo::ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory(),
        );
    };
    refresh(&mut system);
    let cpu_start = system.process(pid).unwrap().accumulated_cpu_time();
    let mut peak_rss = 0;
    let mut delivered = vec![0_usize; count];
    let mut ticks = 0_u32;
    let mut late = 0;
    let mut service = Vec::new();
    let period = Duration::from_secs_f64(1.0 / rate);
    let started = Instant::now();
    let historical_sampling =
        std::env::var("NIDA_VIDEO_BENCH_HISTORICAL_SAMPLING").is_ok_and(|v| v == "1");
    let mut tick_start_lateness = Vec::new();
    let mut sample_cost = Vec::new();
    pacing_trace::emit("measurement_begin", phase_ms, f64::from(poll_only));
    while started.elapsed().as_secs_f64() < seconds {
        let tick = Instant::now();
        tick_start_lateness.push(
            tick.saturating_duration_since(started + period.mul_f64(f64::from(ticks)))
                .as_secs_f64()
                * 1000.0,
        );
        pacing_trace::emit(
            "render_tick",
            f64::from(ticks),
            started.elapsed().as_secs_f64(),
        );
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        let mut readiness = FrameReadiness::new(rate.round() as u32, false);
        for (index, handle) in handles.iter().enumerate() {
            if let Some(frame) = handle.take_ready_frame_with_readiness(true, &mut readiness) {
                assert_eq!(
                    handle.playback_snapshot().hardware_decode,
                    hardware,
                    "mid-playback fallback"
                );
                match frame.color_data {
                    #[cfg(target_os = "windows")]
                    DecodedPixels::Gpu(native) => native.encode(&mut encoder, &textures[index]),
                    DecodedPixels::Cpu(ref pixels) => {
                        gpu.queue.write_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &textures[index],
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            pixels,
                            wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(extent.width * 4),
                                rows_per_image: Some(extent.height),
                            },
                            extent,
                        );
                        handle.recycle(frame);
                    }
                }
                delivered[index] += 1;
            }
        }
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("concurrent playback blits"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target_view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            for group in &groups {
                pipeline.render(&mut pass, group);
            }
        }
        encoder.on_submitted_work_done(move || {
            pacing_trace::emit("render_complete", f64::from(ticks), 0.0);
        });
        gpu.submit([encoder.finish()]);
        gpu.device
            .poll(if poll_only {
                wgpu::PollType::Poll
            } else {
                wgpu::PollType::wait_indefinitely()
            })
            .unwrap();
        service.push(tick.elapsed().as_secs_f64() * 1000.0);
        ticks += 1;
        let deadline = started + period.mul_f64(f64::from(ticks));
        if Instant::now() > deadline {
            late += 1;
        }
        if !historical_sampling && ticks.is_multiple_of(30) {
            let sampled_at = Instant::now();
            refresh(&mut system);
            peak_rss = peak_rss.max(system.process(pid).unwrap().memory());
            sample_cost.push(sampled_at.elapsed().as_secs_f64() * 1000.0);
        }
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            pacing_trace::emit("render_wait", wait.as_secs_f64(), f64::from(ticks));
            std::thread::sleep(wait);
        }
        if historical_sampling && ticks.is_multiple_of(30) {
            let sampled_at = Instant::now();
            refresh(&mut system);
            peak_rss = peak_rss.max(system.process(pid).unwrap().memory());
            sample_cost.push(sampled_at.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    pacing_trace::emit("measurement_end", elapsed, f64::from(ticks));
    let measured_end_delivery: Vec<_> = handles
        .iter()
        .map(|h| {
            h.delivery
                .as_ref()
                .map(|c| c.snapshot())
                .unwrap_or_default()
        })
        .collect();
    refresh(&mut system);
    let process = system.process(pid).unwrap();
    let cpu_ms = process.accumulated_cpu_time() - cpu_start;
    let produced: Vec<_> = audits
        .iter()
        .map(|audit| audit.produced.load(Ordering::Relaxed))
        .collect();
    let hardware_frames: Vec<_> = audits
        .iter()
        .map(|audit| audit.hardware.load(Ordering::Relaxed))
        .collect();
    let delivery: Vec<_> = handles
        .iter()
        .map(|h| h.playback_snapshot().delivery)
        .collect();
    if resident {
        for counters in &delivery {
            assert!(counters.gpu_frames > 0, "GPU-resident frames required");
            assert_eq!(counters.downloaded_bytes, 0, "unexpected frame download");
            assert_eq!(counters.uploaded_bytes, 0, "unexpected frame upload");
            assert!(counters.peak_slots <= 3);
        }
    }
    drop(handles);
    gpu.device
        .poll(wgpu::PollType::wait_indefinitely())
        .unwrap();
    assert!(gpu.errors.take_faults().is_empty(), "GPU validation fault");
    pacing_trace::flush();
    assert!(
        audits
            .iter()
            .all(|audit| !audit.failed.load(Ordering::Relaxed)),
        "decode error or software fallback during measurement"
    );
    assert!(
        delivered.iter().all(|frames| *frames > 0),
        "a decoder delivered no frames"
    );
    service.sort_by(f64::total_cmp);
    tick_start_lateness.sort_by(f64::total_cmp);
    sample_cost.sort_by(f64::total_cmp);
    let missing: Vec<_> = delivered
        .iter()
        .map(|frames| (ticks as usize).saturating_sub(*frames))
        .collect();
    eprintln!(
        "VIDEO_PLAYBACK {}",
        serde_json::json!({
            "clip": path.to_string_lossy(), "hardware_required": hardware, "clips": count,
            "gpu_resident_required": resident, "delivery_per_clip": delivery,
            "warmup_delivery_per_clip": warmup_delivery,
            "measured_end_delivery_per_clip": measured_end_delivery,
            "diagnostic_phase_ms": phase_ms, "diagnostic_poll_only": poll_only,
            "diagnostic_timer_1ms": std::env::var_os("NIDA_VIDEO_BENCH_TIMER_1MS").is_some(),
            "renderer_adapter": format!("{:?}", gpu.adapter.get_info()),
            "wall_s": elapsed, "source_fps": rate, "render_ticks": ticks,
            "delivered_per_clip": delivered, "empty_output_ticks_per_clip": missing,
        "produced_per_clip": produced, "hardware_frames_per_clip": hardware_frames,
            "late_render_ticks": late,
            "historical_sampling": historical_sampling,
            "readiness_disabled": std::env::var("NIDA_VIDEO_BENCH_NO_READINESS").is_ok_and(|v| v == "1"),
            "tick_start_lateness_p95_ms": tick_start_lateness[((tick_start_lateness.len() - 1) as f64 * 0.95).ceil() as usize],
            "tick_start_lateness_max_ms": tick_start_lateness.last(),
            "process_sample_costs_ms": sample_cost,
            "tick_service_p50_ms": service[service.len() / 2],
            "tick_service_p95_ms": service[((service.len() - 1) as f64 * 0.95).ceil() as usize],
            "cpu_ms": cpu_ms, "cpu_core_percent": cpu_ms as f64 / (elapsed * 10.0),
            "sampled_peak_rss_bytes": peak_rss.max(process.memory())
        })
    );
}
