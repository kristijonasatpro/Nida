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
    learning: Option<(f64, u32, f64)>,
    learned: Option<f64>,
    /// Priming, enough held to start: see [`Landing`].
    landing: Option<Landing>,
    /// It lands at all (see [`Self::without_landing`]).
    lands: bool,
    /// How it last started, until taken (see [`Self::take_start`]).
    started: Option<Start>,
}

/// A reader about to start, watching one cycle of its producer's blocks
/// against its reads with silence playing, its reads' frames let go, so
/// that it starts with its depth before a read averaging its centre. Started
/// at the first depth over its start it sat wherever that block fell against
/// the reads: always just after one, so anywhere over a whole swing (Max5:
/// 3251–3713 samples after each Live close; 448 frames over the phases for
/// 512 against 480, 729 for 480 against 512, in a test).
#[derive(Debug, Clone, Copy)]
struct Landing {
    /// Reads still to watch.
    left: u32,
    /// The depth it started watching at: fallen a block and half a read
    /// under it, its producer has stopped, and it starts with what is left.
    from: usize,
    /// The depths before them, summed, and how many; and the least.
    sum: f64,
    looks: u32,
    least: usize,
    /// Frames its reads would have taken, not yet let go (fractions of one
    /// at a step off 1).
    owed: f64,
    /// Frames still to leave, to raise its depth to the centre.
    hold: usize,
    /// The depth after the last read's frames went, and what it has grown
    /// by since each read (frames let go counted), when it grew. Their
    /// middle is the producer's block. What comes between two reads at most,
    /// a block or two, is what its reserve holds; more than two blocks at
    /// once is a backlog, not the stream (a backlog made a reserve of a
    /// second, Codex). Before, it held the most it grew between two looks
    /// with nothing read: one block or two (480 against 512) by the phase it
    /// started at, and it landed a block higher in some (a test).
    after: Option<usize>,
    grown: [usize; LANDING_READS as usize],
    grew: usize,
    /// How it landed, once decided, for when it starts.
    report: Option<Start>,
}

/// How a reader started, for the log (see [`DriftControl::take_start`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Start {
    /// Landed after watching `looks` reads: the block and the reserve it
    /// saw, its centre, the middle depth it saw before, and the frames let
    /// go (more than nought) or kept back (less).
    Landed {
        looks: u32,
        block: usize,
        reserve: usize,
        center: f64,
        level: f64,
        moved: i64,
    },
    /// Watched, but not a whole cycle of the block it saw: started as it
    /// was, not moved.
    Unmoved { looks: u32, block: usize, center: f64 },
    /// Started without landing, and why.
    Plain { why: &'static str, push: usize, center: f64 },
    /// Its producer stopped while it watched: started with what was left.
    Stopped { looks: u32, center: f64 },
}

impl std::fmt::Display for Start {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::Landed { looks, block, reserve, center, level, moved } => write!(
                f,
                "landed on {center:.0} after {looks} reads (block {block}, reserve {reserve}, it held {level:.0}, {} {})",
                if moved >= 0 { "let go" } else { "kept back" },
                moved.abs()
            ),
            Self::Unmoved { looks, block, center } => {
                write!(f, "started unmoved after {looks} reads: not a whole cycle of block {block} (centre {center:.0})")
            }
            Self::Plain { why, push, center } => write!(f, "started without landing: {why} (block {push}, centre {center:.0})"),
            Self::Stopped { looks, center } => write!(f, "started on what it had: its producer stopped {looks} reads into landing (centre {center:.0})"),
        }
    }
}

/// The most reads a landing watches: two of its producer's blocks against
/// its reads repeat within this many for the usual sizes (16 for 512 against
/// 480, 15 for 480 against 512).
const LANDING_READS: u32 = 32;
/// And at most this long, in input frames: a device reading 4096 frames a
/// call would otherwise wait over a second to start.
const LANDING_MOST_FRAMES: usize = 9_600;

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
/// And at least this long, in frames read (the 128 reads of 480 it was,
/// 1.28 s): a device reading 32 frames a call learnt in 85 ms, a mixer's
/// late block or two in it. Counted as read, at the mix's rate or the
/// device's, not as taken from the FIFO: an 11 kHz input would have learnt
/// for 5.6 s, with the wide band meanwhile, and a slow clock ran it dry
/// (Codex).
const LEARN_FRAMES: f64 = 61_440.0;
/// The depth is smoothed a twentieth of the way each read of 480 frames or
/// more, as it always was, and less for smaller reads, compounded: about
/// 200 ms whatever their size. Each read a twentieth, a device reading 32
/// frames a call smoothed over 13 ms, took a mixer's late block (a 10 ms
/// dip) for a drained FIFO, steered up and stayed there, a little later
/// each time: glides after most Live closes at 32 and 64 (Max5; 24 in 24 in
/// a test with the mixer late now and then). More for bigger reads (0.35
/// of the way for 4096) chased their own swings (Codex).
const SMOOTH_KEEP: f64 = 0.95;
const SMOOTH_FRAMES: f64 = 480.0;
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
            landing: None,
            lands: true,
            started: None,
        }
    }

    /// How it started since this was last asked, for the log: whether it
    /// landed and where, or why not. Kept, not logged, as a device's own
    /// callback reads its ring and may not wait on the log.
    pub fn take_start(&mut self) -> Option<Start> {
        self.started.take()
    }

    /// Starting as soon as it holds enough, without landing (see
    /// [`Landing`]): a live bus's rest of the mix, whose start the live
    /// stream's own opening fade waits for, and which a landing would bring
    /// in after that fade, at full level (Codex).
    #[must_use]
    pub fn without_landing(self) -> Self {
        Self {
            lands: false,
            ..self
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
        // What a read takes, in input frames; and its size as read.
        #[allow(clippy::cast_precision_loss)] // Frames.
        let read = need as f64;
        let take = read * nominal;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
        let need = take.ceil() as usize + 4;
        if self.priming && self.landing.is_some() {
            if let Some(started) = self.land(fifo, take, need) {
                return started.then_some(nominal);
            }
        }
        if self.priming {
            // Held enough to start (its centre and half a swing), what was
            // over that let go (nothing plays yet), it watches a cycle of
            // its producer's blocks and lands on its centre (see
            // [`Landing`]): then it swings about the centre itself, in its
            // band from the first read.
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
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
            let whole = self.landing_reads(take.round() as usize).1;
            if stopped || self.push == 0 || take < 1.0 || !self.lands || !whole {
                // Nothing coming in blocks to watch (stopped, or only a
                // backlog seen), no reads to watch it against, not landing,
                // or a cycle too long to watch (blocks or reads of 1024 and
                // over: a cycle's mean guessed from part of it ran dry,
                // Codex): from what it has.
                let why = if stopped {
                    "its producer stopped"
                } else if self.push == 0 {
                    "no block seen, only what it held"
                } else if take < 1.0 {
                    "no reads to watch it against"
                } else if !self.lands {
                    "not landing"
                } else {
                    "a cycle too long to watch"
                };
                let push = self.push;
                self.start(center);
                self.started = Some(Start::Plain { why, push, center });
            } else {
                // Watch a cycle, then land on the centre.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
                let read = take.round() as usize;
                self.landing = Some(Landing {
                    left: self.landing_reads(read).0,
                    from: fifo.len(),
                    sum: 0.0,
                    looks: 0,
                    least: usize::MAX,
                    owed: 0.0,
                    hold: 0,
                    after: None,
                    grown: [0; LANDING_READS as usize],
                    grew: 0,
                    report: None,
                });
                return self.land(fifo, take, need).and_then(|started| started.then_some(nominal));
            }
        }
        let floor = self.center(need);
        if let Some((sum, looks, frames)) = &mut self.learning {
            *sum += fifo.len() as f64;
            *looks += 1;
            *frames += read;
            // (Read with no size, `step`: its looks alone.)
            if *looks >= LEARN_LOOKS && (read <= 0.0 || *frames >= LEARN_FRAMES) {
                let seen = *sum / f64::from(*looks);
                self.learned = Some(seen.max(floor));
                self.smoothed = seen;
                self.learning = None;
            }
        }
        let center = self.learned.unwrap_or(floor).max(floor);
        let depth = fifo.len() as f64;
        if depth > center * OVERFILL_FACTOR {
            // Cut back to half a swing over the centre, as it starts: cut to
            // the centre itself just after a block came, it swung half a
            // swing under, a block late ran it dry (Codex: 322 of a read of
            // 480, after a backlog).
            #[allow(clippy::cast_precision_loss)] // Frames.
            let keep = center + (swing_between(self.push, need - 4) / 2) as f64;
            let excess = (depth - keep).max(0.0) as usize;
            fifo.drain(..excess.min(fifo.len()));
            self.smoothed = center;
        }
        let share = if read > 0.0 {
            1.0 - SMOOTH_KEEP.powf(read.min(SMOOTH_FRAMES) / SMOOTH_FRAMES)
        } else {
            1.0 - SMOOTH_KEEP
        };
        self.smoothed += (fifo.len() as f64 - self.smoothed) * share;
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

    /// Playing from now, its depth about `center`.
    fn start(&mut self, center: f64) {
        self.priming = false;
        self.landing = None;
        self.looks = 0;
        self.stalled = 0;
        self.smoothed = center;
        self.learning = Some((0.0, 0, 0.0));
        self.learned = None;
    }

    /// How many reads of `read` input frames a landing watches: one cycle
    /// of its producer's blocks against them, within [`LANDING_READS`] and
    /// [`LANDING_MOST_FRAMES`], two at least; and whether that is the whole
    /// cycle (a reader lands only then).
    fn landing_reads(&self, read: usize) -> (u32, bool) {
        let read = read.max(1);
        let cycle = if self.push == 0 {
            8
        } else {
            let (mut a, mut b) = (self.push, read);
            while b != 0 {
                (a, b) = (b, a % b);
            }
            // The least common multiple, in reads.
            self.push / a
        };
        let most = (LANDING_MOST_FRAMES / read).max(2).min(LANDING_READS as usize);
        let reads = cycle.clamp(2, most);
        (u32::try_from(reads).unwrap_or(LANDING_READS), reads >= cycle)
    }

    /// One read of a landing (see [`Landing`]): `Some(false)` while it
    /// watches or raises its depth (silence plays), `Some(true)` once it
    /// starts with this read, `None` if it is not landing.
    fn land(&mut self, fifo: &mut VecDeque<Frame>, take: f64, need: usize) -> Option<bool> {
        let mut landing = self.landing?;
        if fifo.len() + self.push + need / 2 < landing.from {
            // Its producer stopped: what is left plays.
            let center = self.center(need);
            self.start(center);
            self.started = Some(Start::Stopped { looks: landing.looks, center });
            return Some(true);
        }
        if let Some(after) = landing.after {
            let grown = fifo.len().saturating_sub(after);
            if grown > 0 && landing.grew < landing.grown.len() {
                landing.grown[landing.grew] = grown;
                landing.grew += 1;
            }
        }
        if landing.left > 0 {
            landing.sum += fifo.len() as f64;
            landing.looks += 1;
            landing.least = landing.least.min(fifo.len());
            landing.left -= 1;
        } else if landing.hold == 0 {
            // Watched: moved once so its depth before a read averages the
            // centre (frames let go, or left for later reads), the centre
            // held for what comes between two reads as seen. Not let go at
            // all unless the reads watched were a whole cycle of the block
            // seen (a backlog before had it guessed bigger, and the cycle
            // shorter: Codex), nor so far that the least depth seen would
            // hold less than a read and a reserve: reads late and then caught
            // up at once look deeper than the stream is (Codex).
            let mut grown = landing.grown;
            let grown = &mut grown[..landing.grew];
            grown.sort_unstable();
            // Not a whole cycle: as it was before landing existed, not moved
            // either way, its block as first guessed too (a smaller one, with
            // no landing to place it, left it a block nearer dry: Codex).
            let mut whole = false;
            let block = grown.get(grown.len() / 2).copied().unwrap_or(0);
            if block > 0 {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
                let read = (take.round() as usize).max(1);
                let (mut a, mut b) = (block, read);
                while b != 0 {
                    (a, b) = (b, a % b);
                }
                whole = block / a <= landing.looks as usize;
                if whole {
                    self.push = grown.iter().copied().filter(|&g| g <= 2 * block).max().unwrap_or(block);
                }
            }
            let center = self.center(need);
            let mean = landing.sum / f64::from(landing.looks.max(1));
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
            let over = (mean - center).round().abs() as usize;
            let mut moved = 0_i64;
            if mean > center {
                let room = landing.least.saturating_sub(need + self.push);
                if whole {
                    let gone = over.min(room).min(fifo.len());
                    fifo.drain(..gone);
                    moved = i64::try_from(gone).unwrap_or(i64::MAX);
                }
            } else if whole {
                landing.hold = over;
                moved = -i64::try_from(over).unwrap_or(i64::MAX);
            }
            let report = if whole {
                Start::Landed {
                    looks: landing.looks,
                    block,
                    reserve: self.push,
                    center,
                    level: mean,
                    moved,
                }
            } else {
                Start::Unmoved {
                    looks: landing.looks,
                    block,
                    center,
                }
            };
            if landing.hold == 0 {
                self.start(center);
                self.started = Some(report);
                return Some(true);
            }
            landing.report = Some(report);
        }
        // This read's frames let go — less any still to leave.
        landing.owed += take;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Frames.
        let mut let_go = landing.owed.floor() as usize;
        landing.owed -= let_go as f64;
        let kept = let_go.min(landing.hold);
        landing.hold -= kept;
        let_go -= kept;
        fifo.drain(..let_go.min(fifo.len()));
        landing.after = Some(fifo.len());
        if landing.left == 0 && landing.hold == 0 && kept > 0 {
            self.start(self.center(need));
            self.started = landing.report;
            self.landing = None;
            return Some(false);
        }
        self.landing = Some(landing);
        Some(false)
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
        self.landing = None;
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
        // … then a stall's worth at once: cut back to its target and half a
        // block (see the overfill trim).
        fifo.extend((0..1000).map(|_| [0.0; 2]));
        drift.step(&mut fifo, 1.0);
        assert!(
            fifo.len() <= 130,
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
        for phase in 0..15_usize {
            let mut drift = DriftControl::new(1440);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut owed = 0_usize;
            let mut dry = 0;
            let mut started = false;
            let (mut reads, mut steered) = (0_usize, 0);
            let mut out = vec![[0.0; 2]; 4096];
            // Ticks of 256 frames: 3840 frames each 15 ticks, a read of 4096
            // each 16 — the same rate; the blocks at each of their 15 phases.
            for tick in 0..20_000_usize {
                owed += 256;
                if tick % 15 == phase {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 3840));
                }
                if tick % 16 == 15 {
                    if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                        started = true;
                        reads += 1;
                        // Past learning (128 reads), its own swings never steer
                        // it (Codex: a smoothing of 0.43 a read did, at 12.7 s).
                        steered += usize::from(reads > 128 && step != 1.0);
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                }
            }
            let _ = owed;
            assert!(started);
            assert!(dry <= 1, "phase {phase}: ran dry {dry} times");
            assert_eq!(steered, 0, "phase {phase}: steered {steered} reads at matching clocks");
        }
    }

    /// An 11 kHz input on a clock 300 parts a million slow, in callbacks of
    /// 160 frames: never dry (Codex: learning counted in its own frames lasted
    /// 5.6 s with the wide band, and ran dry at 2.7 s).
    #[test]
    fn a_slow_low_rate_input_never_runs_dry() {
        let rate = 11_025.0_f64;
        let nominal = rate / 48_000.0;
        let mut drift = DriftControl::new(220);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 480];
        let (mut dry, mut next_push, mut next_read) = (0, 80.0_f64, 110.25_f64);
        // Ticks of one input frame.
        for tick in 0..(rate as usize) * 20 {
            let t = tick as f64;
            while t >= next_push {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 160));
                next_push += 160.0 * (1.0 + 300e-6);
            }
            while t >= next_read {
                next_read += 110.25;
                if let Some(step) = drift.step_for(&mut fifo, nominal, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "ran dry {dry} times");
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

    /// A reader lands on the same depth whatever the phase of its
    /// producer's blocks against its reads: blocks of 512 against reads of
    /// 480 (an input), 480 against 512 (a device's ring) and the like. It
    /// sat anywhere over a whole swing before (448 and 729 frames for those
    /// two; Max5: 3251–3713 samples after each Live close), and lands at the
    /// low end of that (an input) or in its middle (a ring: two blocks come
    /// between some reads, which its reserve holds) now. It never runs dry.
    /// (Blocks or reads of 1024 and more start as before: their cycle is too
    /// long to watch.)
    #[test]
    fn a_reader_lands_on_one_depth_whatever_the_phase() {
        // (mean depth after a read once settled, dry reads after the start, frames until it played)
        fn settle(push: usize, read: usize, target: usize, empty_first: bool, phase: usize) -> (f64, u32, usize) {
            let mut drift = if empty_first {
                DriftControl::starting_empty(target)
            } else {
                DriftControl::new(target)
            };
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut out = vec![[0.0; 2]; read];
            let (mut sum, mut n, mut dry, mut first) = (0.0, 0u32, 0u32, None);
            for tick in 0..48_000 * 8 {
                if (tick + phase).is_multiple_of(push) {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], push));
                }
                if tick % read == read - 1 {
                    if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                        first.get_or_insert(tick);
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                    if tick > 48_000 * 4 {
                        sum += fifo.len() as f64;
                        n += 1;
                    }
                }
            }
            (sum / f64::from(n), dry, first.unwrap_or(usize::MAX))
        }
        // (blocks, reads, target, a device's ring, the spread allowed,
        // where it settles after a read: the centre less a read)
        for (push, read, target, ring, spread, settles) in [
            (512, 480, 960, false, 8.0, 805.0),
            (256, 480, 960, false, 8.0, 805.0),
            (480, 512, 1440, true, 8.0, 1449.0),
            (480, 64, 1440, true, 8.0, 1376.0),
        ] {
            let runs: Vec<(f64, u32, usize)> =
                (0..push.max(read)).step_by(push.max(read) / 16).map(|p| settle(push, read, target, ring, p)).collect();
            let lo = runs.iter().map(|r| r.0).fold(f64::MAX, f64::min);
            let hi = runs.iter().map(|r| r.0).fold(f64::MIN, f64::max);
            let dry: u32 = runs.iter().map(|r| r.1).sum();
            let start = runs.iter().map(|r| r.2).max().unwrap_or(usize::MAX);
            let case = format!("{push} against {read}: {lo:.0}..{hi:.0}, {dry} dry, playing by {:.0} ms", start as f64 / 48.0);
            assert!(hi - lo <= spread, "{case}");
            assert!((f64::midpoint(lo, hi) - settles).abs() <= spread, "{case}");
            assert_eq!(dry, 0, "{case}");
            assert!(start < 48 * 320, "{case}");
        }
    }

    /// Reads held up while it lands and then made at once, as the mixer
    /// catches up after a late wake: the stream looks deeper than it is
    /// meanwhile, and the landing must not let go so much that it runs dry
    /// (Codex: 469 frames left for a read of 480).
    #[test]
    fn a_landing_through_late_reads_never_runs_dry() {
        for (late_from, phase) in [(1_usize, 0_usize), (2, 100), (3, 200), (4, 300), (5, 400), (6, 500)] {
            let mut drift = DriftControl::new(960);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut out = vec![[0.0; 2]; 480];
            let (mut reads, mut dry, mut owed) = (0_usize, 0, 0_usize);
            for tick in 0..48_000 * 4 {
                if (tick + phase).is_multiple_of(512) {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
                }
                if tick % 480 == 479 {
                    owed += 1;
                    // Seven reads, from the `late_from`th on, wait 60 ms and
                    // come at once.
                    let held = (late_from..late_from + 6).contains(&reads);
                    if held {
                        reads += 1;
                        continue;
                    }
                    for _ in 0..owed {
                        if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                            if reader.read(&mut fifo, step, &mut out) < out.len() {
                                drift.ran_dry();
                                dry += 1;
                            }
                        }
                    }
                    owed = 0;
                    reads += 1;
                }
            }
            assert_eq!(dry, 0, "held from read {late_from}, phase {phase}");
        }
    }

    /// A backlog while it lands (60 blocks at once) is not taken for the
    /// stream: the reader settles near its target again, not a second late
    /// (Codex: a reserve of 31 232 frames, 54 290 held).
    #[test]
    fn a_backlog_while_landing_is_not_held_for_good() {
        let mut drift = DriftControl::new(960);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 480];
        let (mut reads, mut dry, mut sum, mut n) = (0_usize, 0, 0.0, 0u32);
        for tick in 0..48_000_usize * 10 {
            if tick.is_multiple_of(512) {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
            }
            if tick % 480 == 479 {
                if reads == 8 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 60 * 512));
                }
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
                if tick > 48_000 * 8 {
                    sum += fifo.len() as f64;
                    n += 1;
                }
                reads += 1;
            }
        }
        let settled = sum / f64::from(n);
        assert!(settled < 2_000.0, "settled {settled:.0} frames deep");
        assert_eq!(dry, 0);
    }

    /// A backlog cut back after it landed, then a block late: it still
    /// holds a read and a block (Codex: 512 against 480, 60 blocks before
    /// read 8 at phase 160, the delivery at 10 080 held to 10 592; cut back
    /// to the centre, read 21 had 322 of 480).
    #[test]
    fn a_backlog_cut_back_keeps_its_reserve() {
        let phase = 160;
        let mut drift = DriftControl::new(960);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 480];
        let (mut reads, mut dry, mut late) = (0_usize, 0, false);
        for tick in 0..48_000_usize * 3 {
            if tick == 10_592 && late {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
                late = false;
            }
            if (tick + phase).is_multiple_of(512) {
                if tick == 10_080 {
                    late = true;
                } else {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
                }
            }
            if tick % 480 == 479 {
                if reads == 8 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 60 * 512));
                }
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
                reads += 1;
            }
        }
        assert_eq!(dry, 0);
    }

    /// Blocks of 1024 behind a backlog at its start, then one block late
    /// now and then: it never runs dry (Codex: the backlog had its block
    /// guessed 3840, a short cycle watched, and a read 19 frames short).
    #[test]
    fn big_blocks_behind_a_backlog_never_run_dry() {
        for phase in [0_usize, 160, 320, 480, 640, 800] {
            let mut drift = DriftControl::new(960);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut out = vec![[0.0; 2]; 480];
            let (mut reads, mut dry, mut late) = (0_usize, 0, 0_usize);
            for tick in 0..48_000_usize * 6 {
                if (tick + phase).is_multiple_of(1024) {
                    // Every 50th block a block late, and the 54th (Codex's).
                    if (tick / 1024).is_multiple_of(50) || (tick + phase) / 1024 == 54 {
                        late += 1024;
                    } else {
                        fifo.extend(std::iter::repeat_n([0.5, 0.5], 1024 + late));
                        late = 0;
                    }
                }
                if tick % 480 == 479 {
                    if reads == 2 {
                        fifo.extend(std::iter::repeat_n([0.5, 0.5], 15_360));
                    }
                    if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                    reads += 1;
                }
            }
            assert_eq!(dry, 0, "phase {phase}");
        }
    }

    /// The same with clocks 300 parts a million apart (349 and 478 frames
    /// apart before). At 44.1 kHz in (a step off 1) its blocks against its
    /// reads repeat only after seconds: it starts as before, 409 apart, and
    /// never runs dry.
    #[test]
    fn a_reader_off_rate_lands_on_one_depth_too() {
        // Producer `push` input frames every `push` ticks (ticks: input
        // frames at the producer's clock, `fast` parts a million fast);
        // reads of `read` output frames at step `nominal`.
        fn settle(push: usize, read: usize, nominal: f64, fast: f64, target: usize, phase: usize) -> (f64, u32) {
            let mut drift = DriftControl::new(target);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut out = vec![[0.0; 2]; read];
            let period = read as f64 * nominal * (1.0 + fast * 1e-6);
            let mut next_read = period;
            let (mut sum, mut n, mut dry) = (0.0, 0u32, 0u32);
            for tick in 0..44_100 * 20 {
                if (tick + phase).is_multiple_of(push) {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], push));
                }
                if tick as f64 >= next_read {
                    next_read += period;
                    if let Some(step) = drift.step_for(&mut fifo, nominal, out.len()) {
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                    if tick > 44_100 * 10 {
                        sum += fifo.len() as f64;
                        n += 1;
                    }
                }
            }
            (sum / f64::from(n), dry)
        }
        for (name, push, nominal, fast) in [
            ("44.1k 512 in, same clock", 512, 44_100.0 / 48_000.0, 0.0),
            ("48k 512 in, 300 ppm fast", 512, 1.0, 300.0),
            ("48k 512 in, 300 ppm slow", 512, 1.0, -300.0),
        ] {
            let runs: Vec<(f64, u32)> = (0..push).step_by(32).map(|p| settle(push, 480, nominal, fast, 960, p)).collect();
            let lo = runs.iter().map(|r| r.0).fold(f64::MAX, f64::min);
            let hi = runs.iter().map(|r| r.0).fold(f64::MIN, f64::max);
            let dry: u32 = runs.iter().map(|r| r.1).sum();
            if nominal == 1.0 {
                assert!(hi - lo <= 8.0, "{name}: {lo:.0}..{hi:.0}");
            }
            assert_eq!(dry, 0, "{name}");
        }
    }

    /// A stream with the mixer waking late now and then: `ring`, the mixer
    /// pushes 480 a block into a device read `n` at a time; else the device
    /// pushes `n` into a FIFO the mixer reads 480 at a time. Every block is
    /// up to 1.5 ms late, one in 50 by 5-15 ms (then caught up). Times in
    /// frames at 48 kHz.
    fn rng(state: &mut u64) -> f64 {
        *state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((*state >> 11) as f64) / ((1u64 << 53) as f64)
    }
    // (resting depth after a read, reads steered after the first second, dry reads)
    fn jittered(ring: bool, n: usize, seed: u64) -> (f64, u32, u32) {
        let mut state = seed;
        let target = if ring { 1440 } else { 960 };
        let mut drift = if ring { DriftControl::starting_empty(target) } else { DriftControl::new(target) };
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let read = if ring { n } else { 480 };
        let mut out = vec![[0.0; 2]; read];
        // The mixer's wakes: every 480 frames, late by 0-72 frames (1.5 ms),
        // and one in 50 by 240-720 (5-15 ms; then it catches up).
        let mut mix_events: Vec<usize> = Vec::new();
        let phase = (rng(&mut state) * 480.0) as usize;
        for k in 0..2_000 {
            let due = phase + k * 480;
            let late = if rng(&mut state) < 0.02 { 240.0 + rng(&mut state) * 480.0 } else { rng(&mut state) * 72.0 };
            mix_events.push(due + late as usize);
        }
        // Catching up: a block is never mixed before the one before it.
        for i in 1..mix_events.len() {
            mix_events[i] = mix_events[i].max(mix_events[i - 1]);
        }
        let dev_phase = (rng(&mut state) * n as f64) as usize;
        let (mut sum, mut cnt, mut steered, mut dry) = (0.0, 0u32, 0u32, 0u32);
        let mut mi = 0;
        let end = 48_000 * 8;
        let mut started_at = None;
        for tick in 0..end {
            // Device event at its own exact period.
            let dev = (tick + dev_phase).is_multiple_of(n);
            // Mixer events due at this tick (possibly several).
            let mut mixes = 0;
            while mi < mix_events.len() && mix_events[mi] == tick {
                mixes += 1;
                mi += 1;
            }
            let (pushes, reads) = if ring { (mixes, usize::from(dev)) } else { (usize::from(dev), mixes) };
            for _ in 0..pushes {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], if ring { 480 } else { n }));
            }
            for _ in 0..reads {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    started_at.get_or_insert(tick);
                    if started_at.is_some_and(|s| tick > s + 48_000) && step != 1.0 {
                        steered += 1;
                    }
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
                if tick > end / 2 {
                    sum += fifo.len() as f64;
                    cnt += 1;
                }
            }
        }
        (sum / f64::from(cnt.max(1)), steered, dry)
    }

    /// A device reading 32, 64 or 128 frames a call never glides for the
    /// mixer's late blocks: smoothed over 13 ms (20 reads of 32), a 10 ms
    /// dip looked like a FIFO draining, and it steered up for good (glides
    /// after most Live closes, Max5; 24 of 24 here).
    #[test]
    fn a_mixer_late_now_and_then_never_makes_a_small_ring_glide() {
        for n in [32, 64, 128] {
            let runs: Vec<(f64, u32, u32)> = (1..=12).map(|seed| jittered(true, n, seed * 7919)).collect();
            let glides = runs.iter().filter(|r| r.1 > 0).count();
            let dry: u32 = runs.iter().map(|r| r.2).sum();
            assert_eq!((glides, dry), (0, 0), "reads of {n}: glided in {glides} of 12, ran dry {dry} times");
        }
    }

    /// The same streams, every size, by seed (exploration: run with
    /// --ignored --nocapture).
    #[test]
    #[ignore = "exploration"]
    fn landing_with_jitter() {
        for (ring, n) in [(true, 32), (true, 64), (true, 128), (true, 256), (true, 512), (false, 32), (false, 128), (false, 256), (false, 512)] {
            let runs: Vec<(f64, u32, u32)> = (1..=24).map(|seed| jittered(ring, n, seed * 7919)).collect();
            let lo = runs.iter().map(|r| r.0).fold(f64::MAX, f64::min);
            let hi = runs.iter().map(|r| r.0).fold(f64::MIN, f64::max);
            let glides = runs.iter().filter(|r| r.1 > 0).count();
            let dry: u32 = runs.iter().map(|r| r.2).sum();
            eprintln!("{} {n}: settles {lo:.0}..{hi:.0} (spread {:.0}), glided in {glides}/24, dry {dry}", if ring { "ring 480 ->" } else { "input -> 480, n =" }, hi - lo);
        }
    }

    /// How a reader started is told once, for the log: landed (blocks of
    /// 512 against reads of 480), or without landing and why (a backlog and
    /// no block).
    #[test]
    fn a_reader_tells_how_it_started() {
        let mut drift = DriftControl::new(960);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut reader = RateReader::default();
        let mut out = vec![[0.0; 2]; 480];
        let mut told = Vec::new();
        for tick in 0..48_000_usize {
            if tick.is_multiple_of(512) {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 512));
            }
            if tick % 480 == 479 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                    }
                }
                told.extend(drift.take_start());
            }
        }
        assert!(
            matches!(told[..], [Start::Landed { looks: 16, block: 512, reserve: 512, .. }]),
            "{told:?}"
        );
        let mut burst = DriftControl::new(960);
        let mut fifo: VecDeque<Frame> = (0..44_100).map(|_| [0.3; 2]).collect();
        for _ in 0..5 {
            if burst.step_for(&mut fifo, 1.0, 480).is_some() {
                break;
            }
        }
        assert!(matches!(burst.take_start(), Some(Start::Plain { why, .. }) if why.contains("no block")));
        assert_eq!(burst.take_start(), None, "told once");
    }

    /// Blocks of no fixed size, then one late: never dry (Codex: 441 and 442
    /// frames at 44.1 kHz, or 1601 and 1602 over a network, had a common
    /// divisor of 1, passed every read for a whole cycle, landed with a
    /// reserve of one read, and a late block left a read 159 short).
    #[test]
    fn blocks_of_no_fixed_size_never_run_dry() {
        // (rate, the two sizes, target, first block at, held from, until)
        for (rate, sizes, target, first, held, until) in [
            (44_100.0_f64, [441_usize, 442], 882, 130_usize, 42_514_usize, 43_218_usize),
            (48_000.0, [1601, 1602], 2880, 130, 42_514, 43_218),
        ] {
            let nominal = rate / 48_000.0;
            let period = 480.0 * nominal;
            let mut drift = DriftControl::new(target);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut out = vec![[0.0; 2]; 480];
            let (mut dry, mut next_read, mut i, mut owed) = (0, period - 1.0, 0_usize, 0.0_f64);
            let mut due = first as f64;
            for tick in 0..(rate as usize) * 3 {
                let t = tick as f64;
                // Blocks due at their size's pace; those due while held come
                // after, in order.
                while t >= due {
                    owed += 1.0;
                    due += sizes[0] as f64 + 0.5;
                }
                if !(held..until).contains(&tick) {
                    while owed >= 1.0 {
                        fifo.extend(std::iter::repeat_n([0.5, 0.5], sizes[i % 2]));
                        i += 1;
                        owed -= 1.0;
                    }
                }
                while t >= next_read {
                    next_read += period;
                    if let Some(step) = drift.step_for(&mut fifo, nominal, out.len()) {
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                }
            }
            assert_eq!(dry, 0, "blocks of {sizes:?}: ran dry {dry} times");
        }
    }
    /// Blocks of 480 and 512 by turns, then some held: never dry (Codex:
    /// their common divisor, 32, set a reserve of 480 under the 512 that
    /// came, and a read was 28 short).
    #[test]
    fn blocks_of_two_sizes_by_turns_never_run_dry() {
        let mut drift = DriftControl::new(960);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 480];
        let (mut dry, mut owed, mut i) = (0, 0_usize, 0_usize);
        for tick in 0..48_000_usize * 3 {
            if tick.is_multiple_of(496) {
                owed += 1;
            }
            if !(41_168..41_872).contains(&tick) {
                while owed > 0 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], if i.is_multiple_of(2) { 480 } else { 512 }));
                    i += 1;
                    owed -= 1;
                }
            }
            if tick % 480 == 479 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "ran dry {dry} times");
    }
    /// Blocks of 512, 512 and 2048 by turns, then some held: never dry
    /// (Codex: they looked like blocks of 512, the landing cut its reserve
    /// from 2048 to 512, let go 1634 frames, and a read was 255 short).
    #[test]
    fn blocks_of_512_512_and_2048_never_run_dry() {
        let mut drift = DriftControl::new(960);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 480];
        let (mut dry, mut owed) = (0, Vec::<usize>::new());
        for tick in 0..48_000_usize * 3 {
            match tick % 3072 {
                0 | 512 => owed.push(512),
                2560 => owed.push(2048),
                _ => {}
            }
            if !(42_496..43_200).contains(&tick) {
                for size in owed.drain(..) {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], size));
                }
            }
            if tick % 480 == 479 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "ran dry {dry} times");
    }
    /// A device reading 512 frames a call from the mixer's blocks of 480,
    /// one block 14.7 ms late: never dry (Codex: holding one block between
    /// reads, not the two that come between some, ran it dry). And the seed
    /// Codex found running it dry under the jitter.
    #[test]
    fn a_ring_read_512_at_a_time_holds_a_late_block() {
        let mut drift = DriftControl::starting_empty(1440);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 512];
        let (mut dry, mut owed) = (0, 0_usize);
        for tick in 0..48_000_usize * 3 {
            // Pushes due every 480; the one due at 48 960 comes 704 late,
            // and those after it catch up in order.
            if tick.is_multiple_of(480) {
                owed += 1;
            }
            let late = (48_960..48_960 + 704).contains(&tick);
            while owed > 0 && !late {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 480));
                owed -= 1;
            }
            if tick % 512 == 511 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "ran dry {dry} times");
        assert_eq!(jittered(true, 512, 96 * 7919).2, 0, "seed 96 ran dry");
    }
    /// Two more schedules that ran it dry where the old start did not
    /// (Codex): a ring read 128 at a time, its landing drained under a read
    /// while the mixer was late, started a read it could not fill; and a
    /// ring read 512 at a time let go more by its depth's middle than its
    /// mean, and a late block left a read 26 short.
    #[test]
    fn late_blocks_while_landing_never_run_a_ring_dry() {
        for (read, phase, holds) in [
            (128_usize, 30_usize, [(2_300_usize, 2_500_usize), (4_000, 5_400)]),
            (512, 0, [(4_200, 4_650), (18_300, 20_000)]),
        ] {
            let mut drift = DriftControl::starting_empty(1440);
            let mut reader = RateReader::default();
            let mut fifo: VecDeque<Frame> = VecDeque::new();
            let mut out = vec![[0.0; 2]; read];
            let (mut dry, mut owed) = (0, 0_usize);
            for tick in 0..48_000_usize * 3 {
                if (tick + phase).is_multiple_of(480) {
                    owed += 1;
                }
                if !holds.iter().any(|(a, b)| (*a..*b).contains(&tick)) {
                    while owed > 0 {
                        fifo.extend(std::iter::repeat_n([0.5, 0.5], 480));
                        owed -= 1;
                    }
                }
                if tick % read == read - 1 {
                    if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                        if reader.read(&mut fifo, step, &mut out) < out.len() {
                            drift.ran_dry();
                            dry += 1;
                        }
                    }
                }
            }
            assert_eq!(dry, 0, "reads of {read}: ran dry {dry} times");
        }
    }

    /// Three more schedules from a review of a later landing, which the
    /// landing as it is survives (Codex): a ring read 128 at a time with the
    /// mixer late twice; an 11 kHz input of 128-frame blocks read 110.25 at a
    /// time with its blocks held; and a ring whose reads grow from 32 frames
    /// to 8192.
    #[test]
    fn more_late_schedules_never_run_dry() {
        // A ring read 128 at a time, blocks of 480 at 273 + k * 480.
        let mut drift = DriftControl::starting_empty(1440);
        let mut reader = RateReader::default();
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 128];
        let (mut dry, mut owed) = (0, 0_usize);
        for tick in 0..48_000_usize * 3 {
            if tick >= 273 && (tick - 273).is_multiple_of(480) {
                owed += 1;
            }
            if !(2_600..2_830).contains(&tick) && !(17_000..18_200).contains(&tick) {
                while owed > 0 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 480));
                    owed -= 1;
                }
            }
            if tick % 128 == 127 {
                if let Some(step) = drift.step_for(&mut fifo, 1.0, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "a ring read 128 at a time: ran dry {dry} times");
        // An 11 kHz input, blocks of 128 at 78 + k * 128, read 110.25 at a time.
        let nominal = 11_025.0 / 48_000.0;
        let mut drift = DriftControl::new(220);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let mut out = vec![[0.0; 2]; 480];
        let (mut dry, mut owed, mut next_read) = (0, 0_usize, 110.25_f64 - 1.0);
        for tick in 0..11_025_usize * 3 {
            if tick >= 78 && (tick - 78).is_multiple_of(128) {
                owed += 1;
            }
            if !(11_000..11_248).contains(&tick) {
                while owed > 0 {
                    fifo.extend(std::iter::repeat_n([0.5, 0.5], 128));
                    owed -= 1;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            while tick as f64 >= next_read {
                next_read += 110.25;
                if let Some(step) = drift.step_for(&mut fifo, nominal, out.len()) {
                    if reader.read(&mut fifo, step, &mut out) < out.len() {
                        drift.ran_dry();
                        dry += 1;
                    }
                }
            }
        }
        assert_eq!(dry, 0, "an 11 kHz input: ran dry {dry} times");
        // A ring read 32 at a time, then 8192.
        let mut drift = DriftControl::starting_empty(1440);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        let (mut short, mut next) = (0, 31_usize);
        for tick in 0..48_000_usize * 3 {
            if tick.is_multiple_of(480) {
                fifo.extend(std::iter::repeat_n([0.5, 0.5], 480));
            }
            if tick == next {
                let n = if tick < 2_015 { 32 } else { 8_192 };
                next += if tick + 32 < 2_015 { 32 } else if tick < 2_015 { 2_015 - tick } else { 8_192 };
                let mut out = vec![[0.0; 2]; n];
                if let Some(step) = drift.step_for(&mut fifo, 1.0, n) {
                    if reader.read(&mut fifo, step, &mut out) < n {
                        drift.ran_dry();
                        short += 1;
                    }
                }
            }
        }
        assert_eq!(short, 0, "reads grown to 8192: {short} short");
    }

    /// An empty look, then one burst and no more: it starts all the same
    /// (taken for a block, once it sees its producer has stopped: 9 reads).
    #[test]
    fn a_burst_after_an_empty_look_still_starts() {
        let mut drift = DriftControl::new(960);
        let mut fifo: VecDeque<Frame> = VecDeque::new();
        assert_eq!(drift.step_for(&mut fifo, 1.0, 480), None);
        fifo.extend(std::iter::repeat_n([0.3, 0.3], 44_100));
        let started = (0..20).any(|_| drift.step_for(&mut fifo, 1.0, 480).is_some());
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
