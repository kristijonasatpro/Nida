//! Audio input and analysis for audio-reactive shaders

pub mod app_sound;
pub mod device_watch;
#[cfg(target_os = "macos")]
pub mod latency;

use anyhow::{Context, Result};
use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::Sample;
use crossbeam_channel::{bounded, Receiver, Sender};
use rustfft::{num_complex::Complex, FftPlanner};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Opaque audio source identifier.
pub type AudioSourceId = u32;

/// Opaque token identifying a single PCM passthrough subscription.
pub type PcmToken = u64;

/// Bounded capacity (in chunks) of a passthrough PCM channel. One chunk is
/// produced per cpal callback (~10ms), so 300 ≈ 3 s of slack before drops:
/// a recording's ffmpeg reads the sound only once the picture's first frame
/// has come, and a first take can wait more than half a second for it.
const PCM_CHANNEL_CAPACITY: usize = 300;

/// Monotonic source of unique [`PcmToken`]s.
static NEXT_PCM_TOKEN: AtomicU64 = AtomicU64::new(1);

/// Native PCM layout of a capture device, reported to passthrough subscribers
/// so they can build matching ffmpeg input args.
#[derive(Debug, Clone, Copy)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A chunk of raw interleaved PCM (native channel count, native sample rate),
/// tee'd from the cpal capture callback for output passthrough. Distinct from
/// [`AudioData`], which is mono-downmixed and FFT-processed for analysis.
pub struct PcmChunk {
    /// Interleaved `f32` samples in the device's native channel count.
    pub samples: Vec<f32>,
    /// Samples lost just before this chunk — dropped on a full channel, or
    /// never made — which a consumer timing sound by its sample count puts
    /// back as silence, here and not elsewhere: put before chunks that came
    /// earlier, as a total lost once was, it pushed them late.
    pub lost_before: u64,
}

/// A registered passthrough consumer. The cpal callback fans raw PCM out to
/// every subscriber on a source (a "tee").
#[derive(Clone)]
struct PcmSubscriber {
    token: PcmToken,
    sender: Sender<PcmChunk>,
    dropped: Arc<AtomicU64>,
    lost_samples: Arc<AtomicU64>,
    /// Samples lost since the last chunk that got through: told with the next.
    pending_lost: Arc<AtomicU64>,
    /// A live bus's feed (see [`AudioManager::subscribe_live`]): two
    /// channels' frames, one by one, instead of chunks.
    live: Option<LiveFrames>,
}

/// The most mics a live bus takes from one capture.
pub const LIVE_MICS: usize = 8;

/// One frame of a live bus's mics: each mic's left and right, in turn.
pub type LiveFrame = [f32; 2 * LIVE_MICS];

/// How many frames a live feed's capture has promised — each callback's
/// whole chunk, counted before it pushes the first of them — and how many
/// are gone (taken, or let go when full). Its frames are pushed one by one,
/// so a reader looking in mid-chunk finds it short; these say what will be
/// in hand once the chunk is in, wherever the reader looked in.
#[derive(Debug, Default)]
pub struct LiveCounts {
    pub promised: AtomicU64,
    pub gone: AtomicU64,
    /// The capture's last chunk, frames: the least a reader must hold to
    /// last from one of its callbacks to the next.
    pub chunk: AtomicU64,
}

impl LiveCounts {
    /// The frames in the queue once the chunk being pushed is in, for a
    /// reader that has just seen `waiting` in it — never more than there
    /// will be — and the capture's chunk as of then. Promised is read
    /// before gone, so frames the capture lets go in between count against
    /// it and not twice; everything seen waiting was promised before, and
    /// its chunk stored before that. None promised yet (counts not kept):
    /// `waiting`.
    pub fn coming(&self, waiting: usize) -> (usize, usize) {
        let promised = self.promised.load(Ordering::Acquire);
        let chunk = usize::try_from(self.chunk.load(Ordering::Relaxed)).unwrap_or(usize::MAX);
        if promised == 0 {
            return (waiting, chunk);
        }
        let gone = self.gone.load(Ordering::Acquire);
        let coming = usize::try_from(promised.saturating_sub(gone)).unwrap_or(usize::MAX);
        (coming, chunk)
    }
}

/// The frames of a live bus's mics on one capture — every mic in each
/// frame, so the mics stay in step — taken in the capture's own callback:
/// no chunk made, nothing waited on; a full queue lets its oldest frame go
/// (see [`AudioManager::subscribe_live`]).
#[derive(Clone)]
struct LiveFrames {
    /// Each mic's (left, right) channels.
    pairs: Vec<(usize, usize)>,
    channels: usize,
    tx: Sender<LiveFrame>,
    /// The queue's own end, to let its oldest frame go when it is full.
    rx: Receiver<LiveFrame>,
    counts: Arc<LiveCounts>,
}

impl LiveFrames {
    fn feed(&self, samples: &[f32]) {
        let channels = self.channels.max(1);
        let frames = (samples.len() / channels) as u64;
        self.counts.chunk.store(frames, Ordering::Relaxed);
        self.counts.promised.fetch_add(frames, Ordering::Release);
        for frame in samples.chunks_exact(channels) {
            let mut out: LiveFrame = [0.0; 2 * LIVE_MICS];
            for (i, &(l, r)) in self.pairs.iter().take(LIVE_MICS).enumerate() {
                out[2 * i] = frame.get(l).copied().unwrap_or(0.0);
                out[2 * i + 1] = frame.get(r).copied().unwrap_or(0.0);
            }
            if let Err(crossbeam_channel::TrySendError::Full(f)) = self.tx.try_send(out) {
                // Counted before it goes (and uncounted if the reader took it
                // first): a reader looking in between never finds more coming
                // than will.
                self.counts.gone.fetch_add(1, Ordering::Release);
                if self.rx.try_recv().is_err() {
                    self.counts.gone.fetch_sub(1, Ordering::Release);
                }
                let _ = self.tx.try_send(f);
            }
        }
    }
}

/// A live bus's feed from a capture: its mics' frames as they come (see
/// [`AudioManager::subscribe_live`]).
pub struct LiveTap {
    pub frames: Receiver<LiveFrame>,
    pub counts: Arc<LiveCounts>,
    pub token: PcmToken,
    pub rate: u32,
}

/// Frames a live feed holds before letting its oldest go: 341 ms at 48 kHz,
/// far more than a live bus keeps waiting — a capture's chunk of up to 4096
/// frames held twice over, and room to spare (see `LiveMix::mix`).
pub const LIVE_FRAMES_HELD: usize = 16_384;

/// Handle returned by [`AudioManager::subscribe_pcm`]. The receiver yields raw
/// PCM; `format` describes its layout; `token` identifies the subscription for
/// [`AudioManager::unsubscribe_pcm`]; `dropped` counts backpressure drops.
pub struct PcmSubscription {
    pub receiver: Receiver<PcmChunk>,
    pub format: AudioFormat,
    pub token: PcmToken,
    pub dropped: Arc<AtomicU64>,
    /// Total samples lost to those drops. The consumer owes the stream this
    /// much silence; see `AudioPipe::start` in the ffmpeg subprocess.
    pub lost_samples: Arc<AtomicU64>,
}

/// Fan a chunk of raw interleaved PCM out to every passthrough subscriber.
///
/// Never blocks — this runs on the real-time capture callback, where waiting on
/// a consumer would stall the device and corrupt every listener. So on a full
/// channel the chunk is dropped and counted.
///
/// The *duration* of the drop is counted too, and that is the part that matters
/// downstream. Video can drop a frame and lose nothing but a moment of motion,
/// because each surviving frame still says when it belongs. A PCM stream has no
/// such labels: it is timed purely by how many samples have gone past, so
/// silently dropping some does not leave a hole, it drags everything after it
/// earlier. Consumers replace `lost_samples` with silence to keep the count
/// honest. Disconnected subscribers (the output stopped without unsubscribing)
/// are silently skipped.
fn fan_out_pcm(subs: &[PcmSubscriber], samples: &[f32]) {
    for sub in subs {
        // A live bus's feed is fed callback by callback (see `Gathered::tee`).
        if sub.live.is_some() {
            continue;
        }
        let lost_before = sub.pending_lost.swap(0, Ordering::Relaxed);
        match sub.sender.try_send(PcmChunk {
            samples: samples.to_vec(),
            lost_before,
        }) {
            Ok(()) | Err(crossbeam_channel::TrySendError::Disconnected(_)) => {}
            Err(crossbeam_channel::TrySendError::Full(chunk)) => {
                sub.dropped.fetch_add(1, Ordering::Relaxed);
                sub.lost_samples
                    .fetch_add(samples.len() as u64, Ordering::Relaxed);
                sub.pending_lost.fetch_add(
                    chunk.lost_before + chunk.samples.len() as u64,
                    Ordering::Relaxed,
                );
            }
        }
    }
}

/// A capture's sound on its way to its subscribers: a live bus's feed at
/// once, every callback; the chunk subscribers (the mixer's inputs,
/// recordings) in chunks of `chunk` samples or more, gathered here. A live
/// bus sets its device to 64 frames (1.3 ms) a callback, for every stream
/// on it: a chunk each would be eight times the chunks — a channel's
/// [`PCM_CHANNEL_CAPACITY`] then held 0.4 s, not the 3 s a first take may
/// need — and an allocation per subscriber every 1.3 ms in the device's
/// callback.
struct Gathered {
    samples: Vec<f32>,
    /// Samples to a chunk, at least: whole frames.
    chunk: usize,
    /// The most held at once, whole frames: `samples` is made this big
    /// beforehand, and a callback bigger than it goes in pieces, so the
    /// callback never allocates.
    most: usize,
    /// Its stream's subscribers, for what is left when the stream goes.
    subs: Arc<ArcSwap<Vec<PcmSubscriber>>>,
}

impl Gathered {
    /// Room for a chunk and `frames` more of `channels` channels.
    fn new(
        chunk: usize,
        frames: usize,
        channels: usize,
        subs: Arc<ArcSwap<Vec<PcmSubscriber>>>,
    ) -> Self {
        let most = chunk + frames * channels.max(1);
        Self {
            samples: Vec::with_capacity(most),
            chunk,
            most,
            subs,
        }
    }

    /// A callback's `samples` (whole frames) to `subs`.
    fn tee(&mut self, subs: &[PcmSubscriber], mut samples: impl Iterator<Item = f32>) {
        loop {
            let start = self.samples.len();
            let room = self.most.saturating_sub(start);
            self.samples.extend(samples.by_ref().take(room));
            if self.samples.len() == start {
                return;
            }
            for sub in subs {
                if let Some(live) = &sub.live {
                    live.feed(&self.samples[start..]);
                }
            }
            if self.samples.len() >= self.chunk {
                fan_out_pcm(subs, &self.samples);
                self.samples.clear();
            }
        }
    }

    /// Nothing fed: what was gathered let go with it.
    fn clear(&mut self) {
        self.samples.clear();
    }
}

impl Drop for Gathered {
    /// The stream gone (closed, or reopened: its subscribers kept for the
    /// new one), what it gathered still goes to them. Dropped, up to 10 ms
    /// went missing uncounted, and a recording's sound ran that much ahead
    /// of its picture from then on. cpal frees the callback, and this with
    /// it, as its stream is dropped, before a reopened stream feeds — unless
    /// a device's disconnect handler holds the stream at that moment: then
    /// this comes a few milliseconds late, after the new stream's first
    /// chunk. Counted still, so nothing drifts; not worth a handshake.
    fn drop(&mut self) {
        if !self.samples.is_empty() {
            fan_out_pcm(&self.subs.load(), &self.samples);
        }
    }
}

/// How long a chunk of a capture's sound is gathered to at least (see
/// [`Gathered`]): 10 ms, as most devices call back anyway.
const PCM_CHUNK_SECS: f32 = 0.01;

/// Audio buffer size (samples per channel) — 256 @ 48kHz ≈ 5.3ms latency
pub const AUDIO_BUFFER_SIZE: usize = 256;
/// FFT size — 2048 @ 48kHz = 23Hz/bin resolution for clean bass separation
pub const FFT_SIZE: usize = 2048;
/// Hop size for overlapping analysis frames
const FFT_HOP: usize = AUDIO_BUFFER_SIZE;
/// Number of beat intervals to keep for BPM calculation
const BPM_HISTORY_SIZE: usize = 16;
/// Minimum time between beats (in seconds) to avoid double-triggering
const MIN_BEAT_INTERVAL: f32 = 0.2; // Max ~300 BPM
/// Maximum time between beats before we reset BPM tracking
const MAX_BEAT_INTERVAL: f32 = 2.0; // Min ~30 BPM
/// Window size for adaptive onset threshold (median of recent spectral flux)
const ONSET_MEDIAN_WINDOW: usize = 8;
/// Spectral flux must exceed median * this multiplier + offset to trigger onset
const ONSET_THRESHOLD_MULTIPLIER: f32 = 1.5;
/// Minimum spectral flux to trigger onset (prevents triggers in silence)
const ONSET_THRESHOLD_OFFSET: f32 = 0.01;
/// Reject beat intervals that deviate >15% from median
const TEMPO_TOLERANCE: f32 = 0.15;

/// Information about a detected audio input device.
/// What `list` returns, if it does within `wait` — run on a thread of its own
/// — or else the receiver it will arrive on.
fn within<T: Send + 'static>(
    wait: std::time::Duration,
    list: impl FnOnce() -> T + Send + 'static,
) -> Result<T, std::sync::mpsc::Receiver<T>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("audio-input-listing".into())
        .spawn(move || {
            let _ = tx.send(list());
        });
    if let Err(e) = spawned {
        log::warn!("Listing sound inputs on a thread of its own: {e}");
        return Err(rx);
    }
    rx.recv_timeout(wait).map_err(|_| rx)
}

/// A listing of the input devices still being made (see
/// [`AudioManager::new_waiting_at_most`]).
pub type InputListing = std::sync::mpsc::Receiver<(Vec<AudioDeviceInfo>, Option<AudioSourceId>)>;

#[derive(Debug, Clone)]
pub struct AudioDeviceInfo {
    pub id: AudioSourceId,
    pub name: String,
}

/// How long a device that would not open is left before it is looked for
/// again by a modulator, which asks every frame.
const OPEN_RETRY: std::time::Duration = std::time::Duration::from_secs(2);

/// An input device's name as listed: its description's, or a stand-in by
/// its place when it has none.
fn input_name(i: usize, device: &cpal::Device) -> String {
    device.description().map_or_else(
        |_| format!("Audio Input {i}"),
        |desc| desc.name().to_string(),
    )
}

/// Where `device` was, listed under a stand-in name, its description
/// unreadable then — "Audio Input N", N its place, which its number need
/// not be (see [`number_devices`]): it is known only by its place. `None`
/// for a device listed by its name.
fn stand_in_place(device: &AudioDeviceInfo) -> Option<usize> {
    device.name.strip_prefix("Audio Input ")?.parse().ok()
}

/// Where the listed input `id` is in `now`, the inputs' names as the OS
/// lists them at this moment: the device of its name — of two with one
/// name, the one in the same place among them — or, listed under a
/// stand-in name, the one in its place. `None` when it is gone.
///
/// By name, because the OS's order changes when a device is plugged in: a
/// USB capture box puts its sound first, and by place an input listed
/// before it would open that instead of its own device.
fn now_at(listed: &[AudioDeviceInfo], id: AudioSourceId, now: &[String]) -> Option<usize> {
    let device = listed.iter().find(|d| d.id == id)?;
    if let Some(place) = stand_in_place(device) {
        return Some(place).filter(|&at| at < now.len());
    }
    let twin = listed
        .iter()
        .filter(|d| d.name == device.name && d.id < id)
        .count();
    now.iter()
        .enumerate()
        .filter(|(_, name)| **name == device.name)
        .nth(twin)
        .map(|(at, _)| at)
}

/// A listing of the inputs, `new` with its `default` (both numbered by
/// place), numbered for as long as Nida runs: a device — its name, its place
/// among those with it — keeps the number it was first given (`numbers`);
/// one never listed before takes the next (`next`), never one another had.
/// Numbered by place, a device plugged in ahead moved the others, and the
/// captures open, by number, were another's; a number freed by a device
/// unplugged and given to the next plugged in pointed what referred to the
/// first at it. The first listing is numbered by place, as before.
fn number_devices(
    numbers: &mut HashMap<(String, usize), AudioSourceId>,
    next: &mut AudioSourceId,
    new: Vec<AudioDeviceInfo>,
    default: Option<AudioSourceId>,
) -> (Vec<AudioDeviceInfo>, Option<AudioSourceId>) {
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut default_now = None;
    let numbered = new
        .into_iter()
        .map(|device| {
            let twin = seen.entry(device.name.clone()).or_default();
            let key = (device.name.clone(), *twin);
            *twin += 1;
            let id = *numbers.entry(key).or_insert_with(|| {
                let id = *next;
                *next += 1;
                id
            });
            if default == Some(device.id) {
                default_now = Some(id);
            }
            AudioDeviceInfo { id, ..device }
        })
        .collect();
    (numbered, default_now)
}

/// The samples a stream at `rate` with `channels` missed in `gap`: whole
/// frames of every channel — a part frame put the channels after it out of
/// step, left in right's place.
fn missed_samples(gap: Duration, rate: u32, channels: u16) -> u64 {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = (gap.as_secs_f64() * f64::from(rate)).round() as u64;
    frames * u64::from(channels)
}

/// Whether opening `id` failed within [`OPEN_RETRY`] before `now`.
fn tried_lately(failed: &HashMap<AudioSourceId, Instant>, id: AudioSourceId, now: Instant) -> bool {
    failed
        .get(&id)
        .is_some_and(|t| now.saturating_duration_since(*t) < OPEN_RETRY)
}

/// Audio analysis data sent to the rendering thread
#[derive(Clone)]
pub struct AudioData {
    /// Raw waveform data (normalized -1.0 to 1.0)
    pub waveform: Vec<f32>,
    /// FFT magnitude spectrum (0.0 to 1.0, normalized)
    pub fft: Vec<f32>,
    /// Current RMS level (0.0 to 1.0)
    pub level: f32,
    /// Detected BPM (if available)
    pub bpm: Option<f32>,
    /// Time since last beat (in seconds)
    pub time_since_beat: f32,
    /// Sample rate of the audio stream (needed to convert FFT bins to Hz)
    pub sample_rate: f32,
}

impl Default for AudioData {
    fn default() -> Self {
        Self {
            waveform: vec![0.0; AUDIO_BUFFER_SIZE],
            fft: vec![0.0; FFT_SIZE / 2],
            level: 0.0,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        }
    }
}

impl AudioData {
    /// Hz width of each FFT bin (derived from actual FFT data length)
    fn bin_width(&self) -> f32 {
        let fft_size = self.fft.len() * 2;
        self.sample_rate / fft_size as f32
    }

    /// Get energy in an arbitrary frequency range (Hz).
    /// This is the core method — bass/mid/treble are just presets on top of it.
    pub fn energy_in_range(&self, freq_low: f32, freq_high: f32) -> f32 {
        if self.fft.is_empty() {
            return 0.0;
        }
        let bw = self.bin_width();
        if bw <= 0.0 {
            return 0.0;
        }
        let bin_low = ((freq_low / bw).floor() as usize).min(self.fft.len() - 1);
        let bin_high = ((freq_high / bw).ceil() as usize).min(self.fft.len());
        if bin_high <= bin_low {
            return 0.0;
        }
        let slice = &self.fft[bin_low..bin_high];
        // RMS energy then dB-based perceptual mapping
        let rms = (slice.iter().map(|v| v * v).sum::<f32>() / slice.len() as f32).sqrt();
        if rms < 1e-6 {
            return 0.0;
        }
        let db = 20.0 * rms.log10();
        ((db + 60.0) / 60.0).clamp(0.0, 1.0)
    }

    /// Get bass level (low frequencies, ~20-250Hz)
    pub fn bass(&self) -> f32 {
        self.energy_in_range(20.0, 250.0)
    }

    /// Get mid level (mid frequencies, ~250-2000Hz)
    pub fn mid(&self) -> f32 {
        self.energy_in_range(250.0, 2000.0)
    }

    /// Get treble level (high frequencies, ~2000Hz+)
    pub fn treble(&self) -> f32 {
        self.energy_in_range(2000.0, 20000.0)
    }

    /// Get beat phase (0.0 to 1.0, where 0.0 is on the beat)
    pub fn beat_phase(&self) -> f32 {
        if let Some(bpm) = self.bpm {
            let beat_duration = 60.0 / bpm;
            (self.time_since_beat / beat_duration).fract()
        } else {
            0.0
        }
    }
}

/// Compute the adaptive onset threshold from a window of spectral flux values.
/// Returns `median(flux_history) * ONSET_THRESHOLD_MULTIPLIER + ONSET_THRESHOLD_OFFSET`.
///
/// Uses `select_nth_unstable_by` (quickselect, O(n)) instead of a full sort
/// to find the median without allocating a new Vec.
///
/// # Panics
///
/// Panics if `flux_history` contains a NaN, since the median selection compares
/// samples with `partial_cmp`. Spectral flux is computed from finite magnitudes,
/// so this cannot happen for well-formed input.
pub fn compute_onset_threshold(flux_history: &[f32]) -> f32 {
    if flux_history.is_empty() {
        return ONSET_THRESHOLD_OFFSET;
    }
    let mut buf = flux_history.to_vec();
    let mid = buf.len() / 2;
    buf.select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).unwrap());
    buf[mid] * ONSET_THRESHOLD_MULTIPLIER + ONSET_THRESHOLD_OFFSET
}

/// Estimate BPM from a history of beat intervals using median-based outlier rejection.
/// Returns `None` if fewer than 4 intervals, or if the resulting BPM is outside 30-300.
///
/// Uses `select_nth_unstable_by` (quickselect, O(n)) for the median, then
/// filters outliers using the original (unsorted) slice to avoid a second allocation.
///
/// # Panics
///
/// Panics if `beat_intervals` contains a NaN, since the median selection compares
/// intervals with `partial_cmp`. Intervals are derived from monotonic timestamps,
/// so this cannot happen for well-formed input.
pub fn estimate_bpm(beat_intervals: &[f32]) -> Option<f32> {
    if beat_intervals.len() < 4 {
        return None;
    }
    let mut buf = beat_intervals.to_vec();
    let mid = buf.len() / 2;
    buf.select_nth_unstable_by(mid, |a, b| a.partial_cmp(b).unwrap());
    let median = buf[mid];
    // Filter outliers and compute average in a single pass — no intermediate Vec.
    let (sum, count) = beat_intervals.iter().fold((0.0f32, 0u32), |(s, c), &iv| {
        if (iv - median).abs() / median < TEMPO_TOLERANCE {
            (s + iv, c + 1)
        } else {
            (s, c)
        }
    });
    if count >= 2 {
        let bpm = 60.0 / (sum / count as f32);
        if (30.0..=300.0).contains(&bpm) {
            return Some(bpm);
        }
    }
    None
}

/// How long a capture that has called back may go without a callback
/// before it is taken for lost: an unplugged device's stream stops calling,
/// said or not.
const LOST_AFTER: Duration = Duration::from_secs(2);

/// How long a capture may send only exact zeros, the devices having
/// changed, before it is opened again: a microphone or a line with anything
/// on it never sends exact zeros.
const SILENT_AFTER: Duration = Duration::from_secs(10);

/// How long after the devices change a capture sending only zeros is
/// opened again (see [`AudioManager::reopen_silent`]): only then — a device
/// made anew by its driver is a change, and opening on the show's thread
/// every so often for an input that is merely quiet (HDMI with no sound
/// playing) would hitch the picture.
const SILENT_WINDOW: Duration = Duration::from_secs(60);

/// How long a capture may take to call back at all: a Bluetooth microphone
/// can take seconds to start, and taken for lost sooner it would be opened
/// again and again, never heard.
const START_GRACE: Duration = Duration::from_secs(10);

/// Whether a capture's stream still runs: macOS said its device went (the
/// stream's error), or it has stopped calling back (see [`LOST_AFTER`],
/// [`START_GRACE`]).
struct Liveness {
    gone: AtomicBool,
    since: Instant,
    /// When it last called back, in milliseconds from `since`, plus one: 0
    /// while it never has.
    heard_ms: AtomicU64,
    /// When it last sent a sample that was not exactly zero, the same way.
    voiced_ms: AtomicU64,
}

impl Liveness {
    fn new() -> Self {
        Self {
            gone: AtomicBool::new(false),
            since: Instant::now(),
            heard_ms: AtomicU64::new(0),
            voiced_ms: AtomicU64::new(0),
        }
    }

    /// It sent a sample that was not exactly zero: an atomic store.
    fn voiced(&self) {
        let ms = u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX - 1);
        self.voiced_ms.store(ms + 1, Ordering::Relaxed);
    }

    /// Whether it calls back but has sent nothing but exact zeros for
    /// [`SILENT_AFTER`] — a device that is open but not its sound's (one
    /// opened as its driver made it anew), or a capture box with no signal.
    fn silent(&self, now: Instant) -> bool {
        if self.heard_ms.load(Ordering::Relaxed) == 0 {
            return false;
        }
        let voiced = match self.voiced_ms.load(Ordering::Relaxed) {
            0 => self.since,
            ms => self.since + Duration::from_millis(ms - 1),
        };
        now.saturating_duration_since(voiced) > SILENT_AFTER
    }

    /// The stream called back: on the real-time thread, so an atomic store.
    fn heard(&self) {
        let ms = u64::try_from(self.since.elapsed().as_millis()).unwrap_or(u64::MAX - 1);
        self.heard_ms.store(ms + 1, Ordering::Relaxed);
    }

    fn lost(&self, now: Instant) -> bool {
        if self.gone.load(Ordering::Relaxed) {
            return true;
        }
        match self.heard_ms.load(Ordering::Relaxed) {
            0 => now.saturating_duration_since(self.since) > START_GRACE,
            ms => {
                let heard = self.since + Duration::from_millis(ms - 1);
                now.saturating_duration_since(heard) > LOST_AFTER
            }
        }
    }
}

/// A capture's passthrough subscribers, swapped lock-free (see
/// [`ActiveAudioSource::pcm_subs`]).
type PcmSubscribers = Arc<ArcSwap<Vec<PcmSubscriber>>>;

/// No subscribers: a capture opened again feeds no one until it is handed
/// the old one's.
fn no_subscribers() -> PcmSubscribers {
    Arc::new(ArcSwap::from_pointee(Vec::new()))
}

/// Opens the capture of device `id` among those listed, feeding the
/// subscribers given: the OS's device (see [`open_device`]), or a test's
/// stand-in.
type Opener = Arc<
    dyn Fn(&[AudioDeviceInfo], AudioSourceId, PcmSubscribers) -> Result<ActiveAudioSource>
        + Send
        + Sync,
>;

/// What a capture is opened for, on a thread of its own (see
/// [`AudioManager::start_open`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenKind {
    /// Taken afresh, for an input that asked (see
    /// [`AudioManager::subscribe_pcm_soon`]).
    Fresh,
    /// Opened again beside the old stream, which then hands its subscribers
    /// over (see [`AudioManager::reopen_silent`]). With `then_closed_first`
    /// (a Reset), a device that will not open twice at once is closed, then
    /// opened.
    Again { then_closed_first: bool },
    /// Opened again with the old stream closed first (see
    /// [`AudioManager::reopen`]).
    ClosedFirst,
}

/// A capture being opened on a thread of its own.
struct Opening {
    kind: OpenKind,
    /// When it was asked for (see [`OPEN_RETRY`]).
    started: Instant,
    landed: std::sync::mpsc::Receiver<Landed>,
}

/// What came of an opening, and when the old stream was closed for it.
struct Landed {
    result: Result<ActiveAudioSource>,
    closed_from: Option<Instant>,
}

impl Landed {
    /// No word from the thread opening it: not opened.
    fn nowhere() -> Self {
        Self {
            result: Err(anyhow::anyhow!("the thread opening it stopped")),
            closed_from: None,
        }
    }
}

/// How long a capture opened afresh waits for the input that asked for it
/// before it is closed: let go of while opening, it held its device open
/// for no one.
const UNCLAIMED: Duration = Duration::from_secs(5);

/// A capture's stream to close, and what to do once it is closed — done
/// wherever it is dropped: on its thread (see [`AudioManager::let_go`]), or
/// here when there is no thread to close it on. Its sender, dropped then,
/// tells it is closed.
struct ToClose {
    stream: Option<Box<dyn Send>>,
    then: Option<Box<dyn FnOnce() + Send>>,
    _closed: std::sync::mpsc::Sender<()>,
}

impl Drop for ToClose {
    fn drop(&mut self) {
        drop(self.stream.take());
        if let Some(then) = self.then.take() {
            then();
        }
    }
}

/// An active audio source with its own capture stream.
struct ActiveAudioSource {
    /// The stream, kept to be closed (none in tests, which have no device).
    stream: Option<Box<dyn Send>>,
    /// Whether the stream feeds its subscribers: not while the stream it
    /// takes over from still runs (see [`AudioManager::land`]).
    feeding: Arc<AtomicBool>,
    /// The device's name, to say which went.
    name: String,
    liveness: Arc<Liveness>,
    receiver: Receiver<AudioData>,
    /// Latest polled data (cached between polls)
    pub latest: AudioData,
    /// Native sample rate (Hz) of the capture device.
    sample_rate_hz: u32,
    /// Native channel count of the capture device.
    channels: u16,
    /// Passthrough subscribers. Swapped lock-free so the real-time cpal
    /// callback reads without locking while subscribe/unsubscribe mutate.
    pcm_subs: Arc<ArcSwap<Vec<PcmSubscriber>>>,
}

/// Manages audio device enumeration and multiple simultaneous audio input streams.
///
/// Capture is **lazy and derived from state**: a device is open only while it has
/// at least one *holder*. Holders come in three kinds (see
/// [/spec/audio-capture-lifecycle.md](/spec/audio-capture-lifecycle.md)):
/// modulation references (`mod_refs`, reconciled per-frame via
/// [`set_modulation_refs`](Self::set_modulation_refs)), PCM passthrough
/// subscribers (`pcm_subs` on each source), and manual pins (`manual_pins`, via
/// [`open_source`](Self::open_source)). A device with none of these is orphaned
/// and closed.
pub struct AudioManager {
    /// Detected audio input devices (refreshed on scan).
    devices: Vec<AudioDeviceInfo>,
    /// Active audio sources keyed by `AudioSourceId`.
    active: HashMap<AudioSourceId, ActiveAudioSource>,
    /// Resolved default input id (OS default matched by name, else first),
    /// cached at scan time. Used to resolve `AudioBand { source_id: None }`.
    default_source_id: Option<AudioSourceId>,
    /// Devices explicitly pinned open by a consumer (`open_source`).
    manual_pins: HashSet<AudioSourceId>,
    /// Devices referenced by `AudioBand` modulators (last reconciled set).
    mod_refs: HashSet<AudioSourceId>,
    /// When opening each device last failed (see [`OPEN_RETRY`]).
    failed: HashMap<AudioSourceId, Instant>,
    /// When the devices last changed (see [`Self::devices_changed`]).
    changed_at: Option<Instant>,
    /// The captures opened again, or tried, since: once each per change.
    reopened_since_change: HashSet<AudioSourceId>,
    /// Each device's number for as long as Nida runs, by its name and its
    /// place among those with it (see [`number_devices`]).
    numbers: HashMap<(String, usize), AudioSourceId>,
    /// The number the next device never listed before takes.
    next_number: AudioSourceId,
    /// Opens the captures (see [`Opener`]).
    opener: Opener,
    /// The captures being opened, each on a thread of its own: opening one
    /// takes the OS ~0.1 s, and the show's frames do not wait for it.
    opening: HashMap<AudioSourceId, Opening>,
    /// The captures opened afresh and not asked for since, and when.
    unclaimed: HashMap<AudioSourceId, Instant>,
    /// How many captures have been opened: the mixer looks for its inputs
    /// again at once when this moves.
    landed: u64,
    /// Each device's streams being closed, each told closed on its receiver:
    /// the device is opened again only once they are — a device that opens
    /// once at a time (ALSA's own) refused it while one still was.
    closing: HashMap<AudioSourceId, Vec<std::sync::mpsc::Receiver<()>>>,
}

/// Capture from the device listed as `id` in `devices`, found among the
/// OS's by its name (see [`now_at`]), feeding `pcm_subs`. Takes the OS
/// ~0.1 s: run off the show's frames (see [`AudioManager::start_open`]).
fn open_device(
    listed: &[AudioDeviceInfo],
    id: AudioSourceId,
    pcm_subs: PcmSubscribers,
) -> Result<ActiveAudioSource> {
    // An id never listed — a show's modulator from another Mac — is not
    // looked for: listing the OS's devices is slow.
    anyhow::ensure!(listed.iter().any(|d| d.id == id), "Audio device not found");
    let host = cpal::default_host();
    let devices: Vec<cpal::Device> = host
        .input_devices()
        .context("Failed to enumerate audio devices")?
        .collect();
    let now: Vec<String> = devices
        .iter()
        .enumerate()
        .map(|(i, d)| input_name(i, d))
        .collect();
    let at = now_at(listed, id, &now).context("Audio device not found")?;
    let device = devices
        .into_iter()
        .nth(at)
        .context("Audio device not found")?;

    let dev_name = &now[at];
    log::info!("Opening audio source {id}: {dev_name}");

    let config = device
        .default_input_config()
        .context("Failed to get default audio input config")?;
    let sample_rate = config.sample_rate() as f32;
    let sample_rate_hz = config.sample_rate() as u32;
    let channels = config.channels();
    log::info!("Audio config for '{dev_name}': {config:?}");
    #[cfg(target_os = "macos")]
    if let Some(late) = device.id().ok().and_then(|id| latency::of_input(&id.1)) {
        log::info!(
            "Audio input '{dev_name}' says it comes late by: {}",
            late.describe(sample_rate_hz)
        );
    }

    let (sender, receiver) = bounded::<AudioData>(16);
    let liveness = Arc::new(Liveness::new());
    let feeding = Arc::new(AtomicBool::new(true));

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => AudioManager::build_stream::<f32>(
            &device,
            &config.into(),
            sender,
            sample_rate,
            pcm_subs.clone(),
            Arc::clone(&liveness),
            Arc::clone(&feeding),
            dev_name,
        )?,
        cpal::SampleFormat::I16 => AudioManager::build_stream::<i16>(
            &device,
            &config.into(),
            sender,
            sample_rate,
            pcm_subs.clone(),
            Arc::clone(&liveness),
            Arc::clone(&feeding),
            dev_name,
        )?,
        cpal::SampleFormat::U16 => AudioManager::build_stream::<u16>(
            &device,
            &config.into(),
            sender,
            sample_rate,
            pcm_subs.clone(),
            Arc::clone(&liveness),
            Arc::clone(&feeding),
            dev_name,
        )?,
        _ => anyhow::bail!("Unsupported sample format"),
    };

    stream.play().context("Failed to start audio stream")?;

    Ok(ActiveAudioSource {
        stream: Some(Box::new(stream)),
        feeding,
        name: dev_name.clone(),
        liveness,
        receiver,
        latest: AudioData {
            sample_rate,
            ..AudioData::default()
        },
        sample_rate_hz,
        channels,
        pcm_subs,
    })
}

impl Default for AudioManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioManager {
    pub fn new() -> Self {
        let mut mgr = Self::blank();
        // Enumerate devices and cache the default input, but open nothing —
        // capture starts only when a holder appears (issue #76).
        mgr.scan_devices();
        mgr
    }

    /// A manager with no devices listed and nothing open.
    fn blank() -> Self {
        Self {
            devices: Vec::new(),
            active: HashMap::new(),
            default_source_id: None,
            manual_pins: HashSet::new(),
            mod_refs: HashSet::new(),
            failed: HashMap::new(),
            changed_at: None,
            reopened_since_change: HashSet::new(),
            numbers: HashMap::new(),
            next_number: 0,
            opener: Arc::new(open_device),
            opening: HashMap::new(),
            unclaimed: HashMap::new(),
            landed: 0,
            closing: HashMap::new(),
        }
    }

    /// Choose which enumerated input is the default: the OS default input matched
    /// by name if it is present in the scanned list, otherwise the first
    /// enumerated device. Returns `None` when no inputs exist. Pure so it can be
    /// unit-tested without audio hardware.
    fn pick_default_input(
        default_name: Option<&str>,
        devices: &[AudioDeviceInfo],
    ) -> Option<AudioSourceId> {
        if let Some(name) = default_name {
            if let Some(dev) = devices.iter().find(|d| d.name == name) {
                return Some(dev.id);
            }
        }
        devices.first().map(|d| d.id)
    }

    /// A manager listing `names` and asking the OS nothing, for a test.
    #[cfg(test)]
    pub(crate) fn listing(names: &[&str]) -> Self {
        Self {
            devices: tests::info(names),
            ..Self::blank()
        }
    }

    /// A manager with the input devices listed — or, when listing takes
    /// longer than `wait`, none yet and the listing still going: the
    /// receiver gets it when it is done (hand it to [`Self::adopt`]).
    ///
    /// Listing sound inputs asks `CoreAudio` about them, and while macOS has
    /// not decided whether the app may use the microphone that waits for its
    /// prompt — hidden behind a full-screen output, it waited for ever, with
    /// no window and no API. Started this way, Nida comes up regardless.
    pub fn new_waiting_at_most(wait: std::time::Duration) -> (Self, Option<InputListing>) {
        let mut mgr = Self::blank();
        match within(wait, Self::list_inputs) {
            Ok(listing) => {
                mgr.adopt(listing);
                (mgr, None)
            }
            Err(rx) => {
                log::warn!(
                    "Sound inputs not listed within {:.0} s — macOS may be asking for \
                     microphone access; carrying on without them for now",
                    wait.as_secs_f64()
                );
                (mgr, Some(rx))
            }
        }
    }

    /// Take a listing made by [`Self::new_waiting_at_most`]'s helper.
    pub fn adopt(&mut self, (devices, default): (Vec<AudioDeviceInfo>, Option<AudioSourceId>)) {
        let (devices, default) =
            number_devices(&mut self.numbers, &mut self.next_number, devices, default);
        self.devices = devices;
        self.default_source_id = default;
        log::info!("Audio scan: found {} input device(s)", self.devices.len());
        for dev in &self.devices {
            log::info!("  Audio {}: {}", dev.id, dev.name);
        }
    }

    /// Scan for available audio input devices and cache the resolved default.
    pub fn scan_devices(&mut self) {
        self.adopt(Self::list_inputs());
    }

    /// List the inputs on a thread of its own — listing can take a while,
    /// and the show's frames do not wait for it. `None` if no thread
    /// starts.
    pub fn list_in_background() -> Option<InputListing> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("audio-input-listing".into())
            .spawn(move || {
                let _ = tx.send(Self::list_inputs());
            })
            .ok()?;
        Some(rx)
    }

    /// Take a listing made after the devices changed: each device keeps
    /// its number (see [`number_devices`]), so the captures open and what
    /// refers to them stay on their devices. Said in the log only if the
    /// devices are not the same.
    pub fn readopt(&mut self, listing: (Vec<AudioDeviceInfo>, Option<AudioSourceId>)) {
        let names =
            |list: &[AudioDeviceInfo]| list.iter().map(|d| d.name.clone()).collect::<Vec<_>>();
        if names(&listing.0) == names(&self.devices) {
            return;
        }
        self.adopt(listing);
    }

    /// The sound devices changed (macOS said so): the captures sending only
    /// zeros may be opened again, once each, for a while (see
    /// [`Self::reopen_silent`]).
    pub fn devices_changed(&mut self) {
        self.changed_at = Some(Instant::now());
        self.reopened_since_change.clear();
    }

    /// Whether the devices were said to have changed — for tests.
    #[cfg(test)]
    pub(crate) fn changed_for_tests(&self) -> bool {
        self.changed_at.is_some()
    }

    /// Within [`SILENT_WINDOW`] of the devices changing, open again, in
    /// place and once each, the captures that have sent only exact zeros
    /// for [`SILENT_AFTER`]: a new stream from the device feeds the same
    /// subscribers, so the mixer and an output taking the device straight
    /// keep listening. A device whose driver made it anew kept its old
    /// capture open and silent, with no error (an UltraStudio, 2026-10-28).
    /// Opened on a thread of their own (see [`Self::poll_opens`]): the ids
    /// of those being opened again.
    pub fn reopen_silent(&mut self) -> Vec<AudioSourceId> {
        let now = Instant::now();
        if self
            .changed_at
            .is_none_or(|at| now.saturating_duration_since(at) > SILENT_WINDOW)
        {
            return Vec::new();
        }
        let silent: Vec<AudioSourceId> = self
            .active
            .iter()
            .filter(|(id, source)| {
                source.liveness.silent(now)
                    && !self.reopened_since_change.contains(*id)
                    && !self.opening.contains_key(*id)
            })
            .map(|(id, _)| *id)
            .collect();
        for &id in &silent {
            self.reopened_since_change.insert(id);
            self.start_open(
                id,
                OpenKind::Again {
                    then_closed_first: false,
                },
                None,
            );
        }
        silent
    }

    /// Open capture `id` again in place, as asked (a Reset input): see
    /// [`Self::reopen_silent`]. A device that will not open twice at once
    /// (an exclusive one) is closed first, then opened; not opened then, it
    /// is let go and taken afresh once it is back. Whether it was open, to
    /// be opened again.
    pub fn reopen(&mut self, id: AudioSourceId) -> bool {
        if !self.active.contains_key(&id) {
            return false;
        }
        match self.opening.get_mut(&id) {
            // Being opened again already: closed first too, if need be.
            Some(opening) => {
                if let OpenKind::Again { then_closed_first } = &mut opening.kind {
                    *then_closed_first = true;
                }
            }
            None => self.start_open(
                id,
                OpenKind::Again {
                    then_closed_first: true,
                },
                None,
            ),
        }
        true
    }

    /// Open capture `id` for `kind` on a thread of its own — `closing`, the
    /// old stream, closed there first — feeding no one until it is taken
    /// (see [`Self::poll_opens`]).
    fn start_open(&mut self, id: AudioSourceId, kind: OpenKind, closing: Option<Box<dyn Send>>) {
        let devices = self.devices.clone();
        let opener = Arc::clone(&self.opener);
        let (tx, landed) = std::sync::mpsc::channel();
        let closes = self.closing.remove(&id).unwrap_or_default();
        let spawned = std::thread::Builder::new()
            .name("audio-open".into())
            .spawn(move || {
                // Its streams being closed, closed first (see `closing`).
                for closed in closes {
                    let _ = closed.recv();
                }
                let closed_from = closing.map(|stream| {
                    let from = Instant::now();
                    drop(stream);
                    from
                });
                let result = opener(&devices, id, no_subscribers());
                let _ = tx.send(Landed {
                    result,
                    closed_from,
                });
            });
        // No thread: taken as not opened (see `Landed::nowhere`).
        if let Err(e) = spawned {
            log::warn!("Audio source {id}: no thread to open it on: {e}");
        }
        self.opening.insert(
            id,
            Opening {
                kind,
                started: Instant::now(),
                landed,
            },
        );
    }

    /// Close capture `id`'s stream on a thread of its own — closing takes
    /// the OS ~20 ms, and the show's frames do not wait for it — then do
    /// `then`; it is opened again only once closed (see `closing`).
    fn let_go(
        &mut self,
        id: AudioSourceId,
        stream: Option<Box<dyn Send>>,
        then: Option<Box<dyn FnOnce() + Send>>,
    ) {
        if stream.is_none() {
            if let Some(then) = then {
                then();
            }
            return;
        }
        let (closed, told) = std::sync::mpsc::channel();
        let to_close = ToClose {
            stream,
            then,
            _closed: closed,
        };
        // No thread: closed here, as it is dropped (see `ToClose`).
        let _ = std::thread::Builder::new()
            .name("audio-close".into())
            .spawn(move || drop(to_close));
        self.closing.entry(id).or_default().push(told);
    }

    /// Take the captures opened since, each as it was asked for — call once
    /// a frame. Whether one was taken afresh. One opened afresh and not
    /// asked for within [`UNCLAIMED`] is closed.
    pub fn poll_opens(&mut self) -> bool {
        let mut fresh = false;
        let ids: Vec<AudioSourceId> = self.opening.keys().copied().collect();
        for id in ids {
            let landed = match self.opening.get(&id).map(|o| o.landed.try_recv()) {
                Some(Ok(landed)) => landed,
                Some(Err(std::sync::mpsc::TryRecvError::Disconnected)) => Landed::nowhere(),
                _ => continue,
            };
            if let Some(opening) = self.opening.remove(&id) {
                fresh |= self.land(id, opening.kind, opening.started, landed);
            }
        }
        self.close_unclaimed(Instant::now());
        // Streams closed since: none to wait for.
        self.closing.retain(|_, closes| {
            closes.retain(|closed| {
                matches!(closed.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty))
            });
            !closes.is_empty()
        });
        fresh
    }

    /// How many captures have been taken afresh so far.
    pub fn landed(&self) -> u64 {
        self.landed
    }

    /// Wait for capture `id`'s opening, if it is being opened, and take it.
    fn wait_for(&mut self, id: AudioSourceId) {
        if let Some(opening) = self.opening.remove(&id) {
            let landed = opening.landed.recv().unwrap_or_else(|_| Landed::nowhere());
            self.land(id, opening.kind, opening.started, landed);
        }
    }

    /// Close the captures opened afresh that no one has asked for within
    /// [`UNCLAIMED`].
    fn close_unclaimed(&mut self, now: Instant) {
        let settled: Vec<AudioSourceId> = self
            .unclaimed
            .keys()
            .copied()
            .filter(|id| self.has_holder(*id) || !self.active.contains_key(id))
            .collect();
        for id in settled {
            self.unclaimed.remove(&id);
        }
        let stale: Vec<AudioSourceId> = self
            .unclaimed
            .iter()
            .filter(|(_, at)| now.saturating_duration_since(**at) > UNCLAIMED)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.unclaimed.remove(&id);
            self.close_if_orphaned(id);
        }
    }

    /// Take capture `id`, opened for `kind` (see [`OpenKind`]) as asked at
    /// `started`: afresh, it is open; again, in the old one's format, it
    /// takes over the old one's subscribers and the old is closed; in
    /// another format than they take, both are let go and the mixer takes
    /// the device afresh (see `connect_inputs`); not opened again, the old
    /// keeps its subscribers — unless it was closed for it, or is closed
    /// first now (a Reset). Opened again with the old let go meanwhile, it
    /// is taken as if afresh: one asking for it now waits for it (see
    /// `ensure_open`). Whether it was taken.
    fn land(
        &mut self,
        id: AudioSourceId,
        kind: OpenKind,
        started: Instant,
        landed: Landed,
    ) -> bool {
        let Landed {
            result,
            closed_from,
        } = landed;
        let Some(old) = self.active.get_mut(&id).filter(|_| kind != OpenKind::Fresh) else {
            return match result {
                Ok(new) if !self.active.contains_key(&id) => {
                    self.failed.remove(&id);
                    self.active.insert(id, new);
                    self.unclaimed.insert(id, Instant::now());
                    self.landed += 1;
                    true
                }
                // Opened meanwhile, waited for (see `ensure_open`).
                Ok(new) => {
                    self.let_go(id, new.stream, None);
                    false
                }
                // Looked for again from when it was asked for: one asking
                // again every OPEN_RETRY is not put off twice as long.
                Err(e) if kind == OpenKind::Fresh => {
                    self.failed.insert(id, started);
                    log::warn!("Audio source {id} not opened: {e}");
                    false
                }
                Err(e) => {
                    log::debug!("Audio source {id}, let go meanwhile, not opened again: {e}");
                    false
                }
            };
        };
        let (name, format) = (old.name.clone(), (old.sample_rate_hz, old.channels));
        match result {
            Ok(new) if (new.sample_rate_hz, new.channels) == format => {
                if kind == OpenKind::ClosedFirst {
                    // The sound missed while it was closed, told to each
                    // subscriber as lost — whole frames of every channel, or
                    // the channels after it are put out of step: a
                    // recording owes it as silence, or its sound runs ahead
                    // of its picture from then on.
                    let gap = closed_from.map_or(Duration::ZERO, |from| from.elapsed());
                    let missed = missed_samples(gap, format.0, format.1);
                    let subs = old.pcm_subs.load_full();
                    for sub in subs.iter() {
                        sub.lost_samples.fetch_add(missed, Ordering::Relaxed);
                        sub.pending_lost.fetch_add(missed, Ordering::Relaxed);
                    }
                    new.pcm_subs.store(subs);
                    self.active.insert(id, new);
                } else {
                    // The old stream feeds its subscribers until it has
                    // stopped, on a thread of its own; the new one, handed
                    // them now, feeds them only from then on: never two
                    // feeding one subscriber, and no more than a callback's
                    // sound between them.
                    new.feeding.store(false, Ordering::Relaxed);
                    new.pcm_subs.store(old.pcm_subs.load_full());
                    let feeding = Arc::clone(&new.feeding);
                    let stream = old.stream.take();
                    self.active.insert(id, new);
                    self.let_go(
                        id,
                        stream,
                        Some(Box::new(move || feeding.store(true, Ordering::Relaxed))),
                    );
                }
                self.landed += 1;
                log::info!("Audio source {id} ('{name}') opened again");
            }
            Ok(new) => {
                log::info!(
                    "Audio source {id} ('{name}') opens in another format now: taken afresh"
                );
                self.let_go(id, new.stream, None);
                if let Some(old) = self.active.remove(&id) {
                    self.let_go(id, old.stream, None);
                }
            }
            Err(e)
                if kind
                    == (OpenKind::Again {
                        then_closed_first: true,
                    }) =>
            {
                log::info!(
                    "Audio source {id} ('{name}') would not open beside its stream ({e}): \
                     closing it first"
                );
                let closing = old.stream.take();
                self.start_open(id, OpenKind::ClosedFirst, closing);
            }
            Err(e) if kind == OpenKind::ClosedFirst => {
                log::info!(
                    "Audio source {id} ('{name}') not opened again ({e}): let go, taken \
                     afresh once it is back"
                );
                if let Some(old) = self.active.remove(&id) {
                    self.let_go(id, old.stream, None);
                }
            }
            Err(e) => log::debug!("Audio source {id} ('{name}') not opened again: {e}"),
        }
        false
    }

    /// The input devices, and which is the default.
    fn list_inputs() -> (Vec<AudioDeviceInfo>, Option<AudioSourceId>) {
        let host = cpal::default_host();
        let devices: Vec<AudioDeviceInfo> = host
            .input_devices()
            .map(|devs| {
                devs.enumerate()
                    .map(|(i, d)| AudioDeviceInfo {
                        id: i as AudioSourceId,
                        name: input_name(i, &d),
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Cache the default input (matched by OS-default name so we prefer the
        // user's mic/interface over a silent BlackHole loopback that merely
        // enumerates first). Queried here, off the per-frame hot path.
        let default_name = host
            .default_input_device()
            .and_then(|d| d.description().ok().map(|desc| desc.name().to_string()));
        let default = Self::pick_default_input(default_name.as_deref(), &devices);
        (devices, default)
    }

    /// Get the list of detected audio input devices.
    pub fn devices(&self) -> &[AudioDeviceInfo] {
        &self.devices
    }

    /// Resolved default input id, used to resolve `AudioBand { source_id: None }`.
    pub fn default_source_id(&self) -> Option<AudioSourceId> {
        self.default_source_id
    }

    /// Manually pin a source open (a co-equal-consumer holder).
    ///
    /// Pins the device so it stays open regardless of modulation/passthrough
    /// holders, and starts capture if it isn't already running — on a thread
    /// of its own (see [`Self::poll_opens`]). Used by the HTTP API
    /// (`POST /audio/sources/{id}/open`) and `ToggleAudioSource`. Release
    /// with [`close_source`](Self::close_source).
    ///
    /// # Errors
    ///
    /// Returns an error if no input device is listed as `id`.
    pub fn open_source(&mut self, id: AudioSourceId) -> Result<()> {
        self.manual_pins.insert(id);
        anyhow::ensure!(
            self.devices.iter().any(|d| d.id == id),
            "Audio device not found"
        );
        // Asked for now: tried now.
        self.failed.remove(&id);
        self.open_soon(id);
        Ok(())
    }

    /// Open capture `id` on a thread of its own, if it is listed and is
    /// neither open, being opened, nor tried within [`OPEN_RETRY`].
    fn open_soon(&mut self, id: AudioSourceId) {
        let idle = !self.active.contains_key(&id) && !self.opening.contains_key(&id);
        if idle
            && !tried_lately(&self.failed, id, Instant::now())
            && self.devices.iter().any(|d| d.id == id)
        {
            self.start_open(id, OpenKind::Fresh, None);
        }
    }

    /// Start capture for a source if it isn't already active, and wait for
    /// it: one being opened is waited for. Does not register any holder —
    /// callers manage holder sets themselves. A failure is noted (see
    /// [`OPEN_RETRY`]).
    fn ensure_open(&mut self, id: AudioSourceId) -> Result<()> {
        if self.active.contains_key(&id) {
            return Ok(()); // Already open
        }
        // Opened as any other — once its streams being closed are — and
        // waited for. Being opened again for one let go meanwhile, and not
        // opened then (a device that opens once at a time, still held by
        // the old stream), it is opened afresh now the old has gone.
        let again = self
            .opening
            .get(&id)
            .is_some_and(|opening| opening.kind != OpenKind::Fresh);
        if !self.opening.contains_key(&id) {
            self.start_open(id, OpenKind::Fresh, None);
        }
        self.wait_for(id);
        if again && !self.active.contains_key(&id) {
            self.start_open(id, OpenKind::Fresh, None);
            self.wait_for(id);
        }
        anyhow::ensure!(self.active.contains_key(&id), "Audio device would not open");
        Ok(())
    }

    /// [`Self::subscribe_pcm`] for one that must not wait: a source not open
    /// is opened on a thread of its own (see [`Self::poll_opens`]), and
    /// `None` given until it is.
    pub fn subscribe_pcm_soon(&mut self, id: AudioSourceId) -> Option<PcmSubscription> {
        // Closed and being opened again (a Reset): taken once it is open,
        // charged for no sound missed before it came (see `land`).
        if self.closed_to_open_again(id) {
            return None;
        }
        if self.active.contains_key(&id) {
            return self.subscribe_pcm(id);
        }
        self.open_soon(id);
        None
    }

    /// Whether capture `id` is closed, being opened again (see
    /// [`OpenKind::ClosedFirst`]).
    fn closed_to_open_again(&self, id: AudioSourceId) -> bool {
        self.opening
            .get(&id)
            .is_some_and(|opening| opening.kind == OpenKind::ClosedFirst)
    }

    /// Subscribe to raw PCM passthrough for a source, opening it if needed.
    ///
    /// Returns a fresh receiver plus the device's native [`AudioFormat`] so the
    /// caller can build matching ffmpeg input args. The audio callback fans raw
    /// interleaved `f32` to this and every other subscriber off one hardware
    /// clock, keeping analysis and passthrough coherent. Returns `None` if the
    /// source can't be opened.
    pub fn subscribe_pcm(&mut self, id: AudioSourceId) -> Option<PcmSubscription> {
        // Closed and being opened again (a Reset): waited for, so as to be
        // charged for no sound missed before it came (see `land`).
        if self.closed_to_open_again(id) {
            self.wait_for(id);
        }
        if !self.active.contains_key(&id) {
            if let Err(e) = self.ensure_open(id) {
                log::warn!("subscribe_pcm: failed to open audio source {id}: {e}");
                return None;
            }
        }
        let source = self.active.get(&id)?;
        let format = AudioFormat {
            sample_rate: source.sample_rate_hz,
            channels: source.channels,
        };
        let (sender, receiver) = bounded::<PcmChunk>(PCM_CHANNEL_CAPACITY);
        let token = NEXT_PCM_TOKEN.fetch_add(1, Ordering::Relaxed);
        let dropped = Arc::new(AtomicU64::new(0));
        let lost_samples = Arc::new(AtomicU64::new(0));
        let sub = PcmSubscriber {
            token,
            sender,
            dropped: dropped.clone(),
            lost_samples: lost_samples.clone(),
            pending_lost: Arc::default(),
            live: None,
        };
        source.pcm_subs.rcu(|cur| {
            let mut next = Vec::with_capacity(cur.len() + 1);
            next.extend(cur.iter().cloned());
            next.push(sub.clone());
            next
        });
        log::info!("PCM passthrough subscriber {token} added to source {id}");
        Some(PcmSubscription {
            receiver,
            format,
            token,
            dropped,
            lost_samples,
        })
    }

    /// A live bus's feed from capture `id` (open already): each of its mics'
    /// channels `(left, right)` (0-based; the same for a mono mic), every mic
    /// in each frame, straight from the capture's callback. Released by
    /// [`Self::unsubscribe_pcm`] with its token. None for more than
    /// [`LIVE_MICS`] mics or a channel the capture does not have.
    pub fn subscribe_live(&mut self, id: AudioSourceId, mics: &[(u16, u16)]) -> Option<LiveTap> {
        let source = self.active.get(&id)?;
        let channels = usize::from(source.channels);
        if mics.is_empty()
            || mics.len() > LIVE_MICS
            || mics.iter().any(|&(l, r)| usize::from(l.max(r)) >= channels)
        {
            return None;
        }
        let (tx, rx) = bounded::<LiveFrame>(LIVE_FRAMES_HELD);
        let counts = Arc::new(LiveCounts::default());
        let token = NEXT_PCM_TOKEN.fetch_add(1, Ordering::Relaxed);
        let sub = PcmSubscriber {
            token,
            sender: bounded::<PcmChunk>(1).0,
            dropped: Arc::default(),
            lost_samples: Arc::default(),
            pending_lost: Arc::default(),
            live: Some(LiveFrames {
                pairs: mics
                    .iter()
                    .map(|&(l, r)| (usize::from(l), usize::from(r)))
                    .collect(),
                channels,
                tx,
                rx: rx.clone(),
                counts: Arc::clone(&counts),
            }),
        };
        source.pcm_subs.rcu(|cur| {
            let mut next = Vec::with_capacity(cur.len() + 1);
            next.extend(cur.iter().cloned());
            next.push(sub.clone());
            next
        });
        log::info!(
            "Live feed {token} added to source {id} ({} mic(s))",
            mics.len()
        );
        Some(LiveTap {
            frames: rx,
            counts,
            token,
            rate: source.sample_rate_hz,
        })
    }

    /// Remove a PCM passthrough subscriber when its output stops.
    ///
    /// Dropping the last passthrough subscriber releases that holder; the device
    /// is closed if no modulation ref or manual pin still needs it.
    pub fn unsubscribe_pcm(&mut self, id: AudioSourceId, token: PcmToken) {
        if let Some(source) = self.active.get(&id) {
            source.pcm_subs.rcu(|cur| {
                cur.iter()
                    .filter(|s| s.token != token)
                    .cloned()
                    .collect::<Vec<_>>()
            });
            log::info!("PCM passthrough subscriber {token} removed from source {id}");
        }
        self.close_if_orphaned(id);
    }

    /// Release a manual pin (`open_source`) and close the device if orphaned.
    pub fn close_source(&mut self, id: AudioSourceId) {
        self.manual_pins.remove(&id);
        self.close_if_orphaned(id);
    }

    /// Whether a source still has any holder: a manual pin, a modulation ref,
    /// or at least one PCM passthrough subscriber.
    fn has_holder(&self, id: AudioSourceId) -> bool {
        self.manual_pins.contains(&id)
            || self.mod_refs.contains(&id)
            || self
                .active
                .get(&id)
                .is_some_and(|s| !s.pcm_subs.load().is_empty())
    }

    /// Close and stop the `cpal` stream for a source if it has no remaining
    /// holder. No-op if the source is not open or is still held.
    fn close_if_orphaned(&mut self, id: AudioSourceId) {
        if self.has_holder(id) {
            return;
        }
        if let Some(source) = self.active.remove(&id) {
            self.let_go(id, source.stream, None);
            log::info!("Closed audio source {id} (no remaining holders)");
        }
    }

    /// Reconcile the set of devices referenced by `AudioBand` modulators.
    ///
    /// Declarative per-frame entry point: `needed` is the resolved set of device
    /// ids that modulators currently require (with `None` already resolved to the
    /// default input). Opens newly-needed devices and closes devices that dropped
    /// out of the set and are no longer held by a pin or passthrough subscriber.
    /// See [/spec/audio-capture-lifecycle.md](/spec/audio-capture-lifecycle.md).
    pub fn set_modulation_refs(&mut self, needed: &BTreeSet<AudioSourceId>) {
        // Devices leaving the modulation set: drop the ref, then close if orphaned.
        let dropped: Vec<AudioSourceId> = self
            .mod_refs
            .iter()
            .filter(|id| !needed.contains(id))
            .copied()
            .collect();
        for id in dropped {
            self.mod_refs.remove(&id);
            self.close_if_orphaned(id);
        }
        // Devices entering the modulation set: register the ref, then open
        // it on a thread of its own — one that would not open only every
        // OPEN_RETRY, as this runs every frame.
        for &id in needed {
            self.mod_refs.insert(id);
            self.open_soon(id);
        }
    }

    /// Resolve a list of `AudioBand` device selections into the concrete set of
    /// device ids that must be open. `None` selections resolve to `default` (and
    /// are dropped when no default input exists). Pure so it can be unit-tested
    /// without audio hardware.
    pub fn needed_from_bands(
        bands: &[Option<AudioSourceId>],
        default: Option<AudioSourceId>,
    ) -> BTreeSet<AudioSourceId> {
        bands.iter().filter_map(|sel| sel.or(default)).collect()
    }

    /// Poll all active sources for latest data. Call once per frame.
    pub fn poll(&mut self) {
        for source in self.active.values_mut() {
            while let Ok(data) = source.receiver.try_recv() {
                source.latest = data;
            }
        }
    }

    /// Get the latest `AudioData` for a specific source.
    pub fn get_data(&self, id: AudioSourceId) -> Option<&AudioData> {
        self.active.get(&id).map(|s| &s.latest)
    }

    /// Get the first active source's data (convenience for default/primary audio).
    pub fn get_primary_data(&self) -> &AudioData {
        static DEFAULT: std::sync::LazyLock<AudioData> =
            std::sync::LazyLock::new(AudioData::default);
        // Return first active source's data, or a static default
        self.active
            .values()
            .next()
            .map_or_else(|| &*DEFAULT, |s| &s.latest)
    }

    /// Get IDs of all active (open) sources.
    pub fn active_source_ids(&self) -> Vec<AudioSourceId> {
        self.active.keys().copied().collect()
    }

    /// Check if any source is active.
    pub fn has_active_source(&self) -> bool {
        !self.active.is_empty()
    }

    /// Let go of the captures whose stream has stopped — the device
    /// unplugged, said by macOS or gone quiet (see [`Liveness`]). Kept, a
    /// dead stream held its subscribers silent for good while the mixer's
    /// input looked connected. Their subscribers' channels close; the mixer
    /// subscribes again once the device is back (`connect_inputs`), opened
    /// by its name. The names of those let go.
    pub fn forget_lost(&mut self) -> Vec<String> {
        let now = Instant::now();
        // One being opened again is quiet for it, not lost — closed first
        // for a Reset, it is quiet until opened.
        let lost: Vec<AudioSourceId> = self
            .active
            .iter()
            .filter(|(id, source)| source.liveness.lost(now) && !self.opening.contains_key(*id))
            .map(|(id, _)| *id)
            .collect();
        lost.into_iter()
            .filter_map(|id| {
                let mut source = self.active.remove(&id)?;
                self.let_go(id, source.stream.take(), None);
                self.failed.insert(id, now);
                log::warn!(
                    "Audio source {id} ('{}') stopped: let go, opened again once it is back",
                    source.name
                );
                Some(source.name)
            })
            .collect()
    }

    /// Open again, every [`OPEN_RETRY`], the sources pinned open (see
    /// [`Self::open_source`]) that are not: let go when their device went
    /// (see [`Self::forget_lost`]), they had no other way back.
    pub fn reopen_pins(&mut self) {
        let pins: Vec<AudioSourceId> = self.manual_pins.iter().copied().collect();
        for id in pins {
            self.open_soon(id);
        }
    }

    /// How many channels capture `id` has, if it is open.
    pub fn channels_of(&self, id: AudioSourceId) -> Option<u16> {
        self.active.get(&id).map(|s| s.channels)
    }

    /// Whether subscription `token` to source `id` still runs.
    pub fn serves(&self, id: AudioSourceId, token: PcmToken) -> bool {
        self.active
            .get(&id)
            .is_some_and(|s| s.pcm_subs.load().iter().any(|p| p.token == token))
    }

    /// Captures opened with no device behind them, as listed — for tests.
    #[cfg(test)]
    pub(crate) fn open_without_devices_for_test(&mut self) {
        self.opener = Arc::new(|listed, id, pcm_subs| {
            let name = listed
                .iter()
                .find(|d| d.id == id)
                .map(|d| d.name.clone())
                .context("Audio device not found")?;
            Ok(ActiveAudioSource {
                stream: None,
                feeding: Arc::new(AtomicBool::new(true)),
                name,
                liveness: Arc::new(Liveness::new()),
                receiver: bounded(1).1,
                latest: AudioData::default(),
                sample_rate_hz: 48_000,
                channels: 2,
                pcm_subs,
            })
        });
    }

    /// Wait for every capture being opened, and take it — for tests.
    #[cfg(test)]
    pub(crate) fn wait_opens_for_test(&mut self) {
        while let Some(&id) = self.opening.keys().next() {
            self.wait_for(id);
        }
    }

    /// Source `id` open as `name` with no device behind it, and a
    /// subscription to it — for tests.
    #[cfg(test)]
    pub(crate) fn open_for_test(&mut self, id: AudioSourceId, name: &str) -> PcmSubscription {
        self.active.insert(
            id,
            ActiveAudioSource {
                stream: None,
                feeding: Arc::new(AtomicBool::new(true)),
                name: name.into(),
                liveness: Arc::new(Liveness::new()),
                receiver: bounded(1).1,
                latest: AudioData::default(),
                sample_rate_hz: 48_000,
                channels: 2,
                pcm_subs: Arc::new(ArcSwap::from_pointee(Vec::new())),
            },
        );
        self.subscribe_pcm(id).expect("open")
    }

    /// Source `id`'s stream as macOS says when its device goes — for tests.
    #[cfg(test)]
    pub(crate) fn lose_for_test(&self, id: AudioSourceId) {
        if let Some(source) = self.active.get(&id) {
            source.liveness.gone.store(true, Ordering::Relaxed);
        }
    }

    /// Source `id`'s stream as one started `ago`, called back as it started
    /// (`heard`) or never — for tests.
    #[cfg(test)]
    pub(crate) fn quiet_for_test(&mut self, id: AudioSourceId, ago: Duration, heard: bool) {
        if let Some(source) = self.active.get_mut(&id) {
            source.liveness = Arc::new(Liveness {
                gone: AtomicBool::new(false),
                since: Instant::now()
                    .checked_sub(ago)
                    .expect("a time that far back"),
                heard_ms: AtomicU64::new(u64::from(heard)),
                voiced_ms: AtomicU64::new(0),
            });
        }
    }

    // Each argument is a piece of the stream's own state, handed to its
    // callback.
    #[allow(clippy::too_many_arguments)]
    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        sender: Sender<AudioData>,
        sample_rate: f32,
        pcm_subs: Arc<ArcSwap<Vec<PcmSubscriber>>>,
        liveness: Arc<Liveness>,
        feeding: Arc<AtomicBool>,
        name: &str,
    ) -> Result<cpal::Stream>
    where
        T: cpal::Sample + cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        // Said as its sound first comes, and again when it changes (a live
        // bus on the same device sets its buffer smaller: the size is the
        // device's, not the stream's): how much comes at a time — a driver
        // that hands sound over in big blocks holds it back by them. A few
        // times at most: a driver whose blocks alternate would fill the log.
        let name = name.to_string();
        let mut told_frames = 0usize;
        let mut tellings = 0u8;
        let channels = config.channels as usize;
        // The chunk subscribers' sound, gathered to 10 ms (see `Gathered`):
        // room for a chunk and a big callback, made here, not in it.
        let chunk = ((sample_rate * PCM_CHUNK_SECS) as usize).max(1) * channels.max(1);
        let mut gathered = Gathered::new(chunk, 8192, channels, Arc::clone(&pcm_subs));
        let mut sample_buffer: Vec<f32> = Vec::with_capacity(AUDIO_BUFFER_SIZE);
        let mut fft_planner = FftPlanner::new();
        let fft = Arc::new(fft_planner.plan_fft_forward(FFT_SIZE));

        // Pre-compute Hann window to reduce spectral leakage
        let hann_window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();

        // Ring buffer for overlapping FFT frames
        let mut ring_buffer: Vec<f32> = vec![0.0; FFT_SIZE];
        let mut ring_write_pos: usize = 0;

        // Spectral flux onset detection state
        let mut prev_fft_magnitudes: Vec<f32> = vec![0.0; FFT_SIZE / 2];
        let mut flux_history: Vec<f32> = Vec::with_capacity(ONSET_MEDIAN_WINDOW + 1);

        // Pre-allocated FFT input buffer (H4: avoid per-callback allocation)
        let mut fft_input: Vec<Complex<f32>> = vec![Complex::new(0.0, 0.0); FFT_SIZE];

        // BPM detection state
        let mut last_beat_time = Instant::now();
        let mut beat_intervals: Vec<f32> = Vec::with_capacity(BPM_HISTORY_SIZE);
        let mut current_bpm: Option<f32> = None;

        let callback_liveness = Arc::clone(&liveness);
        let stream = device.build_input_stream(
            config,
            move |data: &[T], _: &cpal::InputCallbackInfo| {
                let frames = data.len() / channels.max(1);
                if frames != told_frames && tellings < 6 {
                    tellings += 1;
                    let was = if told_frames == 0 {
                        String::new()
                    } else {
                        format!(", was {told_frames}")
                    };
                    told_frames = frames;
                    log::info!(
                        "Audio input '{name}': {frames} frames a call ({:.1} ms of sound{was})",
                        frames as f64 * 1000.0 / f64::from(sample_rate.max(1.0))
                    );
                }
                callback_liveness.heard();
                if data.iter().any(|s| <f32 as Sample>::from_sample(*s) != 0.0) {
                    callback_liveness.voiced();
                }
                // Passthrough tee: forward raw interleaved PCM, untouched by the
                // FFT pipeline, to every subscriber. Skipped entirely when there
                // are no subscribers so the video-only path pays nothing.
                let subs = pcm_subs.load();
                if !subs.is_empty() && feeding.load(Ordering::Relaxed) {
                    gathered.tee(&subs, data.iter().map(|s| <f32 as Sample>::from_sample(*s)));
                } else {
                    gathered.clear();
                }

                for chunk in data.chunks(channels) {
                    let sample: f32 = chunk
                        .iter()
                        .map(|s| <f32 as Sample>::from_sample(*s))
                        .sum::<f32>()
                        / channels as f32;
                    sample_buffer.push(sample);
                }

                while sample_buffer.len() >= FFT_HOP {
                    const NOISE_FLOOR: f32 = 1e-4;

                    // Extract waveform chunk for GPU (256 samples)
                    let waveform: Vec<f32> = sample_buffer.drain(..FFT_HOP).collect();
                    let level =
                        (waveform.iter().map(|s| s * s).sum::<f32>() / FFT_HOP as f32).sqrt();

                    // Write hop into ring buffer (wrapping)
                    for (i, &s) in waveform.iter().enumerate() {
                        ring_buffer[(ring_write_pos + i) % FFT_SIZE] = s;
                    }
                    ring_write_pos = (ring_write_pos + FFT_HOP) % FFT_SIZE;

                    // Extract linearized 2048-sample frame, apply Hann window
                    // Reuse pre-allocated fft_input buffer (H4 optimisation)
                    for i in 0..FFT_SIZE {
                        let idx = (ring_write_pos + i) % FFT_SIZE;
                        fft_input[i] = Complex::new(ring_buffer[idx] * hann_window[i], 0.0);
                    }
                    fft.process(&mut fft_input);

                    // Magnitude scaling with noise floor
                    let scale = 2.0 / FFT_SIZE as f32;
                    let fft_magnitudes: Vec<f32> = fft_input[..FFT_SIZE / 2]
                        .iter()
                        .map(|c: &Complex<f32>| {
                            let mag = c.norm() * scale;
                            if mag < NOISE_FLOOR {
                                0.0
                            } else {
                                mag
                            }
                        })
                        .collect();

                    // Spectral flux onset detection
                    let spectral_flux: f32 = fft_magnitudes
                        .iter()
                        .zip(prev_fft_magnitudes.iter())
                        .map(|(curr, prev)| (curr - prev).max(0.0))
                        .sum();

                    flux_history.push(spectral_flux);
                    if flux_history.len() > ONSET_MEDIAN_WINDOW {
                        flux_history.remove(0);
                    }
                    let onset_threshold = compute_onset_threshold(&flux_history);

                    let now = Instant::now();
                    let elapsed = now.duration_since(last_beat_time).as_secs_f32();
                    let is_onset = spectral_flux > onset_threshold && elapsed > MIN_BEAT_INTERVAL;
                    prev_fft_magnitudes.clone_from(&fft_magnitudes);

                    // BPM estimation with outlier rejection
                    if is_onset {
                        if elapsed < MAX_BEAT_INTERVAL {
                            beat_intervals.push(elapsed);
                            if beat_intervals.len() > BPM_HISTORY_SIZE {
                                beat_intervals.remove(0);
                            }
                            if let Some(bpm) = estimate_bpm(&beat_intervals) {
                                current_bpm = Some(bpm);
                            }
                        } else {
                            beat_intervals.clear();
                            current_bpm = None;
                        }
                        last_beat_time = now;
                    }

                    let time_since_beat = now.duration_since(last_beat_time).as_secs_f32();

                    let data = AudioData {
                        waveform,
                        fft: fft_magnitudes,
                        level,
                        bpm: current_bpm,
                        time_since_beat,
                        sample_rate,
                    };

                    let _ = sender.try_send(data);
                }
            },
            move |err| {
                log::error!("Audio stream error: {err}");
                if matches!(err, cpal::StreamError::DeviceNotAvailable) {
                    liveness.gone.store(true, Ordering::Relaxed);
                }
            },
            None,
        )?;

        Ok(stream)
    }
}

/// Audio texture manager - creates wgpu textures for ISF audio inputs
pub struct AudioTextures {
    /// Waveform texture (ISF "audio" input type)
    pub waveform_texture: wgpu::Texture,
    pub waveform_view: wgpu::TextureView,

    /// FFT texture (ISF "audioFFT" input type)
    pub fft_texture: wgpu::Texture,
    pub fft_view: wgpu::TextureView,
}

impl AudioTextures {
    /// Create audio textures
    pub fn new(device: &wgpu::Device) -> Self {
        // Create 1D-like textures (width=buffer_size, height=1)
        let waveform_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Audio Waveform Texture"),
            size: wgpu::Extent3d {
                width: AUDIO_BUFFER_SIZE as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let fft_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Audio FFT Texture"),
            size: wgpu::Extent3d {
                width: (FFT_SIZE / 2) as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R32Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });

        let waveform_view = waveform_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let fft_view = fft_texture.create_view(&wgpu::TextureViewDescriptor::default());

        Self {
            waveform_texture,
            waveform_view,
            fft_texture,
            fft_view,
        }
    }

    /// Update textures with new audio data
    pub fn update(&self, queue: &wgpu::Queue, audio_data: &AudioData) {
        // Update waveform texture
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.waveform_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&audio_data.waveform),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(AUDIO_BUFFER_SIZE as u32 * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: AUDIO_BUFFER_SIZE as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );

        // Update FFT texture
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &self.fft_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytemuck::cast_slice(&audio_data.fft),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some((FFT_SIZE / 2) as u32 * 4),
                rows_per_image: None,
            },
            wgpu::Extent3d {
                width: (FFT_SIZE / 2) as u32,
                height: 1,
                depth_or_array_layers: 1,
            },
        );
    }
}

#[cfg(test)]
mod tests {

    /// A live feed takes its two channels, frame by frame; full, it lets
    /// its oldest frames go rather than wait.
    #[test]
    fn a_live_feed_takes_its_channels_and_lets_the_oldest_go() {
        let (tx, rx) = crossbeam_channel::bounded(4);
        // Two mics: channels 2 and 0, and a mono one on channel 1.
        let counts = std::sync::Arc::new(super::LiveCounts::default());
        let live = super::LiveFrames {
            pairs: vec![(2, 0), (1, 1)],
            channels: 3,
            tx,
            rx: rx.clone(),
            counts: std::sync::Arc::clone(&counts),
        };
        live.feed(&[0.1, 0.2, 0.3, 1.1, 1.2, 1.3]);
        let first = rx.try_recv().unwrap();
        assert_eq!(
            first[..4],
            [0.3, 0.1, 0.2, 0.2],
            "each mic in the one frame"
        );
        assert_eq!(rx.try_recv().unwrap()[..2], [1.3, 1.1]);
        let many: Vec<f32> = (0..18).map(|i| i as f32).collect();
        live.feed(&many);
        assert_eq!(rx.len(), 4);
        assert_eq!(rx.try_recv().unwrap()[..2], [8.0, 6.0], "the oldest let go");
        // Every frame promised (two, then six), less the two it let go: the
        // three queued and the three taken here (a reader counts its own).
        assert_eq!(counts.promised.load(Ordering::Relaxed), 8);
        assert_eq!(
            counts.gone.load(Ordering::Relaxed),
            2,
            "its own let go, counted"
        );
        assert_eq!(counts.coming(rx.len()).0, rx.len() + 3);
        assert_eq!(counts.chunk.load(Ordering::Relaxed), 6, "its last chunk");
    }

    fn listed(devices: &[(u32, &str)]) -> Vec<AudioDeviceInfo> {
        devices
            .iter()
            .map(|(id, name)| AudioDeviceInfo {
                id: *id,
                name: (*name).to_string(),
            })
            .collect()
    }

    /// A device keeps its number for as long as Nida runs: listed again
    /// after one is plugged in ahead of it, and unplugged and plugged back
    /// in; one never listed before takes a number no other had; the first
    /// listing is numbered by place, and the default follows its device.
    #[test]
    fn a_device_keeps_its_number_while_nida_runs() {
        let mut numbers = HashMap::new();
        let mut next = 0;
        let mut number = |list: &[(u32, &str)], default| {
            let (numbered, default) =
                super::number_devices(&mut numbers, &mut next, listed(list), default);
            let ids: Vec<(u32, String)> = numbered.into_iter().map(|d| (d.id, d.name)).collect();
            (ids, default)
        };
        let (ids, _) = number(&[(0, "Mic"), (1, "HDMI")], None);
        assert_eq!(
            ids,
            [(0, "Mic".into()), (1, "HDMI".into())],
            "by place at first"
        );
        // A capture box plugged in lists first; the default is HDMI.
        let (ids, default) = number(&[(0, "USB Capture"), (1, "Mic"), (2, "HDMI")], Some(2));
        assert_eq!(
            ids,
            [
                (2, "USB Capture".into()),
                (0, "Mic".into()),
                (1, "HDMI".into())
            ]
        );
        assert_eq!(default, Some(1), "HDMI, by its number");
        // HDMI unplugged, then another device plugged in: it does not take
        // HDMI's number; HDMI back keeps it.
        number(&[(0, "USB Capture"), (1, "Mic")], None);
        let (ids, _) = number(&[(0, "USB Capture"), (1, "Mic"), (2, "Scarlett")], None);
        assert_eq!(ids[2], (3, "Scarlett".into()));
        let (ids, _) = number(&[(0, "HDMI"), (1, "Mic")], None);
        assert_eq!(ids, [(1, "HDMI".into()), (0, "Mic".into())]);
        // Two of one name keep theirs by place among them.
        let mut numbers = HashMap::new();
        let mut next = 0;
        let (two, _) = super::number_devices(
            &mut numbers,
            &mut next,
            listed(&[(0, "USB Audio"), (1, "USB Audio")]),
            None,
        );
        let (three, _) = super::number_devices(
            &mut numbers,
            &mut next,
            listed(&[(0, "USB Audio"), (1, "Mic"), (2, "USB Audio")]),
            None,
        );
        assert_eq!(two.iter().map(|d| d.id).collect::<Vec<_>>(), [0, 1]);
        assert_eq!(three.iter().map(|d| d.id).collect::<Vec<_>>(), [0, 2, 1]);
    }

    /// A capture opened anew for a test: no device, the subscribers given.
    fn reopened(name: &str, subs: &super::PcmSubscribers) -> super::ActiveAudioSource {
        super::ActiveAudioSource {
            stream: None,
            feeding: Arc::new(AtomicBool::new(true)),
            name: name.into(),
            liveness: Arc::new(super::Liveness::new()),
            receiver: bounded(1).1,
            latest: AudioData::default(),
            sample_rate_hz: 48_000,
            channels: 2,
            pcm_subs: Arc::clone(subs),
        }
    }

    /// The OS's devices stood in for by `open` (see [`super::Opener`]).
    fn opening_with(
        mgr: &mut AudioManager,
        open: impl Fn(AudioSourceId, &super::PcmSubscribers) -> anyhow::Result<super::ActiveAudioSource>
            + Send
            + Sync
            + 'static,
    ) {
        mgr.opener = Arc::new(move |_, id, subs| open(id, &subs));
    }

    /// A stream stand-in that says when it is closed, closing taking `takes`.
    struct Closing {
        closed: Arc<AtomicBool>,
        takes: Duration,
    }

    impl Drop for Closing {
        fn drop(&mut self) {
            std::thread::sleep(self.takes);
            self.closed.store(true, Ordering::Relaxed);
        }
    }

    /// Within a minute of the devices changing, a capture sending only
    /// zeros for ten seconds is opened again in place — its subscribers
    /// still served — once per change; never without a change; one that
    /// never called back is not silent; one that will not open keeps its
    /// subscribers; one opening in another format is let go.
    #[test]
    fn a_capture_sending_only_zeros_is_opened_again_after_a_change() {
        let mut mgr = AudioManager::listing(&[]);
        let sub = mgr.open_for_test(4, "USB Capture HDMI");
        opening_with(&mut mgr, |_, subs| Ok(reopened("USB Capture HDMI", subs)));
        let again = |mgr: &mut AudioManager| {
            let ids = mgr.reopen_silent();
            mgr.wait_opens_for_test();
            ids
        };
        mgr.quiet_for_test(4, Duration::from_secs(11), true);
        assert!(again(&mut mgr).is_empty(), "no change: merely quiet");
        mgr.devices_changed();
        mgr.quiet_for_test(4, Duration::from_secs(5), true);
        assert!(again(&mut mgr).is_empty(), "five seconds of zeros");
        mgr.quiet_for_test(4, Duration::from_secs(11), true);
        assert_eq!(again(&mut mgr), [4]);
        assert!(mgr.serves(4, sub.token), "its subscriber still served");
        mgr.quiet_for_test(4, Duration::from_secs(11), true);
        assert!(again(&mut mgr).is_empty(), "once per change");
        mgr.devices_changed();
        assert_eq!(again(&mut mgr), [4], "a new change");
        // Long after the change: left as it is.
        mgr.changed_at = Instant::now().checked_sub(Duration::from_secs(61));
        mgr.reopened_since_change.clear();
        mgr.quiet_for_test(4, Duration::from_secs(11), true);
        assert!(again(&mut mgr).is_empty(), "a minute on");
        mgr.devices_changed();
        // A capture that never called back is not silent: lost, if anything.
        let _sub = mgr.open_for_test(5, "Mic");
        mgr.quiet_for_test(5, Duration::from_secs(11), false);
        assert!(!again(&mut mgr).contains(&5));
        // Not opened again: the old capture keeps its subscribers, tried once.
        let line = mgr.open_for_test(7, "HDMI");
        mgr.quiet_for_test(7, Duration::from_secs(11), true);
        let tried = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&tried);
        opening_with(&mut mgr, move |id, _| {
            if id == 7 {
                counted.fetch_add(1, Ordering::Relaxed);
            }
            Err(anyhow::anyhow!("not there just now"))
        });
        again(&mut mgr);
        again(&mut mgr);
        assert_eq!(tried.load(Ordering::Relaxed), 1, "once per change");
        assert!(mgr.serves(7, line.token), "kept, with its subscriber");
        // Opened again in another format: let go, taken afresh.
        let _sub = mgr.open_for_test(6, "Line");
        mgr.quiet_for_test(6, Duration::from_secs(11), true);
        opening_with(&mut mgr, |_, subs| {
            let mut source = reopened("Line", subs);
            source.channels = 16;
            Ok(source)
        });
        assert_eq!(again(&mut mgr), [6]);
        assert!(
            !mgr.active.contains_key(&6),
            "let go, taken afresh in its new format"
        );
    }

    /// Reset: a device that will not open while its old stream runs is
    /// closed first, then opened — its subscribers kept, the sound missed
    /// told; not opened then, it is let go.
    #[test]
    fn a_reset_device_busy_while_open_is_closed_first() {
        let mut mgr = AudioManager::listing(&[]);
        let sub = mgr.open_for_test(3, "hw:Line");
        let closed = Arc::new(AtomicBool::new(false));
        if let Some(source) = mgr.active.get_mut(&3) {
            source.stream = Some(Box::new(Closing {
                closed: Arc::clone(&closed),
                takes: Duration::ZERO,
            }));
        }
        let tries = Arc::new(AtomicUsize::new(0));
        let (old_closed, counted) = (Arc::clone(&closed), Arc::clone(&tries));
        opening_with(&mut mgr, move |_, subs| {
            counted.fetch_add(1, Ordering::Relaxed);
            anyhow::ensure!(old_closed.load(Ordering::Relaxed), "device busy");
            std::thread::sleep(Duration::from_millis(50));
            Ok(reopened("hw:Line", subs))
        });
        assert!(mgr.reopen(3));
        mgr.wait_opens_for_test();
        assert_eq!(
            tries.load(Ordering::Relaxed),
            2,
            "beside it, then closed first"
        );
        assert!(mgr.serves(3, sub.token), "its subscriber kept");
        let lost = sub.lost_samples.load(Ordering::Relaxed);
        assert!(
            lost >= 48_000 * 2 / 20,
            "50 ms closed, told as lost: {lost} samples"
        );
        assert_eq!(lost % 2, 0, "whole frames of both channels");
        opening_with(&mut mgr, |_, _| Err(anyhow::anyhow!("gone")));
        assert!(mgr.reopen(3));
        mgr.wait_opens_for_test();
        assert!(
            !mgr.active.contains_key(&3),
            "let go, taken afresh once back"
        );
    }

    /// Opening a capture takes the OS ~0.1 s and closing one ~20 ms: the
    /// show's frames wait for neither. Asked for, a capture opens on a
    /// thread of its own, once, and is taken when open; opened again, the
    /// same; let go, it is closed on a thread of its own.
    #[test]
    fn captures_open_and_close_off_the_shows_frames() {
        let mut mgr = AudioManager::listing(&["Slow Mic"]);
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        opening_with(&mut mgr, move |_, subs| {
            counted.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(300));
            Ok(reopened("Slow Mic", subs))
        });
        let asked = Instant::now();
        assert!(mgr.subscribe_pcm_soon(0).is_none(), "opening");
        assert!(mgr.subscribe_pcm_soon(0).is_none(), "still opening");
        assert!(!mgr.poll_opens(), "not open yet");
        assert!(
            asked.elapsed() < Duration::from_millis(150),
            "not waited for"
        );
        let landed = mgr.landed();
        while !mgr.poll_opens() {
            assert!(asked.elapsed() < Duration::from_secs(5), "never opened");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(mgr.landed(), landed + 1);
        assert_eq!(opens.load(Ordering::Relaxed), 1, "opened once");
        let sub = mgr.subscribe_pcm_soon(0).expect("open");
        // Opened again: not waited for either, its subscriber still served.
        mgr.devices_changed();
        mgr.quiet_for_test(0, Duration::from_secs(11), true);
        let again = Instant::now();
        assert_eq!(mgr.reopen_silent(), [0]);
        assert!(
            again.elapsed() < Duration::from_millis(150),
            "not waited for"
        );
        mgr.wait_opens_for_test();
        assert!(mgr.serves(0, sub.token));
        // Let go: closed on a thread of its own.
        let closed = Arc::new(AtomicBool::new(false));
        if let Some(source) = mgr.active.get_mut(&0) {
            source.stream = Some(Box::new(Closing {
                closed: Arc::clone(&closed),
                takes: Duration::from_millis(300),
            }));
        }
        let close = Instant::now();
        mgr.unsubscribe_pcm(0, sub.token);
        assert!(!mgr.active.contains_key(&0), "no one listens: closed");
        assert!(
            close.elapsed() < Duration::from_millis(150),
            "not waited for"
        );
        while !closed.load(Ordering::Relaxed) {
            assert!(close.elapsed() < Duration::from_secs(5), "never closed");
            std::thread::sleep(Duration::from_millis(10));
        }
        // Its device gone: let go, closed on a thread of its own too.
        let _sub = mgr.open_for_test(1, "Gone Mic");
        let gone = Arc::new(AtomicBool::new(false));
        if let Some(source) = mgr.active.get_mut(&1) {
            source.stream = Some(Box::new(Closing {
                closed: Arc::clone(&gone),
                takes: Duration::from_millis(300),
            }));
        }
        mgr.lose_for_test(1);
        let lost = Instant::now();
        assert_eq!(mgr.forget_lost(), ["Gone Mic"]);
        assert!(
            lost.elapsed() < Duration::from_millis(150),
            "not waited for"
        );
    }

    /// One that must have its capture at once — an output starting — while
    /// it is being opened waits for that opening: not opened twice.
    #[test]
    fn a_capture_asked_for_at_once_while_opening_is_opened_once() {
        let mut mgr = AudioManager::listing(&["Mic"]);
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        opening_with(&mut mgr, move |_, subs| {
            counted.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(100));
            Ok(reopened("Mic", subs))
        });
        assert!(mgr.subscribe_pcm_soon(0).is_none(), "opening");
        assert!(mgr.subscribe_pcm(0).is_some(), "waited for");
        assert_eq!(opens.load(Ordering::Relaxed), 1, "opened once");
    }

    /// A stream stand-in for source `id` that says when it is closed,
    /// closing taking `takes`.
    fn closing_in(mgr: &mut AudioManager, id: AudioSourceId, takes: Duration) -> Arc<AtomicBool> {
        let closed = Arc::new(AtomicBool::new(false));
        if let Some(source) = mgr.active.get_mut(&id) {
            source.stream = Some(Box::new(Closing {
                closed: Arc::clone(&closed),
                takes,
            }));
        }
        closed
    }

    /// Opened again, a capture takes its subscribers over at once but feeds
    /// them only once the old stream has stopped: never two streams feeding
    /// one subscriber.
    #[test]
    fn a_capture_opened_again_feeds_only_once_the_old_has_stopped() {
        let mut mgr = AudioManager::listing(&[]);
        let sub = mgr.open_for_test(4, "HDMI");
        let closed = closing_in(&mut mgr, 4, Duration::from_millis(200));
        opening_with(&mut mgr, |_, subs| Ok(reopened("HDMI", subs)));
        assert!(mgr.reopen(4));
        mgr.wait_opens_for_test();
        assert!(mgr.serves(4, sub.token), "taken over at once");
        let feeding = Arc::clone(&mgr.active[&4].feeding);
        assert!(!feeding.load(Ordering::Relaxed), "not while the old runs");
        let since = Instant::now();
        while !feeding.load(Ordering::Relaxed) {
            assert!(since.elapsed() < Duration::from_secs(5), "never fed");
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(closed.load(Ordering::Relaxed), "the old stopped first");
    }

    /// A capture being opened again — closed first for a Reset, quiet until
    /// it opens — is not let go as lost meanwhile.
    #[test]
    fn a_capture_being_opened_again_is_not_taken_for_lost() {
        let mut mgr = AudioManager::listing(&[]);
        let sub = mgr.open_for_test(3, "Line");
        opening_with(&mut mgr, |_, subs| {
            std::thread::sleep(Duration::from_millis(100));
            Ok(reopened("Line", subs))
        });
        assert!(mgr.reopen(3));
        mgr.lose_for_test(3);
        assert!(mgr.forget_lost().is_empty(), "being opened again");
        mgr.wait_opens_for_test();
        assert!(mgr.serves(3, sub.token), "its subscriber kept");
    }

    /// One taking a capture while it is closed and opened again (a Reset)
    /// waits for it — or, one that must not wait, is told to ask again —
    /// and is charged for no sound missed before it came.
    #[test]
    fn one_taking_a_capture_closed_for_a_reset_waits_and_owes_nothing() {
        let mut mgr = AudioManager::listing(&[]);
        let _first = mgr.open_for_test(3, "hw:Line");
        let closed = closing_in(&mut mgr, 3, Duration::ZERO);
        opening_with(&mut mgr, move |_, subs| {
            anyhow::ensure!(closed.load(Ordering::Relaxed), "device busy");
            std::thread::sleep(Duration::from_millis(100));
            Ok(reopened("hw:Line", subs))
        });
        assert!(mgr.reopen(3));
        let since = Instant::now();
        while !mgr.closed_to_open_again(3) {
            assert!(
                since.elapsed() < Duration::from_secs(5),
                "never closed first"
            );
            mgr.poll_opens();
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(mgr.subscribe_pcm_soon(3).is_none(), "asked again once open");
        let landed = mgr.landed();
        let late = mgr.subscribe_pcm(3).expect("waited for");
        mgr.wait_opens_for_test();
        assert_eq!(late.lost_samples.load(Ordering::Relaxed), 0);
        assert!(mgr.landed() > landed, "the mixer asks again at once");
    }

    /// A capture being opened again whose last listener lets go meanwhile
    /// is taken by one asking for it at once, not let go with the old.
    #[test]
    fn a_capture_opened_again_for_no_one_is_taken_by_whoever_asks_meanwhile() {
        let mut mgr = AudioManager::listing(&[]);
        let sub = mgr.open_for_test(4, "HDMI");
        opening_with(&mut mgr, |_, subs| {
            std::thread::sleep(Duration::from_millis(100));
            Ok(reopened("HDMI", subs))
        });
        assert!(mgr.reopen(4));
        mgr.unsubscribe_pcm(4, sub.token);
        assert!(!mgr.active.contains_key(&4), "let go");
        assert!(mgr.subscribe_pcm(4).is_some(), "taken as it opened");
    }

    /// A stream stand-in for a device that opens once at a time: counted
    /// in `live` until it is closed, closing taking `takes`.
    struct Exclusive {
        live: Arc<AtomicUsize>,
        takes: Duration,
    }

    impl Drop for Exclusive {
        fn drop(&mut self) {
            std::thread::sleep(self.takes);
            self.live.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// A device is opened again only once its stream let go has closed —
    /// whether it is asked for soon or at once: one that opens once at a
    /// time refused it otherwise.
    #[test]
    fn a_device_is_opened_again_only_once_its_old_stream_has_closed() {
        let mut mgr = AudioManager::listing(&["hw:Line"]);
        let live = Arc::new(AtomicUsize::new(0));
        let device = Arc::clone(&live);
        opening_with(&mut mgr, move |_, subs| {
            anyhow::ensure!(device.load(Ordering::Relaxed) == 0, "device busy");
            device.fetch_add(1, Ordering::Relaxed);
            let mut source = reopened("hw:Line", subs);
            source.stream = Some(Box::new(Exclusive {
                live: Arc::clone(&device),
                takes: Duration::from_millis(200),
            }));
            Ok(source)
        });
        assert!(mgr.subscribe_pcm_soon(0).is_none(), "opening");
        mgr.wait_opens_for_test();
        let first = mgr.subscribe_pcm_soon(0).expect("open");
        mgr.unsubscribe_pcm(0, first.token);
        assert_eq!(live.load(Ordering::Relaxed), 1, "still closing");
        assert!(mgr.subscribe_pcm_soon(0).is_none(), "opening again");
        mgr.wait_opens_for_test();
        let again = mgr.subscribe_pcm_soon(0).expect("opened once closed");
        mgr.unsubscribe_pcm(0, again.token);
        assert_eq!(live.load(Ordering::Relaxed), 1, "still closing");
        assert!(mgr.subscribe_pcm(0).is_some(), "at once, once closed");
    }

    /// One that must have a capture at once, asked while it was being
    /// opened again for the last listener, who let go meanwhile — the
    /// device, opening once at a time, refusing that while its old stream
    /// ran — has it opened afresh once the old has closed.
    #[test]
    fn a_capture_asked_for_after_a_refused_reopen_is_opened_afresh() {
        let mut mgr = AudioManager::listing(&["hw:Line"]);
        let live = Arc::new(AtomicUsize::new(0));
        let device = Arc::clone(&live);
        opening_with(&mut mgr, move |_, subs| {
            std::thread::sleep(Duration::from_millis(50));
            anyhow::ensure!(device.load(Ordering::Relaxed) == 0, "device busy");
            device.fetch_add(1, Ordering::Relaxed);
            let mut source = reopened("hw:Line", subs);
            source.stream = Some(Box::new(Exclusive {
                live: Arc::clone(&device),
                takes: Duration::from_millis(100),
            }));
            Ok(source)
        });
        assert!(mgr.subscribe_pcm_soon(0).is_none());
        mgr.wait_opens_for_test();
        let first = mgr.subscribe_pcm_soon(0).expect("open");
        assert!(mgr.reopen(0), "opened again beside its stream: refused");
        mgr.unsubscribe_pcm(0, first.token);
        assert!(
            mgr.subscribe_pcm(0).is_some(),
            "afresh, once the old closed"
        );
    }

    /// A device that would not open is looked for again from when it was
    /// asked for, not from when the answer came: asked every `OPEN_RETRY`, as
    /// the mixer asks, it is not put off twice as long.
    #[test]
    fn a_device_that_would_not_open_is_looked_for_again_from_when_it_was_asked_for() {
        let mut mgr = AudioManager::listing(&["Mic"]);
        opening_with(&mut mgr, |_, _| {
            std::thread::sleep(Duration::from_millis(100));
            Err(anyhow::anyhow!("not there"))
        });
        let asked = Instant::now();
        assert!(mgr.subscribe_pcm_soon(0).is_none());
        mgr.wait_opens_for_test();
        assert!(
            mgr.failed[&0] < asked + Duration::from_millis(50),
            "noted as asked for"
        );
    }

    /// A capture opened afresh and not asked for since — the input that
    /// asked removed while it opened — is closed after a while; one asked
    /// for again in time is kept.
    #[test]
    fn a_capture_no_one_takes_once_open_is_closed() {
        let mut mgr = AudioManager::listing(&["Mic"]);
        opening_with(&mut mgr, |_, subs| Ok(reopened("Mic", subs)));
        assert!(mgr.subscribe_pcm_soon(0).is_none());
        mgr.wait_opens_for_test();
        assert!(mgr.active.contains_key(&0));
        mgr.close_unclaimed(Instant::now() + super::UNCLAIMED / 2);
        assert!(mgr.active.contains_key(&0), "waits for whoever asked");
        mgr.close_unclaimed(Instant::now() + super::UNCLAIMED * 2);
        assert!(!mgr.active.contains_key(&0), "no one did: closed");
        assert!(mgr.subscribe_pcm_soon(0).is_none());
        mgr.wait_opens_for_test();
        let _sub = mgr.subscribe_pcm_soon(0).expect("open");
        mgr.close_unclaimed(Instant::now() + super::UNCLAIMED * 2);
        assert!(mgr.active.contains_key(&0), "taken: kept");
    }

    /// Sound missed is counted in whole frames of every channel.
    #[test]
    fn sound_missed_is_whole_frames() {
        let gap = Duration::from_micros(50_020);
        assert_eq!(super::missed_samples(gap, 48_000, 2), 2 * 2401);
        assert_eq!(super::missed_samples(gap, 48_000, 16), 16 * 2401);
    }

    /// A device listed under a stand-in name is found by its place, whatever
    /// its number.
    #[test]
    fn a_device_under_a_stand_in_name_is_found_by_its_place() {
        let listed = listed(&[(0, "Mic"), (1, "Audio Input 0")]);
        let now = ["Audio Input 0".to_string(), "Mic".to_string()];
        assert_eq!(super::now_at(&listed, 1, &now), Some(0));
        assert_eq!(super::now_at(&listed, 0, &now), Some(1));
    }

    /// A capture whose device went — macOS said so, or its stream stopped
    /// calling back — is let go, its subscribers told; one still running is
    /// kept. Kept, a dead stream held its listeners silent for good.
    #[test]
    fn a_capture_whose_device_went_is_let_go() {
        use super::AudioManager;
        use std::time::Duration;
        let mut mgr = AudioManager::listing(&["Scarlett", "BlackHole", "USB Mixer", "AirPods"]);
        let said = mgr.open_for_test(0, "Scarlett");
        let quiet = mgr.open_for_test(1, "BlackHole");
        let fine = mgr.open_for_test(2, "USB Mixer");
        let starting = mgr.open_for_test(3, "AirPods");
        assert!(mgr.forget_lost().is_empty(), "all running");
        mgr.lose_for_test(0);
        mgr.quiet_for_test(1, Duration::from_secs(3), true);
        // A Bluetooth microphone slow to start is given its time.
        mgr.quiet_for_test(3, Duration::from_secs(3), false);
        let mut lost = mgr.forget_lost();
        lost.sort();
        assert_eq!(lost, ["BlackHole", "Scarlett"]);
        assert!(!mgr.serves(0, said.token) && !mgr.serves(1, quiet.token));
        assert!(mgr.serves(2, fine.token), "the one still running stays");
        assert!(mgr.serves(3, starting.token), "and the one starting");
        mgr.quiet_for_test(3, Duration::from_secs(11), false);
        assert_eq!(mgr.forget_lost(), ["AirPods"], "but not for ever");
        assert!(
            matches!(
                said.receiver.try_recv(),
                Err(crossbeam_channel::TryRecvError::Disconnected)
            ),
            "its listeners hear it went"
        );
    }

    /// A source pinned open whose device went is looked for again — every
    /// two seconds, not every frame: let go, it had no other way back.
    #[test]
    fn a_pinned_source_whose_device_went_is_looked_for_again() {
        use super::AudioManager;
        use std::time::{Duration, Instant};
        let mut mgr = AudioManager::listing(&["Scarlett"]);
        mgr.manual_pins.insert(0);
        let _sub = mgr.open_for_test(0, "Scarlett");
        mgr.lose_for_test(0);
        assert_eq!(mgr.forget_lost(), ["Scarlett"]);
        let lost_at = mgr.failed[&0];
        // No Scarlett here: each look fails — and is noted, to be tried
        // again later.
        opening_with(&mut mgr, |_, _| Err(anyhow::anyhow!("not here")));
        mgr.reopen_pins();
        mgr.wait_opens_for_test();
        assert_eq!(mgr.failed[&0], lost_at, "not looked for again at once");
        let long_ago = Instant::now()
            .checked_sub(Duration::from_secs(3))
            .expect("a time that far back");
        mgr.failed.insert(0, long_ago);
        mgr.reopen_pins();
        mgr.wait_opens_for_test();
        assert!(mgr.failed[&0] > long_ago, "looked for again");
    }

    /// Listing sound inputs that takes longer than the startup will wait —
    /// macOS asking for the microphone — does not hold the startup up: the
    /// listing arrives later.
    #[test]
    fn a_slow_listing_does_not_hold_up_the_start() {
        let started = std::time::Instant::now();
        let slow = super::within(std::time::Duration::from_millis(50), || {
            std::thread::sleep(std::time::Duration::from_millis(300));
            7
        });
        assert!(started.elapsed() < std::time::Duration::from_millis(200));
        let later = slow.expect_err("not within the wait");
        assert_eq!(
            later.recv_timeout(std::time::Duration::from_secs(2)).ok(),
            Some(7),
            "it arrives later"
        );
        let quick = super::within(std::time::Duration::from_secs(2), || 3);
        assert_eq!(quick.ok(), Some(3));
    }
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn hann_window_shape() {
        let window: Vec<f32> = (0..FFT_SIZE)
            .map(|i| {
                0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos())
            })
            .collect();
        // Endpoints should be ~0
        assert!(window[0].abs() < 1e-6, "Hann window start should be ~0");
        assert!(
            window[FFT_SIZE - 1].abs() < 1e-6,
            "Hann window end should be ~0"
        );
        // Middle should be ~1
        let mid = window[FFT_SIZE / 2];
        assert!(
            (mid - 1.0).abs() < 0.01,
            "Hann window midpoint should be ~1.0, got {mid}"
        );
        // Should be symmetric
        for i in 0..FFT_SIZE / 2 {
            assert!(
                (window[i] - window[FFT_SIZE - 1 - i]).abs() < 1e-6,
                "Hann window should be symmetric at index {i}"
            );
        }
    }

    #[test]
    fn bin_resolution_at_48khz() {
        let data = AudioData {
            fft: vec![0.0; FFT_SIZE / 2],
            sample_rate: 48000.0,
            ..AudioData::default()
        };
        let bw = data.bin_width();
        // 48000 / 2048 = 23.4375 Hz/bin
        assert!(
            (bw - 23.4375).abs() < 0.01,
            "Bin width should be ~23.4Hz, got {bw}"
        );
        // Bass range (20-250Hz) should span ~10 bins
        let bass_bins = (250.0 / bw).ceil() as usize - (20.0 / bw).floor() as usize;
        assert!(
            bass_bins >= 9,
            "Bass range should span >=9 bins, got {bass_bins}"
        );
    }

    #[test]
    fn spectral_flux_detects_onset_not_steady_state() {
        // Steady-state: identical frames produce zero flux
        let frame_a = vec![0.1_f32; FFT_SIZE / 2];
        let flux_steady: f32 = frame_a
            .iter()
            .zip(frame_a.iter())
            .map(|(c, p)| (c - p).max(0.0))
            .sum();
        assert!(
            flux_steady.abs() < 1e-6,
            "Steady-state spectral flux should be ~0"
        );

        // Onset: sharp increase produces positive flux
        let frame_b: Vec<f32> = vec![0.5; FFT_SIZE / 2];
        let flux_onset: f32 = frame_b
            .iter()
            .zip(frame_a.iter())
            .map(|(c, p)| (c - p).max(0.0))
            .sum();
        assert!(flux_onset > 0.0, "Onset spectral flux should be positive");

        // Decrease: energy drop produces zero flux (half-wave rectified)
        let flux_decrease: f32 = frame_a
            .iter()
            .zip(frame_b.iter())
            .map(|(c, p)| (c - p).max(0.0))
            .sum();
        assert!(
            flux_decrease.abs() < 1e-6,
            "Energy decrease should produce zero flux"
        );
    }

    #[test]
    fn bpm_outlier_rejection() {
        // Simulate beat intervals: mostly ~0.5s (120 BPM) with one outlier
        let intervals: Vec<f32> = vec![0.50, 0.51, 0.49, 0.50, 0.52, 0.48, 1.2, 0.50];
        let mut sorted = intervals.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];

        let stable: Vec<f32> = intervals
            .iter()
            .filter(|&&iv| (iv - median).abs() / median < TEMPO_TOLERANCE)
            .copied()
            .collect();

        // Outlier (1.2s) should be rejected
        assert!(
            !stable.contains(&1.2),
            "Outlier interval should be rejected"
        );
        assert!(stable.len() >= 6, "Most intervals should survive filtering");

        let avg = stable.iter().sum::<f32>() / stable.len() as f32;
        let bpm = 60.0 / avg;
        assert!((bpm - 120.0).abs() < 5.0, "BPM should be ~120, got {bpm}");
    }

    #[test]
    fn bpm_stability_consistent_beats() {
        // All consistent intervals at 128 BPM (0.46875s)
        let interval = 60.0 / 128.0;
        let intervals: Vec<f32> = vec![interval; BPM_HISTORY_SIZE];
        let mut sorted = intervals.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let median = sorted[sorted.len() / 2];

        let stable: Vec<f32> = intervals
            .iter()
            .filter(|&&iv| (iv - median).abs() / median < TEMPO_TOLERANCE)
            .copied()
            .collect();

        assert_eq!(
            stable.len(),
            BPM_HISTORY_SIZE,
            "All consistent intervals should pass"
        );
        let avg = stable.iter().sum::<f32>() / stable.len() as f32;
        let bpm = 60.0 / avg;
        assert!(
            (bpm - 128.0).abs() < 0.1,
            "BPM should be exactly ~128, got {bpm}"
        );
    }

    #[test]
    fn energy_in_range_with_new_fft_size() {
        // With 2048-point FFT at 48kHz, bin width is ~23.4Hz
        // Create synthetic FFT with energy only in bins 2-4 (~47-94Hz)
        let mut fft = vec![0.0_f32; FFT_SIZE / 2];
        fft[2] = 0.5;
        fft[3] = 0.5;
        fft[4] = 0.5;

        let data = AudioData {
            fft,
            sample_rate: 48000.0,
            waveform: vec![0.0; AUDIO_BUFFER_SIZE],
            level: 0.0,
            bpm: None,
            time_since_beat: 0.0,
        };

        // Energy in the active range should be non-zero
        let energy = data.energy_in_range(40.0, 100.0);
        assert!(energy > 0.0, "Should detect energy in 40-100Hz range");

        // Energy outside the active range should be zero
        let energy_high = data.energy_in_range(5000.0, 10000.0);
        assert!(
            energy_high < 1e-6,
            "Should detect no energy in 5k-10kHz range"
        );
    }

    // ── Chaos Tests Round 2: Audio frequency edge cases ─────────────────

    #[test]
    fn chaos_energy_in_range_nan_freq_low() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(f32::NAN, 1000.0);
        // NaN / bw → NaN, floor as usize → 0 (saturating), .min(len-1) → 0
        // Must not panic
        assert!(
            val.is_finite() || val == 0.0,
            "NaN freq_low should not crash"
        );
    }

    #[test]
    fn chaos_energy_in_range_nan_freq_high() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(100.0, f32::NAN);
        let _ = val; // must not panic
    }

    #[test]
    fn chaos_energy_in_range_both_nan() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(f32::NAN, f32::NAN);
        let _ = val; // must not panic
    }

    #[test]
    fn chaos_energy_in_range_negative_frequencies() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(-1000.0, -500.0);
        // Negative / bw → negative, floor as usize → 0 (saturating)
        assert!(
            val >= 0.0,
            "negative freq should not produce negative energy"
        );
    }

    #[test]
    fn chaos_energy_in_range_infinity() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(0.0, f32::INFINITY);
        let _ = val; // must not panic
    }

    #[test]
    fn chaos_energy_in_range_inverted_range() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(5000.0, 100.0);
        assert_eq!(val, 0.0, "inverted range should return 0.0");
    }

    #[test]
    fn chaos_energy_in_range_zero_sample_rate() {
        let data = AudioData {
            waveform: vec![0.0; 128],
            fft: vec![0.5; 1024],
            level: 0.5,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 0.0,
        };
        let val = data.energy_in_range(100.0, 1000.0);
        // bin_width = 0 / 2048 = 0, guarded by bw <= 0.0 check
        assert_eq!(val, 0.0, "zero sample rate should return 0.0");
    }

    #[test]
    fn chaos_energy_in_range_empty_fft() {
        let data = AudioData {
            waveform: vec![],
            fft: vec![],
            level: 0.0,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(20.0, 20000.0);
        assert_eq!(val, 0.0, "empty FFT should return 0.0");
    }

    // ── Phase 19a: PCM passthrough tap ─────────────────────────────────

    /// A subscriber plus its receiver, drop counter and lost-sample counter.
    fn make_sub(cap: usize) -> (PcmSubscriber, Receiver<PcmChunk>, Arc<AtomicU64>) {
        let (sub, rx, dropped, _) = make_sub_counted(cap);
        (sub, rx, dropped)
    }

    fn make_sub_counted(
        cap: usize,
    ) -> (
        PcmSubscriber,
        Receiver<PcmChunk>,
        Arc<AtomicU64>,
        Arc<AtomicU64>,
    ) {
        let (sender, receiver) = bounded::<PcmChunk>(cap);
        let dropped = Arc::new(AtomicU64::new(0));
        let lost_samples = Arc::new(AtomicU64::new(0));
        let sub = PcmSubscriber {
            token: NEXT_PCM_TOKEN.fetch_add(1, Ordering::Relaxed),
            sender,
            dropped: dropped.clone(),
            lost_samples: lost_samples.clone(),
            pending_lost: Arc::default(),
            live: None,
        };
        (sub, receiver, dropped, lost_samples)
    }

    #[test]
    fn pcm_fan_out_delivers_to_all_subscribers() {
        let (first, rx1, _) = make_sub(4);
        let (second, rx2, _) = make_sub(4);
        let subs = vec![first, second];
        let samples = vec![0.1, -0.2, 0.3, -0.4];

        fan_out_pcm(&subs, &samples);

        assert_eq!(rx1.try_recv().unwrap().samples, samples);
        assert_eq!(rx2.try_recv().unwrap().samples, samples);
    }

    #[test]
    fn pcm_fan_out_counts_drops_when_full() {
        // Capacity 2, receiver never drains: 5 sends → 2 buffered, 3 dropped.
        let (sub, _rx, dropped) = make_sub(2);
        let subs = vec![sub];
        let samples = vec![0.0; 8];

        for _ in 0..5 {
            fan_out_pcm(&subs, &samples);
        }

        assert_eq!(dropped.load(Ordering::Relaxed), 3);
    }

    /// A drop must record how much *time* was lost, not just that one happened.
    ///
    /// Consumers time the PCM stream by counting samples, so the only way to
    /// survive a dropout is to put back exactly as many samples as went
    /// missing. A chunk count cannot say that — chunks vary in length — so the
    /// sample total is what the writer needs.
    #[test]
    fn pcm_fan_out_records_how_many_samples_were_lost() {
        // Capacity 2, receiver never drains: 5 sends of 8 samples → 2 buffered,
        // 3 dropped, so 24 samples owed back to the stream.
        let (sub, _rx, dropped, lost) = make_sub_counted(2);
        let subs = vec![sub];

        for _ in 0..5 {
            fan_out_pcm(&subs, &[0.0; 8]);
        }

        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        assert_eq!(lost.load(Ordering::Relaxed), 24);
    }

    /// What a full channel threw away is told with the next chunk that gets
    /// through, so silence goes there — after the chunks queued before it.
    #[test]
    fn pcm_fan_out_tells_the_next_chunk_what_was_lost_before_it() {
        let (sub, rx, _) = make_sub(1);
        let subs = vec![sub];
        fan_out_pcm(&subs, &[0.1; 4]);
        fan_out_pcm(&subs, &[0.2; 6]);
        fan_out_pcm(&subs, &[0.3; 8]);
        let first = rx.try_recv().expect("the first got through");
        assert_eq!(first.lost_before, 0);
        fan_out_pcm(&subs, &[0.4; 4]);
        let next = rx.try_recv().expect("room again");
        assert_eq!(next.samples, vec![0.4; 4]);
        assert_eq!(next.lost_before, 14, "the two thrown away, before it");
    }

    #[test]
    fn pcm_fan_out_skips_disconnected_without_counting_drops() {
        let (sub, rx, dropped) = make_sub(2);
        drop(rx); // receiver gone → Disconnected, not Full
        let subs = vec![sub];

        fan_out_pcm(&subs, &[0.0; 4]);

        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    /// Small callbacks (a live bus's 64 frames) reach the chunk subscribers
    /// gathered to a chunk; a live feed takes each at once; a callback as
    /// long as a chunk goes out whole.
    #[test]
    fn small_callbacks_are_gathered_into_chunks_and_live_feeds_take_each() {
        let (chunked, rx, _) = make_sub(8);
        let (mut live_sub, _, _) = make_sub(8);
        let (tx, live_rx) = bounded(64);
        live_sub.live = Some(LiveFrames {
            pairs: vec![(0, 1)],
            channels: 2,
            tx,
            rx: live_rx.clone(),
            counts: Arc::default(),
        });
        let subs = vec![chunked, live_sub];
        let mut gathered = Gathered::new(8, 28, 2, Arc::new(ArcSwap::from_pointee(Vec::new())));
        gathered.tee(&subs, [0.1_f32; 4].into_iter());
        assert!(rx.try_recv().is_err(), "half a chunk: held");
        assert_eq!(live_rx.len(), 2, "the live feed: its two frames at once");
        gathered.tee(&subs, [0.2_f32; 4].into_iter());
        let chunk = rx.try_recv().expect("a whole chunk");
        assert_eq!(chunk.samples, [[0.1_f32; 4], [0.2; 4]].concat());
        assert_eq!(live_rx.len(), 4);
        assert!(gathered.samples.is_empty());
        gathered.tee(&subs, [0.3_f32; 12].into_iter());
        assert_eq!(rx.try_recv().expect("whole").samples.len(), 12);
        assert_eq!(live_rx.len(), 10, "fed once, not again with the chunk");
    }

    /// A callback bigger than room was made for goes in pieces, whole
    /// frames each, every one to the live feed too: the gathering never
    /// grows (an allocation in the device's callback).
    #[test]
    fn a_callback_bigger_than_the_room_goes_in_pieces() {
        let (chunked, rx, _) = make_sub(8);
        let (mut live_sub, _, _) = make_sub(8);
        let (tx, live_rx) = bounded(64);
        live_sub.live = Some(LiveFrames {
            pairs: vec![(0, 1)],
            channels: 2,
            tx,
            rx: live_rx.clone(),
            counts: Arc::default(),
        });
        let subs = vec![chunked, live_sub];
        let mut gathered = Gathered::new(8, 4, 2, Arc::new(ArcSwap::from_pointee(Vec::new())));
        let room = gathered.samples.capacity();
        gathered.tee(&subs, (0..40).map(|i| i as f32));
        let pieces: Vec<Vec<f32>> = rx.try_iter().map(|c| c.samples).collect();
        assert_eq!(pieces.iter().map(Vec::len).collect::<Vec<_>>(), [16, 16, 8]);
        assert_eq!(
            pieces.concat(),
            (0..40).map(|i| i as f32).collect::<Vec<_>>(),
            "in order"
        );
        assert_eq!(live_rx.len(), 20, "every frame to the live feed, once");
        assert_eq!(gathered.samples.capacity(), room, "never grown");
    }

    /// A stream gone — closed, or reopened with its subscribers kept — hands
    /// them what it had gathered: nothing goes missing uncounted.
    #[test]
    fn what_was_gathered_goes_out_when_the_stream_goes() {
        let (chunked, rx, _) = make_sub(8);
        let subs = Arc::new(ArcSwap::from_pointee(vec![chunked]));
        let mut gathered = Gathered::new(8, 28, 2, Arc::clone(&subs));
        gathered.tee(&subs.load(), [0.4_f32; 4].into_iter());
        assert!(rx.try_recv().is_err(), "half a chunk: held");
        drop(gathered);
        assert_eq!(
            rx.try_recv().expect("the rest, handed over").samples,
            [0.4_f32; 4]
        );
    }

    #[test]
    fn pcm_fan_out_empty_is_noop() {
        let subs: Vec<PcmSubscriber> = Vec::new();
        fan_out_pcm(&subs, &[0.0; 4]); // must not panic
    }

    fn dev(id: AudioSourceId, name: &str) -> AudioDeviceInfo {
        AudioDeviceInfo {
            id,
            name: name.to_string(),
        }
    }

    #[test]
    fn pick_default_input_prefers_os_default_by_name() {
        let devices = vec![dev(0, "BlackHole 2ch"), dev(1, "MacBook Pro Microphone")];
        // OS default is the mic (id 1), even though BlackHole enumerates first.
        assert_eq!(
            AudioManager::pick_default_input(Some("MacBook Pro Microphone"), &devices),
            Some(1)
        );
    }

    #[test]
    fn pick_default_input_falls_back_to_first_when_no_default() {
        let devices = vec![dev(0, "BlackHole 2ch"), dev(1, "MacBook Pro Microphone")];
        assert_eq!(AudioManager::pick_default_input(None, &devices), Some(0));
    }

    #[test]
    fn pick_default_input_falls_back_to_first_when_default_absent() {
        let devices = vec![dev(0, "BlackHole 2ch"), dev(1, "MacBook Pro Microphone")];
        // Default reported by the OS isn't in the scanned list → first device.
        assert_eq!(
            AudioManager::pick_default_input(Some("USB Interface"), &devices),
            Some(0)
        );
    }

    #[test]
    fn pick_default_input_none_when_no_devices() {
        assert_eq!(
            AudioManager::pick_default_input(Some("MacBook Pro Microphone"), &[]),
            None
        );
        assert_eq!(AudioManager::pick_default_input(None, &[]), None);
    }

    fn names(of: &[&str]) -> Vec<String> {
        of.iter().map(|n| (*n).to_string()).collect()
    }

    /// A listing as the OS makes it: ids by place.
    pub(super) fn info(of: &[&str]) -> Vec<AudioDeviceInfo> {
        of.iter()
            .enumerate()
            .map(|(i, n)| AudioDeviceInfo {
                id: i as AudioSourceId,
                name: (*n).to_string(),
            })
            .collect()
    }

    /// A device plugged in comes first in the OS's list — a USB capture
    /// box's sound — and an input listed before it still opens its own
    /// device, not the one now in its place.
    #[test]
    fn a_device_plugged_in_in_front_takes_no_inputs_place() {
        let listed = info(&["Scarlett Solo 4th Gen", "ZoomAudioDevice"]);
        let now = names(&[
            "Blackmagic Design",
            "Scarlett Solo 4th Gen",
            "ZoomAudioDevice",
        ]);
        assert_eq!(
            now_at(&listed, 0, &now),
            Some(1),
            "the Scarlett, not the Blackmagic"
        );
        assert_eq!(now_at(&listed, 1, &now), Some(2));
        assert_eq!(now_at(&listed, 2, &now), None, "never listed");
    }

    /// Of two inputs with one name, each opens the one in its place among
    /// them; one no longer there opens nothing; one listed under a stand-in
    /// name, its description unreadable then, opens the device in its place.
    #[test]
    fn twins_keep_their_places_and_a_stand_in_its_place() {
        let listed = info(&["USB Audio", "USB Audio"]);
        let now = names(&["Mic", "USB Audio", "USB Audio"]);
        assert_eq!(now_at(&listed, 0, &now), Some(1));
        assert_eq!(now_at(&listed, 1, &now), Some(2));
        assert_eq!(now_at(&listed, 1, &names(&["Mic", "USB Audio"])), None);
        let stand_in = info(&["Mic", "Audio Input 1"]);
        assert_eq!(
            now_at(&stand_in, 1, &names(&["Mic", "Readable Now"])),
            Some(1)
        );
    }

    /// A modulator's device that would not open is not looked for again
    /// every frame — listing the OS's devices is slow — but after a while;
    /// one never listed is not looked for at all.
    #[test]
    fn a_device_that_would_not_open_is_tried_again_after_a_while() {
        let mut mgr = AudioManager::listing(&["Scarlett Solo 4th Gen"]);
        let at = Instant::now();
        mgr.failed.insert(0, at);
        mgr.set_modulation_refs(&BTreeSet::from([0]));
        assert_eq!(mgr.failed.get(&0), Some(&at), "not tried this frame");
        assert!(mgr.opening.is_empty(), "not being opened");
        assert!(tried_lately(&mgr.failed, 0, at + OPEN_RETRY / 2));
        assert!(!tried_lately(&mgr.failed, 0, at + OPEN_RETRY));
        assert!(!tried_lately(&mgr.failed, 1, at), "another device");
        mgr.open_soon(5);
        assert!(mgr.opening.is_empty(), "never listed: not looked for");
        assert!(
            super::open_device(&mgr.devices, 5, super::no_subscribers()).is_err(),
            "never listed"
        );
    }

    #[test]
    fn needed_from_bands_resolves_none_to_default() {
        // A `None` band resolves to the default input; an explicit device is kept.
        let bands = vec![None, Some(3)];
        let needed = AudioManager::needed_from_bands(&bands, Some(1));
        assert!(needed.contains(&1), "None must resolve to default (1)");
        assert!(needed.contains(&3), "explicit device 3 must be kept");
        assert_eq!(needed.len(), 2);
    }

    #[test]
    fn needed_from_bands_dedups_shared_device() {
        // Two bands on the same device collapse to one capture.
        let bands = vec![Some(2), Some(2), None];
        let needed = AudioManager::needed_from_bands(&bands, Some(2));
        assert_eq!(needed.len(), 1);
        assert!(needed.contains(&2));
    }

    #[test]
    fn needed_from_bands_drops_none_without_default() {
        // No default input available → `None` bands demand nothing.
        let bands = vec![None, None];
        let needed = AudioManager::needed_from_bands(&bands, None);
        assert!(needed.is_empty());
    }

    #[test]
    fn needed_from_bands_empty_is_empty() {
        let needed = AudioManager::needed_from_bands(&[], Some(0));
        assert!(needed.is_empty(), "no bands → capture nothing (issue #76)");
    }

    #[test]
    fn audio_format_carries_native_layout() {
        let fmt = AudioFormat {
            sample_rate: 44100,
            channels: 2,
        };
        assert_eq!(fmt.sample_rate, 44100);
        assert_eq!(fmt.channels, 2);
    }

    #[test]
    fn chaos_energy_in_range_single_bin_fft() {
        let data = AudioData {
            waveform: vec![0.0; 2],
            fft: vec![1.0],
            level: 1.0,
            bpm: None,
            time_since_beat: 0.0,
            sample_rate: 48000.0,
        };
        let val = data.energy_in_range(0.0, 48000.0);
        // single bin FFT — should not panic
        assert!(val.is_finite());
    }
}
