//! A measurement of the Mac's media engine as Nida uses it: one and two
//! H.264 encoders at once — the stream's and a recording's, each on a thread
//! of its own, as live — fed moving 4K frames as fast as they take them, or
//! at the show's rate, with several frames in flight and the engine's speed
//! hint on or off. Nothing else runs: no show, no GPU, no ffmpeg — so what
//! it finds is the engine's, and what Nida's encoder does around it.
//!
//! Run with `nida --bench-encoders` on a Mac with nothing on air: it keeps
//! the media engine busy for about two minutes.

use std::collections::HashMap;
use std::sync::{Arc, Barrier, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::vt_encoder::{Encoded, Stages, Tuning, VtEncoder};
use super::VideoCodec;

const WIDTH: u32 = 3840;
const HEIGHT: u32 = 2160;
const FPS: u32 = 60;
/// Not counted: the engine settling.
const WARM_UP: Duration = Duration::from_secs(2);
/// Counted.
const MEASURED: Duration = Duration::from_secs(8);
/// No frame out for this long: the engine has stalled, the case is given up.
const STALLED: Duration = Duration::from_secs(5);

/// One way of running the encoders.
struct Case {
    label: &'static str,
    codec: VideoCodec,
    /// Each encoder's rate: kbps, held constant or averaged.
    encoders: &'static [(u32, bool)],
    /// Frames each may hold at once (Nida's writer allows 12 at 60 fps).
    room: u64,
    /// The frame rate the engine is told to expect; none, not told (as
    /// Nida now does, and ffmpeg).
    expected: Option<u32>,
    /// The engine asked to put speed before quality.
    speed: bool,
    /// Fed at the show's rate, as live; else as fast as it takes them.
    paced: bool,
}

/// The stream as measured (20000 kbps held constant) and a recording
/// (8000 kbps averaged).
const STREAM: (u32, bool) = (20_000, true);
const RECORDING: (u32, bool) = (8_000, false);
const BOTH: &[(u32, bool)] = &[STREAM, RECORDING];

const fn case(
    label: &'static str,
    encoders: &'static [(u32, bool)],
    expected: Option<u32>,
) -> Case {
    Case {
        label,
        codec: VideoCodec::H264,
        encoders,
        room: 12,
        expected,
        speed: false,
        paced: false,
    }
}

const CASES: &[Case] = &[
    case(
        "stream alone, told 60 fps (Nida until now)",
        &[STREAM],
        Some(60),
    ),
    case("stream alone, not told a rate", &[STREAM], None),
    case(
        "stream + recording, told 60 fps (Nida until now)",
        BOTH,
        Some(60),
    ),
    case("stream + recording, told 59 fps", BOTH, Some(59)),
    case("stream + recording, not told a rate", BOTH, None),
    Case {
        paced: true,
        ..case(
            "stream + recording at 60 fps as live, told 60",
            BOTH,
            Some(60),
        )
    },
    Case {
        paced: true,
        ..case(
            "stream + recording at 60 fps as live, not told a rate",
            BOTH,
            None,
        )
    },
    Case {
        room: 24,
        ..case("stream + recording, told 60, 24 in flight", BOTH, Some(60))
    },
    Case {
        speed: true,
        ..case(
            "stream + recording, not told a rate, speed before quality",
            BOTH,
            None,
        )
    },
    case(
        "three encoders, not told a rate",
        &[STREAM, RECORDING, RECORDING],
        None,
    ),
    Case {
        codec: VideoCodec::Hevc,
        ..case("HEVC stream + recording, not told a rate", BOTH, None)
    },
];

/// What one encoder did in the counted time.
#[derive(Default)]
struct Measured {
    /// Frames submitted, and those that came out encoded, dropped or failed.
    submitted: u64,
    encoded: u64,
    dropped: u64,
    failed: Option<String>,
    /// Submitted to out, each frame.
    latency: Vec<Duration>,
    /// Inside `encode`, each frame.
    stages: Vec<Stages>,
    /// Waiting for room in the engine.
    waited: Duration,
}

/// Run every case; the report, line by line.
pub fn run() -> String {
    let started = Instant::now();
    let frames = Arc::new(synthetic(WIDTH as usize, HEIGHT as usize, 8));
    let mut report = vec![format!(
        "Media engine bench: {WIDTH}x{HEIGHT} at {FPS} fps, {} s counted after {} s, \
         each encoder on its own thread",
        MEASURED.as_secs(),
        WARM_UP.as_secs()
    )];
    for case in CASES {
        report.push(String::new());
        report.push(format!("{}:", case.label));
        let measured = run_case(case, &frames);
        for (n, (m, (kbps, constant))) in measured.iter().zip(case.encoders).enumerate() {
            report.push(describe(n + 1, *kbps, *constant, m));
        }
        if measured.len() > 1 {
            let total: u64 = measured.iter().map(|m| m.encoded).sum();
            report.push(format!("  together {:.1} fps", per_second(total)));
        }
    }
    report.push(String::new());
    report.push(format!("Done in {:.0} s.", started.elapsed().as_secs_f64()));
    report.join("\n")
}

fn per_second(frames: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let frames = frames as f64;
    frames / MEASURED.as_secs_f64()
}

/// One encoder's line: its rate out, what was lost, how long frames took,
/// where `encode` spent its time, and how long it waited for room.
fn describe(n: usize, kbps: u32, constant: bool, m: &Measured) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let pick = |mut v: Vec<Duration>, q: f64| -> f64 {
        if v.is_empty() {
            return 0.0;
        }
        v.sort();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let at = ((v.len() - 1) as f64 * q).round() as usize;
        ms(v[at])
    };
    let latency = |q| pick(m.latency.clone(), q);
    let stage = |f: fn(&Stages) -> Duration, q| pick(m.stages.iter().map(f).collect(), q);
    let measured_ms = ms(MEASURED);
    let mut line = format!(
        "  encoder {n} ({kbps} kbps {}): {:.1} fps out of {:.1} in, {} dropped; \
         in to out p50 {:.1} ms, p95 {:.1}, max {:.1}; encode() pool {:.2}/{:.2} ms, \
         copy {:.2}/{:.2}, engine call {:.2}/{:.2} (p50/p95); waited for room {:.0}% of the time",
        if constant { "constant" } else { "average" },
        per_second(m.encoded),
        per_second(m.submitted),
        m.dropped,
        latency(0.5),
        latency(0.95),
        latency(1.0),
        stage(|s| s.pool, 0.5),
        stage(|s| s.pool, 0.95),
        stage(|s| s.copy, 0.5),
        stage(|s| s.copy, 0.95),
        stage(|s| s.call, 0.5),
        stage(|s| s.call, 0.95),
        ms(m.waited) / measured_ms * 100.0,
    );
    if let Some(why) = &m.failed {
        line.push_str("; FAILED: ");
        line.push_str(why);
    }
    line
}

/// Run `case`'s encoders at once, each fed on its own thread.
fn run_case(case: &Case, frames: &Arc<Vec<Vec<u8>>>) -> Vec<Measured> {
    let barrier = Arc::new(Barrier::new(case.encoders.len()));
    let threads: Vec<_> = case
        .encoders
        .iter()
        .map(|&rate| {
            let (barrier, frames) = (Arc::clone(&barrier), Arc::clone(frames));
            let how = How {
                codec: case.codec,
                room: case.room,
                expected: case.expected,
                speed: case.speed,
                paced: case.paced,
            };
            std::thread::spawn(move || feed(rate, how, &frames, &barrier))
        })
        .collect();
    threads
        .into_iter()
        .map(|t| {
            t.join().unwrap_or_else(|_| Measured {
                failed: Some("its thread panicked".into()),
                ..Measured::default()
            })
        })
        .collect()
}

/// How one encoder of a case runs.
#[derive(Clone, Copy)]
struct How {
    codec: VideoCodec,
    room: u64,
    expected: Option<u32>,
    speed: bool,
    paced: bool,
}

/// One encoder, fed for the warm-up and the counted time.
fn feed(
    (kbps, constant): (u32, bool),
    how: How,
    frames: &[Vec<u8>],
    barrier: &Barrier,
) -> Measured {
    let How {
        codec,
        room,
        expected,
        speed,
        paced,
    } = how;
    let (tx, rx) = crossbeam_channel::unbounded();
    let tuning = Tuning {
        expected_fps: expected,
    };
    let made = VtEncoder::tuned(codec, (WIDTH, HEIGHT), FPS, (kbps, constant), 2, tx, tuning)
        .and_then(|mut e| {
            if speed {
                e.prioritize_speed()?;
            }
            Ok(e)
        });
    let mut encoder = match made {
        Ok(encoder) => encoder,
        Err(e) => {
            barrier.wait();
            return Measured {
                failed: Some(format!("{e:#}")),
                ..Measured::default()
            };
        }
    };
    // When each frame went in, and whether it counts; read as it comes out.
    let submitted: Arc<Mutex<HashMap<u64, (Instant, bool)>>> = Arc::default();
    let out = {
        let submitted = Arc::clone(&submitted);
        std::thread::spawn(move || {
            let mut m = Measured::default();
            for event in rx {
                let now = Instant::now();
                let slot = match &event {
                    Encoded::Frame { slot, .. } | Encoded::Dropped { slot } => *slot,
                    Encoded::Failed(why) => {
                        m.failed.get_or_insert_with(|| why.clone());
                        continue;
                    }
                };
                let went = submitted
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .remove(&slot);
                let Some((at, counts)) = went else {
                    continue;
                };
                if !counts {
                    continue;
                }
                match event {
                    Encoded::Frame { .. } => {
                        m.encoded += 1;
                        m.latency.push(now - at);
                    }
                    _ => m.dropped += 1,
                }
            }
            m
        })
    };
    let mut stages = Vec::new();
    let (mut waited, mut fed) = (Duration::ZERO, 0u64);
    barrier.wait();
    let start = Instant::now();
    let mut slot = 0u64;
    let mut failed = None;
    loop {
        let since = start.elapsed();
        if since >= WARM_UP + MEASURED {
            break;
        }
        let counts = since >= WARM_UP;
        if paced {
            #[allow(clippy::cast_precision_loss)]
            let due = start + Duration::from_secs_f64(slot as f64 / f64::from(FPS));
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
        }
        let asked = Instant::now();
        while encoder.in_flight() >= room && !encoder.failed() {
            if asked.elapsed() > STALLED {
                failed = Some(format!("no frame out for {} s", STALLED.as_secs()));
                break;
            }
            std::thread::sleep(Duration::from_micros(200));
        }
        if failed.is_some() || encoder.failed() {
            failed.get_or_insert_with(|| "the media engine failed".into());
            break;
        }
        if counts {
            waited += asked.elapsed();
        }
        let at = Instant::now();
        submitted
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(slot, (at, counts));
        #[allow(clippy::cast_possible_truncation)]
        let frame = &frames[slot as usize % frames.len()];
        if let Err(e) = encoder.encode(frame, slot) {
            failed = Some(format!("{e:#}"));
            break;
        }
        if counts {
            stages.push(encoder.last_stages());
            fed += 1;
        }
        slot += 1;
    }
    encoder.finish();
    drop(encoder);
    let mut m = out.join().unwrap_or_default();
    m.submitted = fed;
    m.stages = stages;
    m.waited = waited;
    if failed.is_some() {
        m.failed = failed;
    }
    m
}

/// `count` NV12 frames that move and have detail everywhere — a gradient
/// with noise over it — for the engine to work at as at a show.
fn synthetic(width: usize, height: usize, count: usize) -> Vec<Vec<u8>> {
    let mut seed = 0x2545_f491_u32;
    (0..count)
        .map(|n| {
            let mut nv12 = vec![128u8; width * height * 3 / 2];
            let (luma, chroma) = nv12.split_at_mut(width * height);
            for (i, px) in luma.iter_mut().enumerate() {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                let (x, y) = (i % width, i / width);
                #[allow(clippy::cast_possible_truncation)] // Wrapped on purpose.
                let ramp = ((x + y) / 8 + n * 24) as u8;
                *px = ramp.wrapping_add((seed & 0x1f) as u8);
            }
            for (i, px) in chroma.iter_mut().enumerate() {
                #[allow(clippy::cast_possible_truncation)]
                let wave = ((i / 2 % width) / 16 + n * 8) as u8;
                *px = 96u8.wrapping_add(wave % 64);
            }
            nv12
        })
        .collect()
}
