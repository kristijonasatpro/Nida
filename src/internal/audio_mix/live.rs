//! A live bus's mixing, inside its output device's callback: its mics,
//! straight from their capture's callback on the same device, each through
//! its own processing, fader and balance, then the bus's compressor, fader,
//! limiter and delay — what the mixer does, a callback at a time (64 frames,
//! 1.3 ms) rather than a 10 ms block behind two drift buffers. The sound of
//! a mic is out about three buffers after it came in.
//!
//! Nothing here waits or allocates: settings come over a `try_lock`, taken
//! only when they changed (a miss is taken the next call); gains are
//! atomics, glided a sample at a time.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::core::{MAX_DELAY_MS, SAMPLE_RATE};
use super::dsp::{
    Compressor, CompressorConfig, DeEsserConfig, EqConfig, GateConfig, LevelerConfig, Limiter,
    LimiterConfig, LowCutConfig,
};
use super::dynamics::SourceChain;
use super::reader::{DriftControl, Frame, RateReader};
use crate::audio::{LiveFrame, LIVE_MICS};

/// The largest buffer a live bus takes: a device that will not go this low
/// is played the usual way.
pub const LIVE_MOST_FRAMES: u32 = 1024;

/// The most frames one callback mixes: twice the most asked for. A live bus
/// waits for two callbacks' worth, and its feed holds 4096 frames.
pub const LIVE_MOST_CALL: usize = 2 * LIVE_MOST_FRAMES as usize;

/// A live mic's processing and balance, as its strip's.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveMicSettings {
    pub low_cut: LowCutConfig,
    pub gate: GateConfig,
    pub eq: EqConfig,
    pub de_esser: DeEsserConfig,
    pub leveler: LevelerConfig,
    pub compressor: CompressorConfig,
    /// −1 (left) to 1 (right).
    pub balance: f32,
}

/// A live bus's processing, as its strips' and its own: its mics in its
/// feed's order.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LiveSettings {
    pub mics: [LiveMicSettings; LIVE_MICS],
    pub compressor: CompressorConfig,
    pub limiter: LimiterConfig,
    pub delay_ms: f32,
}

/// A live bus's settings, from the engine to its device's callback.
#[derive(Debug)]
pub struct LiveControl {
    settings: Mutex<LiveSettings>,
    /// Counts changes: the callback looks only when it moved.
    version: AtomicU64,
    /// Counts shows loaded: the callback starts its processing afresh.
    afresh: AtomicU64,
    /// Each mic's leveler gain as the mixer's own leveler for its strip
    /// has it now (an `f32`'s bits, dB): where a callback opened again
    /// starts riding (see [`LiveControl::set_levelers`]).
    levelers: [AtomicU32; LIVE_MICS],
}

impl Default for LiveControl {
    fn default() -> Self {
        Self {
            settings: Mutex::default(),
            version: AtomicU64::new(0),
            afresh: AtomicU64::new(0),
            // None yet: a fresh leveler.
            levelers: std::array::from_fn(|_| AtomicU32::new(f32::NAN.to_bits())),
        }
    }
}

impl LiveControl {
    /// New settings, if they are.
    pub fn set(&self, settings: &LiveSettings) {
        if let Ok(mut held) = self.settings.lock() {
            if *held != *settings {
                *held = *settings;
                self.version.fetch_add(1, Ordering::Release);
            }
        }
    }

    /// Start the processing afresh, as a show loaded does the mixer's: what
    /// its levelers learned and its compressors and delay held was the old
    /// show's.
    pub fn start_afresh(&self) {
        // The old show's leveler gains go first: a callback made between
        // this and the next tracking would otherwise start from them.
        for slot in &self.levelers {
            slot.store(f32::NAN.to_bits(), Ordering::Relaxed);
        }
        self.afresh.fetch_add(1, Ordering::Release);
    }

    /// Shows loaded so far, for tests.
    #[cfg(test)]
    pub fn afreshes(&self) -> u64 {
        self.afresh.load(Ordering::Acquire)
    }

    /// Each mic's leveler gain, dB, as the mixer's own leveler for its
    /// strip has it now, in the feed's order.
    pub fn set_levelers(&self, gains: impl IntoIterator<Item = f32>) {
        for (slot, db) in self.levelers.iter().zip(gains) {
            slot.store(db.to_bits(), Ordering::Relaxed);
        }
    }

    /// Mic `i`'s leveler gain as last set, for tests.
    #[cfg(test)]
    pub fn leveler(&self, i: usize) -> f32 {
        f32::from_bits(self.levelers[i].load(Ordering::Relaxed))
    }

    /// The settings now, waited for: off the callback.
    fn now(&self) -> (LiveSettings, u64) {
        let version = self.version.load(Ordering::Acquire);
        let settings = self.settings.lock().map(|s| *s).unwrap_or_default();
        (settings, version)
    }

    /// The settings, if they changed since `seen` and are not being changed
    /// this instant: never waits (the callback's).
    fn changed(&self, seen: &mut u64) -> Option<LiveSettings> {
        let version = self.version.load(Ordering::Acquire);
        if version == *seen {
            return None;
        }
        let settings = *self.settings.try_lock().ok()?;
        *seen = version;
        Some(settings)
    }
}

/// A live bus's mics: their frames, every mic in each, straight from
/// their capture's callback; each mic's gain (an `f32`'s bits: fader, On
/// and fades; nought while it is off), in the frames' order; the bus's
/// settings; and the rest of the bus, from the mix (see `core::LiveSend`).
pub struct LiveMics {
    pub frames: crossbeam_channel::Receiver<LiveFrame>,
    /// What their capture has promised and what is gone: what will be in
    /// hand once its chunk in flight is in.
    pub counts: Arc<crate::audio::LiveCounts>,
    pub gains: Vec<Arc<AtomicU32>>,
    pub control: Arc<LiveControl>,
    pub rest: crossbeam_channel::Receiver<Frame>,
}

/// How many frames of the rest of a live bus wait to be sent at most: half
/// a second, as a device's ring.
pub const LIVE_REST_FRAMES: usize = SAMPLE_RATE as usize / 2;

/// What a live bus's callback did with its mics' queue, for the log: what
/// made the sound start, jump or run short, to the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveEventKind {
    /// Two callbacks' worth came: playing from here.
    Started,
    /// Frames let go (too many in hand).
    LetGo,
    /// Ran short: silence, and waiting to start again.
    Short,
    /// The capture's chunk changed size.
    Chunk,
    /// The quiet after a device opened is over: the mics fade in.
    Heard,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveEvent {
    pub kind: LiveEventKind,
    /// Frames the callback had played before it.
    pub at: u64,
    pub waiting: usize,
    pub coming: usize,
    /// A callback's worth then.
    pub n: usize,
    pub chunk: usize,
    /// Frames let go, for a start or a let-go.
    pub let_go: usize,
    /// Microseconds since the mix was made (its stream built): against the
    /// frames played, whether its callbacks ran ahead of the clock.
    pub since_us: u64,
}

impl std::fmt::Display for LiveEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.kind {
            LiveEventKind::Started => "started",
            LiveEventKind::LetGo => "let go",
            LiveEventKind::Short => "ran short",
            LiveEventKind::Chunk => "capture chunk now",
            LiveEventKind::Heard => "mics fading in",
        };
        write!(
            f,
            "{what} at frame {} (in hand {}, coming {}, a callback's worth {}, capture chunk {}, let go {}; {:.1} ms after the stream was made)",
            self.at,
            self.waiting,
            self.coming,
            self.n,
            self.chunk,
            self.let_go,
            self.since_us as f64 / 1000.0
        )
    }
}

/// A device just opened, its sound — mics and the rest of the bus — waits
/// this long after starting before it fades in: 120 ms. The device may still
/// be changing its buffer then: a chunk left over is let go about 20 ms in
/// (Max5: 20.2 ms, every time), and one open in six swallows the new
/// stream's first 70–95 ms (Max5: it came in that late at full level, the
/// bus's other sound with the mic) — both in silence, the fade-in heard.
const LIVE_OPEN_QUIET_FRAMES: usize = SAMPLE_RATE as usize * 12 / 100;

/// The longest a device just opened keeps the rest of its bus silent waiting
/// for its mics to start: half a second — mics that never come must not keep
/// the bus's other sound away for good.
const LIVE_OPEN_WAIT_MOST: u64 = SAMPLE_RATE as u64 / 2;

/// How many of a callback's events wait for the log: more are dropped.
const LIVE_EVENTS_HELD: usize = 64;

/// A callback's events on their way to the log: a ring made beforehand, one
/// writer (the callback) and one reader (the outputs thread), each side only
/// storing and loading atomics — the callback never waits, and drops an
/// event when the ring is full.
pub struct LiveEvents {
    slots: Box<[[AtomicU64; 8]]>,
    /// Events written, and read, so far.
    written: AtomicUsize,
    read: AtomicUsize,
}

impl Default for LiveEvents {
    fn default() -> Self {
        Self {
            slots: (0..LIVE_EVENTS_HELD).map(|_| std::array::from_fn(|_| AtomicU64::new(0))).collect(),
            written: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
        }
    }
}

impl LiveEvents {
    /// The callback's side: written, or dropped when full.
    fn push(&self, e: LiveEvent) {
        let written = self.written.load(Ordering::Relaxed);
        if written - self.read.load(Ordering::Acquire) >= LIVE_EVENTS_HELD {
            return;
        }
        let kind = match e.kind {
            LiveEventKind::Started => 0,
            LiveEventKind::LetGo => 1,
            LiveEventKind::Short => 2,
            LiveEventKind::Chunk => 3,
            LiveEventKind::Heard => 4,
        };
        let fields = [
            kind,
            e.at,
            e.waiting as u64,
            e.coming as u64,
            e.n as u64,
            e.chunk as u64,
            e.let_go as u64,
            e.since_us,
        ];
        for (slot, v) in self.slots[written % LIVE_EVENTS_HELD].iter().zip(fields) {
            slot.store(v, Ordering::Relaxed);
        }
        self.written.store(written + 1, Ordering::Release);
    }

    /// The reader's side: every event written since last asked.
    pub fn drain(&self) -> Vec<LiveEvent> {
        let written = self.written.load(Ordering::Acquire);
        let mut read = self.read.load(Ordering::Relaxed);
        let mut out = Vec::new();
        #[allow(clippy::cast_possible_truncation)] // Frame counts written as usize.
        while read < written {
            let f: [u64; 8] = std::array::from_fn(|i| self.slots[read % LIVE_EVENTS_HELD][i].load(Ordering::Relaxed));
            out.push(LiveEvent {
                kind: match f[0] {
                    0 => LiveEventKind::Started,
                    1 => LiveEventKind::LetGo,
                    2 => LiveEventKind::Short,
                    3 => LiveEventKind::Chunk,
                    _ => LiveEventKind::Heard,
                },
                at: f[1],
                waiting: f[2] as usize,
                coming: f[3] as usize,
                n: f[4] as usize,
                chunk: f[5] as usize,
                let_go: f[6] as usize,
                since_us: f[7],
            });
            read += 1;
        }
        self.read.store(read, Ordering::Release);
        out
    }
}

/// How long a whole chunk too many must stay in hand before it is let go:
/// 20 ms, many reads at a small buffer, a few at the largest.
const LIVE_LATE_FRAMES: usize = SAMPLE_RATE as usize / 50;

/// How deep the rest of a live bus is held: as a device's ring is, so it
/// is heard as late as the usual way — the mix's 10 ms timer is another
/// clock than the device's.
const LIVE_REST_DEPTH_SECS: f64 = 0.03;

/// The rest of a live bus, read at the device's pace through a FIFO held at
/// [`LIVE_REST_DEPTH_SECS`]: dry or priming, that part is silent and the
/// mics go on.
struct Rest {
    rx: crossbeam_channel::Receiver<Frame>,
    fifo: VecDeque<Frame>,
    reader: RateReader,
    drift: DriftControl,
    block: Vec<Frame>,
}

impl Rest {
    fn new(rx: crossbeam_channel::Receiver<Frame>) -> Self {
        // What waited for a device that was not reading is stale.
        while rx.try_recv().is_ok() {}
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let depth = (f64::from(SAMPLE_RATE) * LIVE_REST_DEPTH_SECS) as usize;
        Self {
            rx,
            fifo: VecDeque::with_capacity(LIVE_REST_FRAMES),
            reader: RateReader::default(),
            // Emptied again at the first callback: what the mix sent while
            // the stream was built is as late.
            drift: DriftControl::starting_empty(depth),
            block: vec![[0.0; 2]; LIVE_MOST_CALL],
        }
    }

    /// Add `block.len()` frames of it into `block`.
    fn add_into(&mut self, block: &mut [Frame]) {
        while self.fifo.len() < LIVE_REST_FRAMES {
            let Ok(frame) = self.rx.try_recv() else {
                break;
            };
            self.fifo.push_back(frame);
        }
        let part = &mut self.block[..block.len()];
        part.fill([0.0; 2]);
        if let Some(step) = self.drift.step_for(&mut self.fifo, 1.0, part.len()) {
            if self.reader.read(&mut self.fifo, step, part) < part.len() {
                self.drift.ran_dry();
            }
        }
        for (out, sound) in block.iter_mut().zip(part.iter()) {
            out[0] += sound[0];
            out[1] += sound[1];
        }
    }
}

/// A gain moved toward its target a little each frame: a change makes no
/// click (about 10 ms to settle at 48 kHz).
const LIVE_GAIN_STEP: f32 = 0.002;

/// A bus's delay inside the callback: a ring made beforehand for the
/// longest ([`MAX_DELAY_MS`]). A longer delay brings in silence and a
/// shorter one skips ahead, as the mixer's does.
struct LiveDelay {
    held: Vec<Frame>,
    at: usize,
    frames: usize,
}

impl LiveDelay {
    fn new() -> Self {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let most = (f64::from(MAX_DELAY_MS) / 1000.0 * f64::from(SAMPLE_RATE)).round() as usize;
        Self {
            held: vec![[0.0; 2]; most + 1],
            at: 0,
            frames: 0,
        }
    }

    fn run(&mut self, block: &mut [Frame], delay_ms: f32) {
        let ms = if delay_ms.is_finite() {
            delay_ms.clamp(0.0, MAX_DELAY_MS)
        } else {
            0.0
        };
        let len = self.held.len();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let frames =
            ((f64::from(ms) / 1000.0 * f64::from(SAMPLE_RATE)).round() as usize).min(len - 1);
        if frames > self.frames {
            // Longer: what is now that far back was heard already, or is
            // from long ago — silence in its place.
            for back in self.frames + 1..=frames {
                self.held[(self.at + len - back) % len] = [0.0; 2];
            }
        }
        self.frames = frames;
        if frames == 0 {
            return;
        }
        for slot in block.iter_mut() {
            self.held[self.at] = *slot;
            *slot = self.held[(self.at + len - frames) % len];
            self.at = (self.at + 1) % len;
        }
    }
}

/// The mixing of a live bus inside its device's callback: its mics' frames
/// taken as they are needed, a callback's worth kept in hand.
pub struct LiveMix {
    mics: LiveMics,
    /// Each mic's gain now, each side, moving toward fader × balance.
    now: Vec<Frame>,
    bus_gain: Arc<AtomicU32>,
    bus_now: f32,
    /// Frames a callback takes (the buffer asked for).
    frames: usize,
    /// Enough waiting to start: two callbacks' worth, one kept in hand so
    /// the order of the input's and output's callbacks does not matter.
    primed: bool,
    /// The most in hand at a read this capture period, and the frames read
    /// in it (see `mix`).
    period_most: usize,
    period_read: usize,
    /// Periods in a row, and their frames, with at least a read's worth too
    /// many at their fullest; the least too many among them; and the
    /// callback's worth they were counted at.
    late_periods: u32,
    late_for: usize,
    late_by: usize,
    /// Frames found late for long enough, to let go at the next read that
    /// still has them.
    let_go_owed: usize,
    /// The callback's worth, capture chunk and read they were counted at.
    late_sizes: (usize, usize, usize),
    underruns: Arc<AtomicU64>,
    /// What it did with the queue, for the log (see [`LiveEvent`]); the
    /// frames played so far; the capture chunk last seen.
    events: Arc<LiveEvents>,
    played: u64,
    chunk_seen: usize,
    /// When it was made: for the events.
    made: std::time::Instant,
    /// Frames its mics stay silent yet after its first start (see
    /// [`LIVE_OPEN_QUIET_FRAMES`]); `None` before that start.
    quiet_left: Option<usize>,
    /// The largest callback's worth and capture chunk seen since that first
    /// start: only growing past them starts again in the quiet.
    quiet_most: (usize, usize),
    /// Each mic's processing: low cut, leveler, compressor.
    chains: Vec<SourceChain>,
    balance: Vec<f32>,
    compressor: Compressor,
    limiter: Limiter,
    delay: LiveDelay,
    delay_ms: f32,
    /// The settings taken, and their version.
    settings: LiveSettings,
    seen: u64,
    /// Shows loaded, as last seen (see [`LiveControl::start_afresh`]).
    afresh: u64,
    /// Each mic's sound this call, made beforehand.
    mic_blocks: Vec<Vec<Frame>>,
    /// The rest of the bus, from the mix.
    rest: Rest,
}

impl LiveMix {
    /// Built off the callback: everything it needs made here.
    pub fn new(
        mics: LiveMics,
        bus_gain: Arc<AtomicU32>,
        frames: usize,
        underruns: Arc<AtomicU64>,
    ) -> Self {
        let count = mics.gains.len().min(LIVE_MICS);
        let (settings, seen) = mics.control.now();
        let afresh = mics.control.afresh.load(Ordering::Acquire);
        let levelers: [f32; LIVE_MICS] = std::array::from_fn(|i| {
            f32::from_bits(mics.control.levelers[i].load(Ordering::Relaxed))
        });
        let mut mix = Self {
            now: vec![[0.0; 2]; count],
            bus_gain,
            bus_now: 0.0,
            frames,
            primed: false,
            period_most: 0,
            period_read: 0,
            late_periods: 0,
            late_for: 0,
            late_by: usize::MAX,
            let_go_owed: 0,
            events: Arc::default(),
            played: 0,
            chunk_seen: 0,
            made: std::time::Instant::now(),
            quiet_left: None,
            quiet_most: (0, 0),
            late_sizes: (0, 0, 0),
            underruns,
            chains: vec![SourceChain::default(); count],
            balance: vec![0.0; count],
            compressor: Compressor::default(),
            limiter: Limiter::default(),
            delay: LiveDelay::new(),
            delay_ms: 0.0,
            settings,
            seen,
            afresh,
            mic_blocks: vec![vec![[0.0; 2]; LIVE_MOST_CALL]; count],
            rest: Rest::new(mics.rest.clone()),
            mics,
        };
        mix.configure(&settings);
        // Its levelers ride on from the mixer's, not from 0 dB: opened
        // again (a buffer changed, Live switched on), the voice did not jump.
        for (chain, db) in mix.chains.iter_mut().zip(levelers) {
            chain.start_leveler_at(db);
        }
        mix
    }

    fn configure(&mut self, settings: &LiveSettings) {
        self.settings = *settings;
        for (i, chain) in self.chains.iter_mut().enumerate() {
            let mic = settings.mics[i];
            chain.configure(&super::dynamics::SourceSettings {
                low_cut: mic.low_cut,
                gate: mic.gate,
                eq: mic.eq,
                de_esser: mic.de_esser,
                leveler: mic.leveler,
                compressor: mic.compressor,
            });
            self.balance[i] = if mic.balance.is_finite() {
                mic.balance.clamp(-1.0, 1.0)
            } else {
                0.0
            };
        }
        self.compressor.configure(settings.compressor.sanitized());
        self.limiter.configure(settings.limiter.sanitized());
        self.delay_ms = settings.delay_ms;
    }

    /// The processing as new — plain data made anew, no allocation — with
    /// its settings; the delay's ring silenced. The gains glide on.
    fn start_afresh(&mut self) {
        self.chains
            .iter_mut()
            .for_each(|c| *c = SourceChain::default());
        self.compressor = Compressor::default();
        self.limiter = Limiter::default();
        self.delay.held.fill([0.0; 2]);
        self.delay.frames = 0;
        let settings = self.settings;
        self.configure(&settings);
    }

    /// What it did with its queue, as it happens (see [`LiveEvent`]).
    pub fn events(&self) -> Arc<LiveEvents> {
        Arc::clone(&self.events)
    }

    fn tell(
        &self,
        kind: LiveEventKind,
        waiting: usize,
        coming: usize,
        n: usize,
        chunk: usize,
        let_go: usize,
    ) {
        self.events.push(LiveEvent {
            kind,
            at: self.played,
            waiting,
            coming,
            n,
            chunk,
            let_go,
            since_us: u64::try_from(self.made.elapsed().as_micros()).unwrap_or(u64::MAX),
        });
    }

    /// The late watch started afresh (see `mix`).
    fn forget_late(&mut self) {
        self.period_most = 0;
        self.period_read = 0;
        self.late_periods = 0;
        self.late_for = 0;
        self.late_by = usize::MAX;
        self.let_go_owed = 0;
    }

    /// Mix `block.len()` frames (at most [`LIVE_MOST_CALL`]) into `block`.
    pub fn mix(&mut self, block: &mut [Frame]) {
        block.fill([0.0; 2]);
        let len = block.len().min(LIVE_MOST_CALL);
        let block = &mut block[..len];
        if let Some(settings) = self.mics.control.changed(&mut self.seen) {
            self.configure(&settings);
        }
        let afresh = self.mics.control.afresh.load(Ordering::Acquire);
        if afresh != self.afresh {
            self.afresh = afresh;
            self.start_afresh();
        }
        let held = self.mics.frames.capacity().unwrap_or(usize::MAX);
        let waiting = self.mics.frames.len();
        // What will be in hand once the capture's chunk in flight is in: it
        // pushes frame by frame on its own thread, and a look in mid-chunk
        // finds it short. (Read after `waiting`: never more than there will
        // be, nor more than the queue holds; the chunk, as of the frames seen.)
        let (coming, chunk) = self.mics.counts.coming(waiting);
        let coming = coming.min(held);
        // A callback's worth: this call's, the bus's buffer, or the
        // capture's chunk, whichever is longest — held twice over, the mics
        // last from one capture callback to the next. The chunk counts up to
        // a quarter of what the queue holds: a longer one could never fill
        // twice over, and the mics would never start.
        let n = len.max(self.frames).max(chunk.min(held / 4));
        // Sizes changed while its mics are still held silent after a start
        // (the device changing its buffer under it): start again at the new
        // sizes. Started on the old capture chunk, it stayed a chunk short of
        // its margin (Max5: 770 samples, not 898, at 256 after 128).
        // Only growing past what it has already met restarts it: sizes that
        // go back and forth (a chunk of 64, then 128, then 64) would restart
        // it for ever, the mics never heard.
        let sizes = (n, chunk, len);
        let quiet = self.quiet_left.is_some_and(|left| left > 0);
        if self.primed && quiet && (n > self.quiet_most.0 || chunk > self.quiet_most.1) {
            self.primed = false;
            self.quiet_left = None;
        }
        if quiet || self.quiet_left.is_none() {
            self.quiet_most = (self.quiet_most.0.max(n), self.quiet_most.1.max(chunk));
        }
        let starting = !self.primed && waiting >= 2 * n;
        if starting {
            self.primed = true;
        }
        if chunk != self.chunk_seen {
            self.chunk_seen = chunk;
            self.tell(LiveEventKind::Chunk, waiting, coming, n, chunk, 0);
        }
        let mut gone = 0_u64;
        // Starting with more than two callbacks' worth: back to exactly two,
        // the oldest whole frames let go (started with more, the mics stayed
        // that much later for as long as it played: one in a few opens was a
        // buffer late, 8.0 ms, not 6.7, at 64 frames). Counted with the chunk
        // in flight: counted without it, the mics stayed as much later as it
        // had yet to push (6 samples on the Studio, every open, in one build
        // and not the one before).
        //
        // Playing, what is in hand at its fullest — the read just after the
        // capture's promise, once in every capture period (a chunk and a
        // read) — is two callbacks' worth, on one clock, and never more. A read's worth or more over that at the
        // fullest of every period for 20 ms (two periods at least): the
        // output lost reads the capture did not (the device changing its
        // buffer under it, or the output held up), or the two callbacks
        // swapped turns — and the mics would stay that much later for good (a
        // buffer late on the Studio, every few opens at 128 after 64). That
        // much is let go at the next read that still has it: one come back
        // meanwhile (a late read caught up) keeps what it has, and a period
        // back to two forgets it. Over it in one period only, it is one read
        // come a little late, and letting go then would click. (With a chunk
        // and a read that do not divide — rare, a device's buffers being
        // powers of two — a period may miss the fullest phase and the loss
        // stay; four callbacks' worth or more is back to two at once anyway.)
        //
        // Never below this call's worth in hand: the rest of the chunk is on
        // its way, but this call plays now.
        if !self.primed || starting || sizes != self.late_sizes {
            self.forget_late();
        }
        self.late_sizes = sizes;
        // Chunk and read alike, every read is the fullest: one back at two
        // callbacks' worth is no longer late (a late read caught up, again
        // and again, must not add up to a loss).
        if chunk == len && coming <= 2 * n {
            self.forget_late();
        }
        self.period_most = self.period_most.max(coming);
        self.period_read += len;
        let mut let_go = 0;
        if self.primed && !starting && self.period_read >= chunk.max(len) + len {
            let over = self.period_most.saturating_sub(2 * n);
            if over >= len {
                self.late_periods += 1;
                self.late_for += self.period_read;
                self.late_by = self.late_by.min(over);
            } else {
                self.forget_late();
            }
            self.period_most = 0;
            self.period_read = 0;
            if self.late_periods >= 2 && self.late_for >= LIVE_LATE_FRAMES {
                self.let_go_owed = self.late_by;
            }
        }
        // Let go only what this read still has over two callbacks' worth.
        if self.let_go_owed > 0 && coming > 2 * n {
            let_go = self.let_go_owed.min(coming - 2 * n);
        }
        if (starting && coming > 2 * n) || (self.primed && coming >= 4 * n) {
            let_go = coming - 2 * n;
        }
        let gone_before = gone;
        if let_go > 0 {
            for _ in 0..let_go.min(waiting.saturating_sub(len)) {
                if self.mics.frames.try_recv().is_ok() {
                    gone += 1;
                }
            }
            // What was seen before the trim says nothing of after it.
            self.forget_late();
        }
        #[allow(clippy::cast_possible_truncation)] // Frames of one call.
        let let_go_now = (gone - gone_before) as usize;
        if starting {
            if self.quiet_left.is_none() {
                self.quiet_left = Some(LIVE_OPEN_QUIET_FRAMES);
            }
            self.tell(
                LiveEventKind::Started,
                waiting,
                coming,
                n,
                chunk,
                let_go_now,
            );
        } else if let_go_now > 0 {
            self.tell(LiveEventKind::LetGo, waiting, coming, n, chunk, let_go_now);
        }
        // Each mic's sound this call; silence for what has not come — and
        // all of it while waiting to start: silence goes through the bus as
        // sound would, so its limiter and delay hold the gap, not stale
        // sound.
        let mut got = 0;
        while self.primed && got < len {
            let Ok(frame) = self.mics.frames.try_recv() else {
                break;
            };
            gone += 1;
            for (i, mic) in self.mic_blocks.iter_mut().enumerate() {
                mic[got] = [frame[2 * i], frame[2 * i + 1]];
            }
            got += 1;
        }
        if gone > 0 {
            self.mics.counts.gone.fetch_add(gone, Ordering::Release);
        }
        for mic in &mut self.mic_blocks {
            mic[got..len].fill([0.0; 2]);
        }
        // Each mic through its processing, then its fader and balance, into
        // the bus — as the mixer's strips.
        for (i, mic) in self.mic_blocks.iter_mut().enumerate() {
            let mic = &mut mic[..len];
            self.chains[i].process(mic);
            let g = self
                .mics
                .gains
                .get(i)
                .map_or(0.0, |g| f32::from_bits(g.load(Ordering::Relaxed)));
            let b = self.balance[i];
            // Silent until the quiet after its first start is over: glided up
            // while waiting to start, a long wait opened at full level, no
            // fade-in (Max5: 2 opens in 16).
            let g = if self.quiet_left.is_none_or(|left| left > 0) { 0.0 } else { g };
            let target = [g * (1.0 - b).min(1.0), g * (1.0 + b).min(1.0)];
            let now = &mut self.now[i];
            for (out, sound) in block.iter_mut().zip(mic.iter()) {
                now[0] += (target[0] - now[0]) * LIVE_GAIN_STEP;
                now[1] += (target[1] - now[1]) * LIVE_GAIN_STEP;
                out[0] += sound[0] * now[0];
                out[1] += sound[1] * now[1];
            }
        }
        // The rest of the bus, from the mix, at the usual delay.
        self.rest.add_into(block);
        // The bus: compressor, fader, limiter, delay — as the mixer's.
        self.compressor.process(block);
        let bus_target = f32::from_bits(self.bus_gain.load(Ordering::Relaxed));
        // The whole stream silent until the quiet after its open is over —
        // waiting for its mics no longer than half a second.
        let opening = match self.quiet_left {
            Some(left) => left > 0,
            None => self.played < LIVE_OPEN_WAIT_MOST,
        };
        let bus_target = if opening { 0.0 } else { bus_target };
        for frame in block.iter_mut() {
            self.bus_now += (bus_target - self.bus_now) * LIVE_GAIN_STEP;
            frame[0] *= self.bus_now;
            frame[1] *= self.bus_now;
        }
        self.limiter.process(block);
        self.delay.run(block, self.delay_ms);
        if self.primed && got < len {
            // Ran short: silence where sound was missing, and wait for two
            // callbacks' worth again.
            self.primed = false;
            self.underruns.fetch_add(1, Ordering::Relaxed);
            self.tell(LiveEventKind::Short, got, got, len, self.chunk_seen, 0);
        }
        self.played += len as u64;
        let mut heard = false;
        if let Some(left) = &mut self.quiet_left {
            heard = *left > 0 && *left <= len;
            *left = left.saturating_sub(len);
        }
        if heard {
            self.tell(LiveEventKind::Heard, waiting, coming, n, chunk, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{LiveControl, LiveMics, LiveMix, LiveSettings};
    use crate::audio::{LiveFrame, LIVE_MICS};
    use crate::internal::audio_mix::dsp::{LimiterConfig, LowCutConfig, LIMITER_LOOKAHEAD};
    use crate::internal::audio_mix::reader::Frame;
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
    use std::sync::Arc;

    struct Rig {
        mix: LiveMix,
        tx: crossbeam_channel::Sender<LiveFrame>,
        counts: Arc<crate::audio::LiveCounts>,
        rest: crossbeam_channel::Sender<Frame>,
        gains: Vec<Arc<AtomicU32>>,
        control: Arc<LiveControl>,
        /// Frames sent so far.
        sent: usize,
        /// The capture's chunk: what `send` sends at a time.
        chunk: usize,
    }

    /// A live bus of `mics` mics at unity, 64 frames a call, with
    /// `settings`.
    fn rig(mics: usize, settings: &LiveSettings) -> Rig {
        rig_holding(mics, settings, 1 << 16)
    }

    /// [`rig`] with its mics' queue holding `held` frames.
    fn rig_holding(mics: usize, settings: &LiveSettings, held: usize) -> Rig {
        let (tx, rx) = crossbeam_channel::bounded(held);
        let gains: Vec<_> = (0..mics)
            .map(|_| Arc::new(AtomicU32::new(1.0_f32.to_bits())))
            .collect();
        let control = Arc::new(LiveControl::default());
        control.set(settings);
        let (rest, rest_rx) = crossbeam_channel::bounded(super::LIVE_REST_FRAMES);
        let counts = Arc::new(crate::audio::LiveCounts::default());
        let mut mix = LiveMix::new(
            LiveMics {
                frames: rx,
                counts: Arc::clone(&counts),
                gains: gains.clone(),
                control: Arc::clone(&control),
                rest: rest_rx,
            },
            Arc::new(AtomicU32::new(1.0_f32.to_bits())),
            64,
            Arc::new(AtomicU64::new(0)),
        );
        // At their gains from the start, and no quiet at the open: both are
        // tested apart.
        mix.now.iter_mut().for_each(|g| *g = [1.0, 1.0]);
        mix.quiet_left = Some(0);
        mix.bus_now = 1.0;
        Rig {
            mix,
            tx,
            counts,
            rest,
            gains,
            control,
            sent: 0,
            chunk: 64,
        }
    }

    /// The limiter off: these hear the sound as sent (a look-ahead late).
    fn plain() -> LiveSettings {
        LiveSettings {
            limiter: LimiterConfig {
                enabled: false,
                ..LimiterConfig::default()
            },
            ..LiveSettings::default()
        }
    }

    impl Rig {
        /// `n` frames from the mics, frame `i` (counted from the first ever
        /// sent) made by `frame`.
        /// `n` frames as the capture sends them: chunk by chunk, each
        /// promised, then pushed.
        fn send(&mut self, n: usize, frame: &impl Fn(usize) -> LiveFrame) {
            let mut left = n;
            while left > 0 {
                let c = left.min(self.chunk);
                self.promise(c);
                self.push(c, frame);
                left -= c;
            }
        }

        /// The capture's callback starting a chunk of `n` frames.
        fn promise(&self, n: usize) {
            self.counts.chunk.store(n as u64, Ordering::Relaxed);
            self.counts.promised.fetch_add(n as u64, Ordering::Release);
        }

        /// `n` frames of a chunk promised, pushed.
        fn push(&mut self, n: usize, frame: &impl Fn(usize) -> LiveFrame) {
            for _ in 0..n {
                self.tx.send(frame(self.sent)).unwrap();
                self.sent += 1;
            }
        }

        /// `calls` calls of 64 frames, the mics sending 64 before each — as
        /// their capture does, at the same clock — after one call's worth
        /// first, as a capture starting ahead of its output: two callbacks'
        /// worth at the first call, exactly what it starts with.
        fn play(&mut self, calls: usize, frame: impl Fn(usize) -> LiveFrame) -> Vec<Frame> {
            if self.sent == 0 {
                self.send(64, &frame);
            }
            let mut out = Vec::new();
            let mut block = vec![[0.0; 2]; 64];
            for _ in 0..calls {
                self.send(64, &frame);
                self.mix.mix(&mut block);
                out.extend_from_slice(&block);
            }
            out
        }
    }

    /// Every one of `mics` mics at `v` on its left and `-v` on its right.
    fn steady(mics: usize, v: f32) -> impl Fn(usize) -> LiveFrame {
        move |_| {
            let mut f = [0.0; 2 * LIVE_MICS];
            for i in 0..mics {
                f[2 * i] = v;
                f[2 * i + 1] = -v;
            }
            f
        }
    }

    /// Mic 0, both sides: a sine at `hz`, peak `a`.
    fn sine(hz: f64, a: f64) -> impl Fn(usize) -> LiveFrame {
        move |i| {
            let v = (a * (std::f64::consts::TAU * hz * i as f64 / 48_000.0).sin()) as f32;
            let mut f = [0.0; 2 * LIVE_MICS];
            f[0] = v;
            f[1] = v;
            f
        }
    }

    fn peak(frames: &[Frame], side: usize) -> f32 {
        frames.iter().map(|f| f[side].abs()).fold(0.0, f32::max)
    }

    /// Silence until two callbacks' worth has come — one kept in hand —
    /// then the mics summed, after the limiter's 1.5 ms look-ahead (held
    /// on or off).
    #[test]
    fn a_live_bus_starts_with_a_callbacks_worth_in_hand() {
        let mut r = rig(2, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(64, &steady(2, 0.25));
        r.mix.mix(&mut block);
        assert!(
            block.iter().all(|f| *f == [0.0, 0.0]),
            "not primed: silence"
        );
        let out = r.play(4, steady(2, 0.25));
        assert!(
            out[..LIMITER_LOOKAHEAD].iter().all(|f| *f == [0.0, 0.0]),
            "the look-ahead"
        );
        assert!(out[LIMITER_LOOKAHEAD..]
            .iter()
            .all(|f| (f[0] - 0.5).abs() < 1e-6 && (f[1] + 0.5).abs() < 1e-6));
        assert_eq!(r.tx.len(), 64, "a callback's worth in hand");
    }

    /// Run short: silence where sound was missing, counted, and two
    /// callbacks' worth waited for again; piled up: back to two.
    #[test]
    fn a_live_bus_short_waits_again_and_piled_up_lets_the_oldest_go() {
        let mut r = rig(1, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &steady(1, 0.5));
        r.mix.mix(&mut block);
        r.mix.mix(&mut block);
        r.send(10, &steady(1, 0.5));
        r.mix.mix(&mut block);
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 1);
        assert!(!r.mix.primed, "waits again");
        // The next call, a look-ahead later: the ten frames that came, then
        // silence for what was missing.
        r.mix.mix(&mut block);
        assert!(
            block[8..18].iter().all(|f| f[0] == 0.5),
            "{:?}",
            &block[..20]
        );
        assert!(block[18..].iter().all(|f| *f == [0.0, 0.0]), "silence");
        r.send(64 * 6, &steady(1, 0.5));
        r.mix.mix(&mut block);
        assert_eq!(r.tx.len(), 64, "back to a callback's worth in hand");
    }

    /// A mic's gain change glides over a few milliseconds: no step.
    #[test]
    fn a_live_mics_gain_change_glides() {
        let mut r = rig(1, &plain());
        r.play(4, steady(1, 1.0));
        r.gains[0].store(0.0_f32.to_bits(), Ordering::Relaxed);
        let out = r.play(4, steady(1, 1.0));
        let steps = out
            .windows(2)
            .map(|w| (w[1][0] - w[0][0]).abs())
            .fold(0.0, f32::max);
        assert!(steps < 0.01, "a step of {steps}");
        let last = out[out.len() - 1][0];
        assert!(last < 0.9 && last > 0.0, "gliding down: {last}");
    }

    /// A mic's own processing runs in the callback: its low cut takes the
    /// rumble out of what the PA plays, as the mixer's strip would.
    #[test]
    fn a_live_mics_low_cut_takes_out_rumble() {
        let mut settings = plain();
        settings.mics[0].low_cut = LowCutConfig {
            enabled: true,
            hz: 100.0,
            slope_db: 24,
        };
        let rumble = |settings: &LiveSettings| {
            let mut r = rig(1, settings);
            let out = r.play(400, sine(40.0, 0.5));
            // Settled: the last quarter second.
            peak(&out[out.len() - 12_000..], 0)
        };
        assert!(rumble(&plain()) > 0.49, "heard as it is without it");
        let cut = rumble(&settings);
        assert!(cut < 0.5 * 0.1, "40 Hz left at {cut}");
    }

    /// A mic's balance, as its strip's: hard left, nothing on the right.
    #[test]
    fn a_live_mics_balance_is_heard() {
        let mut settings = plain();
        settings.mics[0].balance = -1.0;
        let mut r = rig(1, &settings);
        let out = r.play(80, steady(1, 0.5));
        // Past the glide to it.
        let late = &out[out.len() - 64 * 4..];
        assert!(
            late.iter().all(|f| (f[0] - 0.5).abs() < 1e-3),
            "{:?}",
            late[0]
        );
        assert!(peak(late, 1) < 1e-3, "right {}", peak(late, 1));
    }

    /// A mic's gate runs in the callback: the room under its threshold is
    /// turned down by its range on the PA, as on the mixer's strip.
    #[test]
    fn a_live_mics_gate_turns_the_room_down() {
        let mut settings = plain();
        settings.mics[0].gate = crate::internal::audio_mix::dsp::GateConfig {
            enabled: true,
            threshold_db: -30.0,
            range_db: 20.0,
            ..Default::default()
        };
        let quiet = |settings: &LiveSettings| {
            let mut r = rig(1, settings);
            // Past its hold and well into its release: 1.6 s.
            let out = r.play(1_200, sine(300.0, 0.01));
            peak(&out[out.len() - 4_800..], 0)
        };
        let open = quiet(&plain());
        let gated = quiet(&settings);
        assert!(
            (gated / open - 0.1).abs() < 0.02,
            "20 dB down: {gated} against {open}"
        );
    }

    /// How many frames late the mics play when the output first reads with
    /// the capture's third chunk `pushed` frames in (of 64): two chunks
    /// pushed whole before.
    fn frames_late(pushed: Option<usize>) -> usize {
        let mut r = rig(1, &plain());
        // Each frame says which it is.
        let counting = |i: usize| {
            let mut f = [0.0; 2 * LIVE_MICS];
            f[0] = i as f32 * 1e-6;
            f[1] = f[0];
            f
        };
        let mut block = vec![[0.0; 2]; 64];
        r.send(64, &counting);
        r.send(64, &counting);
        match pushed {
            // The chunk whole before the output reads.
            None => {
                r.send(64, &counting);
                r.mix.mix(&mut block);
            }
            // The output reading while the capture is pushing it.
            Some(p) => {
                r.promise(64);
                r.push(p, &counting);
                r.mix.mix(&mut block);
                r.push(64 - p, &counting);
            }
        }
        for _ in 0..20 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        assert_eq!(
            r.mix.underruns.load(Ordering::Relaxed),
            0,
            "played on, never short"
        );
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let played = (f64::from(block[63][0]) * 1e6).round() as usize;
        r.sent - 1 - played
    }

    /// The output's first read lines the mics up on the capture's chunks,
    /// wherever the capture is in pushing one: as late with it half in as
    /// with it all in (counted short, they were as much later as it had yet
    /// to push — 6 samples on the Studio, every open, in one build).
    #[test]
    fn a_live_start_lines_up_on_the_captures_chunks() {
        let whole = frames_late(None);
        // Two callbacks' worth in hand as each read starts, the last frame
        // read the 64th of them; then the bus limiter's look-ahead.
        assert_eq!(
            whole,
            64 + crate::internal::audio_mix::dsp::LIMITER_LOOKAHEAD,
            "the newest frame two callbacks back, less the one read"
        );
        for pushed in [0, 1, 58, 63] {
            assert_eq!(frames_late(Some(pushed)), whole, "{pushed} of 64 pushed");
        }
    }

    /// A capture in chunks twice the output's (128 frames, the output 64):
    /// its start lines up the same, and plays on — a third chunk promised
    /// and not yet pushed must not trim away what this call plays.
    #[test]
    fn a_live_start_on_a_longer_capture_chunk_plays_on() {
        let late = |pushed: Option<usize>| {
            let mut r = rig(1, &plain());
            r.chunk = 128;
            let counting = |i: usize| {
                let mut f = [0.0; 2 * LIVE_MICS];
                f[0] = i as f32 * 1e-6;
                f[1] = f[0];
                f
            };
            let mut block = vec![[0.0; 2]; 64];
            r.send(128, &counting);
            r.send(128, &counting);
            match pushed {
                None => {
                    r.send(128, &counting);
                    r.mix.mix(&mut block);
                }
                Some(p) => {
                    r.promise(128);
                    r.push(p, &counting);
                    r.mix.mix(&mut block);
                    r.push(128 - p, &counting);
                }
            }
            r.mix.mix(&mut block);
            for _ in 0..20 {
                r.send(128, &counting);
                r.mix.mix(&mut block);
                r.mix.mix(&mut block);
            }
            assert_eq!(
                r.mix.underruns.load(Ordering::Relaxed),
                0,
                "{pushed:?}: never short"
            );
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let played = (f64::from(block[63][0]) * 1e6).round() as usize;
            r.sent - 1 - played
        };
        let whole = late(None);
        for pushed in [0, 60, 127] {
            assert_eq!(late(Some(pushed)), whole, "{pushed} of 128 pushed");
        }
    }

    /// The output losing a cycle the capture does not (the device changing
    /// its buffer under it): the mics come back to two callbacks' worth, not
    /// a buffer later for as long as it plays.
    #[test]
    fn a_lost_output_cycle_does_not_leave_the_mics_late() {
        let mut r = rig(1, &plain());
        let counting = |i: usize| {
            let mut f = [0.0; 2 * LIVE_MICS];
            f[0] = i as f32 * 1e-6;
            f[1] = f[0];
            f
        };
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &counting);
        for _ in 0..10 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        // A cycle the output missed.
        r.send(64, &counting);
        for _ in 0..20 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let played = (f64::from(block[63][0]) * 1e6).round() as usize;
        assert_eq!(
            r.sent - 1 - played,
            64 + crate::internal::audio_mix::dsp::LIMITER_LOOKAHEAD,
            "as late as before the lost cycle"
        );
    }

    /// A capture in 128-frame chunks, the output at 64, losing one read or
    /// two the capture does not: the mics come back to as late as before.
    #[test]
    fn lost_reads_on_a_longer_capture_chunk_do_not_leave_the_mics_late() {
        let late = |skipped: &[usize]| {
            let mut r = rig(1, &plain());
            r.chunk = 128;
            let counting = |i: usize| {
                let mut f = [0.0; 2 * LIVE_MICS];
                f[0] = i as f32 * 1e-6;
                f[1] = f[0];
                f
            };
            let mut block = vec![[0.0; 2]; 64];
            r.send(256, &counting);
            for read in 0..120 {
                if read % 2 == 0 {
                    r.send(128, &counting);
                }
                if !skipped.contains(&read) {
                    r.mix.mix(&mut block);
                }
            }
            assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0, "{skipped:?}");
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let played = (f64::from(block[63][0]) * 1e6).round() as usize;
            r.sent - 1 - played
        };
        let none = late(&[]);
        assert_eq!(late(&[21]), none, "one read lost");
        assert_eq!(late(&[21, 22]), none, "two reads lost");
    }

    /// The mics' frame `i`, saying which it is.
    fn counting(i: usize) -> LiveFrame {
        let mut f = [0.0; 2 * LIVE_MICS];
        f[0] = i as f32 * 1e-6;
        f[1] = f[0];
        f
    }

    /// How many frames late the frame just played is.
    fn late_now(r: &Rig, block: &[Frame]) -> usize {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let played = (f64::from(block[block.len() - 1][0]) * 1e6).round() as usize;
        r.sent - 1 - played
    }

    /// A capture in 80-frame chunks and the output reading 64 (sizes that
    /// do not divide, no steady fullest read): nothing let go in steady play.
    #[test]
    fn sizes_that_do_not_divide_play_on_untouched() {
        let mut r = rig(1, &plain());
        r.chunk = 80;
        let mut block = vec![[0.0; 2]; 64];
        r.send(160, &counting);
        let mut lates = Vec::new();
        for step in 0..2_000 {
            let now = step * 16;
            if now % 80 == 0 {
                r.send(80, &counting);
            }
            if now % 64 == 0 {
                r.mix.mix(&mut block);
                if now % 320 == 0 {
                    lates.push(late_now(&r, &block));
                }
            }
        }
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0);
        assert!(
            lates[2..].windows(2).all(|w| w[0] == w[1]),
            "nothing let go: {lates:?}"
        );
    }

    /// One read late (the capture twice before it), then a read without a
    /// capture catching up, after the output had been late a while: what
    /// is back to two keeps what it has — never short.
    #[test]
    fn a_read_caught_up_keeps_what_it_has() {
        let mut r = rig(1, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &counting);
        for _ in 0..10 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        // A cycle lost: late for 7 periods (14 reads, 896 frames) …
        r.send(64, &counting);
        for _ in 0..14 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        // … then the output catches up: a read with no capture before it.
        r.mix.mix(&mut block);
        for _ in 0..40 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0, "never short");
        assert_eq!(
            late_now(&r, &block),
            64 + crate::internal::audio_mix::dsp::LIMITER_LOOKAHEAD
        );
    }

    /// Late a while, then held up past four callbacks' worth: back to two at
    /// once, and no second trim after it from what was seen before — never
    /// short.
    #[test]
    fn a_trim_forgets_what_it_saw_before() {
        let mut r = rig(1, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &counting);
        for _ in 0..10 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        // A cycle lost, late for 7 periods (896 frames, under 20 ms) …
        r.send(64, &counting);
        for _ in 0..14 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        // … then another: four callbacks' worth, back to two at once.
        r.send(64, &counting);
        for _ in 0..40 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0, "never short");
        assert_eq!(
            late_now(&r, &block),
            64 + crate::internal::audio_mix::dsp::LIMITER_LOOKAHEAD
        );
    }

    /// The capture twice and then the output twice, over and over (each
    /// pair back to two callbacks' worth): nothing let go, never short.
    #[test]
    fn late_reads_caught_up_again_and_again_let_nothing_go() {
        let mut r = rig(1, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &counting);
        for _ in 0..10 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        for _ in 0..40 {
            r.send(64, &counting);
            r.send(64, &counting);
            r.mix.mix(&mut block);
            r.mix.mix(&mut block);
        }
        r.send(64, &counting);
        r.mix.mix(&mut block);
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0, "never short");
        assert_eq!(
            late_now(&r, &block),
            64 + crate::internal::audio_mix::dsp::LIMITER_LOOKAHEAD
        );
    }

    /// The callback tells what it did with the queue, to the frame: its
    /// start (and what it let go then), the capture's chunk, a let-go after
    /// a lost cycle, running short.
    #[test]
    fn a_live_mix_tells_what_it_did_with_its_queue() {
        use super::LiveEventKind as K;
        let mut r = rig(1, &plain());
        let events = r.mix.events();
        let mut block = vec![[0.0; 2]; 64];
        r.send(192, &counting);
        r.mix.mix(&mut block);
        for _ in 0..4 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        r.send(64, &counting);
        for _ in 0..20 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        r.mix.mix(&mut block);
        r.mix.mix(&mut block);
        r.mix.mix(&mut block);
        let told = events.drain();
        let kinds: Vec<_> = told.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            [K::Chunk, K::Started, K::LetGo, K::Short],
            "{told:?}"
        );
        assert_eq!(
            (told[1].at, told[1].let_go, told[1].coming),
            (0, 64, 192),
            "started: {}",
            told[1]
        );
        assert_eq!(told[2].let_go, 64, "{}", told[2]);
        assert_eq!(told[0].chunk, 64);
    }

    /// A device just opened: its mics stay silent 30 ms after they start, so
    /// a chunk left over by the device's buffer change is let go in silence;
    /// then they fade in, as late as ever.
    #[test]
    fn a_chunk_left_over_at_an_open_goes_in_silence() {
        let mut r = rig(1, &plain());
        r.mix.quiet_left = None;
        r.mix.now.iter_mut().for_each(|g| *g = [0.0, 0.0]);
        let events = r.mix.events();
        let mut block = vec![[0.0; 2]; 64];
        let mut out = Vec::new();
        r.send(128, &steady(1, 0.5));
        for call in 0..200 {
            r.send(64, &steady(1, 0.5));
            if call == 3 {
                // The device changing its buffer: a cycle lost.
                r.send(64, &steady(1, 0.5));
            }
            r.mix.mix(&mut block);
            out.extend_from_slice(&block);
        }
        let told = events.drain();
        let let_go = told
            .iter()
            .find(|e| e.kind == super::LiveEventKind::LetGo)
            .expect("the chunk let go");
        let heard = told
            .iter()
            .find(|e| e.kind == super::LiveEventKind::Heard)
            .expect("the mics fading in, told");
        let q = super::LIVE_OPEN_QUIET_FRAMES as u64;
        assert!(heard.at >= q && heard.at < q + 64, "at the quiet's end: {}", heard.at);
        assert!(let_go.at < super::LIVE_OPEN_QUIET_FRAMES as u64, "within the quiet: {}", let_go.at);
        let quiet = peak(&out[..super::LIVE_OPEN_QUIET_FRAMES + super::super::dsp::LIMITER_LOOKAHEAD], 0);
        assert!(quiet < 1e-6, "silent until then: {quiet}");
        assert!(peak(&out[out.len() - 64..], 0) > 0.4, "then heard");
    }

    /// The events ring: full, the callback drops new events (never waits);
    /// read, it takes them again.
    #[test]
    fn a_full_events_ring_drops_and_goes_on() {
        let ring = super::LiveEvents::default();
        let event = |at| super::LiveEvent {
            kind: super::LiveEventKind::LetGo,
            at,
            waiting: 1,
            coming: 2,
            n: 3,
            chunk: 4,
            let_go: 5,
            since_us: 6,
        };
        for at in 0..70 {
            ring.push(event(at));
        }
        let first = ring.drain();
        assert_eq!(first.len(), super::LIVE_EVENTS_HELD, "the rest dropped");
        assert_eq!(first[63], event(63));
        ring.push(event(100));
        assert_eq!(ring.drain(), [event(100)], "taken again once read");
    }

    /// The capture's chunk growing just after a start (128, then 256, the
    /// output at 256), while the mics are still held silent: it starts again
    /// at the new sizes — as late in the end as one on 256 from the first.
    #[test]
    fn sizes_changing_in_the_quiet_start_again() {
        // `switching`: 128-frame chunks before each read at first, then from
        // the third read 256-frame chunks after each read (the device's
        // buffer changed, and the callbacks' turns with it); else 256 after
        // each read from the start.
        let late = |switching: bool| {
            let mut r = rig(1, &plain());
            r.mix.quiet_left = None;
            r.mix.now.iter_mut().for_each(|g| *g = [0.0, 0.0]);
            let mut block = vec![[0.0; 2]; 256];
            r.chunk = if switching { 128 } else { 256 };
            r.send(512, &counting);
            for call in 0..80 {
                if switching && call < 2 {
                    r.send(256, &counting);
                    r.mix.mix(&mut block);
                } else {
                    r.chunk = 256;
                    r.mix.mix(&mut block);
                    r.send(256, &counting);
                }
            }
            assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0, "{switching}");
            r.send(256, &counting);
            r.mix.mix(&mut block);
            late_now(&r, &block)
        };
        assert_eq!(late(true), late(false), "a full margin, as if it had started at 256");
    }

    /// The capture's chunk going back and forth (64, 128, 64 …, the output at
    /// 64) just after a device opens: the mics are heard within a tenth of a
    /// second all the same, never short.
    #[test]
    fn sizes_going_back_and_forth_in_the_quiet_still_let_the_mics_in() {
        let mut r = rig(1, &plain());
        r.mix.quiet_left = None;
        r.mix.now.iter_mut().for_each(|g| *g = [0.0, 0.0]);
        let mut block = vec![[0.0; 2]; 64];
        let mut out = Vec::new();
        r.send(256, &steady(1, 0.5));
        // On one clock: a chunk of 128, none, a chunk of 64, again and again —
        // 192 frames each three reads of 64.
        for call in 0..300 {
            match call % 3 {
                0 => {
                    r.chunk = 128;
                    r.send(128, &steady(1, 0.5));
                }
                2 => {
                    r.chunk = 64;
                    r.send(64, &steady(1, 0.5));
                }
                _ => {}
            }
            r.mix.mix(&mut block);
            out.extend_from_slice(&block);
        }
        let heard = out.iter().position(|f| f[0].abs() > 0.01).expect("heard");
        assert!(heard < super::LIVE_OPEN_QUIET_FRAMES + 2_400, "heard {heard} frames in");
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0);
    }

    /// The rest of the bus is silent through an open's quiet too, then fades
    /// in; with no mics coming at all, it plays after half a second anyway.
    #[test]
    fn the_rest_of_the_bus_waits_out_the_quiet_but_not_for_ever() {
        let heard_after = |mics: bool| {
            let mut r = rig(1, &plain());
            r.mix.quiet_left = None;
            r.mix.bus_now = 0.0;
            let mut block = vec![[0.0; 2]; 64];
            if mics {
                r.send(128, &steady(1, 0.0));
            }
            for call in 0..800 {
                for _ in 0..64 {
                    let _ = r.rest.try_send([0.5, 0.5]);
                }
                if mics {
                    r.send(64, &steady(1, 0.0));
                }
                r.mix.mix(&mut block);
                if block.iter().any(|f| f[0].abs() > 0.01) {
                    return call * 64;
                }
            }
            usize::MAX
        };
        let with = heard_after(true);
        let quiet = super::LIVE_OPEN_QUIET_FRAMES;
        assert!((quiet..quiet + 2_400).contains(&with), "after the quiet: {with}");
        let without = heard_after(false);
        assert!((24_000..30_000).contains(&without), "after half a second, mics or not: {without}");
    }

    /// A device waiting a while for its mics before it starts (callbacks
    /// with nothing to play): it still starts silent and fades in after the
    /// quiet, not at full level.
    #[test]
    fn a_long_wait_to_start_still_fades_in() {
        let mut r = rig(1, &plain());
        r.mix.quiet_left = None;
        r.mix.now.iter_mut().for_each(|g| *g = [0.0, 0.0]);
        let mut block = vec![[0.0; 2]; 64];
        for _ in 0..100 {
            r.mix.mix(&mut block);
        }
        let mut out = Vec::new();
        r.send(128, &steady(1, 0.5));
        for _ in 0..200 {
            r.send(64, &steady(1, 0.5));
            r.mix.mix(&mut block);
            out.extend_from_slice(&block);
        }
        let q = super::LIVE_OPEN_QUIET_FRAMES;
        let quiet = peak(&out[..q], 0);
        assert!(quiet < 1e-6, "silent through the quiet: {quiet}");
        let first = out[q + super::super::dsp::LIMITER_LOOKAHEAD..]
            .iter()
            .position(|f| f[0].abs() > 1e-6)
            .expect("heard");
        let early = out[q + super::super::dsp::LIMITER_LOOKAHEAD + first][0].abs();
        assert!(early < 0.01, "faded in, not at full level: {early}");
    }

    /// One output read come a little late, the capture's next chunk already
    /// promised (a whole chunk more in hand at that read alone): nothing let
    /// go — no click — and the mics as late as before.
    #[test]
    fn one_late_output_read_lets_nothing_go() {
        let mut r = rig(1, &plain());
        let counting = |i: usize| {
            let mut f = [0.0; 2 * LIVE_MICS];
            f[0] = i as f32 * 1e-6;
            f[1] = f[0];
            f
        };
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &counting);
        for _ in 0..10 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        // The capture runs twice before this read, then once after the next.
        r.send(64, &counting);
        r.send(64, &counting);
        r.mix.mix(&mut block);
        r.mix.mix(&mut block);
        for _ in 0..30 {
            r.send(64, &counting);
            r.mix.mix(&mut block);
        }
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let played = (f64::from(block[63][0]) * 1e6).round() as usize;
        assert_eq!(
            r.sent - 1 - played,
            64 + crate::internal::audio_mix::dsp::LIMITER_LOOKAHEAD,
            "nothing let go"
        );
    }

    /// A capture in chunks half the output's (64, the output 128), each
    /// read finding one or both of them in: nothing let go — the mics as
    /// late at the end as early on.
    #[test]
    fn a_capture_in_shorter_chunks_is_never_trimmed() {
        let mut r = rig(1, &plain());
        let counting = |i: usize| {
            let mut f = [0.0; 2 * LIVE_MICS];
            f[0] = i as f32 * 1e-6;
            f[1] = f[0];
            f
        };
        let mut block = vec![[0.0; 2]; 128];
        let late = |r: &Rig, block: &[Frame]| {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let played = (f64::from(block[127][0]) * 1e6).round() as usize;
            r.sent - 1 - played
        };
        r.send(256, &counting);
        r.mix.mix(&mut block);
        let mut early = None;
        for call in 0..200 {
            // The second chunk before the read on even calls, after it on odd.
            r.send(64, &counting);
            if call % 2 == 0 {
                r.send(64, &counting);
                r.mix.mix(&mut block);
            } else {
                r.mix.mix(&mut block);
                r.send(64, &counting);
            }
            if call == 20 {
                early = Some(late(&r, &block));
            }
        }
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0);
        // Read at an even call, as at call 20.
        r.send(128, &counting);
        r.mix.mix(&mut block);
        assert_eq!(Some(late(&r, &block)), early, "nothing let go");
    }

    /// However much the capture has promised — a bigger chunk than its last,
    /// its size not yet seen — the start keeps this call's worth to play.
    #[test]
    fn a_live_start_keeps_this_calls_worth_whatever_is_promised() {
        let mut r = rig(1, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(128, &steady(1, 0.5));
        // A 128-frame chunk promised, its size not yet stored, none pushed.
        r.counts.promised.fetch_add(128, Ordering::Release);
        r.mix.mix(&mut block);
        assert_eq!(
            r.mix.underruns.load(Ordering::Relaxed),
            0,
            "played, not short"
        );
        assert!(r.mix.primed, "playing on");
        assert!(r.tx.is_empty(), "this call's worth read, the rest let go");
    }

    /// The capture letting its oldest go (the output held up), caught by
    /// the output between a frame gone and its count: what is coming is
    /// never taken for more than the queue holds, and the trim leaves the
    /// next call its worth.
    #[test]
    fn a_full_queue_mid_overflow_is_not_trimmed_too_far() {
        let mut r = rig_holding(1, &plain(), 512);
        let mut block = vec![[0.0; 2]; 64];
        let v = steady(1, 0.5);
        r.send(128, &v);
        r.mix.mix(&mut block);
        r.send(448, &v);
        assert_eq!(r.tx.len(), 512, "full");
        // A chunk letting the oldest go as it pushes: 63 frames done, the
        // 64th's oldest gone and not yet counted, itself not yet pushed.
        r.promise(64);
        for _ in 0..63 {
            r.mix.mics.frames.try_recv().unwrap();
            r.counts.gone.fetch_add(1, Ordering::Release);
            r.tx.try_send(v(0)).unwrap();
        }
        r.mix.mics.frames.try_recv().unwrap();
        r.mix.mix(&mut block);
        r.counts.gone.fetch_add(1, Ordering::Release);
        r.tx.try_send(v(0)).unwrap();
        // The output's next call before the capture's next chunk.
        r.mix.mix(&mut block);
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0, "never short");
    }

    /// A capture in 4096-frame chunks (a device's longest buffer), the
    /// output at 64: on the feed's own queue, the mics start and play on.
    #[test]
    fn a_capture_in_long_chunks_starts_and_plays_on() {
        let mut r = rig_holding(1, &plain(), crate::audio::LIVE_FRAMES_HELD);
        r.chunk = 4096;
        let mut block = vec![[0.0; 2]; 64];
        for call in 0..512 {
            if call % 64 == 0 {
                r.send(4096, &steady(1, 0.5));
            }
            r.mix.mix(&mut block);
        }
        assert!(r.mix.primed, "started");
        assert_eq!(
            r.mix.underruns.load(Ordering::Relaxed),
            0,
            "never short once started"
        );
        assert!(block.iter().all(|f| f[0] == 0.5), "heard");
    }

    /// In 4096-frame chunks, the output held up until the queue is full:
    /// back to two chunks' worth at its next call, not kept 171 ms late.
    #[test]
    fn a_full_queue_of_long_chunks_comes_back_to_two() {
        let held = crate::audio::LIVE_FRAMES_HELD;
        let mut r = rig_holding(1, &plain(), held);
        r.chunk = 4096;
        let mut block = vec![[0.0; 2]; 64];
        r.send(8192, &steady(1, 0.5));
        r.mix.mix(&mut block);
        assert!(r.mix.primed);
        // Held up: the capture fills the queue.
        let room = held - r.tx.len();
        r.send(room, &steady(1, 0.5));
        // (Its last piece was a remainder: the capture's chunk is 4096.)
        r.counts.chunk.store(4096, Ordering::Relaxed);
        assert_eq!(r.tx.len(), held, "full");
        r.mix.mix(&mut block);
        assert_eq!(
            r.tx.len(),
            8192 - 64,
            "two chunks' worth, less the call read"
        );
    }

    /// A capture in chunks as long as its whole queue, too long to hold
    /// twice over: the mics still start and are heard, not silent for good.
    #[test]
    fn a_capture_chunk_as_long_as_its_queue_still_starts() {
        let held = crate::audio::LIVE_FRAMES_HELD;
        let mut r = rig_holding(1, &plain(), held);
        r.chunk = held;
        let mut block = vec![[0.0; 2]; 64];
        let mut heard = false;
        for call in 0..(held / 64) {
            if call == 0 {
                r.send(held, &steady(1, 0.5));
            }
            r.mix.mix(&mut block);
            heard |= block.iter().any(|f| f[0] == 0.5);
        }
        assert!(heard, "started and heard");
    }

    /// A mic's EQ runs in the callback: a bell lifts its frequency on the PA.
    #[test]
    fn a_live_mics_eq_is_heard() {
        let mut settings = plain();
        settings.mics[0].eq.enabled = true;
        settings.mics[0].eq.bands[1] = crate::internal::audio_mix::dsp::EqBandConfig {
            enabled: true,
            hz: 300.0,
            gain_db: 6.0,
            q: 1.0,
        };
        let level = |settings: &LiveSettings| {
            let mut r = rig(1, settings);
            let out = r.play(400, sine(300.0, 0.1));
            peak(&out[out.len() - 4_800..], 0)
        };
        let ratio = level(&settings) / level(&plain());
        assert!(
            (ratio - 10f32.powf(6.0 / 20.0)).abs() < 0.05,
            "6 dB up: {ratio}"
        );
    }

    /// A mic's de-esser runs in the callback: an "s" is turned down on the PA.
    #[test]
    fn a_live_mics_de_esser_is_heard() {
        let mut settings = plain();
        settings.mics[0].de_esser = crate::internal::audio_mix::dsp::DeEsserConfig {
            enabled: true,
            threshold_db: -35.0,
            ..crate::internal::audio_mix::dsp::DeEsserConfig::default()
        };
        let level = |settings: &LiveSettings| {
            let mut r = rig(1, settings);
            let out = r.play(400, sine(10_000.0, 0.3));
            peak(&out[out.len() - 4_800..], 0)
        };
        let down = 20.0 * (level(&settings) / level(&plain())).log10();
        assert!(down < -6.0, "down by {down:.1} dB");
    }

    /// The bus's limiter runs in the callback: nothing past its ceiling,
    /// however loud the mic.
    #[test]
    fn a_live_bus_never_passes_its_limiters_ceiling() {
        let mut settings = plain();
        settings.limiter = LimiterConfig {
            enabled: true,
            ceiling_db: -6.0,
            release_ms: 100.0,
        };
        let mut r = rig(1, &settings);
        let out = r.play(100, sine(440.0, 1.5));
        // Past its first 10 ms, gliding from the default ceiling to its own.
        let out = &out[64 * 10..];
        let ceiling = 10f32.powf(-6.0 / 20.0);
        assert!(
            peak(out, 0) <= ceiling + 1e-5,
            "{} over {ceiling}",
            peak(out, 0)
        );
        assert!(peak(out, 0) > ceiling * 0.9, "limited, not silent");
    }

    /// The bus's delay runs in the callback: 10 ms set, the sound comes out
    /// 480 frames later than without it.
    #[test]
    fn a_live_bus_is_delayed_as_set() {
        let first_sound = |delay_ms: f32| {
            let mut settings = plain();
            settings.delay_ms = delay_ms;
            let mut r = rig(1, &settings);
            let out = r.play(30, |i| steady(1, if i < 300 { 0.0 } else { 0.5 })(i));
            out.iter().position(|f| f[0] != 0.0)
        };
        let plain_at = first_sound(0.0).expect("heard");
        assert_eq!(plain_at, 300 + LIMITER_LOOKAHEAD, "a look-ahead late");
        assert_eq!(first_sound(10.0), Some(plain_at + 480));
    }

    /// A delay made longer while it plays brings in silence for the time
    /// added, as the mixer's: it never plays again what was heard.
    #[test]
    fn a_longer_delay_brings_silence_not_a_repeat() {
        let mut settings = plain();
        settings.delay_ms = 10.0;
        let mut r = rig(1, &settings);
        // Each frame its own: a repeat would show.
        let ramp = |i: usize| steady(1, 0.1 + (i % 4_000) as f32 / 10_000.0)(i);
        r.play(60, ramp);
        settings.delay_ms = 30.0;
        r.control.set(&settings);
        let out = r.play(30, ramp);
        assert!(
            out[..960].iter().all(|f| f[0] == 0.0),
            "the 20 ms added: silence"
        );
        assert!(
            out[960..].iter().all(|f| f[0] != 0.0),
            "then the sound again"
        );
    }

    /// Opened again (its buffer changed, Live switched on), a live mic's
    /// leveler rides on from the mixer's for the strip, not from 0 dB.
    #[test]
    fn a_live_bus_opened_again_rides_on_from_the_mixers_leveler() {
        let mut settings = plain();
        settings.mics[0].leveler = crate::internal::audio_mix::dsp::LevelerConfig::speech();
        let gain_when_opened = |seed: Option<f32>| {
            let control = Arc::new(LiveControl::default());
            control.set(&settings);
            if let Some(db) = seed {
                control.set_levelers([db]);
            }
            let (_tx, rx) = crossbeam_channel::bounded(1);
            let (_rest, rest_rx) = crossbeam_channel::bounded(1);
            let mix = LiveMix::new(
                LiveMics {
                    frames: rx,
                    counts: Arc::default(),
                    gains: vec![Arc::new(AtomicU32::new(1.0_f32.to_bits()))],
                    control,
                    rest: rest_rx,
                },
                Arc::new(AtomicU32::new(1.0_f32.to_bits())),
                64,
                Arc::new(AtomicU64::new(0)),
            );
            mix.chains[0].snapshot().leveler_gain_db
        };
        assert!((gain_when_opened(Some(6.5)) - 6.5).abs() < 1e-6);
        assert!(gain_when_opened(None).abs() < 1e-6, "none yet: from 0 dB");
    }

    /// A show loaded forgets the old show's leveler gains: a callback made
    /// right after starts from 0 dB, not from them.
    #[test]
    fn a_show_loaded_forgets_the_leveler_gains() {
        let control = LiveControl::default();
        control.set_levelers([6.5]);
        control.start_afresh();
        assert!(control.leveler(0).is_nan());
    }

    /// More than two callbacks' worth waiting when it starts: the oldest go,
    /// so it plays two callbacks behind, not three.
    #[test]
    fn a_live_bus_starts_exactly_two_callbacks_behind() {
        let mut r = rig(1, &plain());
        let mut block = vec![[0.0; 2]; 64];
        r.send(64 * 3, &steady(1, 0.5));
        r.mix.mix(&mut block);
        assert_eq!(r.tx.len(), 64, "one callback's worth in hand, not two");
    }

    /// A show loaded starts the callback's processing afresh, as the
    /// mixer's: a leveler that rode the old show's voice up starts at unity.
    #[test]
    fn a_show_loaded_starts_the_live_processing_afresh() {
        let mut settings = plain();
        settings.mics[0].leveler = crate::internal::audio_mix::dsp::LevelerConfig::speech();
        let mut r = rig(1, &settings);
        // A soft voice for four seconds: ridden up.
        r.play(3_000, sine(300.0, 0.02));
        let ridden = r.mix.chains[0].snapshot().leveler_gain_db;
        assert!(ridden > 3.0, "ridden up: {ridden}");
        r.control.start_afresh();
        r.play(1, sine(300.0, 0.0));
        let gain = r.mix.chains[0].snapshot().leveler_gain_db;
        assert!(gain.abs() < 1e-6, "afresh: {gain}");
        assert_eq!(
            r.mix.chains[0].snapshot().leveler,
            Some(settings.mics[0].leveler.sanitized()),
            "its settings kept"
        );
    }

    /// The rest of the bus — a clip, a remote guest — comes from the mix and
    /// is heard with the mics once its FIFO is 30 ms deep, through the bus's
    /// processing.
    #[test]
    fn a_live_bus_plays_the_rest_of_the_bus_too() {
        let mut settings = plain();
        settings.limiter = LimiterConfig {
            enabled: true,
            ceiling_db: -6.0,
            release_ms: 100.0,
        };
        let mut r = rig(1, &settings);
        // The mix runs ahead by its 10 ms blocks: three of them first.
        for _ in 0..1_440 {
            r.rest.send([0.3, -0.3]).unwrap();
        }
        let mut out = Vec::new();
        for _ in 0..60 {
            for _ in 0..64 {
                r.rest.send([0.3, -0.3]).unwrap();
            }
            out.extend(r.play(1, steady(1, 0.25)));
        }
        let ceiling = 10f32.powf(-6.0 / 20.0);
        let last = out[out.len() - 1];
        assert!(
            (last[0] - ceiling).abs() < 0.02,
            "mic and rest, limited together: {last:?}"
        );
        assert!((last[1] + ceiling).abs() < 0.02, "{last:?}");
    }

    /// What the mix sent while the stream was being built is not heard
    /// late: the rest's FIFO starts empty at the first callback and fills
    /// to its 30 ms.
    #[test]
    fn the_rest_starts_at_its_depth_not_behind() {
        let mut r = rig(1, &plain());
        for _ in 0..5_000 {
            r.rest.send([0.3, -0.3]).unwrap();
        }
        for _ in 0..40 {
            for _ in 0..64 {
                r.rest.send([0.3, -0.3]).unwrap();
            }
            r.play(1, steady(1, 0.0));
        }
        let held = r.mix.rest.fifo.len();
        assert!(held < 1_440 + 2 * 64, "{held} frames behind");
    }

    /// The rest of the bus dry (the mix held up): the mics go on, never
    /// counted short.
    #[test]
    fn the_mics_go_on_while_the_rest_is_dry() {
        let mut r = rig(1, &plain());
        let out = r.play(40, steady(1, 0.5));
        assert!(out[out.len() - 64..]
            .iter()
            .all(|f| (f[0] - 0.5).abs() < 1e-6));
        assert_eq!(r.mix.underruns.load(Ordering::Relaxed), 0);
    }

    /// Settings changed while it plays are taken on the next call, never
    /// waited for.
    #[test]
    fn new_settings_reach_the_callback_on_its_next_call() {
        let mut r = rig(1, &plain());
        r.play(4, steady(1, 0.5));
        let mut settings = plain();
        settings.mics[0].balance = 1.0;
        r.control.set(&settings);
        let out = r.play(12, steady(1, 0.5));
        // Gliding over to the right: the left falls toward nothing.
        let last = out[out.len() - 1];
        assert!(last[0].abs() < 0.5 * 0.5 && last[0].abs() > 0.0, "{last:?}");
        // Held mid-change, the callback goes on with what it has, and takes
        // the change on a later call.
        let control = Arc::clone(&r.control);
        let held = control.settings.lock().unwrap();
        control.version.fetch_add(1, Ordering::Release);
        r.play(1, steady(1, 0.5));
        assert_ne!(
            r.mix.seen,
            control.version.load(Ordering::Acquire),
            "not waited for"
        );
        drop(held);
        r.play(1, steady(1, 0.5));
        assert_eq!(r.mix.seen, control.version.load(Ordering::Acquire));
    }
}
