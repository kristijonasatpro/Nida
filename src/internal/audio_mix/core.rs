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
use super::echo::{Canceller, EchoStats};
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
        let Some(step) = self.drift.step_for(&mut self.fifo, nominal, out.len()) else {
            return;
        };
        if self.reader.read(&mut self.fifo, step, out) < out.len() {
            self.drift.ran_dry();
        }
    }
}

/// One of a source's sends, as the mix plays it.
#[derive(Debug, Clone, PartialEq)]
pub struct Send {
    /// The bus, by UUID.
    pub bus: String,
    /// Its level, linear.
    pub gain: f32,
    /// Taken before the source's fader — a monitor mix, the fader moving
    /// the room and not the stage — or after it.
    pub pre: bool,
}

impl From<&str> for Send {
    /// A send to `bus` at unity, after the fader: as every send was before
    /// sends had levels.
    fn from(bus: &str) -> Self {
        Self {
            bus: bus.to_string(),
            gain: 1.0,
            pre: false,
        }
    }
}

/// A send's gains as last played, before and after the fader: each glides
/// to where it is wanted over a block, a send switched between them
/// crossfading, one turned off fading out in its last block.
#[derive(Debug, Clone)]
struct SendEnv {
    bus: String,
    pre: Frame,
    post: Frame,
}

/// One source strip: a feed with its fader, mute and bus sends.
pub struct Source {
    pub(super) held_peak: Frame,
    pub(super) dynamics: super::dynamics::SourceChain,
    /// Deck UUID for a clip; the input's own UUID for a capture device.
    pub uuid: String,
    pub feed: Feed,
    /// Fader, in dB.
    pub gain_db: f32,
    pub muted: bool,
    /// Buses this source is sent to, with each send's level and point.
    pub sends: Vec<Send>,
    /// A microphone: a sound device's input, not an app's sound or a
    /// camera's. Left out of what a room's speakers are heard to play (see
    /// [`RoomSound`]), and cleaned of it for a bus that asks (see
    /// [`EchoPath`]).
    pub mic: bool,
    /// Its sound with a room's speakers taken out, one for each room a bus
    /// it is sent to removes the echo of.
    pub echo: Vec<EchoPath>,
    /// Each send's gains as last played (see [`SendEnv`]).
    send_env: Vec<SendEnv>,
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
    /// Its level before its fader, mute and gate: what it plays, heard or
    /// not.
    signal: Frame,
    delay: DelayLine,
}

impl Source {
    /// A source at unity gain, unmuted, sent nowhere.
    pub fn new(uuid: impl Into<String>, feed: Feed) -> Self {
        Self {
            uuid: uuid.into(),
            feed,
            dynamics: super::dynamics::SourceChain::default(),
            held_peak: [0.0; 2],
            gain_db: 0.0,
            muted: false,
            sends: Vec::new(),
            mic: false,
            echo: Vec::new(),
            send_env: Vec::new(),
            gate: 1.0,
            solo: false,
            balance: 0.0,
            delay_ms: 0.0,
            current: [0.0; 2],
            meter: [0.0; 2],
            signal: [0.0; 2],
            delay: DelayLine::default(),
        }
    }

    /// Its level before its fader, mute and gate, per channel, linear: what
    /// it plays, whether or not anyone hears it.
    pub fn signal(&self) -> Frame {
        self.signal
    }

    /// Its measured level, for tests.
    #[cfg(test)]
    pub(super) fn set_signal_for_tests(&mut self, signal: Frame) {
        self.signal = signal;
    }

    /// Post-fader peak level per channel, linear.
    pub fn meter(&self) -> Frame {
        self.meter
    }

    fn target_gain(&self) -> Frame {
        self.gain_with(db_to_gain(self.gain_db))
    }

    /// Its gain for a send before its fader: all but the fader — its mute,
    /// On and fades, its balance.
    fn pre_fader_gain(&self) -> Frame {
        self.gain_with(1.0)
    }

    fn gain_with(&self, fader: f32) -> Frame {
        if self.muted {
            return [0.0; 2];
        }
        let g = fader * self.gate.clamp(0.0, 1.0);
        let b = self.balance.clamp(-1.0, 1.0);
        [g * (1.0 - b).min(1.0), g * (1.0 + b).min(1.0)]
    }
}

/// A microphone's sound with what a room's speakers play taken out (see
/// `echo`), for the buses that remove that room's echo: a call's, so the
/// guest does not hear themselves come back. Every other bus has the
/// microphone as it is, sooner and at full quality.
///
/// The canceller hears the microphone first, as it comes from its device:
/// the strip's own processing (a gate, a compressor) would change the echo
/// from moment to moment and keep the canceller from learning it. That
/// processing runs after it, on a chain of its own with the strip's
/// settings, and then the strip's delay. It is mono: the microphone's two
/// sides together (one, for a mono microphone), the same on both.
pub struct EchoPath {
    /// The bus whose sound the room's speakers play, by UUID.
    pub room: String,
    /// Still asked for. One that is not crosses back to the microphone as
    /// it is over a block, and goes.
    pub wanted: bool,
    /// How far its sends have it, 0 to 1, and where to this block: a new
    /// one fades in from the microphone as it is over a block, once its
    /// strip's delay is full of its sound (the strip's own is already).
    weight: f32,
    weight_to: f32,
    canceller: Canceller,
    pub chain: super::dynamics::SourceChain,
    delay: DelayLine,
    mono: Vec<f32>,
    /// This block, cleaned, before the fader.
    block: Vec<Frame>,
    /// And after it.
    post: Vec<Frame>,
    stats: EchoStats,
    blocks: u64,
}

impl EchoPath {
    /// The microphone cleaned of what `room` plays, by `canceller` (made
    /// before the mixer is held: it takes up to a millisecond).
    pub fn new(room: impl Into<String>, canceller: Canceller) -> Self {
        Self {
            room: room.into(),
            wanted: true,
            weight: 0.0,
            weight_to: 0.0,
            canceller,
            chain: super::dynamics::SourceChain::default(),
            delay: DelayLine::default(),
            mono: vec![0.0; BLOCK_FRAMES],
            block: Vec::new(),
            post: Vec::new(),
            stats: EchoStats::default(),
            blocks: 0,
        }
    }

    /// What the canceller says of the echo, as of a tenth of a second ago at
    /// most.
    pub fn stats(&self) -> EchoStats {
        self.stats
    }

    /// How many blocks it has cleaned (tests).
    #[cfg(test)]
    pub fn heard_blocks(&self) -> u64 {
        self.blocks
    }

    /// Clean this block of the microphone as it came (`raw`), then run the
    /// strip's processing and delay over it.
    fn process(&mut self, raw: &[Frame], delay_ms: f32) {
        self.mono.clear();
        self.mono.extend(raw.iter().map(|f| f32::midpoint(f[0], f[1])));
        self.canceller.capture(&mut self.mono);
        self.block.clear();
        self.block.extend(self.mono.iter().map(|&m| [m, m]));
        self.chain.process(&mut self.block);
        self.delay.run(&mut self.block, delay_ms);
        self.blocks += 1;
        // Its delay full, and two blocks more for the canceller's own: in.
        // Not asked for any more: out.
        let full = frames_of(delay_ms) + 2 * BLOCK_FRAMES;
        let heard = usize::try_from(self.blocks).unwrap_or(usize::MAX).saturating_mul(BLOCK_FRAMES);
        self.weight_to = if !self.wanted {
            0.0
        } else if self.weight > 0.0 || heard >= full {
            1.0
        } else {
            0.0
        };
        if self.blocks.is_multiple_of(10) {
            self.stats = self.canceller.stats();
        }
    }

    /// The mixer jumped over `blocks` blocks: the canceller hears that much
    /// silence on both sides (a fifth of a second at most, what the room's
    /// speakers last played gone by then), so its own buffers and the time
    /// of its echo stay as the room's, and it keeps what it learnt of the
    /// room. (Without the silence the echo after a 120 ms hold-up was 4 dB
    /// less far down, at about -90 dBFS: its suppressor covers it. The
    /// silence costs up to 1 ms a microphone, once.)
    fn skipped(&mut self, blocks: u64) {
        for _ in 0..blocks.min(20) {
            self.canceller.render(&[0.0; BLOCK_FRAMES]);
            self.mono.clear();
            self.mono.resize(BLOCK_FRAMES, 0.0);
            self.canceller.capture(&mut self.mono);
        }
        self.delay.advance(usize::try_from(blocks).unwrap_or(usize::MAX).saturating_mul(BLOCK_FRAMES));
        self.chain.skip();
    }
}

/// What a bus plays from everything but microphones, as the room's speakers
/// play it: through the bus's own processing as the rest of it goes (its
/// insert's delay, its compressor's and limiter's gains, its fader, its
/// delay; an insert's effect itself cannot be followed — its filtering the
/// canceller learns as the room's). It is what the echo cancellers of
/// the microphones cleaned of this bus listen for (see [`EchoPath`]). The
/// microphones are left out: the room's own voices, played on its speakers
/// too, are not an echo to take out of the call. A canceller hearing its
/// own microphone in its reference learns to foretell the voice from it and
/// takes a held vowel out (10 dB of one, in a test).
#[derive(Default)]
pub struct RoomSound {
    sum: Vec<Frame>,
    mono: Vec<f32>,
    gains: Vec<f32>,
    /// Held back as the bus's insert holds it.
    insert_delay: DelayLine,
    /// And as its limiter's look-ahead does.
    lookahead: DelayLine,
    delay: DelayLine,
}

impl RoomSound {
    fn apply_gains(&mut self) {
        for (frame, g) in self.sum.iter_mut().zip(&self.gains) {
            frame[0] *= g;
            frame[1] *= g;
        }
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

/// A live bus's sources other than its mics (see `live`): what the mix
/// sends its device's callback, summed after each source's own processing
/// and fader and before the bus's — the callback runs the bus's processing
/// over the mics and this together.
pub struct LiveSend {
    /// Sources mixed in the device's callback, by UUID: left out of this.
    pub mics: Vec<String>,
    pub tx: crossbeam_channel::Sender<Frame>,
}

/// One bus: the sum of what is sent to it, with its own fader, mute and delay,
/// going to sound devices and to recordings and streams.
pub struct Bus {
    pub(super) held_peak: Frame,
    pub(super) dynamics: super::dynamics::BusChain,
    pub uuid: String,
    pub gain_db: f32,
    pub muted: bool,
    /// Delay in milliseconds, to line sound up with a picture that arrives
    /// late (an LED processor, a projector).
    pub delay_ms: f32,
    /// Sound devices this bus plays on.
    pub outputs: Vec<Arc<OutputRing>>,
    /// Played live: its device's callback mixes its mics, and gets the rest
    /// from here.
    pub live: Option<LiveSend>,
    /// Its Audio Unit insert, first in its processing (see `insert`).
    pub insert: Option<super::insert::Insert>,
    /// The bus the room's speakers play, by UUID, when this one removes the
    /// room's echo: its microphones arrive cleaned of it (see [`EchoPath`]).
    /// Kept while it fades out, once [`Self::echo_on`] is not.
    pub echo_room: Option<String>,
    /// Removing the room's echo: turned on or off, the microphones it gets
    /// cross from as they are to cleaned, or back, over a block.
    pub echo_on: bool,
    /// Another room to remove the echo of, once this one has faded out:
    /// the microphones cross back to as they are, then to cleaned of it.
    pub echo_next: Option<String>,
    /// How far it is cleaning them, 0 to 1, and where to this block.
    echo_mix: f32,
    echo_to: f32,
    /// What it plays without its microphones, while a bus removes its echo.
    pub room: Option<RoomSound>,
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
            dynamics: super::dynamics::BusChain::default(),
            held_peak: [0.0; 2],
            muted: false,
            delay_ms: 0.0,
            outputs: Vec::new(),
            live: None,
            insert: None,
            echo_room: None,
            echo_on: false,
            echo_next: None,
            echo_mix: 0.0,
            echo_to: 0.0,
            room: None,
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

    /// Its measured level, for tests.
    #[cfg(test)]
    pub(super) fn set_meter_for_tests(&mut self, meter: Frame) {
        self.meter = meter;
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
    /// A source's block before its fader, for its sends taken there.
    pre_block: Vec<Frame>,
    /// A microphone's block as it came, for its echo cancellers.
    raw_block: Vec<Frame>,
    /// A microphone's blocks crossing between as they are and cleaned,
    /// before and after its fader.
    blend_pre: Vec<Frame>,
    blend_post: Vec<Frame>,
    /// A send's share of a source's block.
    send_block: Vec<Frame>,
    sums: Vec<Vec<Frame>>,
    /// Each live bus's sum without its mics (see [`LiveSend`]).
    live_sums: Vec<Vec<Frame>>,
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
            pre_block: Vec::new(),
            raw_block: Vec::new(),
            blend_pre: Vec::new(),
            blend_post: Vec::new(),
            send_block: Vec::new(),
            sums: Vec::new(),
            live_sums: Vec::new(),
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
            source.dynamics.skip();
            for path in &mut source.echo {
                path.skipped(blocks);
            }
        }
        for bus in self
            .buses
            .iter_mut()
            .chain(std::iter::once(&mut self.monitor))
        {
            bus.delay.advance(frames);
            if let Some(room) = &mut bus.room {
                room.insert_delay.advance(frames);
                room.lookahead.advance(frames);
                room.delay.advance(frames);
            }
            bus.dynamics.skip(blocks as usize);
            if let Some(insert) = &mut bus.insert {
                insert.skipped(blocks);
            }
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
    // A send's gains are compared as set: exactly silent, exactly unity.
    #[allow(clippy::float_cmp)]
    pub fn process_block(&mut self) {
        let n = BLOCK_FRAMES;
        self.scratch.resize(n, [0.0; 2]);
        self.sums.resize_with(self.buses.len(), Vec::new);
        for sum in &mut self.sums {
            sum.clear();
            sum.resize(n, [0.0; 2]);
        }
        self.live_sums.resize_with(self.buses.len(), Vec::new);
        for (sum, bus) in self.live_sums.iter_mut().zip(&self.buses) {
            sum.clear();
            if bus.live.is_some() {
                sum.resize(n, [0.0; 2]);
            }
        }
        self.solo_sum.clear();
        self.solo_sum.resize(n, [0.0; 2]);
        for bus in &mut self.buses {
            bus.echo_to = if bus.echo_on && bus.echo_room.is_some() {
                1.0
            } else {
                0.0
            };
            if let Some(room) = &mut bus.room {
                room.sum.clear();
                room.sum.resize(n, [0.0; 2]);
            }
        }
        let soloing = self.soloing();

        for source in &mut self.sources {
            self.scratch.fill([0.0; 2]);
            source.feed.pull(&mut self.scratch);
            // How its reader started, once (it is read here, on the mixer's
            // thread, where the log may be written).
            if let Feed::Input(input) | Feed::Live(input) = &mut source.feed {
                if let Some(start) = input.drift.take_start() {
                    log::info!("audio source {}: reader {start}", source.uuid);
                }
            }
            // Its echo cancellers hear it as it came, before its processing.
            if !source.echo.is_empty() {
                self.raw_block.clear();
                self.raw_block.extend_from_slice(&self.scratch);
                for path in &mut source.echo {
                    path.process(&self.raw_block, source.delay_ms);
                }
            }
            // Its processing first, so pre-fader listen hears what goes out,
            // as a console's does.
            source.dynamics.process(&mut self.scratch);
            if source.solo {
                add_into(&mut self.solo_sum, &self.scratch);
            }
            source.delay.run(&mut self.scratch, source.delay_ms);
            source.signal = fall(source.signal, peak_of(&self.scratch));
            // Before its fader, for the sends that take it there.
            let pre_heard = source.sends.iter().any(|s| s.pre)
                || source.send_env.iter().any(|e| e.pre != [0.0; 2])
                || !source.echo.is_empty();
            if pre_heard {
                self.pre_block.clear();
                self.pre_block.extend_from_slice(&self.scratch);
            }
            let pre_gain = source.pre_fader_gain();
            let target = source.target_gain();
            let peak = ramp_sides(&mut self.scratch, source.current, target);
            // Its cleaned sound after the fader too, gliding the same way.
            for path in &mut source.echo {
                path.post.clear();
                path.post.extend_from_slice(&path.block);
                ramp_sides(&mut path.post, source.current, target);
            }
            source.current = target;
            source.meter = fall(source.meter, peak);
            source.held_peak =
                std::array::from_fn(|channel| source.held_peak[channel].max(peak[channel]));
            // Each send: its level on the block before or after the fader,
            // gliding there over the block.
            for send in &source.sends {
                if !source.send_env.iter().any(|e| e.bus == send.bus) {
                    source.send_env.push(SendEnv {
                        bus: send.bus.clone(),
                        pre: [0.0; 2],
                        post: [0.0; 2],
                    });
                }
            }
            for env in &mut source.send_env {
                let want = source.sends.iter().find(|s| s.bus == env.bus);
                let pre_to = want
                    .filter(|s| s.pre)
                    .map_or([0.0; 2], |s| [pre_gain[0] * s.gain, pre_gain[1] * s.gain]);
                let post_to = want.filter(|s| !s.pre).map_or([0.0; 2], |s| [s.gain; 2]);
                if let Some(i) = self.buses.iter().position(|b| b.uuid == env.bus) {
                    let to_live = self.buses[i]
                        .live
                        .as_ref()
                        .is_some_and(|live| !live.mics.contains(&source.uuid));
                    // A bus that removes a room's echo gets the microphone
                    // cleaned of it: crossing over a block as either the
                    // bus's echo removal or the cleaning comes or goes.
                    let bus = &self.buses[i];
                    let clean = bus
                        .echo_room
                        .as_deref()
                        .and_then(|room| source.echo.iter().find(|p| p.room == room))
                        .map(|path| (path, bus.echo_mix * path.weight, bus.echo_to * path.weight_to));
                    let (pre_block, post_block) = match clean {
                        Some((path, from, to)) if from == 1.0 && to == 1.0 => (&path.block, &path.post),
                        Some((path, from, to)) if from != 0.0 || to != 0.0 => {
                            blend(&mut self.blend_pre, &self.pre_block, &path.block, from, to);
                            blend(&mut self.blend_post, &self.scratch, &path.post, from, to);
                            (&self.blend_pre, &self.blend_post)
                        }
                        _ => (&self.pre_block, &self.scratch),
                    };
                    // What the room hears from it, unless it is a microphone.
                    let to_room = !source.mic && self.buses[i].room.is_some();
                    for (from, to, block) in [
                        (env.pre, pre_to, pre_block),
                        (env.post, post_to, post_block),
                    ] {
                        if from == [0.0; 2] && to == [0.0; 2] {
                            continue;
                        }
                        // At unity, as it is: the sum as before sends had levels.
                        let share = if from == [1.0; 2] && to == [1.0; 2] {
                            block
                        } else {
                            self.send_block.clear();
                            self.send_block.extend_from_slice(block);
                            ramp_sides(&mut self.send_block, from, to);
                            &self.send_block
                        };
                        add_into(&mut self.sums[i], share);
                        if to_live {
                            add_into(&mut self.live_sums[i], share);
                        }
                        if to_room {
                            if let Some(room) = &mut self.buses[i].room {
                                add_into(&mut room.sum, share);
                            }
                        }
                    }
                }
                env.pre = pre_to;
                env.post = post_to;
            }
            // A send turned off has faded out over this block: gone.
            let sends = &source.sends;
            source
                .send_env
                .retain(|e| sends.iter().any(|s| s.bus == e.bus));
            // A cleaning no longer asked for has crossed back over this
            // block: gone. The rest are where this block took them.
            source.echo.retain(|path| path.wanted || path.weight_to > 0.0);
            for path in &mut source.echo {
                path.weight = path.weight_to;
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

        // A live bus's device, the rest of the bus: a frame at a time, no
        // allocation; full (the device not reading), what does not fit goes.
        for (bus, sum) in self.buses.iter().zip(&self.live_sums) {
            if let Some(live) = &bus.live {
                for frame in sum {
                    if live.tx.try_send(*frame).is_err() {
                        break;
                    }
                }
            }
        }

        for (bus, sum) in self.buses.iter_mut().zip(&mut self.sums) {
            // Its insert first, then its own processing: the limiter stays
            // last, whatever a plug-in does. What the room hears of it
            // without its microphones goes the same way (see `RoomSound`).
            if let Some(insert) = &mut bus.insert {
                insert.process(sum);
            }
            if let Some(room) = &mut bus.room {
                let late = bus.insert.as_ref().map_or(0, super::insert::Insert::late_frames);
                room.insert_delay.run_frames(&mut room.sum, late);
                bus.dynamics.process_pre_tracked(sum, &mut room.gains);
                room.apply_gains();
            } else {
                bus.dynamics.process_pre(sum);
            }
            let target = if bus.muted {
                0.0
            } else {
                db_to_gain(bus.gain_db)
            };
            ramp(sum, bus.current, target);
            if let Some(room) = &mut bus.room {
                ramp(&mut room.sum, bus.current, target);
            }
            bus.current = target;
            // The limiter and loudness meter after the fader: the bus's meter
            // shows what actually goes out.
            if let Some(room) = &mut bus.room {
                bus.dynamics.process_post_tracked(sum, &mut room.gains);
                room.lookahead.run_frames(&mut room.sum, super::dsp::LIMITER_LOOKAHEAD);
                room.apply_gains();
            } else {
                bus.dynamics.process_post(sum);
            }
            let peak = peak_of(sum);
            bus.meter = fall(bus.meter, peak);
            bus.held_peak =
                std::array::from_fn(|channel| bus.held_peak[channel].max(peak[channel]));
            if !soloing && listen.as_deref() == Some(bus.uuid.as_str()) {
                heard.copy_from_slice(sum);
            }
            bus.apply_delay(sum);
            if let Some(room) = &mut bus.room {
                room.delay.run(&mut room.sum, bus.delay_ms);
                room.mono.clear();
                room.mono
                    .extend(room.sum.iter().map(|f| f32::midpoint(f[0], f[1])));
            }
            bus.deliver(sum);
            // Its echo removal where this block took it; faded out, the room
            // is let go (for the next one, if it is changing rooms).
            bus.echo_mix = bus.echo_to;
            if bus.echo_to == 0.0 && !bus.echo_on {
                bus.echo_room = bus.echo_next.take();
                bus.echo_on = bus.echo_room.is_some();
            }
        }

        // What each room's speakers play, to the cancellers listening for
        // it: every block, silence too, so they keep time with the
        // microphones. They have just heard this block's microphones; the
        // echo of this block comes in later ones, the speakers and the
        // microphones' devices being tens of milliseconds behind the mix.
        for source in &mut self.sources {
            for path in &mut source.echo {
                match self
                    .buses
                    .iter()
                    .find(|b| b.uuid == path.room)
                    .and_then(|b| b.room.as_ref())
                {
                    Some(room) => path.canceller.render(&room.mono),
                    None => path.canceller.render(&[0.0; BLOCK_FRAMES]),
                }
            }
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
        self.run_frames(block, frames_of(delay_ms));
    }

    /// Run `block` through the line in place, held back by `frames` frames.
    fn run_frames(&mut self, block: &mut [Frame], frames: usize) {
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

/// A delay in milliseconds as frames, at most [`MAX_DELAY_MS`].
fn frames_of(delay_ms: f32) -> usize {
    let ms = if delay_ms.is_finite() {
        delay_ms.clamp(0.0, MAX_DELAY_MS)
    } else {
        0.0
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    // At most a second's frames.
    let frames = (f64::from(ms) / 1000.0 * f64::from(SAMPLE_RATE)).round() as usize;
    frames
}

/// `out` as `from` crossing to `to`, by a share gliding from `start` to
/// `end` over the block.
fn blend(out: &mut Vec<Frame>, from: &[Frame], to: &[Frame], start: f32, end: f32) {
    let n = from.len().max(1) as f32;
    out.clear();
    out.extend(from.iter().zip(to).enumerate().map(|(i, (a, b))| {
        let w = start + (end - start) * (i + 1) as f32 / n;
        [a[0] + (b[0] - a[0]) * w, a[1] + (b[1] - a[1]) * w]
    }));
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
/// The block's peak per channel.
fn peak_of(block: &[Frame]) -> Frame {
    block.iter().fold([0.0; 2], |peak, frame| {
        [peak[0].max(frame[0].abs()), peak[1].max(frame[1].abs())]
    })
}

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

    fn send(bus: &str, gain: f32, pre: bool) -> Send {
        Send {
            bus: bus.into(),
            gain,
            pre,
        }
    }

    /// Every left sample a tap has had.
    fn left_samples(tap: &BusTap) -> Vec<f32> {
        let mut out = Vec::new();
        while let Ok(chunk) = tap.receiver.try_recv() {
            out.extend(chunk.samples.iter().step_by(2));
        }
        out
    }

    /// A room for the echo tests: a call (`far`, not a microphone) played
    /// on the bus "room", and a microphone on "room", "call" (which removes
    /// the room's echo) and "stream", hearing `near` and the room's speakers
    /// 40 ms after the mix, with a tail. Mixed block by block; each bus's
    /// left side returned.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn echo_room(far: &[f32], near: &[f32], mic_gain_db: f32) -> [Vec<f32>; 3] {
        echo_room_with(far, near, mic_gain_db, |_, _| None)
    }

    /// [`echo_room`], `each` called before every block with its number and
    /// the mix: what it returns, the mixer held up that many blocks there
    /// (the room's speakers silent meanwhile, the microphone's sound of
    /// then let go, as its feed does).
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn echo_room_with(
        far: &[f32],
        near: &[f32],
        mic_gain_db: f32,
        mut each: impl FnMut(usize, &mut MixCore) -> Option<u64>,
    ) -> [Vec<f32>; 3] {
        use super::super::echo::Canceller;
        let blocks = far.len() / BLOCK_FRAMES;
        let call_fifo = Arc::new(Mutex::new(VecDeque::new()));
        let mic_fifo = Arc::new(Mutex::new(VecDeque::new()));
        let mut call = Source::new("call", Feed::Fifo(call_fifo.clone()));
        call.sends = vec!["room".into()];
        let mut mic = Source::new("mic", Feed::Fifo(mic_fifo.clone()));
        mic.mic = true;
        mic.gain_db = mic_gain_db;
        mic.sends = vec!["room".into(), "call".into(), "stream".into()];
        mic.echo = vec![EchoPath::new("room", Canceller::new().expect("canceller"))];
        let mut core = core_with(vec![call, mic], &["room", "call", "stream"]);
        core.buses[0].room = Some(RoomSound::default());
        core.buses[1].echo_room = Some("room".into());
        core.buses[1].echo_on = true;
        let taps = [
            core.tap("room").unwrap(),
            core.tap("call").unwrap(),
            core.tap("stream").unwrap(),
        ];
        // What the room's speakers played, in real time.
        let mut played: Vec<f32> = Vec::new();
        let mut out: [Vec<f32>; 3] = Default::default();
        for b in 0..blocks {
            if let Some(stalled) = each(b, &mut core) {
                core.skipped(stalled, std::time::Instant::now());
                #[allow(clippy::cast_possible_truncation)]
                played.extend(std::iter::repeat_n(0.0, stalled as usize * BLOCK_FRAMES));
            }
            let span = b * BLOCK_FRAMES..(b + 1) * BLOCK_FRAMES;
            call_fifo
                .lock()
                .unwrap()
                .extend(far[span.clone()].iter().map(|&v| [v, v]));
            // What the speakers played reaches the microphone 40 ms on.
            let now = played.len();
            let heard = |i: usize, delay: usize| {
                (now + i)
                    .checked_sub(delay)
                    .and_then(|j| played.get(j))
                    .copied()
                    .unwrap_or(0.0)
            };
            mic_fifo.lock().unwrap().extend(span.clone().enumerate().map(|(k, i)| {
                let echo = 0.33 * heard(k, 1920)
                    + 0.15 * heard(k, 2100)
                    + 0.08 * heard(k, 2600)
                    + 0.04 * heard(k, 3500);
                [near[i] + echo, near[i] + echo]
            }));
            core.process_block();
            for (tap, out) in taps.iter().zip(&mut out) {
                out.extend(left_samples(tap));
            }
            played.extend_from_slice(&out[0][span]);
        }
        out
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn power_db(x: &[f32]) -> f64 {
        let p = x.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / x.len().max(1) as f64;
        10.0 * (p + 1e-20).log10()
    }

    /// A call's bus gets the microphone with the room's speakers taken out;
    /// any other bus gets it as it is.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_calls_bus_gets_the_microphone_without_the_rooms_speakers() {
        let blocks = 1500;
        let far = super::super::echo::test_voice(1, blocks, |_| true);
        let near = vec![0.0; far.len()];
        let [room, call, stream] = echo_room(&far, &near, 0.0);
        let last = (blocks - 300) * BLOCK_FRAMES..;
        // The stream has the echo the microphone heard; the call next to
        // none of it.
        let echo = power_db(&stream[last.clone()]);
        assert!(echo > power_db(&room[last.clone()]) - 15.0, "the mic hears the room: {echo:.1} dB");
        let left = power_db(&call[last]);
        assert!(echo - left > 25.0, "only {:.1} dB taken out of the call's", echo - left);
    }

    /// Blocks `from..to` of a bus's side, as power in dB.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn blocks_db(x: &[f32], from: usize, to: usize) -> f64 {
        power_db(&x[from * BLOCK_FRAMES..to * BLOCK_FRAMES])
    }

    /// A cleaning added to a microphone already speaking, held back 170 ms
    /// to meet a camera: the call's bus keeps the voice as it is until the
    /// cleaning's own delay is full, then crosses over, never silent.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_cleaning_added_to_a_held_back_microphone_waits_for_its_delay() {
        use super::super::echo::Canceller;
        let blocks = 160;
        // A held vowel, never pausing.
        #[allow(clippy::cast_precision_loss)]
        let near: Vec<f32> = (0..blocks * BLOCK_FRAMES)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                0.1 * (1..=6)
                    .map(|h| (std::f32::consts::TAU * 150.0 * h as f32 * t).sin() / h as f32)
                    .sum::<f32>()
            })
            .collect();
        let far = vec![0.0; near.len()];
        let [_, call, stream] = echo_room_with(&far, &near, 0.0, |b, core| {
            if b == 0 {
                core.sources[1].delay_ms = 170.0;
                core.sources[1].echo.clear();
            }
            if b == 60 {
                core.sources[1]
                    .echo
                    .push(EchoPath::new("room", Canceller::new().expect("canceller")));
            }
            None
        });
        for b in 60..blocks {
            let (heard, raw) = (blocks_db(&call, b, b + 1), blocks_db(&stream, b, b + 1));
            assert!(heard > raw - 4.0, "block {b}: the call has {heard:.1} dB, the voice {raw:.1}");
        }
    }

    /// The room's bus compressing hard: what the cancellers hear of it is
    /// compressed alike, or its echo would no longer be what they listen
    /// for.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn the_rooms_compressor_is_heard_by_the_cancellers_too() {
        let blocks = 1500;
        let far = super::super::echo::test_voice(1, blocks, |_| true);
        let near = vec![0.0; far.len()];
        let [_, call, stream] = echo_room_with(&far, &near, 0.0, |b, core| {
            if b == 0 {
                let squash = super::super::dsp::CompressorConfig {
                    enabled: true,
                    threshold_db: -40.0,
                    ratio: 20.0,
                    attack_ms: 1.0,
                    release_ms: 40.0,
                    knee_db: 0.0,
                    makeup_db: 18.0,
                };
                core.buses[0].dynamics.configure(squash, super::super::dsp::LimiterConfig::default());
                // Not on the room itself here: with that much makeup the
                // room would feed back on itself.
                core.sources[1].sends.retain(|s| s.bus != "room");
            }
            None
        });
        let removed = blocks_db(&stream, blocks - 300, blocks) - blocks_db(&call, blocks - 300, blocks);
        // 55 dB; 29 when they heard the bus uncompressed.
        assert!(removed > 40.0, "only {removed:.1} dB taken out under the compressor");
    }

    /// A microphone's send to the call's bus turned off fades out cleaned;
    /// its cleaning stays, for the send coming back.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_send_turned_off_fades_out_cleaned() {
        let blocks = 1503;
        let far = super::super::echo::test_voice(1, blocks, |_| true);
        let near = vec![0.0; far.len()];
        let [_, call, stream] = echo_room_with(&far, &near, 0.0, |b, core| {
            if b == 1500 {
                core.sources[1].sends = vec!["room".into(), "stream".into()];
            }
            if b == 1502 {
                assert_eq!(core.sources[1].echo.len(), 1, "its cleaning kept");
            }
            None
        });
        let fading = blocks_db(&call, 1500, 1501);
        let echo = blocks_db(&stream, 1500, 1501);
        assert!(echo - fading > 15.0, "the fade had the echo {:.1} dB down only", echo - fading);
    }

    /// Echo removal turned off: the call's bus crosses back to the
    /// microphone as it is over a block, and the room is let go.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn echo_removal_turned_off_crosses_back_to_the_microphone() {
        let blocks = 1504;
        let far = super::super::echo::test_voice(1, blocks, |_| true);
        let near = vec![0.0; far.len()];
        let [_, call, stream] = echo_room_with(&far, &near, 0.0, |b, core| {
            if b == 1500 {
                core.buses[1].echo_on = false;
                core.sources[1].echo[0].wanted = false;
            }
            if b == 1502 {
                assert_eq!(core.buses[1].echo_room, None, "the room let go");
                assert!(core.sources[1].echo.is_empty());
            }
            None
        });
        // The fade's first frames (after the bus limiter's look-ahead,
        // what came before) are still cleaned.
        let start = 1500 * BLOCK_FRAMES + super::super::dsp::LIMITER_LOOKAHEAD;
        let fade = &call[start..start + 48];
        let raw = &stream[start..start + 48];
        assert!(power_db(fade) < power_db(raw) - 15.0, "starts cleaned: {:.1} against {:.1}", power_db(fade), power_db(raw));
        let after = &call[1502 * BLOCK_FRAMES..1504 * BLOCK_FRAMES];
        let raw = &stream[1502 * BLOCK_FRAMES..1504 * BLOCK_FRAMES];
        assert!(after.iter().zip(raw).all(|(a, b)| (a - b).abs() < 1e-5), "then as it is");
    }

    /// A cleaning not asked for any more while its send stays on (its input
    /// made a camera's sound): the call's bus crosses back to it as it is
    /// over a block, and the cleaning goes.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_cleaning_let_go_under_a_send_crosses_back() {
        let blocks = 1504;
        let far = super::super::echo::test_voice(1, blocks, |_| true);
        let near = vec![0.0; far.len()];
        let [_, call, stream] = echo_room_with(&far, &near, 0.0, |b, core| {
            if b == 1500 {
                core.sources[1].mic = false;
                core.sources[1].echo[0].wanted = false;
            }
            if b == 1501 {
                assert!(core.sources[1].echo.is_empty(), "let go");
            }
            None
        });
        let start = 1500 * BLOCK_FRAMES + super::super::dsp::LIMITER_LOOKAHEAD;
        assert!(power_db(&call[start..start + 48]) < power_db(&stream[start..start + 48]) - 15.0, "starts cleaned");
        let after = &call[1502 * BLOCK_FRAMES..1504 * BLOCK_FRAMES];
        let raw = &stream[1502 * BLOCK_FRAMES..1504 * BLOCK_FRAMES];
        assert!(after.iter().zip(raw).all(|(a, b)| (a - b).abs() < 1e-5), "then as it is");
    }

    /// The call's bus switched to another room: it crosses back to the
    /// microphone as it is, then to cleaned of the new room, with no jump.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn changing_rooms_crosses_over_without_a_jump() {
        use super::super::echo::Canceller;
        let blocks = 1510;
        // A vowel rising and falling smoothly: a jump would stand out.
        #[allow(clippy::cast_precision_loss)]
        let far: Vec<f32> = (0..blocks * BLOCK_FRAMES)
            .map(|i| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let level = 0.2 + 0.8 * (std::f32::consts::TAU * 1.5 * t).sin().powi(2);
                0.15 * level
                    * (1..=6)
                        .map(|h| (std::f32::consts::TAU * 150.0 * h as f32 * t).sin() / h as f32)
                        .sum::<f32>()
            })
            .collect();
        let near = vec![0.0; far.len()];
        let [_, call, stream] = echo_room_with(&far, &near, 0.0, |b, core| {
            if b == 1500 {
                // As `apply` does, the room changing.
                core.buses[2].room = Some(RoomSound::default());
                core.buses[1].echo_on = false;
                core.buses[1].echo_next = Some("stream".into());
                core.sources[1].echo[0].wanted = false;
                core.sources[1]
                    .echo
                    .push(EchoPath::new("stream", Canceller::new().expect("canceller")));
            }
            if b == 1505 {
                assert_eq!(core.buses[1].echo_room.as_deref(), Some("stream"));
                let rooms: Vec<&str> = core.sources[1].echo.iter().map(|p| p.room.as_str()).collect();
                assert_eq!(rooms, ["stream"], "the old room's cleaning let go");
            }
            None
        });
        let step = |x: &[f32]| x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
        let span = 1499 * BLOCK_FRAMES..1505 * BLOCK_FRAMES;
        let (jump, usual) = (step(&call[span.clone()]), step(&stream[span]));
        let cleaned = blocks_db(&stream, 1400, 1500) - blocks_db(&call, 1400, 1500);
        assert!(cleaned > 20.0, "learnt first: {cleaned:.1} dB");
        // Crossing between two sounds steps a little more than either (1.8
        // times here); a switch at once jumped 7.8 times.
        assert!(jump < 3.0 * usual, "a step of {jump} where the sound itself moves {usual}");
    }

    /// The mixer held up: the cancellers keep time with the room (silence
    /// played while it was held) and what they learnt, so no echo comes
    /// through after it.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_held_up_mixer_keeps_the_cancellers_in_time() {
        let blocks = 1700;
        let far = super::super::echo::test_voice(1, blocks, |_| true);
        let near = vec![0.0; far.len()];
        let [_, call, stream] =
            echo_room_with(&far, &near, 0.0, |b, _| (b == 1500).then_some(12));
        let echo = blocks_db(&stream, 1400, 1500);
        let before = blocks_db(&call, 1400, 1500);
        let after = blocks_db(&call, 1500, 1530);
        assert!(echo - before > 25.0, "learnt: {:.1} dB", echo - before);
        assert!(echo - after > 25.0, "after the hold-up only {:.1} dB down", echo - after);
    }

    /// A microphone played on the room's speakers too is not an echo of
    /// itself: its voice reaches the call's bus, after its fader as the bus
    /// takes it.
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[test]
    fn a_microphone_on_the_rooms_speakers_keeps_its_own_voice_for_the_call() {
        let blocks = 1500;
        // A vowel: 120 Hz and its harmonics, as a voice holds one — the
        // same 10 ms on as before, so a canceller hearing the microphone
        // itself in what the room plays would learn to take it out.
        #[allow(clippy::cast_precision_loss)]
        let near: Vec<f32> = super::super::echo::test_voice(3, blocks, |_| true)
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let t = i as f32 / SAMPLE_RATE as f32;
                let tone: f32 = (1..=12)
                    .map(|h| (std::f32::consts::TAU * 120.0 * h as f32 * t).sin() / h as f32)
                    .sum();
                if *v == 0.0 { 0.0 } else { 0.15 * tone }
            })
            .collect();
        let far = vec![0.0; near.len()];
        let [_, call, stream] = echo_room(&far, &near, -6.0);
        let last = (blocks - 300) * BLOCK_FRAMES..;
        let raw = power_db(&near[last.clone()]);
        // The stream: the voice and its own echo, 6 dB down.
        let heard = power_db(&call[last.clone()]) - raw;
        assert!((heard + 6.0).abs() < 2.0, "the call has the voice {heard:+.1} dB");
        let stream = power_db(&stream[last]) - raw;
        assert!(stream > -7.0, "the stream has it {stream:+.1} dB");
    }

    /// A send carries its level; one before the fader ignores the fader
    /// (riding it for the room leaves the stage's mix as it is) but not the
    /// source's On.
    #[test]
    fn a_send_carries_its_level_before_or_after_the_fader() {
        let (mut mic, _) = fifo_source("mic", 0.5, 20);
        mic.gain_db = -20.0;
        mic.sends = vec![send("room", 0.5, false), send("wedge", 0.5, true)];
        let mut core = core_with(vec![mic], &["room", "wedge"]);
        let room = core.tap("room").unwrap();
        let wedge = core.tap("wedge").unwrap();
        for _ in 0..4 {
            core.process_block();
        }
        let fader = db_to_gain(-20.0);
        assert!(
            (last_frame(&room)[0] - 0.25 * fader).abs() < 1e-5,
            "after: level × fader"
        );
        assert!(
            (last_frame(&wedge)[0] - 0.25).abs() < 1e-5,
            "before: level alone"
        );
        core.sources[0].muted = true;
        core.process_block();
        core.process_block();
        assert_eq!(
            last_frame(&wedge),
            [0.0, 0.0],
            "off is off, before the fader too"
        );
    }

    /// A send switched between before and after the fader, its level
    /// changed, or turned off, while sound plays: each glides over a block —
    /// no step — and a send turned off is forgotten once silent.
    #[test]
    fn send_changes_glide_and_an_off_send_is_forgotten() {
        let (mut mic, _) = fifo_source("mic", 0.5, 40);
        mic.gain_db = -20.0;
        mic.sends = vec![send("wedge", 1.0, false)];
        let mut core = core_with(vec![mic], &["wedge"]);
        let wedge = core.tap("wedge").unwrap();
        for block in 0..20 {
            match block {
                4 => core.sources[0].sends = vec![send("wedge", 1.0, true)],
                8 => core.sources[0].sends = vec![send("wedge", 0.2, true)],
                12 => core.sources[0].sends.clear(),
                _ => {}
            }
            core.process_block();
        }
        let samples = left_samples(&wedge);
        let step = samples
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        // Over a block, 0.45 moves 0.001 a sample; at once, all of it.
        assert!(step < 0.002, "a step of {step}");
        assert!(
            (samples[8 * BLOCK_FRAMES - 1] - 0.5).abs() < 1e-5,
            "before the fader: all of it"
        );
        assert_eq!(*samples.last().unwrap(), 0.0, "off");
        assert!(core.sources[0].send_env.is_empty(), "forgotten once silent");
    }

    #[test]
    fn source_processing_is_once_before_sends_and_fader_and_heard_on_pfl() {
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
            .configure(&super::super::dynamics::SourceSettings {
                compressor: config,
                ..Default::default()
            });
        let mut core = core_with(vec![source], &["main", "aux"]);
        let main = core.tap("main").unwrap();
        let aux = core.tap("aux").unwrap();
        let phones = core.tap_monitor();
        for _ in 0..8 {
            core.process_block();
        }
        let expected = 0.176_776_7 * db_to_gain(-6.0);
        assert!((last_frame(&main)[0] - expected).abs() < 1e-4);
        assert!((last_frame(&aux)[0] - expected).abs() < 1e-4);
        // Pre-fader listen hears the processed sound, before the fader.
        assert!((last_frame(&phones)[0] - 0.176_776_7).abs() < 1e-4);
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
            .configure(config, super::super::LimiterConfig::default());
        bus.gain_db = -6.0;
        bus.delay_ms = 100.0;
        let main = core.tap("main").unwrap();
        let phones = core.tap_monitor();
        for _ in 0..8 {
            core.process_block();
        }
        let expected = 0.176_776_7 * db_to_gain(-6.0);
        assert!((last_frame(&phones)[0] - expected).abs() < 1e-4);
        assert_eq!(last_frame(&main), [0.0, 0.0]);
        for _ in 0..8 {
            core.process_block();
        }
        assert!((last_frame(&main)[0] - expected).abs() < 1e-4);
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
        // The source's 100 ms, and the bus limiter's look-ahead.
        let lookahead_ms =
            super::super::dsp::LIMITER_LOOKAHEAD as f64 * 1000.0 / f64::from(SAMPLE_RATE);
        assert!(
            (out - heard - 100.0 - lookahead_ms).abs() < 0.1,
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

    /// A live bus's device gets from the mix every source sent to the bus
    /// but the mics its own callback mixes, before the bus's fader (the
    /// callback's); the bus as mixed — its recordings — keeps them all.
    #[test]
    fn a_live_bus_sends_its_device_all_but_its_live_mics() {
        let (mut mic, _) = fifo_source("mic", 0.25, 4);
        let (mut clip, _) = fifo_source("clip", 0.5, 4);
        mic.sends = vec!["pa".into()];
        clip.sends = vec!["pa".into()];
        let mut core = core_with(vec![mic, clip], &["pa", "stream"]);
        let (tx, rx) = crossbeam_channel::bounded(4 * BLOCK_FRAMES);
        core.buses[0].live = Some(LiveSend {
            mics: vec!["mic".into()],
            tx,
        });
        core.buses[0].gain_db = -6.0;
        let tap = core.tap("pa").unwrap();
        core.process_block();
        core.process_block();
        let sent: Vec<Frame> = rx.try_iter().collect();
        assert_eq!(sent.len(), 2 * BLOCK_FRAMES, "every frame");
        let last = sent[sent.len() - 1];
        assert!(
            (last[0] - 0.5).abs() < 1e-6 && (last[1] + 0.5).abs() < 1e-6,
            "the clip alone: {last:?}"
        );
        let mixed = last_frame(&tap);
        let fader = db_to_gain(-6.0);
        assert!(
            (mixed[0] - 0.75 * fader).abs() < 1e-5,
            "both, faded: {mixed:?}"
        );
    }

    /// A bus's insert runs first in its processing, before its fader, and
    /// the bus plays two blocks (20 ms) later.
    #[test]
    fn a_bus_insert_runs_first_two_blocks_late() {
        struct Double;
        impl super::super::insert::Effect for Double {
            fn process(&mut self, block: &mut [Frame]) -> Result<(), i32> {
                for f in block.iter_mut() {
                    *f = [f[0] * 2.0, f[1] * 2.0];
                }
                Ok(())
            }
        }
        let (mut src, _) = fifo_source("mic", 0.25, 12);
        src.sends = vec!["pa".into()];
        let mut core = core_with(vec![src], &["pa"]);
        let insert = super::super::insert::Insert::new("pa", Box::new(|| Ok(Box::new(Double))));
        let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while insert.status.state() != super::super::insert::InsertState::Playing
            && std::time::Instant::now() < end
        {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        core.buses[0].insert = Some(insert);
        core.buses[0].gain_db = -6.0;
        let tap = core.tap("pa").unwrap();
        let mut lasts = Vec::new();
        for _ in 0..6 {
            core.process_block();
            std::thread::sleep(std::time::Duration::from_millis(3));
            lasts.push(last_frame(&tap));
        }
        assert_eq!(
            &lasts[..2],
            &[[0.0, 0.0], [0.0, 0.0]],
            "two blocks of silence first"
        );
        let want = 0.25 * 2.0 * db_to_gain(-6.0);
        assert!(
            (lasts[5][0] - want).abs() < 1e-4,
            "doubled, then faded: {:?}",
            lasts[5]
        );
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

    /// What a source plays is measured before its fader, mute and gate: a
    /// muted one, or one its gate holds down, still shows it.
    #[test]
    fn a_muted_or_gated_source_still_shows_what_it_plays() {
        let (mut src, _fifo) = fifo_source("mic", 0.8, 4);
        src.sends = vec!["m".into()];
        src.muted = true;
        let mut core = core_with(vec![src], &["m"]);
        core.process_block();
        core.process_block();
        let mic = core.source("mic").unwrap();
        assert!(mic.meter()[0] < 1e-3, "nothing heard");
        assert!(
            (mic.signal()[0] - 0.8).abs() < 1e-3,
            "what it plays: {:?}",
            mic.signal()
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
        // The source's own peak, over full scale, is held; the bus's limiter
        // kept what went out at its −1 dBFS ceiling.
        assert_eq!(core.source("mic").unwrap().held_peak, [1.25; 2]);
        let ceiling = db_to_gain(-1.0);
        let bus_peak = core.bus("m").unwrap().held_peak;
        assert!(
            bus_peak.iter().all(|p| (p - ceiling).abs() < 1e-3),
            "{bus_peak:?}"
        );
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
        // Under the bus limiter's ceiling, so only the timing is in question.
        let (mut src, _) = fifo_source("clip", 0.5, 8);
        src.sends = vec!["m".into()];
        let mut core = core_with(vec![src], &["m"]);
        core.bus_mut("m").unwrap().delay_ms = 100.0;
        let phones = core.tap_monitor();
        core.process_block();
        core.process_block();
        assert!(
            (last_frame(&phones)[0] - 0.5).abs() < 1e-6,
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
        // The first look learns nothing of its blocks from a backlog: more
        // chunks, a cycle of them watched (see `reader::Landing`), and it
        // plays.
        feed.pull(&mut out);
        for _ in 0..30 {
            tx.send(PcmChunk {
                samples: chunk.clone(),
                lost_before: 0,
            })
            .unwrap();
            out = [[0.0; 2]; BLOCK_FRAMES];
            feed.pull(&mut out);
            if out[BLOCK_FRAMES - 1] != [0.0; 2] {
                break;
            }
        }
        assert!(
            out.iter()
                .skip(4)
                .all(|f| (f[0] - 0.3).abs() < 1e-4 && (f[1] - 0.3).abs() < 1e-4),
            "channel 2 on both sides: {:?}",
            &out[..8]
        );
    }
}
