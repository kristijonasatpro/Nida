//! A measurement, not a test: how late a Blackmagic box's sound comes
//! through macOS (its `CoreAudio` device, as a camera input's sound is taken)
//! against the same sound coming with its pictures through Blackmagic's own
//! API. Both copies of the box's sound are taken at once, each placed on the
//! host clock by when it came, and their content lined up.
//!
//! `nida --blackmagic-sound-lag UltraStudio`, with sound going into the box
//! (talk, or clap) for the twelve seconds it listens, and no input on that
//! box open in Nida. On the main thread: Blackmagic's discovery tells of its
//! devices through the main run loop.

use super::input::{self, Capture, Picture, Taker};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const std::ffi::c_void;
    fn CFRunLoopRunInMode(mode: *const std::ffi::c_void, seconds: f64, after_one: u8) -> i32;
}

const RATE: f64 = 48_000.0;
const LISTEN: Duration = Duration::from_secs(12);
/// Loudness is compared in steps this long: 5 ms.
const STEP: usize = 240;

/// One copy of the sound: mono samples, and for each chunk that came, when
/// it came and how many samples had come by then.
#[derive(Default)]
struct Heard {
    samples: Vec<f32>,
    came: Vec<(f64, usize)>,
}

impl Heard {
    fn push(&mut self, at: f64, mono: impl Iterator<Item = f32>) {
        self.samples.extend(mono);
        self.came.push((at, self.samples.len()));
    }

    /// When sample `k` reached Nida: when the chunk holding it came.
    fn came_at(&self, k: usize) -> Option<f64> {
        let i = self.came.partition_point(|&(_, total)| total <= k);
        self.came.get(i).map(|&(at, _)| at)
    }

    /// Frames a chunk: least, middle, most.
    fn chunk_sizes(&self) -> (usize, usize, usize) {
        let mut sizes: Vec<usize> = self
            .came
            .iter()
            .scan(0, |before, &(_, total)| {
                let size = total - *before;
                *before = total;
                Some(size)
            })
            .collect();
        sizes.sort_unstable();
        match (sizes.first(), sizes.get(sizes.len() / 2), sizes.last()) {
            (Some(&a), Some(&m), Some(&z)) => (a, m, z),
            _ => (0, 0, 0),
        }
    }

    /// Loudness in steps of [`STEP`].
    fn envelope(&self) -> Vec<f64> {
        self.samples
            .as_chunks::<STEP>()
            .0
            .iter()
            .map(|c| (c.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / STEP as f64).sqrt())
            .collect()
    }
}

struct Keep {
    started: Instant,
    heard: Arc<Mutex<Heard>>,
}

impl Taker for Keep {
    fn picture(&mut self, _picture: &Picture<'_>) {}

    fn sound(&mut self, samples: &[i32], channels: usize) {
        let at = self.started.elapsed().as_secs_f64();
        let mono = samples.chunks_exact(channels).map(|f| {
            #[allow(clippy::cast_precision_loss)]
            let left = f[0] as f32 / 2_147_483_648.0;
            left
        });
        self.heard
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(at, mono);
    }
}

/// The lag, in steps, at which `late` best matches `early`, within
/// `±reach` steps, and how well (correlation, 0–1) — each lag judged only
/// on where both have sound at once, and only where that changes: two
/// stretches of silence match perfectly, and say nothing.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn best_lag(early: &[f64], late: &[f64], reach: usize) -> (i64, f64) {
    let least = early.len().min(late.len()) / 2;
    let mut best = (0, f64::MIN);
    for lag in -(reach as i64)..=(reach as i64) {
        let pairs = || {
            early.iter().enumerate().filter_map(move |(i, e)| {
                let j = usize::try_from(i as i64 + lag).ok()?;
                late.get(j).map(|l| (*e, *l))
            })
        };
        let (n, se, sl) = pairs().fold((0usize, 0.0, 0.0), |(n, se, sl), (e, l)| {
            (n + 1, se + e, sl + l)
        });
        if n < least.max(1) {
            continue;
        }
        let (me, ml) = (se / n as f64, sl / n as f64);
        let (num, de, dl) = pairs().fold((0.0, 0.0, 0.0), |(num, de, dl), (e, l)| {
            let (a, b) = (e - me, l - ml);
            (num + a * b, de + a * a, dl + b * b)
        });
        if de < 1e-12 || dl < 1e-12 {
            continue;
        }
        let r = num / (de * dl).sqrt();
        if r > best.1 {
            best = (lag, r);
        }
    }
    best
}

/// Measure how late the box named like `wanted` sends its sound through
/// macOS; the report, line by line.
pub fn run(wanted: &str) -> String {
    match measure(wanted) {
        Ok(report) => report,
        Err(why) => format!("Not measured: {why}"),
    }
}

#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn measure(wanted: &str) -> Result<String, String> {
    let mut report = Vec::new();
    // Discovery started as the app starts it: from a thread of its own.
    std::thread::spawn(input::generation)
        .join()
        .map_err(|_| "discovery did not start")?;
    let looking = Instant::now();
    let device = loop {
        let named: Vec<_> = input::input_devices()
            .into_iter()
            .filter(|d| d.name.contains(wanted))
            .collect();
        if named.len() > 1 {
            return Err(format!(
                "more than one Blackmagic box named like '{wanted}'"
            ));
        }
        if let Some(d) = named.into_iter().next() {
            break d;
        }
        if looking.elapsed() > Duration::from_secs(20) {
            return Err(format!(
                "no Blackmagic box named like '{wanted}' (discovery said {} times; {} with video in)",
                input::generation(),
                input::input_devices().len()
            ));
        }
        // Discovery tells through a run loop: this thread's turned.
        // SAFETY: CoreFoundation's own mode constant; runs this thread's loop.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, 0) };
    };
    let mode = device
        .modes
        .iter()
        .find(|m| m.progressive && m.width == 1920)
        .or(device.modes.first())
        .ok_or("the box offers no format")?;
    let started = Instant::now();
    let sdk = Arc::new(Mutex::new(Heard::default()));
    let capture = Capture::open(
        device.arrival,
        mode.code,
        Box::new(Keep {
            started,
            heard: Arc::clone(&sdk),
        }),
    )
    .map_err(|e| format!("the box did not open through Blackmagic's API: {e}"))?;

    let host = cpal::default_host();
    // The same box as a sound device: by its name exactly, and only one.
    let mut cores: Vec<(String, cpal::Device)> = host
        .input_devices()
        .map_err(|e| format!("no sound devices: {e}"))?
        .filter_map(|d| {
            let name = d.description().ok()?.name().to_string();
            input::same_box(&name, &device.name).then_some((name, d))
        })
        .collect();
    if cores.len() > 1 {
        return Err(format!(
            "more than one sound device is the box '{}'",
            device.name
        ));
    }
    let (core_name, core) = cores.pop().ok_or("the box is no sound device")?;
    report.push(format!(
        "Measuring '{}' through Blackmagic's API against sound device '{core_name}'",
        device.name
    ));
    let config = core
        .default_input_config()
        .map_err(|e| format!("its sound format: {e}"))?;
    let channels = usize::from(config.channels());
    report.push(format!("CoreAudio device: {config:?}"));
    if config.sample_rate() != 48_000 {
        return Err("its sound is not at 48 kHz".into());
    }
    let mac = Arc::new(Mutex::new(Heard::default()));
    let theirs = Arc::clone(&mac);
    let stream = core
        .build_input_stream(
            &config.into(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let at = started.elapsed().as_secs_f64();
                theirs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(at, data.chunks_exact(channels).map(|f| f[0]));
            },
            |e| eprintln!("CoreAudio: {e}"),
            None,
        )
        .map_err(|e| format!("its sound did not open: {e}"))?;
    stream
        .play()
        .map_err(|e| format!("its sound did not start: {e}"))?;
    // The API calls back on its own threads; the run loop turned meanwhile.
    let listening = Instant::now();
    while listening.elapsed() < LISTEN {
        // SAFETY: CoreFoundation's own mode constant; runs this thread's loop.
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.1, 0) };
    }
    drop(stream);
    drop(capture);
    // Discovery may have told of another box like it since: which was
    // measured could not be told.
    let named = input::input_devices()
        .iter()
        .filter(|d| d.name.contains(wanted))
        .count();
    if named > 1 {
        return Err(format!(
            "more than one Blackmagic box named like '{wanted}'"
        ));
    }

    let sdk = sdk.lock().unwrap_or_else(PoisonError::into_inner);
    let mac = mac.lock().unwrap_or_else(PoisonError::into_inner);
    report.push(format!(
        "Blackmagic's API: {} samples in {} chunks; CoreAudio: {} samples in {} chunks",
        sdk.samples.len(),
        sdk.came.len(),
        mac.samples.len(),
        mac.came.len()
    ));
    let (sa, sm, sz) = sdk.chunk_sizes();
    let (ma, mm, mz) = mac.chunk_sizes();
    report.push(format!(
        "Frames a chunk: Blackmagic's API {sa}/{sm}/{sz}, CoreAudio {ma}/{mm}/{mz} (least/middle/most)"
    ));
    let enough = (RATE * 5.0) as usize;
    if sdk.samples.len() < enough || mac.samples.len() < enough {
        return Err(format!(
            "too little sound came (Blackmagic's API {:.1} s, CoreAudio {:.1} s)",
            sdk.samples.len() as f64 / RATE,
            mac.samples.len() as f64 / RATE
        ));
    }
    let (es, em) = (sdk.envelope(), mac.envelope());
    let varies = |v: &[f64]| {
        let mean = v.iter().sum::<f64>() / v.len().max(1) as f64;
        v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len().max(1) as f64 > 1e-10
    };
    if !varies(&es) || !varies(&em) {
        return Err("the sound did not change enough to line up: talk or clap into the box".into());
    }
    // Content: step i of the API's copy is step i + lag of CoreAudio's.
    let (lag, how_well) = best_lag(&es, &em, 600);
    if how_well < 0.5 {
        return Err(format!(
            "the two copies did not line up well enough (match {how_well:.2}): talk or clap into \
             the box, and check no other app takes its sound"
        ));
    }
    // How much later the same sound reached Nida through CoreAudio, step by
    // step where both have it: each by when its chunk came.
    let mut later: Vec<f64> = (0..es.len())
        .filter_map(|i| {
            let j = usize::try_from(i as i64 + lag).ok()?;
            let at_sdk = sdk.came_at(i * STEP + STEP - 1)?;
            let at_mac = mac.came_at(j * STEP + STEP - 1)?;
            Some(at_mac - at_sdk)
        })
        .collect();
    if later.len() < 200 {
        return Err("too little sound in both copies at once".into());
    }
    later.sort_by(f64::total_cmp);
    let pick = |q: f64| later[((later.len() - 1) as f64 * q).round() as usize] * 1000.0;
    report.push(format!(
        "CoreAudio's sound reaches Nida {:.0} ms after the same sound through Blackmagic's API \
         (middle; {:.0}–{:.0} ms over the chunks; match {how_well:.2})",
        pick(0.5),
        pick(0.05),
        pick(0.95)
    ));
    Ok(report.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claps too far apart to line up match nothing — not the silence that
    /// overlaps where neither is.
    #[test]
    fn claps_too_far_apart_match_nothing() {
        let clap = |at: usize| -> Vec<f64> {
            (0..1000)
                .map(|i| {
                    if (at..at + 4).contains(&i) {
                        1.0
                    } else {
                        0.001
                    }
                })
                .collect()
        };
        let (_, how_well) = best_lag(&clap(100), &clap(900), 600);
        assert!(how_well < 0.5, "matched {how_well}");
    }

    /// One clap, two seconds later in the late copy, is found two seconds
    /// later — not at a lag where only silence overlaps.
    #[test]
    fn a_clap_is_lined_up_not_the_silence_around_it() {
        let clap = |at: usize| -> Vec<f64> {
            (0..1000)
                .map(|i| {
                    if (at..at + 4).contains(&i) {
                        1.0
                    } else {
                        0.001
                    }
                })
                .collect()
        };
        // 5 ms steps: 0.5 s is step 100, 2.5 s step 500.
        let (lag, how_well) = best_lag(&clap(100), &clap(500), 600);
        assert_eq!(lag, 400);
        assert!(how_well > 0.99);
    }
}
