use super::*;
use std::time::Instant;

fn millis(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).ceil() as usize]
}

#[test]
#[ignore = "explicit media benchmark; see docs/windows-gpu-decoding.md"]
fn pipeline_measurement() {
    let path = std::env::var_os("VARDA_VIDEO_BENCH_CLIP").expect("VARDA_VIDEO_BENCH_CLIP");
    let hardware = std::env::var_os("VARDA_REQUIRE_HW_DECODE").is_some();
    let upload = std::env::var_os("VARDA_VIDEO_BENCH_UPLOAD").is_some();
    let mut player = VideoPlayer::open(&path, hardware).unwrap();
    let gpu = upload.then(|| crate::renderer::context::GpuContext::new_headless().unwrap());
    let extent = wgpu::Extent3d {
        width: player.width,
        height: player.height,
        depth_or_array_layers: 1,
    };
    let texture = gpu.as_ref().map(|gpu| {
        gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("video diagnostic upload"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
    });
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
    let mut stages = [0.0; 5];
    let mut latencies = Vec::new();
    let mut worker = vec![0; player.frame_byte_size];
    let mut hardware_frames = 0;
    let mut missed = 0;
    let mut previous = f64::NEG_INFINITY;
    let mut downloads = std::collections::HashSet::new();
    let started = Instant::now();
    loop {
        let frame_start = Instant::now();
        let stage = Instant::now();
        let Some(at) = player.decode_ahead().unwrap() else {
            break;
        };
        stages[0] += millis(stage);
        assert!(
            at > previous,
            "non-increasing presentation timestamp {at} after {previous}"
        );
        previous = at;
        std::mem::swap(&mut player.decoded, &mut player.lookahead);
        let on_engine = hwdecode::on_engine(&player.decoded);
        assert_eq!(
            on_engine, hardware,
            "requested hardware benchmark fell back to software"
        );
        let stage = Instant::now();
        if on_engine {
            hardware_frames += 1;
            hwdecode::to_memory(&player.decoded, &mut player.engine_frame).unwrap();
            downloads.insert(player.engine_frame.frame.data(0).as_ptr() as usize);
            std::mem::swap(&mut player.decoded, &mut player.engine_frame.frame);
        }
        stages[1] += millis(stage);
        let stage = Instant::now();
        player.convert_decoded().unwrap();
        stages[2] += millis(stage);
        let stage = Instant::now();
        let data = player.rgb_frame.data(0);
        let stride = player.rgb_frame.stride(0);
        if let Some(tone) = player.tone.as_ref().filter(|_| player.hdr_frame) {
            tone.apply(data, stride, player.scaled, &mut player.sdr);
            turn_into(
                &player.sdr,
                player.scaled.0 as usize * 4,
                player.scaled,
                player.quarter_turns,
                &mut player.frame_data,
            );
        } else {
            turn_into(
                data,
                stride,
                player.scaled,
                player.quarter_turns,
                &mut player.frame_data,
            );
        }
        if player.interlaced_frame && player.quarter_turns.is_multiple_of(2) {
            blend_fields(&mut player.frame_data, player.width as usize * 4);
        }
        worker.copy_from_slice(&player.frame_data);
        std::hint::black_box(&worker);
        stages[3] += millis(stage);
        if on_engine {
            std::mem::swap(&mut player.decoded, &mut player.engine_frame.frame);
        }
        let stage = Instant::now();
        if let (Some(gpu), Some(texture)) = (&gpu, &texture) {
            gpu.queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &worker,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(player.width * 4),
                    rows_per_image: Some(player.height),
                },
                extent,
            );
            gpu.queue.submit([]);
            gpu.device
                .poll(wgpu::PollType::wait_indefinitely())
                .unwrap();
        }
        stages[4] += millis(stage);
        let latency = millis(frame_start);
        missed += usize::from(latency > 1000.0 / player.frame_rate());
        latencies.push(latency);
        if latencies.len() % 30 == 0 {
            refresh(&mut system);
            peak_rss = peak_rss.max(system.process(pid).unwrap().memory());
        }
    }
    let elapsed = started.elapsed().as_secs_f64();
    assert!(!latencies.is_empty(), "no frames decoded");
    refresh(&mut system);
    let process = system.process(pid).unwrap();
    peak_rss = peak_rss.max(process.memory());
    let cpu_ms = process.accumulated_cpu_time() - cpu_start;
    eprintln!(
        "VIDEO_PIPELINE {}",
        serde_json::json!({
            "clip": path.to_string_lossy(), "hardware_frames": hardware_frames,
            "frames": latencies.len(), "upload_and_wait": upload,
            "fps": latencies.len() as f64 / elapsed, "wall_s": elapsed,
            "p50_ms": percentile(&latencies, 0.50), "p95_ms": percentile(&latencies, 0.95),
            "over_frame_budget": missed, "source_fps": player.frame_rate(),
            "cpu_ms": cpu_ms, "cpu_core_percent": cpu_ms as f64 / (elapsed * 10.0),
            "sampled_peak_rss_bytes": peak_rss, "download_addresses": downloads.len(),
            "stage_total_ms": {"demux_decode": stages[0], "download": stages[1],
                "conversion": stages[2], "pack_tonemap_worker_copy": stages[3], "upload_wait": stages[4]}
        })
    );
}
