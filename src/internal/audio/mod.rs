//! Audio input and analysis for audio-reactive shaders

pub mod app_sound;

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
}

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

/// Whether `device` was listed under a stand-in name, its description
/// unreadable then: it is known only by its place.
fn is_stand_in(device: &AudioDeviceInfo) -> bool {
    device.name == format!("Audio Input {}", device.id)
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
    if is_stand_in(device) {
        return Some(id as usize).filter(|&at| at < now.len());
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
}

impl Liveness {
    fn new() -> Self {
        Self {
            gone: AtomicBool::new(false),
            since: Instant::now(),
            heard_ms: AtomicU64::new(0),
        }
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

/// An active audio source with its own capture stream.
struct ActiveAudioSource {
    /// The stream (none in tests, which have no device).
    _stream: Option<cpal::Stream>,
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
}

impl Default for AudioManager {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioManager {
    pub fn new() -> Self {
        let mut mgr = Self {
            devices: Vec::new(),
            active: HashMap::new(),
            default_source_id: None,
            manual_pins: HashSet::new(),
            mod_refs: HashSet::new(),
            failed: HashMap::new(),
        };
        // Enumerate devices and cache the default input, but open nothing —
        // capture starts only when a holder appears (issue #76).
        mgr.scan_devices();
        mgr
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
            active: HashMap::new(),
            default_source_id: None,
            manual_pins: HashSet::new(),
            mod_refs: HashSet::new(),
            failed: HashMap::new(),
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
        let mut mgr = Self {
            devices: Vec::new(),
            active: HashMap::new(),
            default_source_id: None,
            manual_pins: HashSet::new(),
            mod_refs: HashSet::new(),
            failed: HashMap::new(),
        };
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
    /// holders, and starts capture if it isn't already running. Used by the HTTP
    /// API (`POST /audio/sources/{id}/open`) and `ToggleAudioSource`. Release
    /// with [`close_source`](Self::close_source).
    ///
    /// # Errors
    ///
    /// Returns an error if the host cannot enumerate input devices, if the device
    /// listed as `id` is not connected, if it has no usable default input config, if
    /// its sample format is not `f32`/`i16`/`u16`, or if building or starting the
    /// cpal input stream fails.
    pub fn open_source(&mut self, id: AudioSourceId) -> Result<()> {
        self.manual_pins.insert(id);
        self.ensure_open(id)
    }

    /// Start capture for a source if it isn't already active. Idempotent.
    /// Does not register any holder — callers manage holder sets themselves.
    /// A failure is noted (see [`OPEN_RETRY`]).
    fn ensure_open(&mut self, id: AudioSourceId) -> Result<()> {
        if self.active.contains_key(&id) {
            return Ok(()); // Already open
        }
        match self.open_listed(id) {
            Ok(source) => {
                self.failed.remove(&id);
                self.active.insert(id, source);
                Ok(())
            }
            Err(e) => {
                self.failed.insert(id, Instant::now());
                Err(e)
            }
        }
    }

    /// Capture from the device listed as `id`, found among the OS's by its
    /// name (see [`now_at`]).
    fn open_listed(&self, id: AudioSourceId) -> Result<ActiveAudioSource> {
        // An id never listed — a show's modulator from another Mac — is not
        // looked for: listing the OS's devices is slow.
        anyhow::ensure!(
            self.devices.iter().any(|d| d.id == id),
            "Audio device not found"
        );
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
        let at = now_at(&self.devices, id, &now).context("Audio device not found")?;
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

        let (sender, receiver) = bounded::<AudioData>(16);
        let pcm_subs = Arc::new(ArcSwap::from_pointee(Vec::new()));
        let liveness = Arc::new(Liveness::new());

        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => Self::build_stream::<f32>(
                &device,
                &config.into(),
                sender,
                sample_rate,
                pcm_subs.clone(),
                Arc::clone(&liveness),
            )?,
            cpal::SampleFormat::I16 => Self::build_stream::<i16>(
                &device,
                &config.into(),
                sender,
                sample_rate,
                pcm_subs.clone(),
                Arc::clone(&liveness),
            )?,
            cpal::SampleFormat::U16 => Self::build_stream::<u16>(
                &device,
                &config.into(),
                sender,
                sample_rate,
                pcm_subs.clone(),
                Arc::clone(&liveness),
            )?,
            _ => anyhow::bail!("Unsupported sample format"),
        };

        stream.play().context("Failed to start audio stream")?;

        Ok(ActiveAudioSource {
            _stream: Some(stream),
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

    /// Subscribe to raw PCM passthrough for a source, opening it if needed.
    ///
    /// Returns a fresh receiver plus the device's native [`AudioFormat`] so the
    /// caller can build matching ffmpeg input args. The audio callback fans raw
    /// interleaved `f32` to this and every other subscriber off one hardware
    /// clock, keeping analysis and passthrough coherent. Returns `None` if the
    /// source can't be opened.
    pub fn subscribe_pcm(&mut self, id: AudioSourceId) -> Option<PcmSubscription> {
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
        if self.active.remove(&id).is_some() {
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
        // Devices entering the modulation set: register the ref, then ensure
        // open — one that would not open only every OPEN_RETRY, as this runs
        // every frame.
        let now = Instant::now();
        for &id in needed {
            if tried_lately(&self.failed, id, now) {
                continue;
            }
            if self.mod_refs.insert(id) || !self.active.contains_key(&id) {
                if let Err(e) = self.ensure_open(id) {
                    log::warn!("set_modulation_refs: failed to open audio source {id}: {e}");
                    self.mod_refs.remove(&id);
                }
            }
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
        let lost: Vec<AudioSourceId> = self
            .active
            .iter()
            .filter(|(_, source)| source.liveness.lost(now))
            .map(|(id, _)| *id)
            .collect();
        lost.into_iter()
            .filter_map(|id| {
                let source = self.active.remove(&id)?;
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
        let now = Instant::now();
        let closed: Vec<AudioSourceId> = self
            .manual_pins
            .iter()
            .filter(|id| !self.active.contains_key(id) && !tried_lately(&self.failed, **id, now))
            .copied()
            .collect();
        for id in closed {
            if let Err(e) = self.ensure_open(id) {
                log::debug!("Audio source {id} pinned open, not back yet: {e}");
            }
        }
    }

    /// Whether subscription `token` to source `id` still runs.
    pub fn serves(&self, id: AudioSourceId, token: PcmToken) -> bool {
        self.active
            .get(&id)
            .is_some_and(|s| s.pcm_subs.load().iter().any(|p| p.token == token))
    }

    /// Source `id` open as `name` with no device behind it, and a
    /// subscription to it — for tests.
    #[cfg(test)]
    pub(crate) fn open_for_test(&mut self, id: AudioSourceId, name: &str) -> PcmSubscription {
        self.active.insert(
            id,
            ActiveAudioSource {
                _stream: None,
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
            });
        }
    }

    fn build_stream<T>(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        sender: Sender<AudioData>,
        sample_rate: f32,
        pcm_subs: Arc<ArcSwap<Vec<PcmSubscriber>>>,
        liveness: Arc<Liveness>,
    ) -> Result<cpal::Stream>
    where
        T: cpal::Sample + cpal::SizedSample,
        f32: cpal::FromSample<T>,
    {
        let channels = config.channels as usize;
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
                callback_liveness.heard();
                // Passthrough tee: forward raw interleaved PCM, untouched by the
                // FFT pipeline, to every subscriber. Skipped entirely when there
                // are no subscribers so the video-only path pays nothing.
                let subs = pcm_subs.load();
                if !subs.is_empty() {
                    let pcm: Vec<f32> = data
                        .iter()
                        .map(|s| <f32 as Sample>::from_sample(*s))
                        .collect();
                    fan_out_pcm(&subs, &pcm);
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
        mgr.reopen_pins();
        assert_eq!(mgr.failed[&0], lost_at, "not looked for again at once");
        let long_ago = Instant::now()
            .checked_sub(Duration::from_secs(3))
            .expect("a time that far back");
        mgr.failed.insert(0, long_ago);
        mgr.reopen_pins();
        // No Scarlett here, so the look fails — and is noted, to be tried
        // again later.
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
        assert!(mgr.mod_refs.is_empty());
        assert!(tried_lately(&mgr.failed, 0, at + OPEN_RETRY / 2));
        assert!(!tried_lately(&mgr.failed, 0, at + OPEN_RETRY));
        assert!(!tried_lately(&mgr.failed, 1, at), "another device");
        assert!(mgr.open_listed(5).is_err(), "never listed");
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
