//! The mix itself: every source summed onto the buses it is sent to, once per
//! block, with no threads or devices — so it can be tested sample by sample.
//!
//! Gains move in a straight line across a block instead of jumping, because a
//! gain step on a waveform is a click. Meters are post-fader peaks that fall at
//! a fixed rate, like a console's.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use super::clip::ClipAudioShared;
use super::reader::{db_to_gain, DriftControl, Frame, RateReader};
use crate::audio::PcmChunk;

/// The mix runs at 48 kHz stereo: what video, streaming and every desk expect.
pub const SAMPLE_RATE: u32 = 48_000;
/// Frames per block: 10 ms.
pub const BLOCK_FRAMES: usize = 480;
/// Meter fall per block: about 26 dB a second.
const METER_DECAY: f32 = 0.97;
/// Longest bus delay, in milliseconds.
pub const MAX_DELAY_MS: f32 = 1000.0;
/// Capture devices are held about 20 ms deep: enough to ride out scheduling
/// jitter, short enough not to be heard as a lag.
const INPUT_DEPTH_SECS: f64 = 0.02;
/// Sound over a network — NDI — arrives a video frame's worth at a time,
/// and not evenly: held deeper, so a late frame does not run it dry.
const NETWORK_DEPTH_SECS: f64 = 0.06;
/// Taps for recordings and streams hold this many blocks before dropping:
/// three seconds. ffmpeg reads the sound only once the picture's first frame
/// has come, and a first take can wait more than half a second for it.
const TAP_CAPACITY: usize = 300;

/// Where a source's sound comes from.
pub enum Feed {
    /// A video deck's soundtrack, decoded in step with its picture.
    Clip(Arc<ClipAudioShared>),
    /// A capture device: microphone, line in, a USB mixer's return.
    Input(InputFeed),
    /// A live source's sound, an NDI source's: PCM from its receiver, on
    /// its own clock like a device.
    Live(InputFeed),
    /// Frames already at the mix rate, pushed by the caller.
    #[cfg(test)]
    Fifo(Arc<Mutex<VecDeque<Frame>>>),
}

impl Feed {
    /// Fill `out` (already silent) with this block of the source.
    fn pull(&mut self, out: &mut [Frame]) {
        match self {
            Feed::Clip(clip) => clip.pull(out),
            Feed::Input(input) | Feed::Live(input) => input.pull(out),
            #[cfg(test)]
            Feed::Fifo(fifo) => {
                if let Ok(mut fifo) = fifo.lock() {
                    for slot in out.iter_mut() {
                        match fifo.pop_front() {
                            Some(frame) => *slot = frame,
                            None => break,
                        }
                    }
                }
            }
        }
    }
}

/// A capture device feeding one source: its raw PCM, which device channels
/// make left and right, and the reader that brings it onto the mix clock.
pub struct InputFeed {
    rx: crossbeam_channel::Receiver<PcmChunk>,
    rate: u32,
    channels: u16,
    left: u16,
    right: u16,
    fifo: VecDeque<Frame>,
    reader: RateReader,
    drift: DriftControl,
}

impl InputFeed {
    /// A feed from a device delivering `channels`-channel PCM at `rate`,
    /// taking `left` and `right` (0-based device channels; the same channel
    /// twice for a mono microphone).
    pub fn new(
        rx: crossbeam_channel::Receiver<PcmChunk>,
        rate: u32,
        channels: u16,
        left: u16,
        right: u16,
    ) -> Self {
        Self::held(rx, rate, channels, (left, right), INPUT_DEPTH_SECS)
    }

    /// A feed of sound from the network, held deeper than a device's.
    pub fn network(
        rx: crossbeam_channel::Receiver<PcmChunk>,
        rate: u32,
        channels: u16,
        (left, right): (u16, u16),
    ) -> Self {
        Self::held(rx, rate, channels, (left, right), NETWORK_DEPTH_SECS)
    }

    fn held(
        rx: crossbeam_channel::Receiver<PcmChunk>,
        rate: u32,
        channels: u16,
        (left, right): (u16, u16),
        depth_secs: f64,
    ) -> Self {
        let depth = (f64::from(rate.max(1)) * depth_secs) as usize;
        Self {
            rx,
            rate: rate.max(1),
            channels: channels.max(1),
            left,
            right,
            fifo: VecDeque::new(),
            reader: RateReader::default(),
            drift: DriftControl::new(depth),
        }
    }

    fn pull(&mut self, out: &mut [Frame]) {
        let ch = usize::from(self.channels);
        let (l, r) = (usize::from(self.left), usize::from(self.right));
        while let Ok(chunk) = self.rx.try_recv() {
            for frame in chunk.samples.chunks_exact(ch) {
                self.fifo.push_back([
                    frame.get(l).copied().unwrap_or(0.0),
                    frame.get(r).copied().unwrap_or(0.0),
                ]);
            }
        }
        let nominal = f64::from(self.rate) / f64::from(SAMPLE_RATE);
        let Some(step) = self.drift.step(&mut self.fifo, nominal) else {
            return;
        };
        if self.reader.read(&mut self.fifo, step, out) < out.len() {
            self.drift.ran_dry();
        }
    }
}

/// One source strip: a feed with its fader, mute and bus sends.
pub struct Source {
    pub(super) equalizer: super::equalizer::Equalizer,
    pub(super) held_peak: Frame,
    pub(super) dynamics: super::dynamics::Dynamics,
    /// Deck UUID for a clip; the input's own UUID for a capture device.
    pub uuid: String,
    pub feed: Feed,
    /// Fader, in dB.
    pub gain_db: f32,
    pub muted: bool,
    /// Buses this source is sent to, by UUID.
    pub sends: Vec<String>,
    /// How far this source is on air, 0 to 1, for a source that follows a
    /// video bus; 1 for one that does not. Set every frame by the renderer.
    pub gate: f32,
    /// Pre-fader listen: heard on the monitor as it comes in, before its
    /// fader, mute and on-air gate, so a clip or a microphone can be checked
    /// before it goes out. Changes nothing on the buses.
    pub solo: bool,
    /// Left–right balance, -1 to 1: the far side is turned down, the near
    /// side left at full.
    pub balance: f32,
    /// How long its sound is held back, in milliseconds: a microphone's, to
    /// meet a camera's picture that arrives later over USB. Held back as it
    /// comes, before its fader and on-air gate, so a fade or a cut still
    /// happens when it is made; heard at once by pre-fader listen.
    pub delay_ms: f32,
    current: Frame,
    meter: Frame,
    delay: DelayLine,
}

impl Source {
    /// A source at unity gain, unmuted, sent nowhere.
    pub fn new(uuid: impl Into<String>, feed: Feed) -> Self {
        Self {
            uuid: uuid.into(),
            feed,
            dynamics: super::dynamics::Dynamics::default(),
            equalizer: super::equalizer::Equalizer::default(),
            held_peak: [0.0; 2],
            gain_db: 0.0,
            muted: false,
            sends: Vec::new(),
            gate: 1.0,
            solo: false,
            balance: 0.0,
            delay_ms: 0.0,
            current: [0.0; 2],
            meter: [0.0; 2],
            delay: DelayLine::default(),
        }
    }

    /// Post-fader peak level per channel, linear.
    pub fn meter(&self) -> Frame {
        self.meter
    }

    fn target_gain(&self) -> Frame {
        if self.muted {
            return [0.0; 2];
        }
        let g = db_to_gain(self.gain_db) * self.gate.clamp(0.0, 1.0);
        let b = self.balance.clamp(-1.0, 1.0);
        [g * (1.0 - b).min(1.0), g * (1.0 + b).min(1.0)]
    }
}

/// A recording or stream listening to a bus.
struct Tap {
    token: u64,
    sender: crossbeam_channel::Sender<PcmChunk>,
    dropped: Arc<AtomicU64>,
    lost_samples: Arc<AtomicU64>,
    /// Samples lost since the last chunk that got through — on a full
    /// channel, or jumped over by a mixer held up: told with the next.
    pending_lost: u64,
    /// When it was opened: time jumped over before then is not its to owe.
    opened: std::time::Instant,
}

/// What [`MixCore::tap`] hands a recording or stream.
pub struct BusTap {
    pub token: u64,
    /// Interleaved stereo at [`SAMPLE_RATE`], one chunk per block.
    pub receiver: crossbeam_channel::Receiver<PcmChunk>,
    pub dropped: Arc<AtomicU64>,
    /// Samples lost to a full channel, in all.
    pub lost_samples: Arc<AtomicU64>,
    /// When the tap was opened, with the mixer held: its first chunk is the
    /// next block mixed, within one block of this.
    pub opened: std::time::Instant,
}

/// Frames waiting for a sound device, filled by the mix and drained by the
/// device's callback.
#[derive(Default)]
pub struct OutputRing {
    pub frames: Mutex<VecDeque<Frame>>,
}

/// A device that stops reading must not make the ring grow forever.
const OUTPUT_RING_CAP: usize = SAMPLE_RATE as usize / 2;

impl OutputRing {
    fn push(&self, block: &[Frame]) {
        if let Ok(mut frames) = self.frames.lock() {
            frames.extend(block.iter().copied());
            let excess = frames.len().saturating_sub(OUTPUT_RING_CAP);
            frames.drain(..excess);
        }
    }
}

/// One bus: the sum of what is sent to it, with its own fader, mute and delay,
/// going to sound devices and to recordings and streams.
pub struct Bus {
    pub(super) equalizer: super::equalizer::Equalizer,
    pub(super) held_peak: Frame,
    pub(super) dynamics: super::dynamics::Dynamics,
    pub uuid: String,
    pub gain_db: f32,
    pub muted: bool,
    /// Delay in milliseconds, to line sound up with a picture that arrives
    /// late (an LED processor, a projector).
    pub delay_ms: f32,
    /// Sound devices this bus plays on.
    pub outputs: Vec<Arc<OutputRing>>,
    taps: Vec<Tap>,
    current: f32,
    meter: Frame,
    delay: DelayLine,
}

impl Bus {
    /// A bus at unity gain with no delay, going nowhere yet.
    pub fn new(uuid: impl Into<String>) -> Self {
        Self {
            uuid: uuid.into(),
            gain_db: 0.0,
            dynamics: super::dynamics::Dynamics::default(),
            equalizer: super::equalizer::Equalizer::default(),
            held_peak: [0.0; 2],
            muted: false,
            delay_ms: 0.0,
            outputs: Vec::new(),
            taps: Vec::new(),
            current: 0.0,
            meter: [0.0; 2],
            delay: DelayLine::default(),
        }
    }

    /// Post-fader peak level per channel, linear.
    pub fn meter(&self) -> Frame {
        self.meter
    }

    /// Run `block` through the delay line in place.
    fn apply_delay(&mut self, block: &mut [Frame]) {
        self.delay.run(block, self.delay_ms);
    }

    fn deliver(&mut self, block: &[Frame]) {
        for ring in &self.outputs {
            ring.push(block);
        }
        if self.taps.is_empty() {
            return;
        }
        let samples: Vec<f32> = block
            .iter()
            .flat_map(|f| [f[0].clamp(-1.0, 1.0), f[1].clamp(-1.0, 1.0)])
            .collect();
        self.taps.retain_mut(|tap| {
            match tap.sender.try_send(PcmChunk {
                samples: samples.clone(),
                lost_before: tap.pending_lost,
            }) {
                Ok(()) => {
                    tap.pending_lost = 0;
                    true
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    tap.dropped.fetch_add(1, Ordering::Relaxed);
                    tap.lost_samples
                        .fetch_add(samples.len() as u64, Ordering::Relaxed);
                    tap.pending_lost += samples.len() as u64;
                    true
                }
                // The recording stopped without saying so.
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => false,
            }
        });
    }
}

/// Every source and bus, mixed one block at a time.
pub struct MixCore {
    pub sources: Vec<Source>,
    pub buses: Vec<Bus>,
    /// The operator's headphones: a bus of its own, fed by whatever is
    /// soloed, or else by the bus it listens to. Never sent anywhere but its
    /// own device.
    pub monitor: Bus,
    /// The bus the monitor listens to when nothing is soloed; `None` (or a
    /// bus that is gone) is the first bus.
    pub monitor_listen: Option<String>,
    next_token: u64,
    scratch: Vec<Frame>,
    sums: Vec<Vec<Frame>>,
    solo_sum: Vec<Frame>,
}

impl Default for MixCore {
    fn default() -> Self {
        Self {
            sources: Vec::new(),
            buses: Vec::new(),
            monitor: Bus::new("monitor"),
            monitor_listen: None,
            next_token: 0,
            scratch: Vec::new(),
            sums: Vec::new(),
            solo_sum: Vec::new(),
        }
    }
}

impl MixCore {
    #[cfg(test)]
    fn source_mut(&mut self, uuid: &str) -> Option<&mut Source> {
        self.sources.iter_mut().find(|s| s.uuid == uuid)
    }

    #[cfg(test)]
    fn bus_mut(&mut self, uuid: &str) -> Option<&mut Bus> {
        self.buses.iter_mut().find(|b| b.uuid == uuid)
    }

    pub fn source(&self, uuid: &str) -> Option<&Source> {
        self.sources.iter().find(|s| s.uuid == uuid)
    }

    pub fn bus(&self, uuid: &str) -> Option<&Bus> {
        self.buses.iter().find(|b| b.uuid == uuid)
    }

    /// Whether anything is soloed, so the monitor hears only that.
    pub fn soloing(&self) -> bool {
        self.sources.iter().any(|s| s.solo)
    }

    /// Listen to what the monitor plays, as [`Self::tap`] does a bus.
    pub fn tap_monitor(&mut self) -> BusTap {
        self.next_token += 1;
        add_tap(&mut self.monitor, self.next_token)
    }

    /// Listen to a bus, for a recording or stream. `None` if there is no such
    /// bus.
    pub fn tap(&mut self, bus: &str) -> Option<BusTap> {
        self.next_token += 1;
        let token = self.next_token;
        let bus = self.buses.iter_mut().find(|b| b.uuid == bus)?;
        Some(add_tap(bus, token))
    }

    /// Stop listening with a tap from [`Self::tap`].
    pub fn untap(&mut self, token: u64) {
        for bus in self
            .buses
            .iter_mut()
            .chain(std::iter::once(&mut self.monitor))
        {
            bus.taps.retain(|t| t.token != token);
        }
    }

    /// `blocks` blocks of time, due from `from` on, went by without being
    /// mixed — the mixer was held up and jumped ahead. Every tap is told the
    /// part of them since it was opened, so a recording puts silence in their
    /// place and its sound keeps to the clock, rather than coming early from
    /// there on; and the sound held back by delays ages through them, so what
    /// was held comes out no later than it would have.
    pub fn skipped(&mut self, blocks: u64, from: std::time::Instant) {
        let block =
            std::time::Duration::from_secs_f64(BLOCK_FRAMES as f64 / f64::from(SAMPLE_RATE));
        #[allow(clippy::cast_possible_truncation)] // A few seconds of frames at most.
        let frames = (blocks as usize) * BLOCK_FRAMES;
        for source in &mut self.sources {
            source.delay.advance(frames);
        }
        for bus in self
            .buses
            .iter_mut()
            .chain(std::iter::once(&mut self.monitor))
        {
            bus.delay.advance(frames);
            for tap in &mut bus.taps {
                // Blocks due before the tap was opened are not its to owe.
                let before = tap
                    .opened
                    .checked_duration_since(from)
                    .map_or(0.0, |since| since.as_secs_f64() / block.as_secs_f64());
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                // Positive, small.
                let owed = blocks.saturating_sub(before.ceil() as u64);
                let samples = owed * BLOCK_FRAMES as u64 * 2;
                tap.lost_samples.fetch_add(samples, Ordering::Relaxed);
                tap.pending_lost += samples;
            }
        }
    }

    /// Mix one block of [`BLOCK_FRAMES`] and send every bus on its way.
    pub fn process_block(&mut self) {
        let n = BLOCK_FRAMES;
        self.scratch.resize(n, [0.0; 2]);
        self.sums.resize_with(self.buses.len(), Vec::new);
        for sum in &mut self.sums {
            sum.clear();
            sum.resize(n, [0.0; 2]);
        }
        self.solo_sum.clear();
        self.solo_sum.resize(n, [0.0; 2]);
        let soloing = self.soloing();

        for source in &mut self.sources {
            self.scratch.fill([0.0; 2]);
            source.feed.pull(&mut self.scratch);
            if source.solo {
                add_into(&mut self.solo_sum, &self.scratch);
            }
            source.equalizer.process(&mut self.scratch);
            source.dynamics.process(&mut self.scratch);
            source.delay.run(&mut self.scratch, source.delay_ms);
            let target = source.target_gain();
            let peak = ramp_sides(&mut self.scratch, source.current, target);
            source.current = target;
            source.meter = fall(source.meter, peak);
            source.held_peak =
                std::array::from_fn(|channel| source.held_peak[channel].max(peak[channel]));
            for send in &source.sends {
                if let Some(i) = self.buses.iter().position(|b| &b.uuid == send) {
                    for (acc, frame) in self.sums[i].iter_mut().zip(&self.scratch) {
                        acc[0] += frame[0];
                        acc[1] += frame[1];
                    }
                }
            }
        }

        // What the monitor hears when nothing is soloed: its bus as mixed,
        // before that bus's delay — the delay is for a late picture, not the
        // operator's ears.
        let listen = self
            .monitor_listen
            .as_deref()
            .filter(|l| self.buses.iter().any(|b| b.uuid == *l))
            .or_else(|| self.buses.first().map(|b| b.uuid.as_str()))
            .map(str::to_string);
        let mut heard = if soloing {
            std::mem::take(&mut self.solo_sum)
        } else {
            vec![[0.0; 2]; n]
        };

        for (bus, sum) in self.buses.iter_mut().zip(&mut self.sums) {
            bus.equalizer.process(sum);
            bus.dynamics.process(sum);
            let target = if bus.muted {
                0.0
            } else {
                db_to_gain(bus.gain_db)
            };
            let peak = ramp(sum, bus.current, target);
            bus.current = target;
            bus.meter = fall(bus.meter, peak);
            bus.held_peak =
                std::array::from_fn(|channel| bus.held_peak[channel].max(peak[channel]));
            if !soloing && listen.as_deref() == Some(bus.uuid.as_str()) {
                heard.copy_from_slice(sum);
            }
            bus.apply_delay(sum);
            bus.deliver(sum);
        }

        let monitor = &mut self.monitor;
        let target = if monitor.muted {
            0.0
        } else {
            db_to_gain(monitor.gain_db)
        };
        let peak = ramp(&mut heard, monitor.current, target);
        monitor.current = target;
        monitor.meter = fall(monitor.meter, peak);
        monitor.deliver(&heard);
        if soloing {
            self.solo_sum = heard;
        }
    }
}

/// Sound held back by a number of milliseconds, a block at a time.
#[derive(Debug, Default)]
struct DelayLine {
    held: VecDeque<Frame>,
}

impl DelayLine {
    /// Let `frames` frames of time pass unheard: what was held that long
    /// goes, and silence stands for what came meanwhile.
    fn advance(&mut self, frames: usize) {
        let held = self.held.len();
        if frames >= held {
            self.held.iter_mut().for_each(|frame| *frame = [0.0; 2]);
        } else {
            self.held.drain(..frames);
            self.held.extend(std::iter::repeat_n([0.0; 2], frames));
        }
    }

    /// Run `block` through the line in place, held back by `delay_ms` (at
    /// most [`MAX_DELAY_MS`]).
    fn run(&mut self, block: &mut [Frame], delay_ms: f32) {
        let ms = if delay_ms.is_finite() {
            delay_ms.clamp(0.0, MAX_DELAY_MS)
        } else {
            0.0
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // At most a second's frames.
        let frames = (f64::from(ms) / 1000.0 * f64::from(SAMPLE_RATE)).round() as usize;
        if frames == 0 {
            self.held.clear();
            return;
        }
        // A changed delay takes effect by growing or trimming the line: a
        // longer delay repeats nothing and a shorter one skips ahead, which is
        // the same as a console's.
        while self.held.len() < frames {
            self.held.push_front([0.0; 2]);
        }
        let excess = self.held.len() - frames;
        self.held.drain(..excess);
        for slot in block.iter_mut() {
            self.held.push_back(*slot);
            *slot = self.held.pop_front().unwrap_or([0.0; 2]);
        }
    }
}

fn add_into(acc: &mut [Frame], block: &[Frame]) {
    for (a, f) in acc.iter_mut().zip(block) {
        a[0] += f[0];
        a[1] += f[1];
    }
}

fn add_tap(bus: &mut Bus, token: u64) -> BusTap {
    let (sender, receiver) = crossbeam_channel::bounded(TAP_CAPACITY);
    let dropped = Arc::new(AtomicU64::new(0));
    let lost_samples = Arc::new(AtomicU64::new(0));
    let opened = std::time::Instant::now();
    bus.taps.push(Tap {
        token,
        sender,
        dropped: dropped.clone(),
        lost_samples: lost_samples.clone(),
        pending_lost: 0,
        opened,
    });
    BusTap {
        token,
        receiver,
        dropped,
        lost_samples,
        opened,
    }
}

/// Scale `block` by a gain moving linearly from `from` to `to`, returning the
/// peak of each channel afterwards.
/// [`ramp`] with a gain of its own for each side.
fn ramp_sides(block: &mut [Frame], from: Frame, to: Frame) -> Frame {
    let n = block.len().max(1) as f32;
    let mut peak = [0.0f32; 2];
    for (i, frame) in block.iter_mut().enumerate() {
        let t = (i + 1) as f32 / n;
        for c in 0..2 {
            frame[c] *= from[c] + (to[c] - from[c]) * t;
            peak[c] = peak[c].max(frame[c].abs());
        }
    }
    peak
}

fn ramp(block: &mut [Frame], from: f32, to: f32) -> Frame {
    let n = block.len().max(1) as f32;
    let mut peak = [0.0f32; 2];
    for (i, frame) in block.iter_mut().enumerate() {
        let g = from + (to - from) * (i + 1) as f32 / n;
        for c in 0..2 {
            frame[c] *= g;
            peak[c] = peak[c].max(frame[c].abs());
        }
    }
    peak
}

/// A peak meter: jumps up to a new peak, falls back at [`METER_DECAY`].
fn fall(meter: Frame, peak: Frame) -> Frame {
    [
        peak[0].max(meter[0] * METER_DECAY),
        peak[1].max(meter[1] * METER_DECAY),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fifo_source(uuid: &str, level: f32, blocks: usize) -> (Source, Arc<Mutex<VecDeque<Frame>>>) {
        let fifo = Arc::new(Mutex::new(
            std::iter::repeat_n([level, -level], BLOCK_FRAMES * blocks).collect(),
        ));
        (Source::new(uuid, Feed::Fifo(fifo.clone())), fifo)
    }

    fn core_with(sources: Vec<Source>, buses: &[&str]) -> MixCore {
        MixCore {
            sources,
            buses: buses.iter().map(|b| Bus::new(*b)).collect(),
            ..MixCore::default()
        }
    }

    fn last_frame(tap: &BusTap) -> [f32; 2] {
        let mut last = None;
        while let Ok(chunk) = tap.receiver.try_recv() {
            last = Some(chunk);
        }
        let s = last.expect("a block").samples;
        [s[s.len() - 2], s[s.len() - 1]]
    }

    #[test]
    fn source_compression_is_once_before_sends_and_fader_but_not_raw_pfl() {
        let (mut source, _) = fifo_source("mic", 0.5, 20);
        source.sends = vec!["main".into(), "aux".into()];
        source.solo = true;
        source.gain_db = -6.0;
        let config = super::super::CompressorConfig {
            enabled: true,
            threshold_db: -18.0618,
            ratio: 4.0,
            attack_ms: 0.1,
            knee_db: 0.0,
            ..super::super::CompressorConfig::default()
        };
        source
            .dynamics
            .configure(config, Some(super::super::dynamics::Processor::new(config)));
        let mut core = core_with(vec![source], &["main", "aux"]);
        let main = core.tap("main").unwrap();
        let aux = core.tap("aux").unwrap();
        let phones = core.tap_monitor();
        for _ in 0..8 {
            core.process_block();
        }
        let expected = 0.1767767 * db_to_gain(-6.0);
        assert!((last_frame(&main)[0] - expected).abs() < 1e-4);
        assert!((last_frame(&aux)[0] - expected).abs() < 1e-4);
        assert!((last_frame(&phones)[0] - 0.5).abs() < 1e-5);
        assert!((core.source("mic").unwrap().dynamics.snapshot().pre_db[0] + 6.0206).abs() < 1e-3);
    }

    #[test]
    fn bus_compression_reaches_outputs_and_normal_monitor_before_delay() {
        let (mut source, _) = fifo_source("mic", 0.5, 20);
        source.sends = vec!["main".into()];
        let mut core = core_with(vec![source], &["main"]);
        let config = super::super::CompressorConfig {
            enabled: true,
            threshold_db: -18.0618,
            ratio: 4.0,
            attack_ms: 0.1,
            knee_db: 0.0,
            ..super::super::CompressorConfig::default()
        };
        let bus = core.bus_mut("main").unwrap();
        bus.dynamics
            .configure(config, Some(super::super::dynamics::Processor::new(config)));
        bus.gain_db = -6.0;
        bus.delay_ms = 100.0;
        let main = core.tap("main").unwrap();
        let phones = core.tap_monitor();
        for _ in 0..8 {
            core.process_block();
        }
        let expected = 0.1767767 * db_to_gain(-6.0);
        assert!((last_frame(&phones)[0] - expected).abs() < 1e-4);
        assert_eq!(last_frame(&main), [0.0, 0.0]);
        for _ in 0..8 {
            core.process_block();
        }
        assert!((last_frame(&main)[0] - expected).abs() < 1e-4);
    }

    #[test]
    fn equalizer_precedes_compression_and_fader_once_for_all_sends_but_not_pfl() {
        let fixture: Vec<Frame> = (0..BLOCK_FRAMES)
            .map(|index| {
                let sample = (std::f32::consts::TAU * index as f32 / 48.0).sin() * 0.5;
                [sample, sample * 0.5]
            })
            .collect();
        let (mut source, fifo) = fifo_source("mic", 0.0, 0);
        source.sends = vec!["main".into(), "aux".into()];
        source.solo = true;
        source.gain_db = -6.0;
        let mut eq = super::super::EqualizerConfig {
            enabled: true,
            ..super::super::EqualizerConfig::default()
        };
        eq.bands[1].gain_db = -6.0;
        let comp = super::super::CompressorConfig {
            enabled: true,
            ..super::super::CompressorConfig::default()
        };
        drop(
            source
                .equalizer
                .configure(eq, Some(super::super::equalizer::Processor::prepare(eq))),
        );
        source
            .dynamics
            .configure(comp, Some(super::super::dynamics::Processor::new(comp)));
        let mut reference_eq = super::super::equalizer::Equalizer::default();
        drop(reference_eq.configure(eq, Some(super::super::equalizer::Processor::prepare(eq))));
        let mut reference_comp = super::super::dynamics::Dynamics::default();
        reference_comp.configure(comp, Some(super::super::dynamics::Processor::new(comp)));
        let mut core = core_with(vec![source], &["main", "aux"]);
        let main = core.tap("main").unwrap();
        let aux = core.tap("aux").unwrap();
        let phones = core.tap_monitor();
        let mut expected = fixture.clone();
        for _ in 0..20 {
            fifo.lock().unwrap().extend(fixture.iter().copied());
            expected.clone_from(&fixture);
            reference_eq.process(&mut expected);
            reference_comp.process(&mut expected);
            core.process_block();
        }
        let expected = expected[BLOCK_FRAMES - 1].map(|sample| sample * db_to_gain(-6.0));
        for actual in [last_frame(&main), last_frame(&aux)] {
            for channel in 0..2 {
                assert!((actual[channel] - expected[channel]).abs() < 1e-6);
            }
        }
        assert_eq!(last_frame(&phones), fixture[BLOCK_FRAMES - 1]);
        assert!(core
            .source("mic")
            .unwrap()
            .equalizer
            .snapshot()
            .error
            .is_none());
    }

    #[test]
    fn bus_equalizer_reaches_output_and_monitor_before_the_output_delay() {
        let (mut source, _) = fifo_source("mic", 0.125, 30);
        source.sends = vec!["main".into()];
        let mut core = core_with(vec![source], &["main"]);
        let mut eq = super::super::EqualizerConfig {
            enabled: true,
            ..super::super::EqualizerConfig::default()
        };
        eq.low_shelf.gain_db = 6.0;
        let bus = core.bus_mut("main").unwrap();
        bus.gain_db = -6.0;
        bus.delay_ms = 100.0;
        drop(
            bus.equalizer
                .configure(eq, Some(super::super::equalizer::Processor::prepare(eq))),
        );
        let main = core.tap("main").unwrap();
        let phones = core.tap_monitor();
        for _ in 0..8 {
            core.process_block();
        }
        assert!((last_frame(&phones)[0] - 0.125).abs() < 0.002);
        assert_eq!(last_frame(&main), [0.0; 2]);
        for _ in 0..12 {
            core.process_block();
        }
        assert!((last_frame(&main)[0] - 0.125).abs() < 0.002);
    }

    /// A source held back by 100 ms reaches its bus 100 ms after one that is
    /// not, the same moment's sound; pre-fader listen hears it at once.
    #[test]
    fn a_held_back_source_reaches_its_buses_that_much_later() {
        let block_ms = BLOCK_FRAMES as f64 * 1000.0 / f64::from(SAMPLE_RATE);
        let (mut mic, mic_fifo) = fifo_source("mic", 0.0, 0);
        mic.sends = vec!["master".into()];
        mic.delay_ms = 100.0;
        mic.solo = true;
        let mut core = core_with(vec![mic], &["master"]);
        let master = core.tap("master").unwrap();
        let phones = core.tap_monitor();
        // Silence, then sound from the fifth block on.
        for block in 0..20 {
            let level = if block >= 4 { 0.5 } else { 0.0 };
            mic_fifo
                .lock()
                .unwrap()
                .extend(std::iter::repeat_n([level, level], BLOCK_FRAMES));
        }
        let first_loud = |tap: &BusTap| {
            tap.receiver
                .try_iter()
                .flat_map(|chunk| chunk.samples)
                .position(|s| s.abs() > 0.1)
                .map(|i| (i / 2) as f64 * 1000.0 / f64::from(SAMPLE_RATE))
        };
        for _ in 0..20 {
            core.process_block();
        }
        let out = first_loud(&master).expect("heard on Master");
        let heard = first_loud(&phones).expect("heard in the headphones");
        assert!(
            (heard - 4.0 * block_ms).abs() < 0.5,
            "PFL at once: {heard} ms"
        );
        assert!(
            (out - heard - 100.0).abs() < 0.5,
            "Master {out} ms, PFL {heard} ms"
        );
    }

    /// Time the mixer jumped over is told to every tap open then, with the
    /// next block — after the blocks before the jump — so a recording fills
    /// it with silence where it was and its sound stays on the clock; a tap
    /// opened after the jump owes nothing for it.
    #[test]
    fn blocks_jumped_over_are_lost_to_every_tap() {
        let mut core = core_with(Vec::new(), &["master", "stream"]);
        let master = core.tap("master").unwrap();
        let stream = core.tap("stream").unwrap();
        core.process_block();
        core.skipped(12, std::time::Instant::now());
        let late = core.tap("master").unwrap();
        core.process_block();
        let lost = 12 * BLOCK_FRAMES as u64 * 2;
        for tap in [&master, &stream] {
            assert_eq!(tap.lost_samples.load(Ordering::Relaxed), lost);
            let chunks: Vec<u64> = tap.receiver.try_iter().map(|c| c.lost_before).collect();
            assert_eq!(chunks, [0, lost], "before the block after the jump");
        }
        let chunks: Vec<u64> = late.receiver.try_iter().map(|c| c.lost_before).collect();
        assert_eq!(chunks, [0], "opened after it");
    }

    /// A tap opened while the mixer was held up owes only the time since it
    /// was opened, not the whole of the hold-up.
    #[test]
    fn a_tap_opened_during_a_hold_up_owes_only_what_came_after() {
        let mut core = core_with(Vec::new(), &["master"]);
        let held_from = std::time::Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let tap = core.tap("master").unwrap();
        // 40 blocks jumped over, due from before the tap was opened: about
        // ten of them came after it.
        core.skipped(40, held_from);
        core.process_block();
        let owed = tap.receiver.try_recv().expect("a block").lost_before;
        let blocks = owed / (BLOCK_FRAMES as u64 * 2);
        assert!((28..=32).contains(&blocks), "{blocks} blocks owed of 40");
    }

    /// Sound held back by a delay ages through a hold-up: what was held
    /// longer than the delay is gone when the mixer comes back.
    #[test]
    fn held_back_sound_ages_through_a_hold_up() {
        let (mut mic, mic_fifo) = fifo_source("mic", 0.0, 0);
        mic.sends = vec!["master".into()];
        mic.delay_ms = 100.0;
        let mut core = core_with(vec![mic], &["master"]);
        let master = core.tap("master").unwrap();
        mic_fifo
            .lock()
            .unwrap()
            .extend(std::iter::repeat_n([0.5, 0.5], BLOCK_FRAMES * 5));
        for _ in 0..5 {
            core.process_block();
        }
        while master.receiver.try_recv().is_ok() {}
        // 0.4 s jumped over: the 0.1 s held is long gone.
        core.skipped(40, std::time::Instant::now());
        for _ in 0..12 {
            core.process_block();
        }
        let after: Vec<f32> = master.receiver.try_iter().flat_map(|c| c.samples).collect();
        assert_eq!(after.len(), 12 * BLOCK_FRAMES * 2);
        let first = after.iter().position(|s| s.abs() >= 1e-6);
        assert_eq!(first, None, "nothing old comes out after it");
    }

    /// A tap's channel full, the chunks it could not take are told with the
    /// next that gets through.
    #[test]
    fn a_full_tap_tells_the_next_chunk_what_it_lost() {
        let mut core = core_with(Vec::new(), &["master"]);
        let tap = core.tap("master").unwrap();
        for _ in 0..TAP_CAPACITY + 2 {
            core.process_block();
        }
        let queued: Vec<u64> = tap.receiver.try_iter().map(|c| c.lost_before).collect();
        assert_eq!(queued.len(), TAP_CAPACITY);
        assert!(queued.iter().all(|&l| l == 0));
        core.process_block();
        let next = tap.receiver.try_recv().expect("room again");
        assert_eq!(next.lost_before, 2 * BLOCK_FRAMES as u64 * 2);
    }

    #[test]
    fn a_source_reaches_only_the_buses_it_is_sent_to() {
        let (mut src, _) = fifo_source("mic", 0.5, 4);
        src.sends = vec!["stream".into()];
        let mut core = core_with(vec![src], &["stream", "room"]);
        let stream = core.tap("stream").unwrap();
        let room = core.tap("room").unwrap();
        core.process_block();
        core.process_block();
        let s = last_frame(&stream);
        assert!(
            (s[0] - 0.5).abs() < 1e-6 && (s[1] + 0.5).abs() < 1e-6,
            "{s:?}"
        );
        assert_eq!(last_frame(&room), [0.0, 0.0], "not sent to the room");
    }

    #[test]
    fn balance_turns_the_far_side_down_and_leaves_the_near_one() {
        let (mut src, _) = fifo_source("mic", 0.5, 4);
        src.sends = vec!["m".into()];
        src.balance = 0.5;
        let mut core = core_with(vec![src], &["m"]);
        let tap = core.tap("m").unwrap();
        core.process_block();
        core.process_block();
        let s = last_frame(&tap);
        assert!((s[0] - 0.25).abs() < 1e-6, "left at half: {s:?}");
        assert!((s[1] + 0.5).abs() < 1e-6, "right untouched: {s:?}");
        core.sources[0].balance = -1.0;
        core.process_block();
        core.process_block();
        let s = last_frame(&tap);
        assert!((s[0] - 0.5).abs() < 1e-6 && s[1].abs() < 1e-6, "{s:?}");
    }

    #[test]
    fn two_sources_on_one_bus_add_up() {
        let (mut a, _) = fifo_source("a", 0.25, 4);
        let (mut b, _) = fifo_source("b", 0.5, 4);
        a.sends = vec!["m".into()];
        b.sends = vec!["m".into()];
        let mut core = core_with(vec![a, b], &["m"]);
        let tap = core.tap("m").unwrap();
        core.process_block();
        core.process_block();
        assert!((last_frame(&tap)[0] - 0.75).abs() < 1e-6);
    }

    #[test]
    fn faders_and_mutes_apply_after_a_click_free_ramp() {
        let (mut src, _) = fifo_source("clip", 1.0, 8);
        src.sends = vec!["m".into()];
        src.gain_db = -6.0;
        let mut core = core_with(vec![src], &["m"]);
        let tap = core.tap("m").unwrap();
        core.process_block();
        // The first block ramps up from silence rather than jumping.
        let first = tap.receiver.try_recv().unwrap().samples;
        assert!(first[0].abs() < 0.01, "starts near silence: {}", first[0]);
        core.process_block();
        assert!((last_frame(&tap)[0] - 0.501).abs() < 1e-3, "-6 dB");

        core.source_mut("clip").unwrap().muted = true;
        core.process_block();
        core.process_block();
        assert_eq!(last_frame(&tap)[0], 0.0, "muted");

        core.source_mut("clip").unwrap().muted = false;
        core.bus_mut("m").unwrap().gain_db = -96.0;
        core.process_block();
        core.process_block();
        assert_eq!(last_frame(&tap)[0], 0.0, "bus fader down");
    }

    #[test]
    fn the_gate_takes_a_following_source_off_the_air() {
        let (mut src, _) = fifo_source("clip", 1.0, 8);
        src.sends = vec!["m".into()];
        src.gate = 0.0;
        let mut core = core_with(vec![src], &["m"]);
        let tap = core.tap("m").unwrap();
        core.process_block();
        core.process_block();
        assert_eq!(last_frame(&tap)[0], 0.0, "off air, silent");
        core.source_mut("clip").unwrap().gate = 0.5;
        core.process_block();
        core.process_block();
        assert!(
            (last_frame(&tap)[0] - 0.5).abs() < 1e-6,
            "halfway through a fade"
        );
    }

    #[test]
    fn a_bus_delay_holds_the_sound_back_by_that_many_milliseconds() {
        // 20 ms = 960 frames = two blocks of silence before the tone.
        let (mut src, _) = fifo_source("clip", 1.0, 8);
        src.sends = vec!["m".into()];
        let mut core = core_with(vec![src], &["m"]);
        core.bus_mut("m").unwrap().delay_ms = 20.0;
        let tap = core.tap("m").unwrap();
        for _ in 0..4 {
            core.process_block();
        }
        let blocks: Vec<Vec<f32>> = tap.receiver.try_iter().map(|c| c.samples).collect();
        assert!(blocks[0].iter().all(|s| *s == 0.0));
        assert!(blocks[1].iter().all(|s| *s == 0.0));
        assert!(
            blocks[2].iter().any(|s| *s != 0.0),
            "the tone arrives 20 ms late"
        );
    }

    #[test]
    fn meters_show_the_post_fader_peak_and_fall_back() {
        let (mut src, fifo) = fifo_source("mic", 0.8, 2);
        src.sends = vec!["m".into()];
        let mut core = core_with(vec![src], &["m"]);
        core.process_block();
        core.process_block();
        assert!((core.source("mic").unwrap().meter()[0] - 0.8).abs() < 1e-3);
        assert!((core.bus("m").unwrap().meter()[0] - 0.8).abs() < 1e-3);
        fifo.lock().unwrap().clear();
        for _ in 0..100 {
            core.process_block();
        }
        assert!(
            core.source("mic").unwrap().meter()[0] < 0.1,
            "a second later it has fallen"
        );
    }

    #[test]
    fn over_full_scale_peaks_survive_until_ui_reads_them() {
        let (mut source, fifo) = fifo_source("mic", 1.25, 4);
        source.sends = vec!["m".into()];
        let mut core = core_with(vec![source], &["m"]);
        core.process_block();
        core.process_block();
        fifo.lock().unwrap().clear();
        for _ in 0..100 {
            core.process_block();
        }
        assert!(core.bus("m").unwrap().meter()[0] < 1.0);
        assert_eq!(core.bus("m").unwrap().held_peak, [1.25; 2]);
        assert_eq!(core.source("mic").unwrap().held_peak, [1.25; 2]);
    }

    #[test]
    #[ignore = "manual service-time probe; run alone with --nocapture"]
    fn mixer_block_budget_probe() {
        let blocks: usize = std::env::var("NIDA_AUDIO_PROBE_BLOCKS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(2000);
        assert!(blocks >= 100);
        let mode = std::env::var("NIDA_AUDIO_PROBE_EQ").unwrap_or_else(|_| "disabled".into());
        let compress = std::env::var_os("NIDA_AUDIO_PROBE_COMPRESSOR").is_some();
        let compressor = super::super::CompressorConfig {
            enabled: true,
            threshold_db: -36.0,
            ratio: 4.0,
            attack_ms: 10.0,
            release_ms: 100.0,
            makeup_db: 3.0,
            ..super::super::CompressorConfig::default()
        };
        assert!(["disabled", "sources", "sources-and-buses"].contains(&mode.as_str()));
        for count in [1, 8, 16, 32] {
            let mut sources = Vec::new();
            let mut fifos = Vec::new();
            for index in 0..count {
                let (mut source, fifo) = fifo_source(&format!("probe-{index}"), 0.0, 0);
                source.sends = vec!["main".into(), "aux".into()];
                if compress {
                    source.dynamics.configure(
                        compressor,
                        Some(super::super::dynamics::Processor::new(compressor)),
                    );
                }
                if mode != "disabled" {
                    let config = super::super::equalizer::fixture_config();
                    drop(source.equalizer.configure(
                        config,
                        Some(super::super::equalizer::Processor::prepare(config)),
                    ));
                }
                sources.push(source);
                fifos.push(fifo);
            }
            let mut core = core_with(sources, &["main", "aux"]);
            if compress {
                for bus in &mut core.buses {
                    bus.dynamics.configure(
                        compressor,
                        Some(super::super::dynamics::Processor::new(compressor)),
                    );
                }
            }
            if mode == "sources-and-buses" {
                for bus in &mut core.buses {
                    let config = super::super::equalizer::fixture_config();
                    drop(bus.equalizer.configure(
                        config,
                        Some(super::super::equalizer::Processor::prepare(config)),
                    ));
                }
            }
            let fixture: Vec<Frame> = (0..BLOCK_FRAMES)
                .map(|index| {
                    let phase = std::f32::consts::TAU * index as f32 / 48.0;
                    [0.02 * phase.sin(), 0.015 * (phase + 0.5).sin()]
                })
                .collect();
            let mut times = Vec::with_capacity(blocks);
            for index in 0..blocks + 100 {
                for fifo in &fifos {
                    fifo.lock().unwrap().extend(fixture.iter().copied());
                }
                let start = std::time::Instant::now();
                core.process_block();
                let elapsed = start.elapsed().as_secs_f64() * 1e6;
                std::hint::black_box(&core);
                if index >= 100 {
                    times.push(elapsed);
                }
            }
            if mode != "disabled" {
                assert!(core
                    .sources
                    .iter()
                    .all(|source| source.equalizer.snapshot().ready
                        && source.equalizer.snapshot().error.is_none()));
            }
            if mode == "sources-and-buses" {
                assert!(core.buses.iter().all(|bus| bus.equalizer.snapshot().ready
                    && bus.equalizer.snapshot().error.is_none()));
            }
            if compress {
                assert!(core
                    .sources
                    .iter()
                    .all(|source| source.dynamics.snapshot().ready
                        && source.dynamics.snapshot().error.is_none()));
                assert!(core
                    .buses
                    .iter()
                    .all(|bus| bus.dynamics.snapshot().ready
                        && bus.dynamics.snapshot().error.is_none()));
            }
            times.sort_by(f64::total_cmp);
            println!("{{\"mode\":\"{mode}\",\"compressors\":{compress},\"sources\":{count},\"buses\":2,\"blocks\":{blocks},\"p50_us\":{:.3},\"p95_us\":{:.3},\"p99_us\":{:.3},\"max_us\":{:.3},\"service_overruns_10ms\":{}}}",
                times[blocks/2],times[blocks*95/100],times[blocks*99/100],times[blocks-1],times.iter().filter(|time|**time>10_000.0).count());
        }
    }

    #[test]
    fn an_abandoned_tap_is_dropped() {
        let mut core = core_with(vec![], &["m"]);
        let tap = core.tap("m").unwrap();
        drop(tap);
        core.process_block();
        assert!(core.buses[0].taps.is_empty());
    }

    #[test]
    fn the_monitor_listens_to_its_bus_until_something_is_soloed() {
        let (mut music, _) = fifo_source("music", 0.5, 8);
        music.sends = vec!["stream".into()];
        let (mut mic, _) = fifo_source("mic", 0.25, 8);
        // The mic is muted and off air: nothing reaches the buses.
        mic.muted = true;
        mic.gate = 0.0;
        let mut core = core_with(vec![music, mic], &["room", "stream"]);
        core.monitor_listen = Some("stream".into());
        let phones = core.tap_monitor();
        let stream = core.tap("stream").unwrap();
        core.process_block();
        core.process_block();
        assert!(
            (last_frame(&phones)[0] - 0.5).abs() < 1e-6,
            "hears the stream"
        );

        // Soloing the mic: the headphones hear only it, whatever its fader,
        // mute and gate say, and the stream is untouched.
        core.source_mut("mic").unwrap().solo = true;
        core.process_block();
        core.process_block();
        assert!(
            (last_frame(&phones)[0] - 0.25).abs() < 1e-6,
            "hears only the mic"
        );
        assert!(
            (last_frame(&stream)[0] - 0.5).abs() < 1e-6,
            "the stream carries on"
        );

        core.source_mut("mic").unwrap().solo = false;
        core.monitor.gain_db = -6.0;
        core.process_block();
        core.process_block();
        assert!(
            (last_frame(&phones)[0] - 0.25).abs() < 1e-3,
            "back to the stream, at the monitor's -6 dB"
        );
    }

    #[test]
    fn the_monitor_hears_its_bus_before_the_bus_delay() {
        let (mut src, _) = fifo_source("clip", 1.0, 8);
        src.sends = vec!["m".into()];
        let mut core = core_with(vec![src], &["m"]);
        core.bus_mut("m").unwrap().delay_ms = 100.0;
        let phones = core.tap_monitor();
        core.process_block();
        core.process_block();
        assert!(
            (last_frame(&phones)[0] - 1.0).abs() < 1e-6,
            "no delay in the headphones"
        );
    }

    #[test]
    fn an_input_device_is_read_onto_the_mix_clock() {
        // A 44.1 kHz stereo interface where the microphone is on channel 2:
        // the source takes that channel into both sides, at 48 kHz.
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut feed = InputFeed::new(rx, 44_100, 2, 1, 1);
        let chunk: Vec<f32> = (0..4410).flat_map(|_| [0.0, 0.3]).collect();
        for _ in 0..10 {
            tx.send(PcmChunk {
                samples: chunk.clone(),
                lost_before: 0,
            })
            .unwrap();
        }
        let mut out = [[0.0; 2]; BLOCK_FRAMES];
        feed.pull(&mut out);
        assert!(
            out.iter()
                .skip(4)
                .all(|f| (f[0] - 0.3).abs() < 1e-4 && (f[1] - 0.3).abs() < 1e-4),
            "channel 2 on both sides: {:?}",
            &out[..8]
        );
    }
}
