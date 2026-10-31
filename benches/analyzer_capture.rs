//! Native GPU readback cost, including submission, completion, mapping and CPU
//! row packing. Serial completion makes this a transfer microbenchmark, not a
//! measurement of the application's pipelined frame rate. Pipeline creation is
//! outside the timed region. Requires a working GPU.

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use std::time::Duration;
use varda::renderer::{self, GpuContext, ReadbackBuffer, ReadbackFormat, ReadbackFrame};

// Exercise the production reduction pass without making the internal analyzer
// plugin API public solely for benchmarking.
#[allow(dead_code, unused_imports)]
#[path = "../src/internal/analyzer/capture.rs"]
mod capture;

fn completed(gpu: &GpuContext, mut read: impl FnMut() -> Option<ReadbackFrame>) {
    loop {
        if let Some(frame) = read() {
            black_box(frame.bytes());
            return;
        }
        gpu.device
            .poll(wgpu::PollType::wait_indefinitely())
            .expect("GPU completion");
    }
}

fn bench_capture(c: &mut Criterion) {
    let gpu = GpuContext::new_headless().expect("native GPU required for capture benchmark");
    eprintln!("Analyzer capture adapter: {:?}", gpu.adapter.get_info());
    let mut group = c.benchmark_group("analyzer_capture");
    group.sample_size(20);
    group.warm_up_time(Duration::from_millis(500));
    group.measurement_time(Duration::from_secs(2));
    for (width, height) in [(1920, 1080), (3840, 2160)] {
        let source = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("capture benchmark source"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let mut baseline = ReadbackBuffer::new(&gpu.device, width, height, ReadbackFormat::Rgba8);
        group.throughput(Throughput::Bytes(u64::from(width) * u64::from(height) * 4));
        group.bench_function(BenchmarkId::new("full_rgba8", width), |b| {
            b.iter(|| {
                let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                baseline.begin_readback(&mut encoder, &source, std::time::Instant::now());
                gpu.submit([encoder.finish()]);
                completed(&gpu, || baseline.try_read(&gpu.device));
            });
        });
        for long_edge in [256, 1920] {
            let size = capture::capture_size((width, height), long_edge);
            let mut reduced = capture::FrameCapture::new(&gpu.device, (width, height), size);
            group.throughput(Throughput::Bytes(u64::from(size.0) * u64::from(size.1) * 4));
            group.bench_function(BenchmarkId::new(format!("edge_{long_edge}"), width), |b| {
                b.iter(|| {
                    let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
                    reduced.encode(&gpu.device, &mut encoder, &source);
                    gpu.submit([encoder.finish()]);
                    completed(&gpu, || reduced.try_read(&gpu.device));
                });
            });
        }
    }
    group.finish();
}

criterion_group!(benches, bench_capture);
criterion_main!(benches);
