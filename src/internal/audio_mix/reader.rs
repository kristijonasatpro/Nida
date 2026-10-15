//! Reading audio across clocks that disagree.
//!
//! Every device has its own sample clock, and no two agree exactly: a USB
//! interface that says 48 kHz might deliver 48 002 samples a second while the
//! Mac's output takes 47 998. Left alone, the buffer between them grows or
//! drains a few samples a second until it overflows or runs dry — a click every
//! few minutes, for the length of a show.
//!
//! [`RateReader`] reads a FIFO at an adjustable step, interpolating between
//! samples, and [`DriftControl`] nudges that step by at most half a percent to
//! hold the FIFO at a target depth. Half a percent is far below what anyone
//! hears as a pitch change, and several times more than real clocks drift.

use std::collections::VecDeque;

/// One stereo frame: left, right.
pub type Frame = [f32; 2];

/// Reads a FIFO of frames at a fractional step with 4-point cubic
/// (Catmull-Rom) interpolation.
///
/// The frame at the front of the FIFO is kept as history for the
/// interpolator, so a reader holds one frame back. That is one sample of
/// latency, not audible, and it keeps the curve continuous across reads.
#[derive(Debug, Default)]
pub struct RateReader {
    /// Read position past the history frame, in input frames.
    frac: f64,
}

impl RateReader {
    /// Fill `out`, stepping `step` input frames per output frame. Returns how
    /// many frames were produced before the FIFO ran dry; the rest of `out` is
    /// left untouched for the caller to silence.
    pub fn read(&mut self, fifo: &mut VecDeque<Frame>, step: f64, out: &mut [Frame]) -> usize {
        let mut produced = 0;
        for slot in out.iter_mut() {
            let base = self.frac.floor();
            let i = base as usize;
            if i + 3 >= fifo.len() {
                break;
            }
            let t = (self.frac - base) as f32;
            let (xm1, x0, x1, x2) = (fifo[i], fifo[i + 1], fifo[i + 2], fifo[i + 3]);
            for c in 0..2 {
                slot[c] = catmull_rom(xm1[c], x0[c], x1[c], x2[c], t);
            }
            self.frac += step;
            produced += 1;
        }
        let consumed = (self.frac.floor() as usize).min(fifo.len());
        fifo.drain(..consumed);
        self.frac -= consumed as f64;
        produced
    }

    /// Forget the fractional position, for a FIFO that was just cleared.
    pub fn reset(&mut self) {
        self.frac = 0.0;
    }
}

fn catmull_rom(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    x0 + 0.5
        * t
        * (x1 - xm1 + t * (2.0 * xm1 - 5.0 * x0 + 4.0 * x1 - x2 + t * (3.0 * (x0 - x1) + x2 - xm1)))
}

/// Holds a FIFO at a target depth by trimming the read step.
#[derive(Debug)]
pub struct DriftControl {
    /// Depth to hold, in frames.
    target: f64,
    /// Smoothed depth, so one bursty delivery does not swing the step.
    smoothed: f64,
    /// Waiting for the FIFO to refill to the target after running dry.
    priming: bool,
}

/// Largest step correction: half a percent, inaudible as pitch.
const MAX_CORRECTION: f64 = 0.005;
/// How strongly a depth error turns into a step correction.
const CORRECTION_GAIN: f64 = 0.02;
/// Past this many times the target the FIFO is cut back rather than drained
/// slowly: after a stall, catching up at half a percent would take minutes.
const OVERFILL_FACTOR: f64 = 4.0;

impl DriftControl {
    /// Hold a FIFO at `target` frames.
    pub fn new(target: usize) -> Self {
        Self {
            target: target.max(1) as f64,
            smoothed: target as f64,
            priming: true,
        }
    }

    /// The target depth in frames.
    pub fn target(&self) -> usize {
        self.target as usize
    }

    /// The step to read at this time, given the FIFO depth now and the
    /// nominal step (input rate over output rate). `None` means "output
    /// silence": the FIFO ran dry and is refilling to the target first, so
    /// playback resumes cleanly instead of stuttering one block at a time.
    ///
    /// Trims the FIFO when it is far too deep, so latency cannot build up
    /// without limit.
    pub fn step(&mut self, fifo: &mut VecDeque<Frame>, nominal: f64) -> Option<f64> {
        let depth = fifo.len() as f64;
        if self.priming {
            if depth < self.target {
                return None;
            }
            self.priming = false;
            self.smoothed = depth;
        }
        if depth > self.target * OVERFILL_FACTOR {
            let excess = (depth - self.target) as usize;
            fifo.drain(..excess.min(fifo.len()));
            self.smoothed = self.target;
        }
        self.smoothed += (fifo.len() as f64 - self.smoothed) * 0.05;
        let error = (self.smoothed - self.target) / self.target;
        let correction = (error * CORRECTION_GAIN).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        Some(nominal * (1.0 + correction))
    }

    /// Note that a read ran dry, so the next reads wait for the target depth.
    pub fn ran_dry(&mut self) {
        self.priming = true;
    }
}

/// Decibels to a linear gain; anything at or below -96 dB is silence.
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -96.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// A linear level to decibels, floored at -96 dB.
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 0.000_016 {
        -96.0
    } else {
        20.0 * gain.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(n: usize, freq: f32, rate: f32) -> VecDeque<Frame> {
        (0..n)
            .map(|i| {
                let v = (2.0 * std::f32::consts::PI * freq * i as f32 / rate).sin();
                [v, v]
            })
            .collect()
    }

    #[test]
    fn unit_step_reads_the_samples_back_unchanged() {
        let mut fifo: VecDeque<Frame> = (0..100).map(|i| [i as f32, -(i as f32)]).collect();
        let mut reader = RateReader::default();
        let mut out = [[0.0; 2]; 50];
        assert_eq!(reader.read(&mut fifo, 1.0, &mut out), 50);
        // One frame of history: output frame k is input frame k + 1.
        for (k, frame) in out.iter().enumerate() {
            assert!((frame[0] - (k + 1) as f32).abs() < 1e-5, "{k}: {frame:?}");
            assert!((frame[1] + (k + 1) as f32).abs() < 1e-5);
        }
        assert_eq!(fifo.len(), 50, "exactly what was read is consumed");
    }

    #[test]
    fn a_resampled_sine_stays_a_clean_sine() {
        // 44.1 kHz in, 48 kHz out: a 1 kHz tone must come out as 1 kHz with
        // no audible distortion (interpolation error well below -40 dB).
        let mut fifo = sine(44_100, 1000.0, 44_100.0);
        let mut reader = RateReader::default();
        let mut out = vec![[0.0; 2]; 40_000];
        let n = reader.read(&mut fifo, 44_100.0 / 48_000.0, &mut out);
        assert_eq!(n, 40_000);
        let mut worst = 0.0f32;
        let step = 44_100.0 / 48_000.0;
        for (k, frame) in out.iter().enumerate().skip(10) {
            // Output frame k reads input position 1 + k·step (one frame of
            // history).
            let t = (1.0 + k as f64 * step) / 44_100.0;
            let want = (2.0 * std::f64::consts::PI * 1000.0 * t).sin() as f32;
            worst = worst.max((frame[0] - want).abs());
        }
        assert!(
            worst < 0.002,
            "interpolation error {worst} (must be below -54 dB)"
        );
    }

    #[test]
    fn running_dry_produces_what_it_can() {
        let mut fifo: VecDeque<Frame> = (0..10).map(|_| [1.0, 1.0]).collect();
        let mut reader = RateReader::default();
        let mut out = [[0.0; 2]; 20];
        let n = reader.read(&mut fifo, 1.0, &mut out);
        assert_eq!(n, 7, "three frames stay for the interpolator's window");
    }

    #[test]
    fn drift_control_holds_the_fifo_at_its_target_against_a_fast_clock() {
        // The producer runs 0.2 % fast — a bad clock, still inside the
        // correction range. After a minute the FIFO must still sit near the
        // target instead of having grown by 5 760 frames.
        let target = 1440;
        let mut drift = DriftControl::new(target);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut owed = 0.0f64;
        let mut out = [[0.0; 2]; 480];
        for _ in 0..6000 {
            owed += 480.0 * 1.002;
            while owed >= 1.0 {
                fifo.push_back([0.5, 0.5]);
                owed -= 1.0;
            }
            if let Some(step) = drift.step(&mut fifo, 1.0) {
                if reader.read(&mut fifo, step, &mut out) < out.len() {
                    drift.ran_dry();
                }
            }
        }
        let depth = fifo.len() as f64;
        assert!(
            (depth - target as f64).abs() < target as f64 * 0.5,
            "depth {depth} drifted from target {target}"
        );
    }

    #[test]
    fn a_dry_fifo_waits_for_the_target_before_playing_again() {
        let mut drift = DriftControl::new(100);
        let mut fifo: VecDeque<Frame> = (0..50).map(|_| [0.0; 2]).collect();
        assert_eq!(drift.step(&mut fifo, 1.0), None, "priming until 100 frames");
        fifo.extend((0..60).map(|_| [0.0; 2]));
        assert!(drift.step(&mut fifo, 1.0).is_some());
    }

    #[test]
    fn a_stalled_fifo_is_cut_back_to_its_target() {
        let mut drift = DriftControl::new(100);
        let mut fifo: VecDeque<Frame> = (0..100).map(|_| [0.0; 2]).collect();
        drift.step(&mut fifo, 1.0);
        fifo.extend((0..1000).map(|_| [0.0; 2]));
        drift.step(&mut fifo, 1.0);
        assert!(
            fifo.len() <= 110,
            "latency must not build up: {}",
            fifo.len()
        );
    }

    #[test]
    fn decibels_round_trip() {
        assert!((db_to_gain(0.0) - 1.0).abs() < 1e-6);
        assert!((db_to_gain(-6.0) - 0.501).abs() < 1e-3);
        assert_eq!(db_to_gain(-120.0), 0.0);
        assert!((gain_to_db(db_to_gain(-12.0)) + 12.0).abs() < 1e-4);
        assert_eq!(gain_to_db(0.0), -96.0);
    }
}
