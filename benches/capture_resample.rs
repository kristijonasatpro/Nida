//! CPU capture resizing; does not open a display or capture session.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;
use varda::screen_capture::resample::downscale;

fn capture_resample(c: &mut Criterion) {
    let mut group = c.benchmark_group("capture_resample");
    for (sw, sh, dw, dh) in [
        (2560u32, 1080u32, 1920u32, 810u32),
        (3840, 2160, 1920, 1080),
    ] {
        let source: Vec<u8> = (0..sw as usize * sh as usize * 4)
            .map(|i| ((i.wrapping_mul(37).wrapping_add(i / 997)) & 255) as u8)
            .collect();
        group.throughput(Throughput::Elements(u64::from(sw) * u64::from(sh)));
        group.bench_with_input(
            BenchmarkId::new("box", format!("{sw}x{sh}_to_{dw}x{dh}")),
            &source,
            |b, source| b.iter(|| black_box(downscale(black_box(source), sw, sh, dw, dh))),
        );
    }
    group.finish();
}

criterion_group!(benches, capture_resample);
criterion_main!(benches);
