/// Audio onset and autocorrelation tempo-estimation benchmarks.
/// Tempo analysis runs on its own worker, outside the device callback.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use varda::audio::{compute_onset_threshold, estimate_bpm, TempoRange};

/// Realistic spectral flux values (positive, varying magnitude)
fn make_flux_history(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.01 + 0.05 * (i as f32 * 0.7).sin().abs())
        .collect()
}

/// A 120 BPM onset envelope sampled at the device hop rate.
fn make_envelope(seconds: usize) -> Vec<f32> {
    let rate = 48000.0 / 256.0;
    let mut envelope = vec![0.0; (seconds as f32 * rate) as usize];
    for beat in 0..seconds * 2 {
        let at = (beat as f32 * rate / 2.0).round() as usize;
        if let Some(v) = envelope.get_mut(at) {
            *v = 1.0;
        }
    }
    envelope
}

fn bench_onset_threshold(c: &mut Criterion) {
    let mut g = c.benchmark_group("onset_threshold");
    g.sample_size(1000);

    // ONSET_MEDIAN_WINDOW = 8, but test a range
    for n in [4, 8, 16] {
        let flux = make_flux_history(n);
        g.bench_with_input(BenchmarkId::new("analysis", n), &flux, |b, flux| {
            b.iter(|| compute_onset_threshold(flux));
        });
    }
    g.finish();
}

fn bench_bpm_estimation(c: &mut Criterion) {
    let mut g = c.benchmark_group("bpm_estimation");
    g.sample_size(100);

    // Increasing lengths of onset history.
    for n in [4, 6, 8] {
        let envelope = make_envelope(n);
        g.bench_with_input(
            BenchmarkId::new("autocorrelation_seconds", n),
            &envelope,
            |b, envelope| b.iter(|| estimate_bpm(envelope, 48000.0 / 256.0, TempoRange::default())),
        );
    }
    g.finish();
}

criterion_group!(benches, bench_onset_threshold, bench_bpm_estimation);
criterion_main!(benches);
