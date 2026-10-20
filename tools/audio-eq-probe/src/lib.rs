#![allow(
    dead_code,
    reason = "Tests reuse production modules without constructing the full app"
)]

pub const BLOCK_FRAMES: usize = 480;
pub const SAMPLE_RATE: u32 = 48_000;

#[path = "../../../src/internal/audio_mix/dynamics.rs"]
mod dynamics;
#[path = "../../../src/internal/audio_mix/equalizer.rs"]
mod equalizer;
#[path = "../../../src/internal/audio_mix/reader.rs"]
mod reader;

#[cfg(test)]
mod measurements {
    use super::*;
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    struct Allocations;
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static MEASURING: AtomicBool = AtomicBool::new(false);

    unsafe impl GlobalAlloc for Allocations {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            if MEASURING.load(Ordering::Relaxed) {
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            System.alloc(layout)
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            if MEASURING.load(Ordering::Relaxed) {
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            System.alloc_zeroed(layout)
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            if MEASURING.load(Ordering::Relaxed) {
                COUNT.fetch_add(1, Ordering::Relaxed);
            }
            System.realloc(pointer, layout, size)
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            System.dealloc(pointer, layout);
        }
    }

    #[global_allocator]
    static ALLOCATOR: Allocations = Allocations;

    #[test]
    #[ignore = "manual release-profile EQ-only service-time probe"]
    fn equalizer_bank_budget_probe() {
        let enabled = std::env::var_os("NIDA_AUDIO_PROBE_EQ").is_some();
        let blocks: usize = std::env::var("NIDA_AUDIO_PROBE_BLOCKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(5000);
        assert!(blocks >= 100);
        for count in [1, 8, 16, 32] {
            let config = if enabled {
                equalizer::fixture_config()
            } else {
                equalizer::EqualizerConfig::default()
            };
            let mut banks: Vec<_> = (0..count)
                .map(|_| {
                    let mut bank = equalizer::Equalizer::default();
                    drop(bank.configure(config, Some(equalizer::Processor::prepare(config))));
                    bank
                })
                .collect();
            let fixture = std::array::from_fn::<_, BLOCK_FRAMES, _>(|index| {
                let phase = std::f32::consts::TAU * index as f32 / 48.0;
                [0.02 * phase.sin(), 0.015 * (phase + 0.5).sin()]
            });
            let mut scratch = fixture;
            for _ in 0..100 {
                for bank in &mut banks {
                    scratch.copy_from_slice(&fixture);
                    bank.process(&mut scratch);
                }
            }
            let mut times = Vec::with_capacity(blocks);
            COUNT.store(0, Ordering::Relaxed);
            MEASURING.store(true, Ordering::Relaxed);
            for _ in 0..blocks {
                let start = std::time::Instant::now();
                for bank in &mut banks {
                    scratch.copy_from_slice(&fixture);
                    bank.process(&mut scratch);
                    std::hint::black_box(&scratch);
                }
                times.push(start.elapsed().as_secs_f64() * 1e6);
            }
            MEASURING.store(false, Ordering::Relaxed);
            let allocations = COUNT.load(Ordering::Relaxed);
            assert!(banks.iter().all(|bank| bank.snapshot().error.is_none()));
            if enabled {
                assert!(banks.iter().all(|bank| bank.snapshot().ready));
            }
            times.sort_by(f64::total_cmp);
            println!("{{\"enabled\":{enabled},\"equalizers\":{count},\"blocks\":{blocks},\"p50_us\":{:.3},\"p95_us\":{:.3},\"p99_us\":{:.3},\"max_us\":{:.3},\"service_overruns_10ms\":{},\"rust_allocations\":{allocations}}}",
                times[blocks/2],times[blocks*95/100],times[blocks*99/100],times[blocks-1],times.iter().filter(|time|**time>10_000.0).count());
        }
    }
}
