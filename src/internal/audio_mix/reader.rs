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
//!
//! Most of the time the rates match (48 kHz in, 48 kHz out) and the FIFO
//! only swings with the 10 ms blocks: then the step is exactly 1, and the
//! reader glides back onto whole samples and reads them as they are — bit
//! for bit, flat to the top. Interpolated half a sample off, the top was
//! 1.45 dB down at 13 kHz (Max5), and the step chasing every swing moved the
//! sound's delay about.

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
    ///
    /// A step of exactly 1 reads whole samples: off them (after a
    /// correction), it glides back by at most [`ALIGN_STEP`] a frame, then
    /// passes the sound bit for bit.
    // A step of exactly 1 is the rates matching, compared as given.
    #[allow(clippy::float_cmp)]
    pub fn read(&mut self, fifo: &mut VecDeque<Frame>, step: f64, out: &mut [Frame]) -> usize {
        let mut produced = 0;
        for slot in out.iter_mut() {
            let step = if step == 1.0 { self.aligning() } else { step };
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

    /// The step for the next frame at a nominal 1: 1 on whole samples, else
    /// a hair less or more, toward the nearer whole one.
    fn aligning(&self) -> f64 {
        let off = self.frac - self.frac.floor();
        if off == 0.0 {
            1.0
        } else if off < 0.5 {
            1.0 - off.min(ALIGN_STEP)
        } else {
            1.0 + (1.0 - off).min(ALIGN_STEP)
        }
    }
}

/// How far the depth swings between reads, for blocks of `push` read
/// `read` at a time at the same rate: the larger less their greatest common
/// divisor.
fn swing_between(push: usize, read: usize) -> usize {
    if push == 0 || read == 0 {
        return push.max(read);
    }
    let (mut a, mut b) = (push, read);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    push.max(read) - a
}

/// How fast a reader glides back onto whole samples: 100 parts a million —
/// half a sample in a tenth of a second, far under any pitch anyone hears.
const ALIGN_STEP: f64 = 1e-4;

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
    /// Its FIFO emptied at the first read (see [`Self::starting_empty`]).
    empty_first: bool,
    /// Steering the depth back to the target (see [`DEADBAND`]): up
    /// (true) from under the band, or down from over it.
    steering: Option<bool>,
    /// While priming: the depth last seen, and the largest it grew between
    /// two looks — the producer's block (none seen yet: 0).
    last: Option<usize>,
    push: usize,
    /// Looks while priming; and the frames its reads would have taken while
    /// it did not grow, look after look.
    looks: u32,
    stalled: usize,
    /// Watching its depth since it started (see [`LEARN_LOOKS`]): the sum
    /// and looks so far; then the depth its band holds about.
    learning: Option<(f64, u32)>,
    learned: Option<f64>,
}

/// Largest step correction: half a percent, inaudible as pitch.
const MAX_CORRECTION: f64 = 0.005;
/// How strongly a depth error turns into a step correction.
const CORRECTION_GAIN: f64 = 0.02;
/// Within an eighth of the target either way the depth is left alone: it
/// swings with the 10 ms blocks and the callbacks, and chasing that moved
/// the sound's delay and kept the reader off whole samples. Out of that band
/// it is steered back to the target, then left alone again — so a reader
/// starts in it (primed at the target: no glide after a device reopens, as
/// a band over the target had, Max5: 1–2 s at −8 cents each time), sits
/// where the target puts the delay (a band over it added 14 ms), and a slow
/// clock never settles far under the target (a wide band under it let a
/// shallow input FIFO run dry).
const DEADBAND: f64 = 0.25;
/// After it starts, a reader watches its depth this many reads, unsteered,
/// and then holds its band about what it saw (never under its reserve): how
/// the depth swings between reads depends on the producer's blocks and the
/// reads' phase (512 against 480 swings over a whole cycle), which no start
/// can foresee — foreseen wrongly, it steered at the start of every stream.
const LEARN_LOOKS: u32 = 128;
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
            empty_first: false,
            steering: None,
            last: None,
            push: 0,
            looks: 0,
            stalled: 0,
            learning: None,
            learned: None,
        }
    }

    /// As [`Self::new`], its FIFO emptied at the first read: a device's
    /// ring, which the mix goes on feeding while the device closes and
    /// opens again (another pair, a failure) — what piled up meanwhile was
    /// heard that much late, 80 ms easing to 53 over ten seconds at half a
    /// percent. Emptied as the stream first reads, after however long the
    /// device took to open.
    pub fn starting_empty(target: usize) -> Self {
        Self {
            empty_first: true,
            ..Self::new(target)
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
        self.step_for(fifo, nominal, 0)
    }

    /// [`Self::step`] for a read of `need` output frames: the depth it holds
    /// is never less than that read takes (with the interpolator's frames)
    /// and half a block — a device reading 4096 frames a call against a
    /// 1440-frame target ran dry at every read.
    pub fn step_for(&mut self, fifo: &mut VecDeque<Frame>, nominal: f64, need: usize) -> Option<f64> {
        if self.empty_first {
            self.empty_first = false;
            fifo.clear();
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
        let need = (need as f64 * nominal).ceil() as usize + 4;
        if self.priming {
            // Started half a block over the target, what was over that let
            // go (nothing plays yet): the depth then swings about the target
            // itself, in its band from the first read — started at the
            // first depth over the target it sat half a block under, steered
            // up at the start of every stream.
            // (The block learnt from what it grew by between looks, not from
            // what it held at the first: that may be any backlog. None seen
            // in three looks — one burst and no more — it starts from what
            // it holds.)
            // A block over four targets (or two reads) is a backlog, not a
            // block; and a FIFO that stopped growing starts from what it has.
            let depth = fifo.len();
            if let Some(last) = self.last {
                let most = (self.target as usize * 4).max(need * 2);
                self.push = self.push.max(depth.saturating_sub(last).min(most));
                self.stalled = if depth > last { 0 } else { self.stalled + need };
            }
            self.last = Some(depth);
            self.looks = self.looks.saturating_add(1);
            let center = self.center(need);
            // Over the centre by half what the depth swings between reads:
            // the larger of block and read less their greatest common
            // divisor (480 for 512 against 480; none for equal ones; 448 for
            // 480 against 64).
            let swing = swing_between(self.push, need - 4);
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let start = center as usize + swing / 2;
            // Stopped: no growth for four blocks' (or targets') worth of reads
            // — far longer than between two of its blocks.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let stopped = self.stalled >= 4 * self.push.max(self.target as usize) && depth > need;
            if !stopped && ((self.push == 0 && self.looks < 3) || depth < start) {
                return None;
            }
            fifo.drain(..depth.saturating_sub(start));
            self.priming = false;
            self.looks = 0;
            self.stalled = 0;
            self.smoothed = center;
            self.learning = Some((0.0, 0));
            self.learned = None;
        }
        let floor = self.center(need);
        if let Some((sum, looks)) = &mut self.learning {
            *sum += fifo.len() as f64;
            *looks += 1;
            if *looks >= LEARN_LOOKS {
                let seen = *sum / f64::from(*looks);
                self.learned = Some(seen.max(floor));
                self.smoothed = seen;
                self.learning = None;
            }
        }
        let center = self.learned.unwrap_or(floor).max(floor);
        let depth = fifo.len() as f64;
        if depth > center * OVERFILL_FACTOR {
            let excess = (depth - center) as usize;
            fifo.drain(..excess.min(fifo.len()));
            self.smoothed = center;
        }
        self.smoothed += (fifo.len() as f64 - self.smoothed) * 0.05;
        // Watching, it steers only far out (a quarter under or four times
        // over its centre: see the overfill trim above).
        let (low, high) = if self.learning.is_some() {
            (center * (1.0 - DEADBAND), center * OVERFILL_FACTOR)
        } else {
            (center * (1.0 - DEADBAND / 2.0), center * (1.0 + DEADBAND / 2.0))
        };
        if self.smoothed < low {
            self.steering = Some(true);
        } else if self.smoothed > high {
            self.steering = Some(false);
        }
        match self.steering {
            Some(true) if self.smoothed >= center => self.steering = None,
            Some(false) if self.smoothed <= center => self.steering = None,
            _ => {}
        }
        if self.steering.is_none() {
            return Some(nominal);
        }
        let error = (self.smoothed - center) / center;
        let correction = (error * CORRECTION_GAIN * 4.0).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        Some(nominal * (1.0 + correction))
    }

    /// The depth held, for reads of `need` input frames: the target, or more
    /// when the band's floor would otherwise leave less than a read, a whole
    /// block (a delivery slipping past a read takes a block at once) and a
    /// quarter more for the smoothing's lag — a slow clock ran dry at the
    /// floor with less (Codex: 4096-frame reads; 480 against 480).
    fn center(&self, need: usize) -> f64 {
        let reserve = (need + self.push + self.push / 4) as f64;
        self.target.max(reserve / (1.0 - DEADBAND / 2.0))
    }

    /// Note that a read ran dry, so the next reads wait for the target depth.
    pub fn ran_dry(&mut self) {
        self.priming = true;
        self.last = None;
        self.looks = 0;
        self.stalled = 0;
        self.learning = None;
        self.learned = None;
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
        assert_eq!(drift.step(&mut fifo, 1.0), None, "priming");
        fifo.extend((0..60).map(|_| [0.0; 2]));
        assert_eq!(drift.step(&mut fifo, 1.0), None, "a block of 60: waits for 100 + 30");
        fifo.extend((0..30).map(|_| [0.0; 2]));
        assert!(drift.step(&mut fifo, 1.0).is_some());
        assert_eq!(fifo.len(), 130, "started half a block over its target, the rest let go");
    }

    #[test]
    fn a_stalled_fifo_is_cut_back_to_its_target() {
        let mut drift = DriftControl::new(100);
        // Primed on blocks of 50 …
        let mut fifo: VecDeque<Frame> = (0..50).map(|_| [0.0; 2]).collect();
        for _ in 0..3 {
            drift.step(&mut fifo, 1.0);
            fifo.extend((0..50).map(|_| [0.0; 2]));
        }
        assert!(drift.step(&mut fifo, 1.0).is_some(), "playing");
        // … then a stall's worth at once.
        fifo.extend((0..1000).map(|_| [0.0; 2]));
        drift.step(&mut fifo, 1.0);
        assert!(
            fifo.len() <= 110,
            "latency must not build up: {}",
            fifo.len()
        );
    }

    /// A device's ring starts empty at its first read: what the mix fed it
    /// while the device opened is dropped, and it fills to its target from
    /// there — not played late and drained over seconds.
    #[test]
    fn a_devices_ring_starts_empty_at_its_first_read() {
        let mut drift = DriftControl::starting_empty(100);
        let mut fifo: VecDeque<Frame> = (0..300).map(|_| [0.5; 2]).collect();
        assert_eq!(drift.step(&mut fifo, 1.0), None, "emptied: priming");
        assert!(fifo.is_empty());
        // Fed in blocks of 100 from then on: it starts once over its start
        // (its target, raised for a block's reserve, and half its swing),
        // the rest let go — only the first read emptied it.
        let mut fed = 0;
        let started = (0..10).any(|_| {
            fifo.extend((0..100).map(|_| [0.5; 2]));
            fed += 100;
            drift.step(&mut fifo, 1.0).is_some()
        });
        assert!(started, "plays");
        assert!(fifo.len() >= 100 && fifo.len() < fed, "from its start: {} of {fed}", fifo.len());
        // The usual kind starts from what it has, once it has seen a block.
        let mut kept = DriftControl::new(100);
        let mut fifo: VecDeque<Frame> = (0..300).map(|_| [0.5; 2]).collect();
        assert_eq!(kept.step(&mut fifo, 1.0), None, "no block seen yet");
        fifo.extend((0..40).map(|_| [0.5; 2]));
        assert!(kept.step(&mut fifo, 1.0).is_some());
        assert!(fifo.len() >= 100 && fifo.len() < 340, "its start, the backlog let go: {}", fifo.len());
    }

    /// Clocks that agree, the producer in 10 ms blocks and the reader in
    /// 1.3 ms callbacks: once settled, every frame comes out exactly as it
    /// went in, one after another — no interpolation, no wander.
    #[test]
    fn matched_clocks_pass_the_sound_through_bit_for_bit() {
        let mut drift = DriftControl::new(1440);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut next = 0_u32;
        let mut out = Vec::new();
        let mut call = [[0.0; 2]; 64];
        for tick in 0..30_000_u32 {
            // A 480-frame block every 7.5 calls of 64, as 10 ms to 1.3 ms.
            if tick % 15 == 0 || tick % 15 == 7 {
                for _ in 0..480 {
                    let v = f32::from(u16::try_from(next % 60_000).unwrap()) * 1e-5;
                    fifo.push_back([v, -v]);
                    next += 1;
                }
            }
            if let Some(step) = drift.step(&mut fifo, 1.0) {
                let got = reader.read(&mut fifo, step, &mut call);
                if got < call.len() {
                    drift.ran_dry();
                }
                out.extend_from_slice(&call[..got]);
            }
        }
        let tail = &out[out.len() - 48_000..];
        for pair in tail.windows(2) {
            let (a, b) = ((pair[0][0] * 1e5).round(), (pair[1][0] * 1e5).round());
            assert!(b == a + 1.0 || (a == 59_999.0 && b == 0.0), "{a} then {b}");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let sent = f32::from(b as u16) * 1e-5;
            assert_eq!(pair[1][0].to_bits(), sent.to_bits(), "bit for bit");
        }
    }

    /// A shallow FIFO (960 frames) read 480 at a time, fed 512 at a time by
    /// a clock 0.2 % slow: a minute without running dry.
    #[test]
    fn a_slow_clock_into_a_shallow_fifo_never_runs_dry() {
        let mut drift = DriftControl::new(960);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut owed = 0.0_f64;
        let mut dry = 0;
        let mut out = [[0.0; 2]; 480];
        // Ticks of 1 ms: the capture's 512 frames each 10.67 ms of its own
        // (slow) clock, the mixer's 480 each 10 ms.
        for ms in 0..60_000_u32 {
            owed += 48.0 * 0.998;
            while owed >= 512.0 {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
                owed -= 512.0;
            }
            if ms % 10 == 9 {
                if let Some(step) = drift.step(&mut fifo, 1.0) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "ran dry {dry} times");
    }

    /// Primed and then fed as fast as it is read: it starts in its band and
    /// never steers — no glide after a device opens.
    #[test]
    fn a_fresh_reader_at_matching_rates_never_glides() {
        let mut drift = DriftControl::new(1440);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut steps = Vec::new();
        for tick in 0..20_000_u32 {
            if tick % 15 == 0 || tick % 15 == 7 {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 480));
            }
            if let Some(step) = drift.step(&mut fifo, 1.0) {
                steps.push(step);
                let n = 64.min(fifo.len());
                fifo.drain(..n);
            }
        }
        assert!(!steps.is_empty());
        assert!(steps.iter().all(|s| *s == 1.0), "never off 1: {:?}", steps.iter().find(|s| **s != 1.0));
    }

    /// A device reading 4096 frames a call against a 1440-frame target, fed
    /// 3840 at a time: it holds what such a read takes and never runs dry.
    #[test]
    fn big_reads_against_a_small_target_never_run_dry() {
        let mut drift = DriftControl::new(1440);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut owed = 0_usize;
        let mut dry = 0;
        let mut started = false;
        let mut out = vec![[0.0; 2]; 4096];
        // Ticks of 256 frames: 3840 frames each 15 ticks, a read of 4096
        // each 16 — the same rate.
        for tick in 0..20_000_usize {
            owed += 256;
            if tick % 15 == 14 {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 3840));
            }
            if tick % 16 == 15 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    started = true;
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        let _ = owed;
        assert!(started);
        assert!(dry <= 1, "ran dry {dry} times");
    }

    /// The same, with its clock 0.2 % slow: never dry at the band's floor.
    #[test]
    fn big_reads_from_a_slow_clock_never_run_dry() {
        let mut drift = DriftControl::new(1440);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut owed = 0.0_f64;
        let mut dry = 0;
        let mut out = vec![[0.0; 2]; 4096];
        for tick in 0..240_000_usize {
            // Ticks of 16 frames: the producer's 3840 come 0.2 % slow.
            owed += 16.0 * 0.998;
            if owed >= 3840.0 {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 3840));
                owed -= 3840.0;
            }
            if tick % 256 == 255 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert!(dry <= 1, "ran dry {dry} times");
    }

    /// Blocks of 480 read 480 at a time (an input into the mix), its clock
    /// 0.2 % slow: never dry. And at matching rates it never steers.
    #[test]
    fn equal_blocks_and_reads_never_run_dry_nor_steer() {
        let run = |slow: f64| {
            let mut drift = DriftControl::new(960);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut owed = 0.0_f64;
            let (mut dry, mut steered, mut close) = (0, 0, 0);
            let mut out = vec![[0.0; 2]; 480];
            for tick in 0..60_000_usize {
                // Ticks of 8 frames (0.17 ms): the capture's 480, the mix's 480.
                owed += 8.0 * slow;
                if owed >= 480.0 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 480));
                    owed -= 480.0;
                }
                if tick % 60 == 59 {
                    if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                        steered += usize::from(step != 1.0);
                        // Within 64 frames of running dry: no margin left.
                        close += usize::from(fifo.len() < out.len() + 64);
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                }
            }
            (dry, steered, close)
        };
        let (dry, _, close) = run(0.998);
        assert_eq!((dry, close), (0, 0), "slow: dry {dry} times, {close} reads with no margin");
        let (dry, steered, _) = run(1.0);
        assert_eq!((dry, steered), (0, 0), "matching: never dry, never steered");
    }

    /// Blocks of 512 read 480 at a time (an input on a device at its default
    /// buffer), clocks matching, from several starting phases: once it has
    /// watched its depth, it never steers.
    #[test]
    fn blocks_and_reads_of_different_sizes_never_steer_at_matching_rates() {
        for phase in [0_usize, 7, 19, 31, 43] {
            let mut drift = DriftControl::new(960);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let (mut steered, mut dry) = (0, 0);
            let mut out = vec![[0.0; 2]; 480];
            // Ticks of 16 frames: 512 every 32, 480 every 30.
            for tick in 0..30_000_usize {
                if (tick + phase) % 32 == 0 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
                }
                if tick % 30 == 29 {
                    if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                        steered += usize::from(step != 1.0);
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                }
            }
            assert_eq!((steered, dry), (0, 0), "phase {phase}");
        }
    }

    /// An empty look, then one burst and no more: it starts all the same.
    #[test]
    fn a_burst_after_an_empty_look_still_starts() {
        let mut drift = DriftControl::new(960);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        assert_eq!(drift.step_for(&mut fifo, 1.0, 480), None);
        fifo.extend(std::iter::repeat_n([0.3, 0.3], 44_100));
        let started = (0..5).any(|_| drift.step_for(&mut fifo, 1.0, 480).is_some());
        assert!(started, "started");
        assert!(fifo.len() < 10_000, "not on the whole backlog: {}", fifo.len());
    }

    /// Grown by a big block once and then no more, short of where it would
    /// start: once it has stopped growing for long enough, it starts.
    #[test]
    fn a_fifo_that_stops_growing_short_of_its_start_still_starts() {
        let mut drift = DriftControl::new(960);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        assert_eq!(drift.step_for(&mut fifo, 1.0, 480), None);
        fifo.extend(std::iter::repeat_n([0.3, 0.3], 3000));
        let started = (0..60).any(|_| drift.step_for(&mut fifo, 1.0, 480).is_some());
        assert!(started, "started from what it had");
    }

    /// One burst and no more (a backlog collected before the first look):
    /// it still starts, from what it holds, after three looks.
    #[test]
    fn one_burst_and_no_more_still_starts() {
        let mut drift = DriftControl::new(960);
        let mut fifo: VecDeque<Frame> = (0..44_100).map(|_| [0.3; 2]).collect();
        let mut starts = 0;
        for _ in 0..5 {
            if drift.step_for(&mut fifo, 1.0, 480).is_some() {
                starts += 1;
                break;
            }
        }
        assert_eq!(starts, 1, "started");
    }

    /// A reader left half a sample off whole samples (after a correction)
    /// glides back onto them at a step of 1 — no jump on the way — and then
    /// reads them as they are.
    #[test]
    fn a_reader_off_whole_samples_glides_back_onto_them() {
        let mut fifo = sine(30_000, 1_000.0, 48_000.0);
        let mut reader = RateReader { frac: 0.5 };
        let mut out = vec![[0.0; 2]; 24_000];
        assert_eq!(reader.read(&mut fifo, 1.0, &mut out), out.len());
        let bend = out
            .windows(3)
            .map(|w| (w[2][0] - 2.0 * w[1][0] + w[0][0]).abs())
            .fold(0.0, f32::max);
        // A 1 kHz sine bends 0.017 a sample at most; a jump shows far over.
        assert!(bend < 0.02, "a bend of {bend}");
        assert_eq!(reader.frac.fract(), 0.0, "on whole samples");
        let mut more = [[0.0; 2]; 100];
        let before = fifo.clone();
        reader.read(&mut fifo, 1.0, &mut more);
        for (k, frame) in more.iter().enumerate() {
            assert_eq!(*frame, before[k + 1], "as they are");
        }
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
