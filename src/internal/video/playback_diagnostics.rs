use super::*;
use std::time::{Duration, Instant};

#[derive(Default)]
struct Audit {
    produced: std::sync::atomic::AtomicUsize,
    hardware: std::sync::atomic::AtomicUsize,
    failed: AtomicBool,
}

struct AuditedPlayer {
    player: VideoPlayer,
    require_hardware: bool,
    audit: Arc<Audit>,
}

impl FramePlayer for AuditedPlayer {
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
#[ignore = "real-time multi-clip playback and offscreen rendering benchmark"]
fn realtime_measurement() {
    let path = std::env::var_os("VARDA_VIDEO_BENCH_CLIP").expect("VARDA_VIDEO_BENCH_CLIP");
    let hardware = std::env::var_os("VARDA_REQUIRE_HW_DECODE").is_some();
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
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
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
    while started.elapsed().as_secs_f64() < seconds {
        let tick = Instant::now();
        for (index, handle) in handles.iter().enumerate() {
            if let Some(frame) = handle.take_frame() {
                assert_eq!(
                    handle.playback_snapshot().hardware_decode,
                    hardware,
                    "mid-playback fallback"
                );
                gpu.queue.write_texture(
                    wgpu::TexelCopyTextureInfo {
                        texture: &textures[index],
                        mip_level: 0,
                        origin: wgpu::Origin3d::ZERO,
                        aspect: wgpu::TextureAspect::All,
                    },
                    &frame.color_data,
                    wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(extent.width * 4),
                        rows_per_image: Some(extent.height),
                    },
                    extent,
                );
                delivered[index] += 1;
                handle.recycle(frame);
            }
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
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
        gpu.queue.submit([encoder.finish()]);
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .unwrap();
        service.push(tick.elapsed().as_secs_f64() * 1000.0);
        ticks += 1;
        let deadline = started + period.mul_f64(f64::from(ticks));
        if Instant::now() > deadline {
            late += 1;
        }
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
        if ticks % 30 == 0 {
            refresh(&mut system);
            peak_rss = peak_rss.max(system.process(pid).unwrap().memory());
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
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
    drop(handles);
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
    let missing: Vec<_> = delivered
        .iter()
        .map(|frames| (ticks as usize).saturating_sub(*frames))
        .collect();
    eprintln!(
        "VIDEO_PLAYBACK {}",
        serde_json::json!({
            "clip": path.to_string_lossy(), "hardware_required": hardware, "clips": count,
            "renderer_adapter": format!("{:?}", gpu.adapter.get_info()),
            "wall_s": elapsed, "source_fps": rate, "render_ticks": ticks,
            "delivered_per_clip": delivered, "empty_output_ticks_per_clip": missing,
        "produced_per_clip": produced, "hardware_frames_per_clip": hardware_frames,
            "late_render_ticks": late,
            "tick_service_p50_ms": service[service.len() / 2],
            "tick_service_p95_ms": service[((service.len() - 1) as f64 * 0.95).ceil() as usize],
            "cpu_ms": cpu_ms, "cpu_core_percent": cpu_ms as f64 / (elapsed * 10.0),
            "sampled_peak_rss_bytes": peak_rss.max(process.memory())
        })
    );
}
