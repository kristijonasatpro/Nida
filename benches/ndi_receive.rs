//! Serialized receive-side microbenchmark: SDK-frame copy, CPU fallback or GPU
//! expansion, upload and GPU completion. Excludes NDI networking/codec work and
//! pipeline creation; does not measure application frame rate.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use varda::ndi::ffi;
use varda::renderer;

#[path = "../src/internal/ndi/color.rs"]
mod color;
use color::uyvy_to_rgba;
#[allow(dead_code)]
#[path = "../src/internal/ndi/receive.rs"]
mod receive;

fn bench_receive(c: &mut Criterion) {
    let gpu = renderer::GpuContext::new_headless().expect("native GPU required");
    eprintln!("NDI receive adapter: {:?}", gpu.adapter.get_info());
    let mut group = c.benchmark_group("ndi_receive");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    for (width, height) in [(1920, 1080), (3840, 2160)] {
        let mut bytes = [90_u8, 100, 200, 140].repeat(width * height / 2);
        let frame = ffi::NDIlib_video_frame_v2_t {
            xres: i32::try_from(width).expect("fixture width"),
            yres: i32::try_from(height).expect("fixture height"),
            FourCC: ffi::NDIlib_FourCC_video_type_e::UYVY,
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 0.0,
            frame_format_type: 1,
            timecode: 0,
            p_data: bytes.as_mut_ptr(),
            line_stride_in_bytes: i32::try_from(width * 2).expect("fixture stride"),
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        for cpu in [true, false] {
            let slot = receive::FrameSlot::new(cpu);
            let mut target = receive::ReceiveTarget::new(
                &gpu.device,
                "NDI benchmark",
                width as u32,
                height as u32,
            );
            let mut unpacker = receive::UyvyUnpacker::try_new(&gpu.device);
            assert!(unpacker.is_some());
            group.bench_function(
                BenchmarkId::new(if cpu { "cpu_fallback" } else { "gpu_uyvy" }, width),
                |b| {
                    b.iter(|| {
                        assert!(slot.deposit(&frame));
                        let mut encoder = None;
                        assert!(target.update(
                            &slot,
                            &gpu.device,
                            &gpu.queue,
                            &mut unpacker,
                            &mut encoder,
                        ));
                        // Also flush write_texture in the CPU path.
                        gpu.submit(encoder.into_iter().map(wgpu::CommandEncoder::finish));
                        gpu.device
                            .poll(wgpu::PollType::wait_indefinitely())
                            .expect("GPU completion");
                    });
                },
            );
        }
    }
    group.finish();
}

criterion_group!(benches, bench_receive);
criterion_main!(benches);
