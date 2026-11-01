//! The audio mixer: sources on strips, sent to buses, buses to devices and to
//! recordings and streams — the vMix model.
//!
//! - A **source** is a video deck's soundtrack or a capture device (a
//!   microphone, a USB mixer's return). Each has a fader, a mute and a send to
//!   every bus.
//! - A **bus** sums what is sent to it and has its own fader, mute and delay.
//!   It plays on a sound device (the Mac's output, HDMI into a switcher, a USB
//!   interface into the desk) and any recording or stream can take it.
//! - A clip has **Audio Auto**, as in vMix: its sound switches on as it goes
//!   on air (on Main, or on a bus's Program) and off as it leaves, fading
//!   with the picture during a fade. The switch is the strip's own On button,
//!   so the operator can still turn a clip on or off by hand in between. It
//!   is on for every new clip.
//!
//! Mixing runs on its own thread every 10 ms at 48 kHz stereo (see
//! [`core`]). Settings live in [`AudioMixConfig`], saved in the stage file with
//! the rest of the venue's routing; the thread only ever sees a copy.

mod apps;
#[cfg(target_os = "macos")]
mod au_editor;
#[cfg(target_os = "macos")]
mod au_host;
mod clip;
mod core;
mod device;
mod dsp;
mod dynamics;
mod insert;
mod live;
mod outputs;
mod plugins;
pub mod reader;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use self::core::{Bus, Feed, InputFeed, MixCore, OutputRing, Source};
pub use self::core::{BusTap, BLOCK_FRAMES, MAX_DELAY_MS, SAMPLE_RATE};
pub use clip::{ClipAudio, ClipAudioShared};
pub use dsp::{
    CompressorConfig, DeEsserConfig, EqBandConfig, EqConfig, GateConfig, LevelerConfig, LimiterConfig,
    LoudnessSnapshot, LowCutConfig,
};
pub use dynamics::DynamicsSnapshot;
pub use plugins::{component_codes, AudioEffect};
use reader::{db_to_gain, gain_to_db};

// ── Persisted configuration ─────────────────────────────────────────

/// One audio bus as saved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct AudioBusConfig {
    #[serde(default)]
    pub compressor: CompressorConfig,
    /// Its limiter: on, at −1 dBFS, unless switched off — shows saved before
    /// it get it too.
    #[serde(default)]
    pub limiter: LimiterConfig,
    pub uuid: String,
    pub name: String,
    /// Fader, in dB (-96 is off).
    #[serde(default)]
    pub gain_db: f32,
    #[serde(default)]
    pub muted: bool,
    /// Delay in milliseconds, to line the sound up with a late picture.
    #[serde(default)]
    pub delay_ms: f32,
    /// Sound device the bus plays on, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// First output channel of the pair it plays on (0 = outputs 1-2).
    #[serde(default)]
    pub first_channel: u16,
    /// Played live: the mics on its own sound device mixed inside that
    /// device, about 7 ms from mic to speakers — for the room's speakers and
    /// monitors; the rest of the bus at the usual delay (see `live`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub live: bool,
    /// Played live, the device's buffer in frames a call: one of
    /// [`LIVE_FRAMES`]; `None` is 64 (1.3 ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live_frames: Option<u32>,
    /// An Audio Unit effect first in its processing, before its compressor
    /// and limiter. The bus plays 20 ms later with one (its own thread
    /// renders it; see `insert`). Not on a live bus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub insert: Option<AudioInsertConfig>,
    /// A monitor mix: sources reach it before their faders (unless a send
    /// says otherwise), so riding a fader for the room leaves the stage's
    /// mix as it is. Their mute, On and fades still count.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pre_fader: bool,
}

/// Where a send takes its source: as its bus does, or before or after the
/// source's fader whatever the bus.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SendMode {
    #[default]
    Inherit,
    Pre,
    Post,
}

/// A send's own settings, kept while it is off.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct SendConfig {
    /// Its level, −96 (silent) to +10 dB.
    pub gain_db: f32,
    pub mode: SendMode,
}

impl Default for SendConfig {
    fn default() -> Self {
        Self {
            gain_db: 0.0,
            mode: SendMode::Inherit,
        }
    }
}

impl SendConfig {
    /// # Errors
    /// Returns an error for a level not finite or out of −96…+10 dB.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.gain_db.is_finite() && (-96.0..=10.0).contains(&self.gain_db) {
            Ok(())
        } else {
            Err("A send's level is −96 to +10 dB")
        }
    }

    /// Before its source's fader, on a bus that is (`pre_fader`) or not.
    pub fn pre(&self, pre_fader: bool) -> bool {
        match self.mode {
            SendMode::Inherit => pre_fader,
            SendMode::Pre => true,
            SendMode::Post => false,
        }
    }
}

/// A bus's Audio Unit insert, as saved.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct AudioInsertConfig {
    /// The effect, as `type:subtype:manufacturer` codes ("aufx:lpas:appl";
    /// see `audio_mix.effects`).
    pub component: String,
    /// Its name when chosen: what a Mac without it shows.
    pub name: String,
    /// Bypassed: it still runs, the bus is heard without it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bypass: bool,
    /// Its own settings, as set in its window: the plug-in's document
    /// state, a binary property list in base64 — read from it every 2 s
    /// while it plays, and given back to it when it is next made. Kept as
    /// it is for a Mac without the plug-in.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        serialize_with = "shared_text",
        deserialize_with = "shared_text_from"
    )]
    #[schema(value_type = Option<String>)]
    pub settings: Option<Arc<str>>,
}

/// Shared text, written as text: copied into every snapshot by a count.
#[allow(clippy::ref_option)] // The signature serde's `serialize_with` takes.
fn shared_text<S: serde::Serializer>(text: &Option<Arc<str>>, s: S) -> Result<S::Ok, S::Error> {
    match text {
        Some(text) => s.serialize_some(&**text),
        None => s.serialize_none(),
    }
}

fn shared_text_from<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Arc<str>>, D::Error> {
    <Option<String> as serde::Deserialize>::deserialize(d).map(|text| text.map(Arc::from))
}

/// A bus's insert as it runs.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct AudioInsertStatus {
    /// "loading", "playing", "bypassed" or "failed" (the bus is heard
    /// without it).
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Blocks heard without it because it was late.
    pub late_blocks: u64,
    /// How much later the bus plays with it, ms: 20 for the handoff to its
    /// thread, and the plug-in's own (a look-ahead, a voice isolator's)
    /// once it is loaded. Count it in the bus's delay to line sound up with
    /// the picture.
    pub latency_ms: f32,
}

/// The buffers a live bus offers, frames a call at 48 kHz: 0.7, 1.3, 2.7
/// and 5.3 ms. Smaller is sooner, and asks more of the Mac.
pub const LIVE_FRAMES: [u32; 4] = [32, 64, 128, 256];

/// A live bus's buffer as set, or 64.
fn live_frames_of(bus: &AudioBusConfig) -> u32 {
    bus.live_frames.filter(|f| LIVE_FRAMES.contains(f)).unwrap_or(64)
}

/// The buffer each live bus plays at: the smallest set among the live
/// buses on its device. A device has one buffer for all its streams — its
/// inputs too — and the last stream opened set it for every other: a second
/// live bus at 256 put the first, set to 64, at 256 unawares.
fn live_frames_shared(buses: &[AudioBusConfig]) -> HashMap<String, u32> {
    let mut by_device: HashMap<&str, u32> = HashMap::new();
    for bus in buses.iter().filter(|b| b.live) {
        if let Some(device) = bus.device.as_deref() {
            let frames = by_device.entry(device).or_insert(u32::MAX);
            *frames = (*frames).min(live_frames_of(bus));
        }
    }
    buses
        .iter()
        .filter(|b| b.live)
        .filter_map(|b| {
            let device = b.device.as_deref()?;
            Some((b.uuid.clone(), by_device[device]))
        })
        .collect()
}

impl AudioBusConfig {
    /// A new bus at unity, going nowhere.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            uuid: crate::deck::generate_short_uuid(),
            name: name.into(),
            gain_db: 0.0,
            muted: false,
            delay_ms: 0.0,
            device: None,
            first_channel: 0,
            compressor: CompressorConfig::default(),
            limiter: LimiterConfig::default(),
            live: false,
            live_frames: None,
            insert: None,
            pre_fader: false,
        }
    }
}

/// A capture device used as a source.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct AudioInputConfig {
    pub uuid: String,
    pub name: String,
    /// Input device, by name.
    pub device: String,
    /// Device channel for the left side (0-based).
    pub left: u16,
    /// Device channel for the right side; the same as `left` for a mono mic.
    pub right: u16,
    /// The input whose sound this is — a camera or capture card's — by deck
    /// UUID: Audio Auto switches it with that picture, as a clip's
    /// soundtrack is switched with its clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<String>,
    /// The app whose sound this is, by bundle ID — a Zoom or Teams call's,
    /// taken straight from the app (see [`crate::audio::app_sound`]);
    /// `device` is then its tap's device.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

/// [`AudioStripConfig::follow`] naming Main. Bus UUIDs are hex, so this
/// cannot collide with one.
pub const FOLLOW_MAIN: &str = "main";

/// A source strip's settings, keyed by the source's UUID (the deck's, for a
/// clip).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(from = "StripOnDisk")]
pub struct AudioStripConfig {
    #[serde(default)]
    pub compressor: CompressorConfig,
    #[serde(default)]
    pub low_cut: LowCutConfig,
    /// Its gate: off unless set (shows saved before it have none).
    #[serde(default)]
    pub gate: GateConfig,
    /// Its EQ: off and flat unless set.
    #[serde(default)]
    pub eq: EqConfig,
    /// Its de-esser: off unless set.
    #[serde(default)]
    pub de_esser: DeEsserConfig,
    #[serde(default)]
    pub leveler: LevelerConfig,
    pub uuid: String,
    #[serde(default)]
    pub gain_db: f32,
    /// Off: the strip's On button out. Audio Auto presses it.
    #[serde(default)]
    pub muted: bool,
    /// Audio buses this source is sent to, by UUID.
    #[serde(default)]
    pub sends: Vec<String>,
    /// Each send's level and point, by bus UUID — kept while the send is
    /// off; none, a send at unity as its bus takes it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub send_settings: BTreeMap<String, SendConfig>,
    /// Buses it has been sent to, by UUID: on a live bus it stays among the
    /// bus's mics while its send is off (see `AudioEngine::live_direct`) —
    /// set by turning a send on or off, never by its level.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sent_before: Vec<String>,
    /// Audio Auto: switched on as it goes on air, off as it leaves.
    #[serde(default)]
    pub auto: bool,
    /// Where on air counts for Audio Auto: a video bus by UUID,
    /// [`FOLLOW_MAIN`], or `None` for Main and every bus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow: Option<String>,
    /// Left–right balance, -1 (left only) to 1 (right only).
    #[serde(default)]
    pub balance: f32,
    /// How long its sound is held back, in milliseconds: a microphone's, to
    /// meet the picture of a camera that arrives later (see
    /// `core::Source::delay_ms`). Left out of the show while 0.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub delay_ms: f32,
}

/// Saved send settings with a level out of range: that send at unity.
fn sanitized_sends(settings: BTreeMap<String, SendConfig>) -> BTreeMap<String, SendConfig> {
    settings
        .into_iter()
        .map(|(bus, s)| {
            let s = if s.validate().is_ok() {
                s
            } else {
                SendConfig { gain_db: 0.0, ..s }
            };
            (bus, s)
        })
        .collect()
}

impl AudioStripConfig {
    /// Its send to `bus`, as the mix plays it: level and point.
    pub fn send_to(&self, bus: &AudioBusConfig) -> core::Send {
        let s = self.send_settings.get(&bus.uuid).copied().unwrap_or_default();
        core::Send {
            bus: bus.uuid.clone(),
            gain: db_to_gain(s.gain_db),
            pre: s.pre(bus.pre_fader),
        }
    }

    /// Its processing, as the mixer's chain and a live callback take it.
    pub fn processing(&self) -> dynamics::SourceSettings {
        dynamics::SourceSettings {
            low_cut: self.low_cut,
            gate: self.gate,
            eq: self.eq,
            de_esser: self.de_esser,
            leveler: self.leveler,
            compressor: self.compressor,
        }
    }
}

#[allow(clippy::trivially_copy_pass_by_ref)] // The signature serde's `skip_serializing_if` takes.
fn is_zero(ms: &f32) -> bool {
    *ms == 0.0
}

/// A strip as saved, by this build or an older one. Before Audio Auto a
/// clip "followed" a video bus — heard only on air there — and `follow: None`
/// meant heard all the time; that becomes Audio Auto on, on air anywhere.
#[derive(serde::Deserialize)]
struct StripOnDisk {
    #[serde(default)]
    compressor: CompressorConfig,
    #[serde(default)]
    low_cut: LowCutConfig,
    #[serde(default)]
    gate: GateConfig,
    #[serde(default)]
    eq: EqConfig,
    #[serde(default)]
    de_esser: DeEsserConfig,
    #[serde(default)]
    leveler: LevelerConfig,
    uuid: String,
    #[serde(default)]
    gain_db: f32,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    sends: Vec<String>,
    #[serde(default)]
    send_settings: BTreeMap<String, SendConfig>,
    #[serde(default)]
    sent_before: Vec<String>,
    #[serde(default)]
    auto: Option<bool>,
    #[serde(default)]
    follow: Option<String>,
    #[serde(default)]
    balance: f32,
    #[serde(default)]
    delay_ms: f32,
}

impl From<StripOnDisk> for AudioStripConfig {
    fn from(s: StripOnDisk) -> Self {
        let (auto, follow) = match s.auto {
            Some(auto) => (auto, s.follow),
            None => (s.follow.is_some(), None),
        };
        Self {
            uuid: s.uuid,
            compressor: s.compressor.sanitized(),
            low_cut: s.low_cut.sanitized(),
            gate: s.gate.sanitized(),
            eq: s.eq.sanitized(),
            de_esser: s.de_esser.sanitized(),
            leveler: s.leveler.sanitized(),
            gain_db: s.gain_db,
            muted: s.muted,
            sends: s.sends,
            send_settings: sanitized_sends(s.send_settings),
            sent_before: s.sent_before,
            auto,
            follow,
            balance: s.balance,
            delay_ms: s.delay_ms,
        }
    }
}

/// Everything about the mixer that is saved.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct AudioMixConfig {
    #[serde(default)]
    pub buses: Vec<AudioBusConfig>,
    #[serde(default)]
    pub inputs: Vec<AudioInputConfig>,
    #[serde(default)]
    pub strips: Vec<AudioStripConfig>,
    #[serde(default)]
    pub monitor: AudioMonitorConfig,
    /// How long a clip's or playlist's sound fades out when Stop is pressed
    /// on it — on its tile or in the Audio Mixer — before it pauses; 0 cuts.
    /// Left out, [`DEFAULT_STOP_FADE_SECS`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_fade_secs: Option<f32>,
}

/// A Stop's fade, unless another is chosen.
pub const DEFAULT_STOP_FADE_SECS: f32 = 2.0;
/// The longest a Stop's fade can be.
pub const MAX_STOP_FADE_SECS: f32 = 10.0;

/// The operator's headphones.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct AudioMonitorConfig {
    /// Sound device the monitor plays on, by name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// First output channel of the pair it plays on (0 = outputs 1-2).
    #[serde(default)]
    pub first_channel: u16,
    /// Monitor level, in dB.
    #[serde(default)]
    pub gain_db: f32,
    /// Bus heard when nothing is soloed; `None` is the first bus.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
}

impl AudioMixConfig {
    /// A fresh mixer: one bus called Master, which recordings and streams take
    /// by default.
    pub fn with_master() -> Self {
        Self {
            buses: vec![AudioBusConfig::new("Master")],
            ..Self::default()
        }
    }
}

// ── What the UI and API see ─────────────────────────────────────────

/// One bus, as shown.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AudioBusSnapshot {
    pub peak_db: [f32; 2],
    pub dynamics: DynamicsSnapshot,
    #[serde(flatten)]
    pub config: AudioBusConfig,
    /// Whether the chosen device is playing. `None` with no device chosen.
    pub device_ok: Option<bool>,
    /// Why it does not play as set: on another pair of outputs (the device
    /// has fewer), its live mixing refused, or why its device would not
    /// open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_note: Option<String>,
    /// Asked to play live (its mics mixed in its device's own callback):
    /// whether it does. `None` when not asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live: Option<bool>,
    /// Its insert as it runs; `None` without one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub insert_status: Option<AudioInsertStatus>,
    /// Post-fader peak per side, in dB.
    pub meter_db: [f32; 2],
}

/// Where a source's sound comes from.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub enum AudioSourceKind {
    /// A video deck's soundtrack.
    Clip { deck_uuid: String },
    /// A live source's sound: an NDI source's.
    Live { deck_uuid: String },
    /// A capture device.
    Input {
        device: String,
        left: u16,
        right: u16,
        /// The input it is the sound of, by deck UUID.
        #[serde(skip_serializing_if = "Option::is_none")]
        deck: Option<String>,
        /// The app whose sound it is, by bundle ID.
        #[serde(skip_serializing_if = "Option::is_none")]
        app: Option<String>,
    },
}

/// One source strip, as shown.
// A strip's buttons, each shown as it is: not one state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AudioSourceSnapshot {
    pub peak_db: [f32; 2],
    pub dynamics: DynamicsSnapshot,
    pub uuid: String,
    pub name: String,
    pub kind: AudioSourceKind,
    pub gain_db: f32,
    pub muted: bool,
    pub sends: Vec<String>,
    /// Each send's level and point, by bus UUID (none: unity, as its bus).
    pub send_settings: BTreeMap<String, SendConfig>,
    /// Buses it has been sent to (see [`AudioStripConfig::sent_before`]).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sent_before: Vec<String>,
    /// Audio Auto.
    pub auto: bool,
    pub follow: Option<String>,
    pub balance: f32,
    /// How long its sound is held back, in milliseconds.
    pub delay_ms: f32,
    /// How far it is on air where Audio Auto looks, 0 to 1 (0 without
    /// Audio Auto).
    pub on_air: f32,
    /// What its level is multiplied by for a fade: the on-air level while it
    /// moves, else 1.
    pub gate: f32,
    /// Soloed: heard on the monitor before its fader (PFL).
    pub solo: bool,
    /// Whether the source is there: the deck exists, the device is connected.
    pub live: bool,
    /// Post-fader peak per side, in dB.
    pub meter_db: [f32; 2],
    /// Its level before its fader, mute and Audio Auto, per side, in dB:
    /// what it plays, whether or not anyone hears it.
    pub signal_db: [f32; 2],
    /// Why an app's sound, its app running, could not be taken.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refused: Option<String>,
}

/// The monitor, as shown.
#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct AudioMonitorSnapshot {
    #[serde(flatten)]
    pub config: AudioMonitorConfig,
    /// Whether the chosen device is playing. `None` with no device chosen.
    pub device_ok: Option<bool>,
    /// Why it does not play as set (see [`AudioBusSnapshot::device_note`]).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_note: Option<String>,
    /// Whether something is soloed, so the monitor hears only that.
    pub soloing: bool,
    /// Monitor level per side, in dB.
    pub meter_db: [f32; 2],
}

/// An input device, as offered for a new source.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AudioInputDevice {
    pub name: String,
    pub channels: u16,
}

/// The whole mixer, as shown.
#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
pub struct AudioMixSnapshot {
    pub buses: Vec<AudioBusSnapshot>,
    pub sources: Vec<AudioSourceSnapshot>,
    pub monitor: AudioMonitorSnapshot,
    pub output_devices: Vec<String>,
    /// Each listed output device's outputs, by name (0 when it would not
    /// say): the pairs a bus can be set to.
    pub output_channels: std::collections::BTreeMap<String, u16>,
    pub input_devices: Vec<AudioInputDevice>,
    /// Apps whose sound can be taken as an input, making sound first.
    pub apps: Vec<crate::audio::app_sound::RunningApp>,
    /// The Audio Unit effects installed on this Mac, by maker then name —
    /// shared, not copied each frame.
    #[serde(serialize_with = "shared_list")]
    #[schema(value_type = Vec<AudioEffect>)]
    pub effects: Arc<Vec<AudioEffect>>,
    /// How long Stop fades a clip's or playlist's sound before it pauses;
    /// 0 cuts.
    pub stop_fade_secs: f32,
    /// The clips and playlists whose sound is fading out to a stop now, by
    /// deck UUID — a Stop pressed, or the rundown's fade.
    pub stopping: Vec<String>,
}

/// A shared list, written as the list.
fn shared_list<S: serde::Serializer, T: serde::Serialize>(
    list: &Arc<Vec<T>>,
    s: S,
) -> Result<S::Ok, S::Error> {
    serde::Serialize::serialize(list.as_slice(), s)
}

// ── Engine ──────────────────────────────────────────────────────────

/// A live source's sound — an NDI source's — for
/// [`AudioEngine::sync_live`].
pub struct LiveSound {
    pub deck_uuid: String,
    pub name: String,
    /// Changes when the sound has to be picked up afresh: another receiver,
    /// or a new rate or channel count.
    pub key: u64,
    /// The sound, when the mixer does not have it under this key yet
    /// ([`AudioEngine::live_key`]).
    pub feed: Option<LiveFeed>,
}

/// Where a live source's sound comes from: interleaved PCM at its own rate.
pub struct LiveFeed {
    pub rx: crossbeam_channel::Receiver<crate::audio::PcmChunk>,
    pub rate: u32,
    pub channels: u16,
}

/// A clip the scene has, for [`AudioEngine::sync_clips`].
pub struct ClipSource {
    pub deck_uuid: String,
    pub name: String,
    pub shared: Arc<ClipAudioShared>,
}

/// Why a mixer edit was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioMixError {
    NoSuchBus(String),
    NoSuchSource(String),
    NoSuchInput(String),
}

impl std::fmt::Display for AudioMixError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSuchBus(u) => write!(f, "no audio bus with uuid '{u}'"),
            Self::NoSuchSource(u) => write!(f, "no audio source with uuid '{u}'"),
            Self::NoSuchInput(u) => write!(f, "no audio input with uuid '{u}'"),
        }
    }
}

/// An input device's subscription with the capture manager.
/// A live bus wanted: its mics' capture, and each mic by input UUID with
/// its channels.
type LiveWanted = (crate::audio::AudioSourceId, Vec<(String, (u16, u16))>);

/// A live bus's feed (see [`AudioBusConfig::live`]): its mics, every one in each
/// frame, straight from their capture's callback, and each mic — by input
/// UUID, with its channels — with its gain as the bus's callback reads it.
struct LiveBusFeed {
    source_id: crate::audio::AudioSourceId,
    token: crate::audio::PcmToken,
    frames: crossbeam_channel::Receiver<crate::audio::LiveFrame>,
    counts: Arc<crate::audio::LiveCounts>,
    mics: Vec<(String, (u16, u16), Arc<std::sync::atomic::AtomicU32>)>,
    /// Its mics' and its own processing, as the callback reads them.
    control: Arc<live::LiveControl>,
    /// The rest of the bus, from the mix to the callback.
    rest: (
        crossbeam_channel::Sender<reader::Frame>,
        crossbeam_channel::Receiver<reader::Frame>,
    ),
}

/// Sound devices as listed: outputs with their channel counts, inputs; and
/// the Audio Unit effects installed.
type DeviceListing = (Vec<(String, u16)>, Vec<AudioInputDevice>, Vec<AudioEffect>);

struct LiveInput {
    source_id: crate::audio::AudioSourceId,
    token: crate::audio::PcmToken,
    /// Its capture's sample rate.
    rate: u32,
    /// What was subscribed: the device, by name, and its channels.
    device: String,
    left: u16,
    right: u16,
}

impl LiveInput {
    /// Whether `input` is still what this subscribed to.
    fn serves(&self, input: &AudioInputConfig) -> bool {
        input.app.is_none()
            && input.device == self.device
            && input.left == self.left
            && input.right == self.right
    }
}



/// A bus's insert running the Audio Unit `component`, loading on a thread
/// of its own. Out of Nida's process; Apple's own may run in it.
#[cfg(target_os = "macos")]
fn make_insert(bus: &str, component: &str, settings: Option<Vec<u8>>) -> insert::Insert {
    let component = component.to_string();
    insert::Insert::new(
        bus,
        Box::new(move || {
            let apple = component.ends_with(":appl");
            au_host::Hosted::open_as(&component, apple, settings.as_deref()).map(|hosted| {
                log::info!(
                    "Audio insert '{component}' loaded {}, {:.1} ms latency",
                    if hosted.in_process { "inside Nida (Apple's own)" } else { "out of process" },
                    hosted.latency_secs * 1000.0
                );
                Box::new(hosted) as Box<dyn insert::Effect>
            })
        }),
    )
}

#[cfg(not(target_os = "macos"))]
fn make_insert(bus: &str, _component: &str, _settings: Option<Vec<u8>>) -> insert::Insert {
    insert::Insert::new(bus, Box::new(|| Err("Audio Units are only on macOS".to_string())))
}

/// A live bus's processing as set: its own compressor, limiter and delay,
/// and each of `mics` (input UUIDs, in its feed's order) as its strip's low
/// cut, leveler, compressor and balance. A mic's own delay is left out: it
/// meets a late camera's picture, and the room's speakers want the voice as
/// the speaker talks.
fn live_settings<'a>(
    config: &AudioMixConfig,
    bus: &str,
    mics: impl Iterator<Item = &'a str>,
) -> Option<live::LiveSettings> {
    let cfg = config.buses.iter().find(|b| b.uuid == bus)?;
    let mut settings = live::LiveSettings {
        compressor: cfg.compressor,
        limiter: cfg.limiter,
        delay_ms: cfg.delay_ms,
        ..live::LiveSettings::default()
    };
    for (slot, uuid) in settings.mics.iter_mut().zip(mics) {
        if let Some(strip) = config.strips.iter().find(|s| s.uuid == uuid) {
            *slot = live::LiveMicSettings {
                low_cut: strip.low_cut,
                gate: strip.gate,
                eq: strip.eq,
                de_esser: strip.de_esser,
                leveler: strip.leveler,
                compressor: strip.compressor,
                balance: strip.balance,
            };
        }
    }
    Some(settings)
}

/// Key of the monitor's device among the bus devices. Bus UUIDs are hex, so
/// this cannot collide with one.
const MONITOR_KEY: &str = "monitor";

/// How often a missing device is looked for again.
const RETRY: Duration = Duration::from_secs(2);



/// The mixer: its settings, the thread that mixes, and the devices it plays.
pub struct AudioEngine {
    config: AudioMixConfig,
    core: Arc<Mutex<MixCore>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    rings: HashMap<String, Arc<OutputRing>>,
    /// The devices the buses play on, opened on a thread of their own.
    outputs: outputs::Outputs,
    clips: HashMap<String, (String, Arc<ClipAudioShared>)>,
    /// Live sources' sound by deck UUID: (name, key).
    lives: HashMap<String, (String, u64)>,
    inputs: HashMap<String, LiveInput>,
    inputs_tried: Option<Instant>,
    /// The capture manager's count of captures opened at the last try (see
    /// `AudioManager::landed`): one opened since, the inputs are looked
    /// for again at once.
    inputs_landed: u64,
    /// The thread taking apps' sound (see [`Self::keep_apps`]).
    apps: apps::Apps,
    /// Why each app input's sound, its app running, was not taken, by UUID.
    apps_refused: HashMap<String, String>,
    /// Each app input's generation (see [`apps`]): moved on when its sound is
    /// dropped here rather than by the app sound thread.
    apps_generation: HashMap<String, u64>,
    /// Apps whose sound can be taken, as last listed.
    running_apps: Vec<crate::audio::app_sound::RunningApp>,
    /// Last frame's on-air level of every Audio Auto source, 0 to 1.
    on_air: HashMap<String, f32>,
    /// Sources a rundown step is fading out, 0 to 1 — Audio Auto or not:
    /// what their sound is multiplied by.
    step_fades: HashMap<String, f32>,
    /// Sources soloed to the monitor. Live, never saved: a show should not
    /// reopen with the headphones stuck on one microphone.
    solos: std::collections::HashSet<String>,
    /// The buses the mixing thread has, as last applied: a new one is built
    /// before the mixer is held (see [`Self::apply`]).
    applied_buses: std::collections::HashSet<String>,
    output_devices: Vec<String>,
    output_channels: std::collections::BTreeMap<String, u16>,
    input_devices: Vec<AudioInputDevice>,
    effects: Arc<Vec<AudioEffect>>,
    /// Each bus's insert component as the mixing thread has it.
    applied_inserts: HashMap<String, String>,
    /// Every bus's insert made afresh on the next apply — or taken out —
    /// whatever was applied (a show loaded).
    remake_inserts: bool,
    /// Each running insert's settings changes as last taken, by bus.
    insert_settings_seen: HashMap<String, u64>,
    /// A listing of the sound devices being made (see [`Self::rescan_devices`]).
    listing: Option<std::sync::mpsc::Receiver<DeviceListing>>,
    /// Asked to list the devices while a listing was being made: listed
    /// again once it is in — that one may have been made before a device
    /// came.
    list_again: bool,
    /// Each live bus's feed, by bus UUID (see [`AudioBusConfig::live`]).
    live_taps: HashMap<String, LiveBusFeed>,
    /// Why the live bus is played the usual way, as last said.
    live_refused: HashMap<String, String>,
    /// Each live bus's gain as its callback reads it, by bus UUID.
    live_bus_gains: HashMap<String, Arc<std::sync::atomic::AtomicU32>>,
    /// The buses asked to play live now (their devices fed by their
    /// callbacks, not their rings).
    live_posted: std::collections::HashSet<String>,
}

impl Default for AudioEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioEngine {
    /// A mixer with one Master bus, mixing from now on.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to start the mixing thread.
    pub fn new() -> Self {
        Self::with_opener(Box::new(|name, first_channel, ring| {
            device::open(name, first_channel, ring)
                .map(|d| Box::new(d) as Box<dyn outputs::Playing>)
        }))
    }

    /// A mixer whose buses' devices are opened by `open`, taking apps' sound
    /// through the system.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to start the mixing or device thread.
    fn with_opener(open: outputs::Opener) -> Self {
        Self::with_parts(open, Box::new(apps::SystemTaps))
    }

    /// A mixer whose buses' devices are opened by `open` and whose apps'
    /// sound is taken through `taps`.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to start the mixing, device or app sound
    /// thread.
    fn with_parts(open: outputs::Opener, taps: Box<dyn apps::Taps>) -> Self {
        let core = Arc::new(Mutex::new(MixCore::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let core = core.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("audio-mix".into())
                .spawn(move || run_mixer(&core, &stop))
                .expect("failed to start the audio mixing thread")
        };
        let mut engine = Self {
            config: AudioMixConfig::default(),
            core,
            stop,
            thread: Some(thread),
            rings: HashMap::new(),
            outputs: outputs::Outputs::spawn(open),
            clips: HashMap::new(),
            lives: HashMap::new(),
            inputs: HashMap::new(),
            inputs_tried: None,
            inputs_landed: 0,
            apps: apps::Apps::spawn(taps),
            apps_refused: HashMap::new(),
            apps_generation: HashMap::new(),
            running_apps: Vec::new(),
            on_air: HashMap::new(),
            step_fades: HashMap::new(),
            solos: std::collections::HashSet::new(),
            applied_buses: std::collections::HashSet::new(),
            output_devices: Vec::new(),
            output_channels: std::collections::BTreeMap::new(),
            effects: Arc::default(),
            applied_inserts: HashMap::new(),
            remake_inserts: false,
            insert_settings_seen: HashMap::new(),
            input_devices: Vec::new(),
            listing: None,
            list_again: false,
            live_taps: HashMap::new(),
            live_refused: HashMap::new(),
            live_bus_gains: HashMap::new(),
            live_posted: std::collections::HashSet::new(),
        };
        engine.load(AudioMixConfig::with_master());
        engine
    }

    /// The saved settings.
    pub fn config(&self) -> &AudioMixConfig {
        &self.config
    }

    /// Whether any meter moves: a bus heard above `floor_db` dBFS, or a
    /// source playing above it — heard or not, as its strip shows it.
    pub fn sounding(&self, floor_db: f32) -> bool {
        let floor = 10f32.powf(floor_db / 20.0);
        let Ok(core) = self.core.lock() else {
            return false;
        };
        let loud = |[l, r]: reader::Frame| l.max(r) > floor;
        core.buses.iter().any(|bus| loud(bus.meter()))
            || core.sources.iter().any(|source| loud(source.signal()))
    }

    /// How long Stop fades a sound out before it pauses; 0 cuts.
    pub fn stop_fade_secs(&self) -> f32 {
        self.config
            .stop_fade_secs
            .filter(|secs| secs.is_finite())
            .map_or(DEFAULT_STOP_FADE_SECS, |secs| secs.clamp(0.0, MAX_STOP_FADE_SECS))
    }

    /// Set how long Stop fades a sound out; 0 cuts. Refused unless finite
    /// and from 0 to [`MAX_STOP_FADE_SECS`].
    pub fn set_stop_fade_secs(&mut self, secs: f32) -> bool {
        if !secs.is_finite() || !(0.0..=MAX_STOP_FADE_SECS).contains(&secs) {
            return false;
        }
        self.config.stop_fade_secs = Some(secs);
        true
    }

    /// Replace every setting, as a workspace load does.
    pub fn load(&mut self, mut config: AudioMixConfig) {
        for bus in &mut config.buses {
            bus.compressor = bus.compressor.sanitized();
            bus.limiter = bus.limiter.sanitized();
        }
        for strip in &mut config.strips {
            strip.compressor = strip.compressor.sanitized();
            strip.low_cut = strip.low_cut.sanitized();
            strip.gate = strip.gate.sanitized();
            strip.eq = strip.eq.sanitized();
            strip.de_esser = strip.de_esser.sanitized();
            strip.leveler = strip.leveler.sanitized();
        }
        // An app's sound whose input the show no longer has, or has as
        // something else — another app, a microphone under the same UUID —
        // stops now, not when the app sound thread next looks.
        let stale: Vec<String> = self
            .config
            .inputs
            .iter()
            .filter(|old| {
                old.app.is_some()
                    && !config
                        .inputs
                        .iter()
                        .any(|new| new.uuid == old.uuid && new.app == old.app)
            })
            .map(|old| old.uuid.clone())
            .collect();
        if let Ok(mut core) = self.core.lock() {
            core.sources.retain(|s| !stale.contains(&s.uuid));
            for source in &mut core.sources {
                source.dynamics = dynamics::SourceChain::default();
                source.held_peak = [0.0; 2];
            }
            for bus in &mut core.buses {
                // In place: no allocation with the mixer held.
                bus.dynamics.reset();
                bus.held_peak = [0.0; 2];
            }
        }
        // A live bus's callback too, on its next call.
        for feed in self.live_taps.values() {
            feed.control.start_afresh();
        }
        for uuid in stale {
            // Its refusal was the old app's: another app under the UUID,
            // not running, would show it.
            self.apps_refused.remove(&uuid);
            *self.apps_generation.entry(uuid).or_default() += 1;
        }
        self.config = config;
        // Its inserts made afresh, as its other processing is — and a bus
        // without one in the new show loses the old show's.
        self.remake_inserts = true;
        // Inputs are re-subscribed on the next `connect_inputs`.
        self.inputs_tried = None;
        self.apply();
    }

    /// Set bus `uuid`'s Audio Unit insert (`component` codes) or take it
    /// out. Named as this Mac lists it, else by its codes. The effect the
    /// bus has already, asked for again, is kept as it is — its settings
    /// and bypass with it.
    ///
    /// # Errors
    ///
    /// No such bus.
    pub fn set_insert(&mut self, uuid: &str, component: Option<String>) -> Result<(), AudioMixError> {
        let has = self
            .config
            .buses
            .iter()
            .find(|b| b.uuid == uuid)
            .ok_or_else(|| AudioMixError::NoSuchBus(uuid.into()))?
            .insert
            .as_ref()
            .map(|i| i.component.as_str());
        if has == component.as_deref() {
            return Ok(());
        }
        let insert = component.map(|component| AudioInsertConfig {
            name: self.effect_name(&component).unwrap_or_else(|| component.clone()),
            component,
            bypass: false,
            settings: None,
        });
        self.edit_bus(uuid, |b| b.insert = insert)
    }

    /// Open the window of bus `uuid`'s insert on this Mac's screen: the
    /// plug-in's own, or a generic list of its controls.
    ///
    /// # Errors
    ///
    /// No such bus, no effect on it, or the effect not loaded (yet).
    pub fn open_insert_window(&self, uuid: &str) -> Result<(), String> {
        let bus = self
            .config
            .buses
            .iter()
            .find(|b| b.uuid == uuid)
            .ok_or_else(|| format!("no audio bus with uuid '{uuid}'"))?;
        let insert = bus.insert.as_ref().ok_or_else(|| format!("'{}' has no effect", bus.name))?;
        let handle = self
            .core
            .lock()
            .ok()
            .and_then(|core| core.bus(uuid)?.insert.as_ref()?.status.handle())
            .ok_or_else(|| format!("'{}' is not loaded", insert.name))?;
        #[cfg(target_os = "macos")]
        {
            let unit = handle
                .downcast_ref::<au_host::SharedUnit>()
                .ok_or_else(|| format!("'{}' has no window", insert.name))?;
            au_editor::open(uuid.to_string(), format!("{} — {}", bus.name, insert.name), unit.clone());
            Ok(())
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = handle;
            Err("Audio Units are only on macOS".into())
        }
    }

    /// Close the window of bus `uuid`'s insert, if open.
    pub fn close_insert_window(&self, uuid: &str) {
        #[cfg(target_os = "macos")]
        au_editor::close(uuid.to_string());
        #[cfg(not(target_os = "macos"))]
        let _ = uuid;
    }

    /// An installed effect's name, by its component codes.
    pub fn effect_name(&self, component: &str) -> Option<String> {
        self.effects.iter().find(|e| e.component == component).map(|e| e.name.clone())
    }

    /// The first bus, which new recordings and streams take.
    pub fn default_bus(&self) -> Option<&str> {
        self.config.buses.first().map(|b| b.uuid.as_str())
    }

    /// Clear a strip's held sample peaks without changing its audio settings.
    pub fn reset_meter(&mut self, uuid: &str, bus: bool) {
        if let Ok(mut core) = self.core.lock() {
            if bus {
                if let Some(target) = core.buses.iter_mut().find(|target| target.uuid == uuid) {
                    target.held_peak = [0.0; 2];
                }
            } else if let Some(target) = core.sources.iter_mut().find(|target| target.uuid == uuid)
            {
                target.held_peak = [0.0; 2];
            }
        }
    }

    /// Start a bus's loudness readings again — integrated, short-term and
    /// momentary — as a show starts. Its held peak stays.
    pub fn reset_loudness(&mut self, uuid: &str) {
        if let Ok(mut core) = self.core.lock() {
            if let Some(target) = core.buses.iter_mut().find(|target| target.uuid == uuid) {
                target.dynamics.reset_loudness();
            }
        }
    }

    /// Whether a bus with this UUID exists.
    pub fn has_bus(&self, uuid: &str) -> bool {
        self.config.buses.iter().any(|b| b.uuid == uuid)
    }

    /// A bus's name.
    pub fn bus_name(&self, uuid: &str) -> Option<&str> {
        self.config
            .buses
            .iter()
            .find(|b| b.uuid == uuid)
            .map(|b| b.name.as_str())
    }

    // ── Buses ───────────────────────────────────────────────────────

    /// Add a bus, returning its UUID.
    pub fn add_bus(&mut self, name: &str) -> String {
        let bus = AudioBusConfig::new(name);
        let uuid = bus.uuid.clone();
        self.config.buses.push(bus);
        self.apply();
        uuid
    }

    /// Remove a bus and every send to it.
    ///
    /// # Errors
    ///
    /// [`AudioMixError::NoSuchBus`] if there is no such bus.
    pub fn remove_bus(&mut self, uuid: &str) -> Result<(), AudioMixError> {
        let before = self.config.buses.len();
        self.config.buses.retain(|b| b.uuid != uuid);
        if self.config.buses.len() == before {
            return Err(AudioMixError::NoSuchBus(uuid.into()));
        }
        for strip in &mut self.config.strips {
            strip.sends.retain(|s| s != uuid);
            strip.send_settings.remove(uuid);
            strip.sent_before.retain(|s| s != uuid);
        }
        if self.config.monitor.listen.as_deref() == Some(uuid) {
            self.config.monitor.listen = None;
        }
        // Its device is let go on the next frame, as it is no longer wanted.
        self.rings.remove(uuid);
        self.apply();
        Ok(())
    }

    /// Change one bus's settings with `edit`.
    ///
    /// # Errors
    ///
    /// [`AudioMixError::NoSuchBus`] if there is no such bus.
    pub fn edit_bus(
        &mut self,
        uuid: &str,
        edit: impl FnOnce(&mut AudioBusConfig),
    ) -> Result<(), AudioMixError> {
        let bus = self
            .config
            .buses
            .iter_mut()
            .find(|b| b.uuid == uuid)
            .ok_or_else(|| AudioMixError::NoSuchBus(uuid.into()))?;
        edit(bus);
        bus.gain_db = bus.gain_db.clamp(-96.0, 12.0);
        bus.delay_ms = bus.delay_ms.clamp(0.0, MAX_DELAY_MS);
        bus.compressor = bus.compressor.sanitized();
        bus.limiter = bus.limiter.sanitized();
        bus.live_frames = bus.live_frames.filter(|f| LIVE_FRAMES.contains(f));
        self.apply();
        Ok(())
    }

    // ── Sources ─────────────────────────────────────────────────────

    /// Add a capture device as a source, returning its UUID. Sent to the
    /// first bus, heard all the time.
    pub fn add_input(&mut self, name: &str, device: &str, left: u16, right: u16) -> String {
        self.add_device_input(name, device, left, right, None)
    }

    /// Add a capture device as the sound of an input's picture (a camera or
    /// capture card), returning its UUID. Like a clip's soundtrack it has
    /// Audio Auto on, so it is off until its picture goes on air.
    pub fn add_input_of_deck(
        &mut self,
        name: &str,
        device: &str,
        (left, right): (u16, u16),
        deck_uuid: &str,
    ) -> String {
        self.add_device_input(name, device, left, right, Some(deck_uuid))
    }

    fn add_device_input(
        &mut self,
        name: &str,
        device: &str,
        left: u16,
        right: u16,
        deck: Option<&str>,
    ) -> String {
        let uuid = crate::deck::generate_short_uuid();
        self.config.inputs.push(AudioInputConfig {
            uuid: uuid.clone(),
            name: name.to_string(),
            device: device.to_string(),
            left,
            right,
            deck: deck.map(str::to_string),
            app: None,
        });
        let sends = self.default_bus().map(str::to_string).into_iter().collect();
        self.config.strips.push(AudioStripConfig {
            compressor: CompressorConfig::default(),
            low_cut: LowCutConfig::default(),
            gate: GateConfig::default(),
            eq: EqConfig::default(),
            de_esser: DeEsserConfig::default(),
            leveler: LevelerConfig::default(),
            uuid: uuid.clone(),
            gain_db: 0.0,
            muted: deck.is_some(),
            sends,
            send_settings: BTreeMap::new(),
            sent_before: Vec::new(),
            auto: deck.is_some(),
            follow: None,
            balance: 0.0,
                delay_ms: 0.0,
        });
        self.inputs_tried = None;
        self.apply();
        uuid
    }

    /// Add an app's sound — a Zoom or Teams call's — as a source, returning
    /// its UUID: taken straight from the app while it runs (see
    /// [`Self::keep_apps`]), on like a microphone.
    pub fn add_app_input(&mut self, name: &str, app: &str) -> String {
        self.add_app_input_of(name, app, None)
    }

    /// As [`Self::add_app_input`], the sound of the input `deck` (by deck
    /// UUID) when one is given — PowerPoint's, of a PowerPoint input: Audio
    /// Auto switches it with that picture, and it goes when the input does.
    pub fn add_app_input_of(&mut self, name: &str, app: &str, deck: Option<&str>) -> String {
        let uuid =
            self.add_device_input(name, &crate::audio::app_sound::device_name(app), 0, 1, deck);
        if let Some(input) = self.config.inputs.iter_mut().find(|i| i.uuid == uuid) {
            input.app = Some(app.to_string());
        }
        uuid
    }

    /// The app whose sound the input `uuid` is, by bundle ID.
    #[must_use]
    pub fn app_of(&self, uuid: &str) -> Option<&str> {
        self.config
            .inputs
            .iter()
            .find(|i| i.uuid == uuid)?
            .app
            .as_deref()
    }

    /// The sound of app `app` tied to the deck `from` tied to `to` instead:
    /// one app's sound kept for the input of it still there.
    pub fn retie_app_inputs(&mut self, app: &str, from: &str, to: &str) {
        for input in &mut self.config.inputs {
            if input.app.as_deref() == Some(app) && input.deck.as_deref() == Some(from) {
                input.deck = Some(to.to_string());
            }
        }
    }

    /// Remove the inputs that are the sound of a deck that is going.
    pub fn remove_inputs_of_deck(&mut self, deck_uuid: &str) {
        let theirs: Vec<String> = self
            .config
            .inputs
            .iter()
            .filter(|i| i.deck.as_deref() == Some(deck_uuid))
            .map(|i| i.uuid.clone())
            .collect();
        for uuid in theirs {
            let _ = self.remove_input(&uuid);
        }
    }

    /// Open again the devices whose sound the inputs taken with deck
    /// `deck_uuid` take (a Reset input), in place: they keep listening. One
    /// not taken yet is looked for again at once.
    pub fn reopen_inputs_of_deck(
        &mut self,
        deck_uuid: &str,
        capture: &mut crate::audio::AudioManager,
    ) {
        for id in self.devices_of_deck(deck_uuid) {
            capture.reopen(id);
        }
        self.inputs_tried = None;
    }

    /// The capture devices the inputs taken with deck `deck_uuid` listen to.
    fn devices_of_deck(&self, deck_uuid: &str) -> Vec<crate::audio::AudioSourceId> {
        let mut ids: Vec<crate::audio::AudioSourceId> = self
            .config
            .inputs
            .iter()
            .filter(|i| i.app.is_none() && i.deck.as_deref() == Some(deck_uuid))
            .filter_map(|i| self.inputs.get(&i.uuid).map(|live| live.source_id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Whether a source is there to edit: a clip or live source in the
    /// scene now, or a configured input.
    fn has_source(&self, uuid: &str) -> bool {
        self.clips.contains_key(uuid)
            || self.lives.contains_key(uuid)
            || self.config.inputs.iter().any(|i| i.uuid == uuid)
    }

    /// Every source Audio Auto switches, with the deck whose picture it
    /// follows and where on air counts: (strip, deck, follow).
    pub fn auto_sources(&self) -> Vec<(String, String, Option<String>)> {
        self.config
            .strips
            .iter()
            .filter(|s| s.auto)
            .filter_map(|s| {
                let deck = if self.clips.contains_key(&s.uuid) || self.lives.contains_key(&s.uuid) {
                    s.uuid.clone()
                } else {
                    self.config
                        .inputs
                        .iter()
                        .find(|i| i.uuid == s.uuid)?
                        .deck
                        .clone()?
                };
                Some((s.uuid.clone(), deck, s.follow.clone()))
            })
            .collect()
    }

    /// Remove a capture-device source. Its subscription is released on the
    /// next [`Self::connect_inputs`].
    ///
    /// # Errors
    ///
    /// [`AudioMixError::NoSuchInput`] if there is no such input.
    pub fn remove_input(&mut self, uuid: &str) -> Result<(), AudioMixError> {
        let before = self.config.inputs.len();
        self.config.inputs.retain(|i| i.uuid != uuid);
        if self.config.inputs.len() == before {
            return Err(AudioMixError::NoSuchInput(uuid.into()));
        }
        self.config.strips.retain(|s| s.uuid != uuid);
        self.solos.remove(uuid);
        self.apps_refused.remove(uuid);
        *self.apps_generation.entry(uuid.to_string()).or_default() += 1;
        if let Ok(mut core) = self.core.lock() {
            core.sources.retain(|s| s.uuid != uuid);
        }
        Ok(())
    }

    /// Change one source strip's settings with `edit`. The source must be in
    /// the scene now (a clip) or configured (an input).
    ///
    /// # Errors
    ///
    /// [`AudioMixError::NoSuchSource`] if there is no such source.
    pub fn edit_source(
        &mut self,
        uuid: &str,
        edit: impl FnOnce(&mut AudioStripConfig),
    ) -> Result<(), AudioMixError> {
        let known = self.has_source(uuid);
        let Some(strip) = self.config.strips.iter_mut().find(|s| s.uuid == uuid) else {
            return Err(AudioMixError::NoSuchSource(uuid.into()));
        };
        if !known {
            return Err(AudioMixError::NoSuchSource(uuid.into()));
        }
        edit(strip);
        strip.gain_db = strip.gain_db.clamp(-96.0, 12.0);
        strip.balance = if strip.balance.is_finite() {
            strip.balance.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        strip.delay_ms = if strip.delay_ms.is_finite() {
            strip.delay_ms.clamp(0.0, MAX_DELAY_MS)
        } else {
            0.0
        };
        strip.sends.dedup();
        strip.compressor = strip.compressor.sanitized();
        strip.low_cut = strip.low_cut.sanitized();
        strip.gate = strip.gate.sanitized();
        strip.eq = strip.eq.sanitized();
        strip.de_esser = strip.de_esser.sanitized();
        strip.leveler = strip.leveler.sanitized();
        self.apply();
        Ok(())
    }

    /// Solo a source to the monitor (pre-fader listen), or stop.
    ///
    /// # Errors
    ///
    /// [`AudioMixError::NoSuchSource`] if there is no such source.
    pub fn set_solo(&mut self, uuid: &str, solo: bool) -> Result<(), AudioMixError> {
        let known = self.has_source(uuid);
        if !known {
            return Err(AudioMixError::NoSuchSource(uuid.into()));
        }
        if solo {
            self.solos.insert(uuid.to_string());
        } else {
            self.solos.remove(uuid);
        }
        self.apply();
        Ok(())
    }

    /// Change the monitor's settings with `edit`.
    ///
    /// # Errors
    ///
    /// [`AudioMixError::NoSuchBus`] if it is set to listen to a bus that does
    /// not exist.
    pub fn edit_monitor(
        &mut self,
        edit: impl FnOnce(&mut AudioMonitorConfig),
    ) -> Result<(), AudioMixError> {
        let mut monitor = self.config.monitor.clone();
        edit(&mut monitor);
        if let Some(bus) = &monitor.listen {
            if !self.has_bus(bus) {
                return Err(AudioMixError::NoSuchBus(bus.clone()));
            }
        }
        monitor.gain_db = monitor.gain_db.clamp(-96.0, 12.0);
        self.config.monitor = monitor;
        self.apply();
        Ok(())
    }

    /// Listen to what the monitor plays (diagnostics and tests).
    pub fn tap_monitor(&self) -> Option<BusTap> {
        Some(self.core.lock().ok()?.tap_monitor())
    }

    /// Bring the clip sources in line with the decks that have sound. A clip
    /// seen for the first time is sent to the first bus with Audio Auto on,
    /// so it is heard when it goes on air. Settings for a deck that goes away
    /// are kept, for an undo that brings it back.
    pub fn sync_clips(&mut self, clips: Vec<ClipSource>) {
        let same = clips.len() == self.clips.len()
            && clips.iter().all(|c| {
                self.clips
                    .get(&c.deck_uuid)
                    .is_some_and(|(name, shared)| name == &c.name && Arc::ptr_eq(shared, &c.shared))
            });
        if same {
            return;
        }
        let default_send: Vec<String> =
            self.default_bus().map(str::to_string).into_iter().collect();
        for clip in &clips {
            if !self.config.strips.iter().any(|s| s.uuid == clip.deck_uuid) {
                self.config.strips.push(AudioStripConfig {
                    compressor: CompressorConfig::default(),
                    low_cut: LowCutConfig::default(),
                    gate: GateConfig::default(),
                    eq: EqConfig::default(),
                    de_esser: DeEsserConfig::default(),
                    leveler: LevelerConfig::default(),
                    uuid: clip.deck_uuid.clone(),
                    gain_db: 0.0,
                    // Off air until it is taken there.
                    muted: true,
                    sends: default_send.clone(),
                    send_settings: BTreeMap::new(),
                    sent_before: Vec::new(),
                    auto: true,
                    follow: None,
                    balance: 0.0,
                delay_ms: 0.0,
                });
            }
        }
        self.clips = clips
            .into_iter()
            .map(|c| (c.deck_uuid, (c.name, c.shared)))
            .collect();
        if let Ok(mut core) = self.core.lock() {
            let inputs: Vec<&str> = self.config.inputs.iter().map(|i| i.uuid.as_str()).collect();
            core.sources.retain(|s| match &s.feed {
                Feed::Clip(shared) => self
                    .clips
                    .get(&s.uuid)
                    .is_some_and(|(_, live)| Arc::ptr_eq(live, shared)),
                Feed::Input(_) => inputs.contains(&s.uuid.as_str()),
                // Kept in step by `sync_live`.
                Feed::Live(_) => true,
                #[cfg(test)]
                Feed::Fifo(_) => true,
            });
            for (uuid, (_, shared)) in &self.clips {
                if core.source(uuid).is_none() {
                    core.sources
                        .push(Source::new(uuid.clone(), Feed::Clip(shared.clone())));
                }
            }
        }
        self.apply();
    }

    /// The key a live source's sound is picked up under, if the mixer has it.
    pub fn live_key(&self, deck_uuid: &str) -> Option<u64> {
        self.lives.get(deck_uuid).map(|(_, key)| *key)
    }

    /// Bring the live sources' sound in line with the scene: a source seen
    /// for the first time gets a strip like a clip's — Audio Auto on, off
    /// until it goes on air, sent to the first bus — and one picked up
    /// under a new key gets its new feed. Settings for a source that goes
    /// are kept, for an undo that brings it back.
    pub fn sync_live(&mut self, sounds: Vec<LiveSound>) {
        let same = sounds.len() == self.lives.len()
            && sounds.iter().all(|s| {
                s.feed.is_none()
                    && self
                        .lives
                        .get(&s.deck_uuid)
                        .is_some_and(|(name, key)| name == &s.name && *key == s.key)
            });
        if same {
            return;
        }
        let default_send: Vec<String> =
            self.default_bus().map(str::to_string).into_iter().collect();
        for sound in &sounds {
            if !self.config.strips.iter().any(|s| s.uuid == sound.deck_uuid) {
                self.config.strips.push(AudioStripConfig {
                    compressor: CompressorConfig::default(),
                    low_cut: LowCutConfig::default(),
                    gate: GateConfig::default(),
                    eq: EqConfig::default(),
                    de_esser: DeEsserConfig::default(),
                    leveler: LevelerConfig::default(),
                    uuid: sound.deck_uuid.clone(),
                    gain_db: 0.0,
                    muted: true,
                    sends: default_send.clone(),
                    send_settings: BTreeMap::new(),
                    sent_before: Vec::new(),
                    auto: true,
                    follow: None,
                    balance: 0.0,
                delay_ms: 0.0,
                });
            }
        }
        if let Ok(mut core) = self.core.lock() {
            core.sources.retain(|s| {
                !matches!(s.feed, Feed::Live(_)) || sounds.iter().any(|l| l.deck_uuid == s.uuid)
            });
            for sound in &sounds {
                let Some(feed) = &sound.feed else {
                    continue;
                };
                core.sources.retain(|s| s.uuid != sound.deck_uuid);
                let right = u16::from(feed.channels > 1);
                let input =
                    InputFeed::network(feed.rx.clone(), feed.rate, feed.channels, (0, right));
                core.sources
                    .push(Source::new(sound.deck_uuid.clone(), Feed::Live(input)));
            }
        }
        self.lives = sounds
            .into_iter()
            .map(|s| (s.deck_uuid, (s.name, s.key)))
            .collect();
        self.apply();
    }

    /// This frame's on-air level (0 to 1) of every source with Audio Auto,
    /// keyed by UUID, measured where it looks ([`AudioStripConfig::follow`]);
    /// missing is off air.
    ///
    /// Going on air presses the strip's On button, leaving releases it — the
    /// operator can still press it in between. A fade moves the level
    /// through the fade, so the sound fades with the picture: in as it
    /// starts, out until it is gone, and then the button is released. The
    /// first time a source is seen its button is put where it would have
    /// been switched to.
    pub fn set_on_air(&mut self, levels: HashMap<String, f32>) {
        let mut switched = false;
        let linked: Vec<&str> = self
            .config
            .inputs
            .iter()
            .filter(|i| i.deck.is_some())
            .map(|i| i.uuid.as_str())
            .collect();
        for strip in &mut self.config.strips {
            let follows_a_picture = self.clips.contains_key(&strip.uuid)
                || self.lives.contains_key(&strip.uuid)
                || linked.contains(&strip.uuid.as_str());
            if !strip.auto || !follows_a_picture {
                continue;
            }
            let now = levels.get(&strip.uuid).is_some_and(|l| *l > 0.0);
            let edge = self
                .on_air
                .get(&strip.uuid)
                .is_none_or(|l| (*l > 0.0) != now);
            if edge && strip.muted == now {
                strip.muted = !now;
                switched = true;
            }
        }
        self.on_air = levels;
        if switched {
            self.apply();
        } else {
            self.apply_gates();
        }
    }

    /// This frame's level (0 to 1) of every source a rundown step is fading
    /// out, keyed by UUID; missing is not faded. It scales the sound as an
    /// Audio Auto fade does — with it, for a source on air — whether the
    /// strip has Audio Auto or not: music on no screen fades out too.
    pub fn set_step_fades(&mut self, levels: HashMap<String, f32>) {
        if levels == self.step_fades {
            return;
        }
        self.step_fades = levels;
        self.apply_gates();
    }

    /// A source's rundown fade as last set, for tests.
    #[cfg(test)]
    pub(crate) fn step_fade(&self, uuid: &str) -> Option<f32> {
        self.step_fades.get(uuid).copied()
    }

    /// Take the sound of the apps the inputs name, each while it runs (see
    /// [`apps`]): tell the app sound thread which apps are wanted, and mix
    /// the sound it hands back — or stop mixing it when the app is let go.
    fn keep_apps(&mut self) {
        let wanted: Vec<apps::Want> = self
            .config
            .inputs
            .iter()
            .filter_map(|i| {
                let generation = self.apps_generation.get(&i.uuid).copied().unwrap_or(0);
                i.app.clone().map(|app| (i.uuid.clone(), app, generation))
            })
            .collect();
        self.apps.want(wanted);
        let events = self.apps.events();
        self.take_app_events(events);
    }

    /// Mix the sound the app sound thread hands back, stop mixing what it let
    /// go, and keep why any was refused.
    fn take_app_events(&mut self, events: Vec<apps::Event>) {
        let mut taken = false;
        for event in events {
            match event {
                apps::Event::Taken {
                    uuid,
                    app,
                    generation,
                    feed,
                } => {
                    // An input removed since it was asked for — or, a show
                    // loaded since, now another app's or another
                    // generation's — takes nothing.
                    if !self.is_current(&uuid, &app, generation) {
                        continue;
                    }
                    self.apps_refused.remove(&uuid);
                    // The tap's two channels: its device holds no others.
                    let source = InputFeed::new(feed.rx, feed.rate, feed.channels, 0, 1);
                    if let Ok(mut core) = self.core.lock() {
                        core.sources.retain(|s| s.uuid != uuid);
                        core.sources.push(Source::new(uuid, Feed::Input(source)));
                    }
                    taken = true;
                }
                apps::Event::Lost {
                    uuid,
                    app,
                    generation,
                } => {
                    // Only while the input is still that app's, that
                    // generation's: a show loaded since may have given its
                    // UUID to a microphone, or tapped it afresh.
                    if self.is_current(&uuid, &app, generation) {
                        self.apps_refused.remove(&uuid);
                        if let Ok(mut core) = self.core.lock() {
                            core.sources.retain(|s| s.uuid != uuid);
                        }
                    }
                }
                apps::Event::Refused {
                    uuid,
                    app,
                    generation,
                    why,
                } => {
                    if self.is_current(&uuid, &app, generation) {
                        self.apps_refused.insert(uuid, why);
                    }
                }
                apps::Event::Listed(apps) => self.running_apps = apps,
            }
        }
        if taken {
            self.apply();
        }
    }

    /// Whether the input `uuid` is still the app `app`'s sound, at
    /// `generation`.
    fn is_current(&self, uuid: &str, app: &str, generation: u64) -> bool {
        self.apps_generation.get(uuid).copied().unwrap_or(0) == generation
            && self
                .config
                .inputs
                .iter()
                .any(|i| i.uuid == uuid && i.app.as_deref() == Some(app))
    }

    /// Subscribe configured inputs to their devices, and release removed
    /// ones. A device that is not connected is looked for again every two
    /// seconds, so a USB mixer plugged in mid-show comes back by itself. A
    /// device not open is opened on a thread of its own — opening takes the
    /// OS ~0.1 s, and the show's frames do not wait — and its inputs taken
    /// as soon as it is open.
    pub fn connect_inputs(&mut self, capture: &mut crate::audio::AudioManager) {
        // Before the apps' sound is taken: the sound released with a
        // subscription is then always the device's, never an app's.
        self.release_inputs(|id, token| capture.unsubscribe_pcm(id, token));
        self.forget_lost_inputs(|id, token| capture.serves(id, token));
        self.keep_apps();
        self.keep_live_taps(capture);
        // An app's sound is taken by `keep_apps`, not through the capture
        // manager.
        let devices = self.config.inputs.iter().filter(|i| i.app.is_none()).count();
        if self.inputs.len() == devices {
            return;
        }
        if self.inputs_tried.is_some_and(|t| t.elapsed() < RETRY)
            && capture.landed() == self.inputs_landed
        {
            return;
        }
        self.inputs_tried = Some(Instant::now());
        self.inputs_landed = capture.landed();
        let pending: Vec<AudioInputConfig> = self
            .config
            .inputs
            .iter()
            .filter(|i| i.app.is_none() && !self.inputs.contains_key(&i.uuid))
            .cloned()
            .collect();
        for input in pending {
            let Some(id) = capture
                .devices()
                .iter()
                .find(|d| d.name == input.device)
                .map(|d| d.id)
            else {
                continue;
            };
            let Some(sub) = capture.subscribe_pcm_soon(id) else {
                continue;
            };
            log::info!(
                "Audio input '{}' on '{}' ({} Hz, {} ch)",
                input.name,
                input.device,
                sub.format.sample_rate,
                sub.format.channels
            );
            let feed = InputFeed::new(
                sub.receiver,
                sub.format.sample_rate,
                sub.format.channels,
                input.left,
                input.right,
            );
            if let Ok(mut core) = self.core.lock() {
                core.sources.retain(|s| s.uuid != input.uuid);
                core.sources
                    .push(Source::new(input.uuid.clone(), Feed::Input(feed)));
            }
            self.inputs.insert(
                input.uuid.clone(),
                LiveInput {
                    source_id: id,
                    token: sub.token,
                    rate: sub.format.sample_rate,
                    device: input.device.clone(),
                    left: input.left,
                    right: input.right,
                },
            );
        }
        self.apply();
    }

    /// Each live bus's mics (see [`AudioBusConfig::live`]): inputs on its
    /// own device sent to it — by bus UUID.
    fn live_direct(&self) -> Vec<(String, Vec<(AudioInputConfig, bool)>)> {
        self.config
            .buses
            .iter()
            .filter(|b| b.live)
            .filter_map(|bus| {
                let device = bus.device.as_deref()?;
                let strip = |i: &AudioInputConfig| self.config.strips.iter().find(|s| s.uuid == i.uuid);
                let on_device = || {
                    self.config
                        .inputs
                        .iter()
                        .filter(|i| i.app.is_none() && i.device == device)
                };
                let sent = |i: &AudioInputConfig| strip(i).is_some_and(|s| s.sends.contains(&bus.uuid));
                let kept = |i: &AudioInputConfig| {
                    strip(i).is_some_and(|s| !s.sends.contains(&bus.uuid) && s.sent_before.contains(&bus.uuid))
                };
                // Its mics: those sent to it, then — while there is room —
                // those whose send is off but kept (its settings saved): a send
                // turned off or on again is then only its gain, gliding in the
                // callback, not the device opened again (a gap for the whole
                // bus, a click as it went). Listed in the inputs' own order, so
                // a toggle never reorders them (another feed: the same reopen).
                let mut room = crate::audio::LIVE_MICS.saturating_sub(on_device().filter(|i| sent(i)).count());
                let chosen: std::collections::HashSet<&str> = on_device()
                    .filter(|i| {
                        if sent(i) {
                            return true;
                        }
                        if kept(i) && room > 0 {
                            room -= 1;
                            return true;
                        }
                        false
                    })
                    .map(|i| i.uuid.as_str())
                    .collect();
                let mics = on_device()
                    .filter(|i| chosen.contains(i.uuid.as_str()))
                    .map(|i| (i.clone(), !sent(i)))
                    .collect();
                Some((bus.uuid.clone(), mics))
            })
            .collect()
    }

    /// Keep each live bus's feed — its mics, straight from their capture
    /// (open already for the mics' own inputs) — taken again when its mics
    /// change, let go when it is no longer live. Played the usual way, and
    /// said once why, when its mics are not all on one 48 kHz capture or
    /// are more than [`crate::audio::LIVE_MICS`].
    fn keep_live_taps(&mut self, capture: &mut crate::audio::AudioManager) {
        let direct = self.live_direct();
        if direct.is_empty() && self.live_taps.is_empty() && self.live_refused.is_empty() {
            return;
        }
        let mut wanted: HashMap<String, LiveWanted> = HashMap::new();
        let mut refused: HashMap<String, String> = HashMap::new();
        for (bus, mics) in &direct {
            // A plug-in is not run in a device's callback (it may hang it).
            if self.config.buses.iter().any(|b| b.uuid == *bus && b.insert.is_some()) {
                refused.insert(bus.clone(), "it has an Audio Unit insert".to_string());
                continue;
            }
            // A mic only kept (its send off) never holds the bus back: left
            // out unless it can play here — connected, on the capture its sent
            // mics are on, on channels that capture has.
            let sent_on = mics
                .iter()
                .filter(|(_, kept)| !kept)
                .find_map(|(m, _)| self.inputs.get(&m.uuid).map(|live| live.source_id));
            let mics: Vec<&AudioInputConfig> = mics
                .iter()
                .filter(|(m, kept)| {
                    !kept
                        || self.inputs.get(&m.uuid).is_some_and(|live| {
                            sent_on.is_none_or(|s| s == live.source_id)
                                && capture
                                    .channels_of(live.source_id)
                                    .is_some_and(|ch| m.left.max(m.right) < ch)
                        })
                })
                .map(|(m, _)| m)
                .collect();
            let connected: Vec<(&AudioInputConfig, &LiveInput)> = mics
                .iter()
                .filter_map(|m| self.inputs.get(&m.uuid).map(|live| (*m, live)))
                .collect();
            if connected.is_empty() || connected.len() != mics.len() {
                continue;
            }
            let source = connected[0].1.source_id;
            if connected.iter().any(|(_, live)| live.source_id != source) {
                refused.insert(bus.clone(), "its mics are on more than one capture".to_string());
                continue;
            }
            if connected[0].1.rate != SAMPLE_RATE {
                refused.insert(
                    bus.clone(),
                    format!("its device runs at {} Hz, not {SAMPLE_RATE}", connected[0].1.rate),
                );
                continue;
            }
            if connected.len() > crate::audio::LIVE_MICS {
                refused.insert(bus.clone(), format!("it has more than {} mics", crate::audio::LIVE_MICS));
                continue;
            }
            let list = connected
                .iter()
                .map(|(m, _)| (m.uuid.clone(), (m.left, m.right)))
                .collect();
            wanted.insert(bus.clone(), (source, list));
        }
        if refused != self.live_refused {
            for (bus, why) in &refused {
                if self.live_refused.get(bus) != Some(why) {
                    log::warn!("Live bus {bus}: played the usual way — {why}");
                }
            }
            self.live_refused = refused;
        }
        // Let go of a feed no longer wanted, or whose mics changed.
        let stale: Vec<String> = self
            .live_taps
            .iter()
            .filter(|(bus, feed)| {
                wanted.get(*bus).is_none_or(|(source, list)| {
                    *source != feed.source_id
                        || list.len() != feed.mics.len()
                        || list.iter().zip(&feed.mics).any(|(a, b)| a.0 != b.0 || a.1 != b.1)
                })
            })
            .map(|(bus, _)| bus.clone())
            .collect();
        let mut changed = !stale.is_empty();
        for bus in stale {
            if let Some(feed) = self.live_taps.remove(&bus) {
                capture.unsubscribe_pcm(feed.source_id, feed.token);
            }
        }
        for (bus, (source, list)) in wanted {
            if self.live_taps.contains_key(&bus) {
                continue;
            }
            changed = true;
            let channels: Vec<(u16, u16)> = list.iter().map(|(_, c)| *c).collect();
            let Some(tap) = capture.subscribe_live(source, &channels) else {
                continue;
            };
            let mics = list
                .into_iter()
                .map(|(uuid, c)| (uuid, c, Arc::new(std::sync::atomic::AtomicU32::new(0))))
                .collect();
            self.live_taps.insert(
                bus,
                LiveBusFeed {
                    source_id: source,
                    token: tap.token,
                    frames: tap.frames,
                    counts: tap.counts,
                    mics,
                    control: Arc::default(),
                    rest: crossbeam_channel::bounded(live::LIVE_REST_FRAMES),
                },
            );
        }
        if changed {
            self.live_feeds_changed();
        }
    }

    /// A live bus's feed taken, let go or made anew: everything applied
    /// again — its mics' gains and processing before it plays, and the
    /// mixer's sender for the rest of the bus (a feed made anew under the
    /// same bus, its mics changed, has a new channel; the old sender, kept,
    /// sent the rest of the bus nowhere).
    fn live_feeds_changed(&mut self) {
        self.apply();
    }

    /// Drop the inputs whose subscription no longer runs (`serves` says
    /// not): their device was lost, unplugged (see
    /// `AudioManager::forget_lost`). Kept, such an input looked connected,
    /// silent, and was never subscribed again; dropped with its sound, it
    /// is looked for every [`RETRY`] and taken again once the device is back.
    fn forget_lost_inputs(
        &mut self,
        serves: impl Fn(crate::audio::AudioSourceId, crate::audio::PcmToken) -> bool,
    ) {
        let lost: Vec<String> = self
            .inputs
            .iter()
            .filter(|(_, live)| !serves(live.source_id, live.token))
            .map(|(uuid, _)| uuid.clone())
            .collect();
        if lost.is_empty() {
            return;
        }
        if let Ok(mut core) = self.core.lock() {
            core.sources.retain(|s| !lost.contains(&s.uuid));
        }
        for uuid in &lost {
            self.inputs.remove(uuid);
        }
    }

    /// Let go, through `unsubscribe`, of the device subscriptions — with
    /// their sound — whose input is no longer what was subscribed (removed,
    /// made an app's sound, or on another device or channels under the same
    /// UUID), and of those whose sound is no longer mixed: dropped since, as
    /// by a show loaded or the input removed, and the input perhaps brought
    /// straight back before this. Kept, such a subscription would make its
    /// input look connected, silent, and never be made again.
    fn release_inputs(
        &mut self,
        mut unsubscribe: impl FnMut(crate::audio::AudioSourceId, crate::audio::PcmToken),
    ) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let inputs = &self.config.inputs;
        let gone: Vec<String> = self
            .inputs
            .iter()
            .filter(|(uuid, live)| {
                core.source(uuid).is_none()
                    || !inputs.iter().any(|i| &i.uuid == *uuid && live.serves(i))
            })
            .map(|(uuid, _)| uuid.clone())
            .collect();
        if gone.is_empty() {
            return;
        }
        core.sources.retain(|s| !gone.contains(&s.uuid));
        drop(core);
        for uuid in gone {
            if let Some(live) = self.inputs.remove(&uuid) {
                unsubscribe(live.source_id, live.token);
            }
        }
    }

    /// Keep the buses' devices playing: say which are wanted (a missing one
    /// is looked for every [`RETRY`] on the device thread), and follow what
    /// plays. Called every frame; never waits on a device.
    pub fn poll_devices(&mut self) {
        self.take_listing();
        self.track_live_levelers();
        self.take_insert_settings();
        let monitor = &self.config.monitor;
        let wanted: Vec<(String, String, u16)> = self
            .config
            .buses
            .iter()
            .filter_map(|b| {
                b.device
                    .clone()
                    .map(|d| (b.uuid.clone(), d, b.first_channel))
            })
            .chain(
                monitor
                    .device
                    .clone()
                    .map(|d| (MONITOR_KEY.to_string(), d, monitor.first_channel)),
            )
            .collect();
        // Each live bus, once its feed is taken.
        let shared = live_frames_shared(&self.config.buses);
        let mut lives: HashMap<String, outputs::LiveWant> = HashMap::new();
        for (bus, feed) in &self.live_taps {
            let bus_gain = Arc::clone(self.live_bus_gains.entry(bus.clone()).or_default());
            let frames = shared.get(bus).copied().unwrap_or(64);
            lives.insert(
                bus.clone(),
                outputs::LiveWant {
                    frames,
                    feed: feed.frames.clone(),
                    counts: Arc::clone(&feed.counts),
                    mics: feed
                        .mics
                        .iter()
                        .map(|(uuid, _, gain)| (uuid.clone(), Arc::clone(gain)))
                        .collect(),
                    bus_gain,
                    control: Arc::clone(&feed.control),
                    rest: feed.rest.1.clone(),
                },
            );
        }
        let posted: std::collections::HashSet<String> = lives.keys().cloned().collect();
        let wanted = wanted
            .into_iter()
            .map(|(key, device, first_channel)| {
                let ring = self.rings.entry(key.clone()).or_default().clone();
                let live = lives.remove(&key);
                outputs::Want {
                    key,
                    device,
                    first_channel,
                    ring,
                    live,
                }
            })
            .collect();
        self.outputs.want(wanted);
        if posted != self.live_posted {
            for bus in posted.difference(&self.live_posted) {
                log::info!("Audio bus {bus} plays live: its mics mixed in its device's callback");
            }
            self.live_posted = posted;
            self.apply();
        }
        if self.outputs.changed() {
            self.apply();
        }
    }

    /// Look for sound devices again (a USB interface plugged in).
    ///
    /// Listed on a thread of its own and taken in on a later frame: listing
    /// asks `CoreAudio` about every device, which with virtual devices
    /// installed takes a while, and waits for the microphone prompt while
    /// macOS has not decided.
    pub fn rescan_devices(&mut self) {
        if self.listing.is_some() {
            self.list_again = true;
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("audio-device-listing".into())
            .spawn(move || {
                let outputs = device::output_devices();
                let inputs = device::input_devices()
                    .into_iter()
                    .map(|(name, channels)| AudioInputDevice { name, channels })
                    .collect();
                let effects = plugins::installed_effects();
                let _ = tx.send((outputs, inputs, effects));
            });
        match spawned {
            Ok(_) => self.listing = Some(rx),
            Err(e) => log::warn!("Listing sound devices: {e}"),
        }
    }

    /// Take in a finished device listing (see [`Self::rescan_devices`]).
    fn take_listing(&mut self) {
        let Some(listing) = &self.listing else {
            return;
        };
        match listing.try_recv() {
            Ok((outputs, inputs, effects)) => {
                self.output_devices = outputs.iter().map(|(name, _)| name.clone()).collect();
                self.output_channels = outputs.into_iter().collect();
                self.input_devices = inputs;
                if *self.effects != effects {
                    self.effects = Arc::new(effects);
                }
                self.listing = None;
                self.outputs.retry_now();
                self.inputs_tried = None;
                if std::mem::take(&mut self.list_again) {
                    self.rescan_devices();
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.listing = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
    }

    /// A listing being made, as a thread makes it — for tests.
    #[cfg(test)]
    pub(crate) fn listing_for_tests(&mut self) -> std::sync::mpsc::Sender<DeviceListing> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.listing = Some(rx);
        tx
    }

    // ── Recordings and streams ──────────────────────────────────────

    /// Listen to a bus, for a recording or stream: 48 kHz stereo f32. The
    /// tap ends by itself when the receiver is dropped.
    pub fn tap(&self, bus: &str) -> Option<BusTap> {
        self.core.lock().ok()?.tap(bus)
    }

    /// End a tap from [`Self::tap`] now rather than on the next block.
    pub fn untap(&self, token: u64) {
        if let Ok(mut core) = self.core.lock() {
            core.untap(token);
        }
    }

    // ── Showing it ──────────────────────────────────────────────────

    /// Why `bus` does not play as set (see
    /// [`AudioBusSnapshot::device_note`]): set live and not taken live, why;
    /// then its device's own note.
    fn bus_note(&self, bus: &AudioBusConfig) -> Option<String> {
        let live = (bus.live && !self.live_posted.contains(&bus.uuid)).then(|| {
            self.live_refused.get(&bus.uuid).map_or_else(
                || "Live: waits for a mic on its own device sent to it".to_string(),
                |why| format!("Live refused ({why}): played the usual way"),
            )
        });
        let parts: Vec<String> = live
            .into_iter()
            .chain(self.outputs.note(&bus.uuid).map(str::to_string))
            .collect();
        (!parts.is_empty()).then(|| parts.join("; "))
    }

    /// Everything the UI and API show, meters included.
    pub fn snapshot(&self) -> AudioMixSnapshot {
        let Ok(core) = self.core.lock() else {
            return AudioMixSnapshot::default();
        };
        let meter_db = |m: [f32; 2]| [gain_to_db(m[0]), gain_to_db(m[1])];
        let buses = self
            .config
            .buses
            .iter()
            .map(|b| AudioBusSnapshot {
                peak_db: core
                    .bus(&b.uuid)
                    .map_or([-96.0; 2], |bus| meter_db(bus.held_peak)),
                dynamics: core.bus(&b.uuid).map_or_else(
                    || DynamicsSnapshot {
                        config: b.compressor,
                        limiter: Some(b.limiter),
                        ..DynamicsSnapshot::default()
                    },
                    |bus| bus.dynamics.snapshot(),
                ),
                config: b.clone(),
                device_ok: b.device.as_ref().map(|_| self.outputs.is_playing(&b.uuid)),
                device_note: b
                    .device
                    .as_ref()
                    .and_then(|_| self.bus_note(b)),
                live: b.live.then(|| {
                    self.live_posted.contains(&b.uuid) && self.outputs.is_playing_live(&b.uuid)
                }),
                insert_status: b.insert.as_ref().and(core.bus(&b.uuid)).and_then(|bus| {
                    let insert = bus.insert.as_ref()?;
                    let state = match insert.status.state() {
                        insert::InsertState::Loading => "loading",
                        insert::InsertState::Failed => "failed",
                        insert::InsertState::Playing if insert.bypass.load(Ordering::Relaxed) => {
                            "bypassed"
                        }
                        insert::InsertState::Playing => "playing",
                    };
                    Some(AudioInsertStatus {
                        state: state.into(),
                        why: insert.status.why(),
                        late_blocks: insert.status.late(),
                        latency_ms: insert.status.latency_ms(),
                    })
                }),
                meter_db: core
                    .bus(&b.uuid)
                    .map_or([-96.0; 2], |bus| meter_db(bus.meter())),
            })
            .collect();
        let strip = |uuid: &str| self.config.strips.iter().find(|s| s.uuid == uuid);
        let mut sources = Vec::new();
        let mut clips: Vec<(&String, &String)> = self
            .clips
            .iter()
            .map(|(uuid, (name, _))| (uuid, name))
            .collect();
        clips.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
        let mut lives: Vec<(&String, &String)> = self
            .lives
            .iter()
            .map(|(uuid, (name, _))| (uuid, name))
            .collect();
        lives.sort_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)));
        let kinds = clips
            .into_iter()
            .map(|(uuid, name)| {
                (
                    uuid.clone(),
                    name.clone(),
                    AudioSourceKind::Clip {
                        deck_uuid: uuid.clone(),
                    },
                )
            })
            .chain(lives.into_iter().map(|(uuid, name)| {
                (
                    uuid.clone(),
                    name.clone(),
                    AudioSourceKind::Live {
                        deck_uuid: uuid.clone(),
                    },
                )
            }))
            .chain(self.config.inputs.iter().map(|i| {
                (
                    i.uuid.clone(),
                    i.name.clone(),
                    AudioSourceKind::Input {
                        device: i.device.clone(),
                        left: i.left,
                        right: i.right,
                        deck: i.deck.clone(),
                        app: i.app.clone(),
                    },
                )
            }));
        for (uuid, name, kind) in kinds {
            let Some(s) = strip(&uuid) else {
                continue;
            };
            let live = core.source(&uuid);
            sources.push(AudioSourceSnapshot {
                peak_db: live.map_or([-96.0; 2], |source| meter_db(source.held_peak)),
                dynamics: live.map_or_else(
                    || DynamicsSnapshot {
                        config: s.compressor,
                        low_cut: Some(s.low_cut),
                        gate: Some(s.gate),
                        eq: Some(s.eq),
                        de_esser: Some(s.de_esser),
                        leveler: Some(s.leveler),
                        ..DynamicsSnapshot::default()
                    },
                    |source| source.dynamics.snapshot(),
                ),
                uuid: uuid.clone(),
                name,
                kind,
                gain_db: s.gain_db,
                muted: s.muted,
                sends: s.sends.clone(),
                send_settings: s.send_settings.clone(),
                sent_before: s.sent_before.clone(),
                auto: s.auto,
                follow: s.follow.clone(),
                balance: s.balance,
                delay_ms: s.delay_ms,
                on_air: if s.auto {
                    self.on_air.get(&uuid).copied().unwrap_or(0.0)
                } else {
                    0.0
                },
                gate: live.map_or(0.0, |l| l.gate),
                solo: self.solos.contains(&uuid),
                live: live.is_some(),
                meter_db: live.map_or([-96.0; 2], |l| meter_db(l.meter())),
                signal_db: live.map_or([-96.0; 2], |l| meter_db(l.signal())),
                refused: self.apps_refused.get(&uuid).cloned(),
            });
        }
        let monitor = AudioMonitorSnapshot {
            config: self.config.monitor.clone(),
            device_ok: self
                .config
                .monitor
                .device
                .as_ref()
                .map(|_| self.outputs.is_playing(MONITOR_KEY)),
            device_note: self
                .config
                .monitor
                .device
                .as_ref()
                .and_then(|_| self.outputs.note(MONITOR_KEY).map(str::to_string)),
            soloing: core.soloing(),
            meter_db: meter_db(core.monitor.meter()),
        };
        AudioMixSnapshot {
            buses,
            sources,
            monitor,
            output_devices: self.output_devices.clone(),
            output_channels: self.output_channels.clone(),
            effects: Arc::clone(&self.effects),
            input_devices: self.input_devices.clone(),
            apps: self.running_apps.clone(),
            stop_fade_secs: self.stop_fade_secs(),
            stopping: Vec::new(),
        }
    }

    // ── Applying settings to the mixing thread ──────────────────────

    /// Copy the settings onto the mixing thread's buses and sources.
    fn apply(&mut self) {
        // A new bus's processing is built here, before the mixer is held,
        // not inside it.
        let mut fresh: HashMap<String, Bus> = self
            .config
            .buses
            .iter()
            .filter(|cfg| !self.applied_buses.contains(&cfg.uuid))
            .map(|cfg| (cfg.uuid.clone(), Bus::new(cfg.uuid.clone())))
            .collect();
        self.applied_buses = self.config.buses.iter().map(|b| b.uuid.clone()).collect();
        // A bus whose insert changed gets its new one made here (its thread
        // starts loading the plug-in), before the mixer is held.
        let mut inserts: HashMap<String, Option<insert::Insert>> = HashMap::new();
        for cfg in &self.config.buses {
            let wanted = cfg.insert.as_ref().map(|i| &i.component);
            if self.remake_inserts || self.applied_inserts.get(&cfg.uuid) != wanted {
                let settings = cfg.insert.as_ref().and_then(|i| i.settings.as_deref()).and_then(|b64| {
                    use base64::Engine as _;
                    base64::engine::general_purpose::STANDARD.decode(b64).ok()
                });
                inserts.insert(
                    cfg.uuid.clone(),
                    wanted.map(|c| make_insert(&cfg.name, c, settings)),
                );
                // A new insert counts its settings changes from nought, and
                // the old one's window goes with it.
                self.insert_settings_seen.remove(&cfg.uuid);
                #[cfg(target_os = "macos")]
                au_editor::close(cfg.uuid.clone());
            }
        }
        // A bus gone (removed, or not in the show loaded): its insert's
        // window goes too.
        for uuid in self.applied_inserts.keys() {
            if !self.config.buses.iter().any(|b| b.uuid == *uuid) {
                self.insert_settings_seen.remove(uuid);
                #[cfg(target_os = "macos")]
                au_editor::close(uuid.clone());
            }
        }
        self.remake_inserts = false;
        self.applied_inserts = self
            .config
            .buses
            .iter()
            .filter_map(|b| Some((b.uuid.clone(), b.insert.as_ref()?.component.clone())))
            .collect();
        let mut let_go: Vec<insert::Insert> = Vec::new();
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let mut old: HashMap<String, Bus> = std::mem::take(&mut core.buses)
            .into_iter()
            .map(|b| (b.uuid.clone(), b))
            .collect();
        for cfg in &self.config.buses {
            let mut bus = old
                .remove(&cfg.uuid)
                .or_else(|| fresh.remove(&cfg.uuid))
                .unwrap_or_else(|| Bus::new(cfg.uuid.clone()));
            bus.gain_db = cfg.gain_db;
            bus.muted = cfg.muted;
            bus.delay_ms = cfg.delay_ms;
            bus.dynamics.configure(cfg.compressor, cfg.limiter);
            if let Some(fresh) = inserts.remove(&cfg.uuid) {
                let_go.extend(std::mem::replace(&mut bus.insert, fresh));
            }
            if let (Some(insert), Some(wanted)) = (&bus.insert, &cfg.insert) {
                insert.bypass.store(wanted.bypass, Ordering::Relaxed);
            }
            // A live bus's device is fed by its callback, not its ring —
            // once it plays live (refused, it plays its ring).
            let live = self.outputs.is_playing_live(&cfg.uuid);
            bus.outputs = match (&cfg.device, self.outputs.is_playing(&cfg.uuid) && !live) {
                (Some(_), true) => self.rings.get(&cfg.uuid).cloned().into_iter().collect(),
                _ => Vec::new(),
            };
            // Played live, the rest of the bus to its callback.
            bus.live = self
                .live_taps
                .get(&cfg.uuid)
                .filter(|_| live)
                .map(|feed| self::core::LiveSend {
                    mics: feed.mics.iter().map(|(uuid, _, _)| uuid.clone()).collect(),
                    tx: feed.rest.0.clone(),
                });
            if let Some(gain) = self.live_bus_gains.get(&cfg.uuid) {
                let g = if cfg.muted { 0.0 } else { db_to_gain(cfg.gain_db) };
                gain.store(g.to_bits(), Ordering::Relaxed);
            }
            core.buses.push(bus);
        }
        for source in &mut core.sources {
            if let Some(strip) = self.config.strips.iter().find(|s| s.uuid == source.uuid) {
                source.gain_db = strip.gain_db;
                source.muted = strip.muted;
                source.balance = strip.balance;
                source.delay_ms = strip.delay_ms;
                source.dynamics.configure(&strip.processing());
                source.sends = strip
                    .sends
                    .iter()
                    .filter_map(|bus| self.config.buses.iter().find(|b| b.uuid == *bus))
                    .map(|bus| strip.send_to(bus))
                    .collect();
            }
            source.solo = self.solos.contains(&source.uuid);
        }
        let monitor = &self.config.monitor;
        core.monitor.gain_db = monitor.gain_db;
        core.monitor_listen.clone_from(&monitor.listen);
        core.monitor.outputs = match (&monitor.device, self.outputs.is_playing(MONITOR_KEY)) {
            (Some(_), true) => self.rings.get(MONITOR_KEY).cloned().into_iter().collect(),
            _ => Vec::new(),
        };
        drop(core);
        // An old insert's thread ends once its side here is gone.
        drop(let_go);
        self.apply_gates();
        self.apply_live_settings();
    }

    /// Each running insert's settings, as its thread last read them, into
    /// the show's bus (base64), for the show to save — only for the effect
    /// the bus still has.
    fn take_insert_settings(&mut self) {
        if self.applied_inserts.is_empty() {
            return;
        }
        // The statuses taken under the mixer's lock; their settings, which
        // may be large, read after it is let go.
        let statuses: Vec<(String, Arc<insert::InsertStatus>)> = match self.core.lock() {
            Ok(core) => core
                .buses
                .iter()
                .filter_map(|b| Some((b.uuid.clone(), Arc::clone(&b.insert.as_ref()?.status))))
                .collect(),
            Err(_) => return,
        };
        let mut taken: Vec<(String, Vec<u8>)> = Vec::new();
        for (uuid, status) in statuses {
            let seen = self.insert_settings_seen.entry(uuid.clone()).or_insert(0);
            if let Some(settings) = status.settings_since(seen) {
                taken.push((uuid, settings));
            }
        }
        for (uuid, settings) in taken {
            let running = self.applied_inserts.get(&uuid);
            if let Some(insert) = self
                .config
                .buses
                .iter_mut()
                .find(|b| b.uuid == uuid)
                .and_then(|b| b.insert.as_mut())
                .filter(|i| Some(&i.component) == running)
            {
                use base64::Engine as _;
                insert.settings = Some(base64::engine::general_purpose::STANDARD.encode(settings).into());
            }
        }
    }

    /// Each live bus's mics' leveler gains, as the mixer's own levelers for
    /// their strips have them now: where a live callback opened again
    /// starts riding (see `live::LiveControl::set_levelers`).
    fn track_live_levelers(&self) {
        if self.live_taps.is_empty() {
            return;
        }
        let Ok(core) = self.core.lock() else {
            return;
        };
        for feed in self.live_taps.values() {
            feed.control.set_levelers(feed.mics.iter().map(|(uuid, _, _)| {
                core.sources
                    .iter()
                    .find(|s| s.uuid == *uuid)
                    .map_or(f32::NAN, |s| s.dynamics.snapshot().leveler_gain_db)
            }));
        }
    }

    /// Each live bus's processing, as its mics' strips and its own: the
    /// callback takes it on its next call.
    fn apply_live_settings(&self) {
        for (bus, feed) in &self.live_taps {
            let mics = feed.mics.iter().map(|(uuid, _, _)| uuid.as_str());
            if let Some(settings) = live_settings(&self.config, bus, mics) {
                feed.control.set(&settings);
            }
        }
    }

    fn apply_gates(&mut self) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        for source in &mut core.sources {
            let strip = self.config.strips.iter().find(|s| s.uuid == source.uuid);
            // Only a level on its way somewhere scales the sound: a fade.
            // Steady on air or off, the On button decides.
            let level = self.on_air.get(&source.uuid).copied().unwrap_or(0.0);
            let auto = if strip.is_some_and(|s| s.auto) && level > 0.0 && level < 1.0 {
                level
            } else {
                1.0
            };
            // A rundown step fading it out, on top.
            let step = self
                .step_fades
                .get(&source.uuid)
                .copied()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
            source.gate = auto * step;
            // A live bus's mic: its gain as the bus's callback reads it — its
            // send's level, and the fader (unless the send is before it), On,
            // Audio Auto's fade and a rundown step's.
            let on = if source.muted { 0.0 } else { source.gate.clamp(0.0, 1.0) };
            for (bus, feed) in &self.live_taps {
                let g = source.sends.iter().find(|s| s.bus == *bus).map_or(0.0, |send| {
                    let fader = if send.pre { 1.0 } else { db_to_gain(source.gain_db) };
                    send.gain * fader * on
                });
                for (uuid, _, gain) in &feed.mics {
                    if *uuid == source.uuid {
                        gain.store(g.to_bits(), Ordering::Relaxed);
                    }
                }
            }
            // A clip playing is decoded heard or not: its meter shows what it
            // plays while nobody hears it — sent nowhere, muted, held down —
            // and it is heard at once when let through, nothing to catch up.
            if let Feed::Clip(shared) = &source.feed {
                shared.set_wanted(true);
            }
        }
    }
}

impl Drop for AudioEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Mix a block every 10 ms, on the wall clock.
///
/// Recordings count time in samples, so the mix must produce exactly 48 000 a
/// second however the thread is scheduled: a late wake mixes the blocks it
/// owes at once. Only after a stall longer than a tenth of a second does it
/// give the time up rather than burst.
fn run_mixer(core: &Mutex<MixCore>, stop: &AtomicBool) {
    let block = Duration::from_secs_f64(BLOCK_FRAMES as f64 / f64::from(SAMPLE_RATE));
    let mut next = Instant::now();
    while !stop.load(Ordering::Acquire) {
        if let Ok(mut core) = core.lock() {
            // Behind before mixing, not after: what the processing held is
            // dropped before it could go out (see `MixCore::skipped`).
            let now = Instant::now();
            if next + Duration::from_millis(100) < now {
                // Too far behind to catch up block by block: jump ahead by
                // whole blocks — the part of one left over stays owed, so
                // jumps add up to the time they jumped — and tell the taps
                // open now, with the mixer still held, what was jumped over
                // (see `MixCore::skipped`): a tap opened after a jump owes
                // nothing for it.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Positive and small.
                let blocks = ((now - next).as_secs_f64() / block.as_secs_f64()) as u32;
                core.skipped(u64::from(blocks), next);
                next += block * blocks;
            }
            core.process_block();
            next += block;
        } else {
            next += block;
        }
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
    }
}

#[cfg(test)]
mod tests {

    /// A Reset input reopens the devices the inputs taken with its deck
    /// listen to — once each — and no other's.
    #[test]
    fn a_decks_sound_is_found_by_the_inputs_taken_with_it() {
        let mut engine = AudioEngine::new();
        let mut capture = crate::audio::AudioManager::listing(&["USB Capture HDMI", "Mic"]);
        let _hdmi = capture.open_for_test(0, "USB Capture HDMI");
        let _mic = capture.open_for_test(1, "Mic");
        engine.add_input_of_deck("Magewell 1-2", "USB Capture HDMI", (0, 1), "deck-a");
        engine.add_input_of_deck("Magewell 3", "USB Capture HDMI", (2, 2), "deck-a");
        engine.add_input_of_deck("Mic", "Mic", (0, 1), "deck-b");
        engine.connect_inputs(&mut capture);
        assert_eq!(engine.devices_of_deck("deck-a"), [0]);
        assert_eq!(engine.devices_of_deck("deck-b"), [1]);
        assert!(engine.devices_of_deck("deck-c").is_empty());
    }

    /// Asked to list the devices while a listing is being made: listed
    /// again once it is in.
    #[test]
    fn devices_asked_for_during_a_listing_are_listed_again_after_it() {
        let mut engine = AudioEngine::new();
        let tx = engine.listing_for_tests();
        engine.rescan_devices();
        assert!(engine.list_again, "noted while the listing is made");
        tx.send((Vec::new(), Vec::new(), Vec::new())).expect("sent");
        engine.take_listing();
        assert!(!engine.list_again);
        assert!(engine.listing.is_some(), "listed again");
    }
    use super::*;

    /// A mixer held up too long to catch up block by block jumps ahead, and
    /// a recording's tap is told what was jumped over: its sound would
    /// otherwise come early by that much for the rest of the take.
    #[test]
    fn a_mixer_held_up_tells_its_taps_what_it_jumped_over() {
        let mut core = MixCore::default();
        core.buses.push(Bus::new("master"));
        let tap = core.tap("master").expect("bus");
        let core = Arc::new(Mutex::new(core));
        let stop = Arc::new(AtomicBool::new(false));
        let running = {
            let (core, stop) = (core.clone(), stop.clone());
            std::thread::spawn(move || run_mixer(&core, &stop))
        };
        std::thread::sleep(Duration::from_millis(50));
        {
            let _held = core.lock().unwrap();
            std::thread::sleep(Duration::from_millis(400));
        }
        std::thread::sleep(Duration::from_millis(50));
        stop.store(true, Ordering::Release);
        running.join().unwrap();
        let lost = tap.lost_samples.load(Ordering::Relaxed) as f64 / 2.0 / f64::from(SAMPLE_RATE);
        let made = tap.receiver.try_iter().map(|c| c.samples.len()).sum::<usize>() as f64
            / 2.0
            / f64::from(SAMPLE_RATE);
        assert!(lost > 0.25, "about 0.4 s jumped over, {lost:.3} s told");
        assert!(
            (made + lost - 0.5).abs() < 0.1,
            "made {made:.3} s + lost {lost:.3} s should be the half second that passed"
        );
    }

    /// The sound handed back for an input is mixed only while the input
    /// still names that app — a show loaded since may have given its UUID to
    /// another; a refusal is kept for the strip until the sound is taken.
    /// PowerPoint's sound taken for a PowerPoint input: switched with that
    /// picture (Audio Auto, off until it is on air), and gone with it.
    #[test]
    fn an_apps_sound_for_an_input_goes_with_its_picture() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid =
            engine.add_app_input_of("PowerPoint", "com.microsoft.Powerpoint", Some("deck0001"));
        let input = engine
            .config()
            .inputs
            .iter()
            .find(|i| i.uuid == uuid)
            .unwrap();
        assert_eq!(input.deck.as_deref(), Some("deck0001"));
        assert_eq!(input.app.as_deref(), Some("com.microsoft.Powerpoint"));
        let strip = engine
            .config()
            .strips
            .iter()
            .find(|s| s.uuid == uuid)
            .unwrap();
        assert!(strip.auto && strip.muted);
        engine.remove_inputs_of_deck("deck0001");
        assert!(!engine.config().inputs.iter().any(|i| i.uuid == uuid));
    }

    #[test]
    fn app_sound_is_mixed_only_for_the_app_its_input_names() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_app_input("Teams", "com.microsoft.teams2");
        let feed = || {
            let (_tx, rx) = crossbeam_channel::bounded(1);
            LiveFeed {
                rx,
                rate: 48_000,
                channels: 2,
            }
        };
        let live = |engine: &AudioEngine| {
            engine
                .snapshot()
                .sources
                .iter()
                .find(|s| s.uuid == uuid)
                .is_some_and(|s| s.live)
        };
        let refused = |engine: &AudioEngine| {
            engine
                .snapshot()
                .sources
                .iter()
                .find(|s| s.uuid == uuid)
                .and_then(|s| s.refused.clone())
        };
        engine.take_app_events(vec![apps::Event::Taken {
            uuid: uuid.clone(),
            app: "us.zoom.xos".into(),
            generation: 0,
            feed: feed(),
        }]);
        assert!(!live(&engine), "another app's sound is not this input's");

        engine.take_app_events(vec![apps::Event::Refused {
            uuid: uuid.clone(),
            app: "com.microsoft.teams2".into(),
            generation: 0,
            why: "the tap was refused (status 1)".into(),
        }]);
        assert_eq!(refused(&engine).as_deref(), Some("the tap was refused (status 1)"));

        engine.take_app_events(vec![apps::Event::Taken {
            uuid: uuid.clone(),
            app: "com.microsoft.teams2".into(),
            generation: 0,
            feed: feed(),
        }]);
        assert!(live(&engine), "its own app's sound is mixed");
        assert_eq!(refused(&engine), None, "and the refusal forgotten");
    }

    /// A show loaded that gives an app input's UUID to a microphone stops
    /// the app's sound at once and forgets its refusal; the app sound
    /// thread's late word that the app was let go leaves the microphone be.
    #[test]
    fn a_show_loaded_over_an_apps_sound_stops_it_and_spares_what_replaced_it() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_app_input("zoom.us", "us.zoom.xos");
        let (_tx, rx) = crossbeam_channel::bounded(1);
        engine.take_app_events(vec![apps::Event::Taken {
            uuid: uuid.clone(),
            app: "us.zoom.xos".into(),
            generation: 0,
            feed: LiveFeed {
                rx,
                rate: 48_000,
                channels: 2,
            },
        }]);
        engine.apps_refused.insert(uuid.clone(), "old".into());
        let live = |engine: &AudioEngine| {
            engine
                .core
                .lock()
                .unwrap()
                .sources
                .iter()
                .any(|s| s.uuid == uuid)
        };
        assert!(live(&engine));

        // The same UUID, now a microphone.
        let mut show = engine.config().clone();
        let input = show.inputs.iter_mut().find(|i| i.uuid == uuid).unwrap();
        input.app = None;
        input.device = "USB Mixer".into();
        engine.load(show);
        assert!(!live(&engine), "the app's sound stops on load");
        assert!(engine.apps_refused.is_empty(), "its refusal is forgotten");

        // The microphone's own sound, then the thread's late word.
        engine.core.lock().unwrap().sources.push(Source::new(
            uuid.clone(),
            Feed::Fifo(Arc::new(Mutex::new(std::collections::VecDeque::new()))),
        ));
        engine.take_app_events(vec![apps::Event::Lost {
            uuid: uuid.clone(),
            app: "us.zoom.xos".into(),
            generation: 0,
        }]);
        assert!(live(&engine), "the microphone keeps its sound");
    }

    /// A stand-in for the system's taps: one app, which a test can quit.
    struct OneApp(Arc<AtomicBool>);

    /// An app tapped by [`OneApp`]: it makes no sound, but keeps its channel
    /// open as a real tap does.
    struct Tapped(#[allow(dead_code)] crossbeam_channel::Sender<crate::audio::PcmChunk>);

    impl apps::Taken for Tapped {
        fn failed(&self) -> bool {
            false
        }
    }

    impl apps::Taps for OneApp {
        fn processes(&mut self) -> Vec<(u32, String, bool)> {
            if self.0.load(Ordering::Relaxed) {
                vec![(5, "us.zoom.xos".into(), true)]
            } else {
                Vec::new()
            }
        }

        fn name(&mut self, _bundle_id: &str) -> String {
            "zoom.us".into()
        }

        fn take(
            &mut self,
            _bundle_id: &str,
            _processes: &[u32],
        ) -> anyhow::Result<(Box<dyn apps::Taken>, LiveFeed)> {
            let (tx, rx) = crossbeam_channel::bounded(8);
            Ok((
                Box::new(Tapped(tx)),
                LiveFeed {
                    rx,
                    rate: 48_000,
                    channels: 2,
                },
            ))
        }
    }

    /// An app's sound is an input like a microphone, kept with the show: its
    /// app named, its device the tap's. The mixer takes its sound once the
    /// app is tapped and lets it go when the app quits; the apps are listed.
    #[test]
    fn an_apps_sound_is_mixed_while_the_app_runs() {
        let running = Arc::new(AtomicBool::new(true));
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(running.clone())),
        );
        let uuid = engine.add_app_input("zoom.us", "us.zoom.xos");
        let input = engine.config().inputs.iter().find(|i| i.uuid == uuid).unwrap();
        assert_eq!(input.app.as_deref(), Some("us.zoom.xos"));
        assert_eq!(input.device, "App sound: us.zoom.xos");
        let saved: AudioMixConfig =
            serde_json::from_str(&serde_json::to_string(engine.config()).unwrap()).unwrap();
        assert_eq!(
            saved.inputs.iter().find(|i| i.uuid == uuid).unwrap().app.as_deref(),
            Some("us.zoom.xos"),
            "kept with the show"
        );

        assert!(mixed(&mut engine, &uuid, true), "mixed once the app is tapped");
        assert_eq!(
            engine.snapshot().apps.first().map(|a| a.name.as_str()),
            Some("zoom.us"),
            "the apps listed"
        );
        running.store(false, Ordering::Relaxed);
        assert!(mixed(&mut engine, &uuid, false), "let go when the app quits");
    }

    /// Whether the input `uuid`'s sound comes to be mixed (`live`) or not,
    /// within a few seconds, the app sound thread heard meanwhile.
    fn mixed(engine: &mut AudioEngine, uuid: &str, live: bool) -> bool {
        let until = Instant::now() + Duration::from_secs(6);
        while Instant::now() < until {
            engine.keep_apps();
            let now = engine
                .snapshot()
                .sources
                .iter()
                .find(|s| s.uuid == uuid)
                .is_some_and(|s| s.live);
            if now == live {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// A show loaded without an app's sound, and the first show straight
    /// back — before the app sound thread heard — mixes it again: the app is
    /// tapped afresh, not left held with its sound gone.
    #[test]
    fn an_apps_sound_dropped_by_a_show_load_comes_back_with_the_show() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(true)))),
        );
        let uuid = engine.add_app_input("zoom.us", "us.zoom.xos");
        assert!(mixed(&mut engine, &uuid, true), "mixed once the app is tapped");

        let show = engine.config().clone();
        let mut without = show.clone();
        without.inputs.retain(|i| i.uuid != uuid);
        engine.load(without);
        assert!(!has_sound(&engine, &uuid), "its sound stops with the show");
        engine.load(show);
        assert!(mixed(&mut engine, &uuid, true), "and comes back with the first");
    }

    /// A tap handed back late — made before a show load dropped the input's
    /// sound and brought the input back — is not mixed: the app sound thread
    /// is about to let it go, and tap the app afresh.
    #[test]
    fn a_tap_from_before_a_show_load_is_not_mixed() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_app_input("zoom.us", "us.zoom.xos");
        let show = engine.config().clone();
        let mut without = show.clone();
        without.inputs.retain(|i| i.uuid != uuid);
        engine.load(without);
        engine.load(show);
        let taken = |generation| {
            let (_tx, rx) = crossbeam_channel::bounded(1);
            apps::Event::Taken {
                uuid: uuid.clone(),
                app: "us.zoom.xos".into(),
                generation,
                feed: LiveFeed {
                    rx,
                    rate: 48_000,
                    channels: 2,
                },
            }
        };
        engine.take_app_events(vec![taken(0)]);
        assert!(!has_sound(&engine, &uuid), "the old tap is not mixed");
        engine.take_app_events(vec![taken(1)]);
        assert!(has_sound(&engine, &uuid), "the fresh one is");
    }

    /// An input given to another app by a show loaded forgets the first
    /// app's refusal: the second, not running, says "not running", not why
    /// the first was not taken.
    #[test]
    fn another_app_under_an_input_does_not_inherit_its_refusal() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_app_input("zoom.us", "us.zoom.xos");
        engine.take_app_events(vec![apps::Event::Refused {
            uuid: uuid.clone(),
            app: "us.zoom.xos".into(),
            generation: 0,
            why: "the tap was refused (status 1)".into(),
        }]);
        let mut show = engine.config().clone();
        let input = show.inputs.iter_mut().find(|i| i.uuid == uuid).unwrap();
        input.app = Some("com.microsoft.teams2".into());
        input.device = crate::audio::app_sound::device_name("com.microsoft.teams2");
        engine.load(show);
        let refused = engine
            .snapshot()
            .sources
            .iter()
            .find(|s| s.uuid == uuid)
            .and_then(|s| s.refused.clone());
        assert_eq!(refused, None);
    }

    /// A microphone whose sound went and that was brought straight back,
    /// before the next frame — removed, or left out of a show while the
    /// clips changed — is subscribed afresh: its subscription is let go.
    #[test]
    fn a_microphone_removed_and_brought_straight_back_is_subscribed_afresh() {
        let ways: [fn(&mut AudioEngine, &str); 2] = [
            |engine, uuid| engine.remove_input(uuid).expect("input"),
            |engine, uuid| {
                let mut without = engine.config().clone();
                without.inputs.retain(|i| i.uuid != uuid);
                engine.load(without);
                engine.sync_clips(vec![ClipSource {
                    deck_uuid: "clip0001".into(),
                    name: "intro.mp4".into(),
                    shared: Arc::new(ClipAudioShared::default()),
                }]);
            },
        ];
        for (n, gone) in ways.into_iter().enumerate() {
            let mut engine = AudioEngine::with_parts(
                Box::new(|_: &str, _: u16, _: Arc<OutputRing>| {
                    Err(anyhow::anyhow!("no devices here"))
                }),
                Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
            );
            let uuid = engine.add_input("Lectern", "USB Mixer", 0, 1);
            subscribed(&mut engine, &uuid, 7);
            let saved = engine.config().clone();
            gone(&mut engine, &uuid);
            assert!(!has_sound(&engine, &uuid), "way {n}: its sound went");
            engine.load(saved);
            let mut let_go = Vec::new();
            engine.release_inputs(|_, token| let_go.push(token));
            assert_eq!(let_go, [7], "way {n}: its subscription is let go");
            assert!(!engine.inputs.contains_key(&uuid), "way {n}: to be made afresh");
        }
    }

    /// The input `uuid` as a microphone subscribed under `token`, its sound
    /// mixed — standing in for [`AudioEngine::connect_inputs`], which needs
    /// the devices.
    fn subscribed(engine: &mut AudioEngine, uuid: &str, token: u64) {
        let input = engine
            .config()
            .inputs
            .iter()
            .find(|i| i.uuid == uuid)
            .expect("input")
            .clone();
        let (_tx, rx) = crossbeam_channel::bounded(1);
        let feed = InputFeed::new(rx, 48_000, 2, input.left, input.right);
        engine.inputs.insert(
            uuid.to_string(),
            LiveInput {
                source_id: 1,
                token,
                rate: 48_000,
                device: input.device,
                left: input.left,
                right: input.right,
            },
        );
        engine
            .core
            .lock()
            .unwrap()
            .sources
            .push(Source::new(uuid.to_string(), Feed::Input(feed)));
    }

    fn has_sound(engine: &AudioEngine, uuid: &str) -> bool {
        engine.core.lock().unwrap().sources.iter().any(|s| s.uuid == uuid)
    }

    /// A microphone whose device was unplugged is dropped with its sound:
    /// kept, it looked connected, silent, and was never subscribed again.
    /// Dropped, it is taken again once the device is back.
    #[test]
    fn a_microphone_whose_device_went_is_dropped_to_be_taken_again() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_input("Lectern", "USB Mixer", 0, 1);
        let mut capture = crate::audio::AudioManager::listing(&["USB Mixer"]);
        let sub = capture.open_for_test(0, "USB Mixer");
        // Subscribed, as `connect_inputs` does with a device there.
        engine.inputs.insert(
            uuid.clone(),
            LiveInput {
                source_id: 0,
                token: sub.token,
                rate: 48_000,
                device: "USB Mixer".into(),
                left: 0,
                right: 1,
            },
        );
        let feed = InputFeed::new(sub.receiver, 48_000, 2, 0, 1);
        engine
            .core
            .lock()
            .unwrap()
            .sources
            .push(Source::new(uuid.clone(), Feed::Input(feed)));
        engine.connect_inputs(&mut capture);
        assert!(has_sound(&engine, &uuid), "running: kept");

        capture.lose_for_test(0);
        assert_eq!(capture.forget_lost(), ["USB Mixer"]);
        engine.connect_inputs(&mut capture);
        assert!(!has_sound(&engine, &uuid), "its sound went with its device");
        assert!(
            !engine.inputs.contains_key(&uuid),
            "to be subscribed again once it is back"
        );
    }

    /// A microphone whose device is not open yet is taken as soon as its
    /// device has opened — on a thread of its own, the show's frames not
    /// waiting — not two seconds on.
    #[test]
    fn a_microphone_is_taken_as_soon_as_its_device_opens() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_input("Lectern", "USB Mixer", 0, 1);
        let mut capture = crate::audio::AudioManager::listing(&["USB Mixer"]);
        capture.open_without_devices_for_test();
        engine.connect_inputs(&mut capture);
        assert!(!has_sound(&engine, &uuid), "its device opening");
        capture.wait_opens_for_test();
        engine.connect_inputs(&mut capture);
        assert!(has_sound(&engine, &uuid), "taken once open");
    }

    /// A microphone's subscription is released, its sound with it, once a
    /// show loaded has its input on other channels, as an app's sound, or not
    /// at all — so that it is subscribed again as what it now is; one still
    /// the same is kept.
    #[test]
    fn a_microphone_is_let_go_with_its_sound_once_its_input_is_something_else() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let kept = engine.add_input("Lectern", "USB Mixer", 0, 1);
        let moved = engine.add_input("Handheld", "USB Mixer", 2, 3);
        let made_app = engine.add_input("Laptop", "USB Mixer", 4, 5);
        let removed = engine.add_input("Spare", "USB Mixer", 6, 7);
        for (token, uuid) in (0..).zip([&kept, &moved, &made_app, &removed]) {
            subscribed(&mut engine, uuid, token);
        }

        let mut show = engine.config().clone();
        let handheld = show.inputs.iter_mut().find(|i| i.uuid == moved).unwrap();
        handheld.left = 8;
        handheld.right = 9;
        let laptop = show.inputs.iter_mut().find(|i| i.uuid == made_app).unwrap();
        laptop.app = Some("us.zoom.xos".into());
        laptop.device = crate::audio::app_sound::device_name("us.zoom.xos");
        show.inputs.retain(|i| i.uuid != removed);
        engine.load(show);

        let mut let_go = Vec::new();
        engine.release_inputs(|_, token| let_go.push(token));
        let_go.sort_unstable();
        assert_eq!(let_go, [1, 2, 3], "each let go");
        assert_eq!(engine.inputs.keys().collect::<Vec<_>>(), [&kept], "the same one kept");
        assert!(has_sound(&engine, &kept), "and its sound");
        for uuid in [&moved, &made_app, &removed] {
            assert!(!has_sound(&engine, uuid), "the sound goes with its subscription");
        }
    }

    /// A microphone made an app's sound and back in two shows loaded in one
    /// frame loses its sound on the second load — and its subscription with
    /// it, so it is subscribed afresh rather than looking connected, silent.
    #[test]
    fn a_microphone_made_an_app_and_back_in_one_frame_is_subscribed_afresh() {
        let mut engine = AudioEngine::with_parts(
            Box::new(|_: &str, _: u16, _: Arc<OutputRing>| Err(anyhow::anyhow!("no devices here"))),
            Box::new(OneApp(Arc::new(AtomicBool::new(false)))),
        );
        let uuid = engine.add_input("Lectern", "USB Mixer", 0, 1);
        subscribed(&mut engine, &uuid, 7);
        let mic = engine.config().clone();
        let mut app = mic.clone();
        let input = app.inputs.iter_mut().find(|i| i.uuid == uuid).unwrap();
        input.app = Some("us.zoom.xos".into());
        input.device = crate::audio::app_sound::device_name("us.zoom.xos");

        engine.load(app);
        engine.load(mic);
        assert!(!has_sound(&engine, &uuid), "the second load drops what is mixed under it");
        let mut let_go = Vec::new();
        engine.release_inputs(|_, token| let_go.push(token));
        assert_eq!(let_go, [7], "the subscription goes with it");
        assert!(!engine.inputs.contains_key(&uuid), "to be made afresh");
    }

    struct FakeDevice(Arc<AtomicBool>);

    impl outputs::Playing for FakeDevice {
        fn failed(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    /// A bus playing on `device`, with `open` opening its devices.
    fn engine_on(device: &str, open: outputs::Opener) -> (AudioEngine, String) {
        let mut engine = AudioEngine::with_opener(open);
        let bus = engine.default_bus().expect("Master").to_string();
        engine
            .edit_bus(&bus, |b| b.device = Some(device.into()))
            .expect("bus");
        (engine, bus)
    }

    fn device_ok(engine: &AudioEngine) -> Option<bool> {
        engine.snapshot().buses[0].device_ok
    }

    /// A bus whose device is missing — a show opened on another Mac, where
    /// finding out takes the OS a third of a second — never holds up a frame,
    /// and the device is still looked for every two seconds.
    #[test]
    fn a_missing_device_never_holds_up_the_frame() {
        let tries = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = Arc::clone(&tries);
        let (mut engine, _) = engine_on(
            "NOPE",
            Box::new(move |name, _, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(400));
                anyhow::bail!("no sound device called '{name}'")
            }),
        );
        let start = Instant::now();
        let mut slowest = Duration::ZERO;
        while start.elapsed() < Duration::from_millis(2600) {
            let frame = Instant::now();
            engine.poll_devices();
            slowest = slowest.max(frame.elapsed());
            std::thread::sleep(Duration::from_millis(16));
        }
        assert!(
            slowest < Duration::from_millis(50),
            "a frame waited {slowest:?}"
        );
        assert!(tries.load(Ordering::Relaxed) >= 2, "looked for again");
        assert_eq!(device_ok(&engine), Some(false));
    }

    /// A live bus's feed made anew under the same bus (its mics changed)
    /// hands the mixer its new channel for the rest of the bus at once.
    #[test]
    fn a_live_feed_made_anew_gets_the_rest_of_the_bus() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let bus = engine.default_bus().expect("Master").to_string();
        engine.outputs.pretend_live(&bus);
        let feed = || {
            let (_tx, frames) = crossbeam_channel::bounded(1);
            LiveBusFeed {
                source_id: 1,
                token: 1,
                frames,
                counts: Arc::default(),
                mics: Vec::new(),
                control: Arc::default(),
                rest: crossbeam_channel::bounded(8),
            }
        };
        // The mixing thread runs meanwhile, sending: the channel itself is
        // compared, not what arrives.
        let sends_to = |engine: &AudioEngine, tx: &crossbeam_channel::Sender<reader::Frame>| {
            let core = engine.core.lock().unwrap();
            let live = core.buses[0].live.as_ref().expect("played live");
            live.tx.same_channel(tx)
        };
        let first = feed();
        let first_rx = first.rest.0.clone();
        engine.live_taps.insert(bus.clone(), first);
        // Taken live: the device thread said so, and the mix applied it.
        engine.apply();
        assert!(sends_to(&engine, &first_rx));
        let again = feed();
        let again_rx = again.rest.0.clone();
        engine.live_taps.insert(bus.clone(), again);
        engine.live_feeds_changed();
        assert!(sends_to(&engine, &again_rx), "the new feed's channel");
    }

    /// Every bus set live gets its mics: the inputs on its own device sent
    /// to it — not another device's, not one sent elsewhere; a bus not set
    /// live, none.
    #[test]
    fn each_live_bus_takes_the_mics_on_its_own_device() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let master = engine.default_bus().expect("Master").to_string();
        let pa = engine.add_bus("PA");
        let wedges = engine.add_bus("Wedges");
        let on = |b: &mut AudioBusConfig| {
            b.device = Some("Scarlett".into());
            b.live = true;
        };
        engine.edit_bus(&pa, on).expect("PA");
        engine.edit_bus(&wedges, on).expect("Wedges");
        engine
            .edit_bus(&master, |b| b.device = Some("Scarlett".into()))
            .expect("Master");
        let lectern = engine.add_input("Lectern", "Scarlett", 0, 0);
        let handheld = engine.add_input("Handheld", "Scarlett", 1, 1);
        let laptop = engine.add_input("Laptop", "USB Audio", 0, 1);
        for (uuid, bus) in [(&lectern, &pa), (&lectern, &wedges), (&handheld, &pa), (&laptop, &pa)] {
            let strip = engine.config.strips.iter_mut().find(|s| s.uuid == *uuid).expect("strip");
            strip.sends.push(bus.clone());
        }
        let mut got: Vec<(String, Vec<String>)> = engine
            .live_direct()
            .into_iter()
            .map(|(bus, mics)| (bus, mics.into_iter().map(|(m, _)| m.uuid).collect()))
            .collect();
        got.sort();
        let mut want = vec![
            (pa.clone(), vec![lectern.clone(), handheld.clone()]),
            (wedges.clone(), vec![lectern.clone()]),
        ];
        want.sort();
        assert_eq!(got, want);
    }

    /// Two live buses on one device play at one buffer, the smallest set:
    /// the device has only one. Another device's keeps its own.
    #[test]
    fn live_buses_on_one_device_share_its_buffer() {
        let bus = |name: &str, device: &str, frames: Option<u32>, live: bool| AudioBusConfig {
            device: Some(device.into()),
            live,
            live_frames: frames,
            ..AudioBusConfig::new(name)
        };
        let mut buses = vec![
            bus("PA", "Scarlett", Some(256), true),
            bus("Wedges", "Scarlett", None, true),
            bus("Lobby", "Octo", Some(128), true),
            bus("Stream", "Scarlett", Some(32), false),
        ];
        // Either way round: the smallest, not the last.
        for _ in 0..2 {
            let shared = live_frames_shared(&buses);
            let of = |name: &str| buses.iter().find(|b| b.name == name).map(|b| shared.get(&b.uuid).copied());
            assert_eq!(of("PA"), Some(Some(64)), "PA at the Wedges' 64");
            assert_eq!(of("Wedges"), Some(Some(64)));
            assert_eq!(of("Lobby"), Some(Some(128)), "another device: its own");
            assert_eq!(of("Stream"), Some(None), "not live");
            buses.swap(0, 1);
        }
    }

    /// A live bus's setting: saved only when set, a buffer kept to those
    /// offered, its state shown.
    #[test]
    fn a_live_bus_is_saved_and_shown_as_set() {
        let plain: AudioBusConfig =
            serde_json::from_value(serde_json::json!({ "uuid": "b", "name": "PA" })).expect("bus");
        assert!(!plain.live && plain.live_frames.is_none(), "an older show: not live");
        let saved = serde_json::to_value(&plain).expect("saved");
        assert!(saved.get("live").is_none() && saved.get("live_frames").is_none(), "{saved}");
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let pa = engine.add_bus("PA");
        engine
            .edit_bus(&pa, |b| {
                b.device = Some("Scarlett".into());
                b.live = true;
                b.live_frames = Some(100);
            })
            .expect("PA");
        let snap = engine.snapshot();
        let bus = snap.buses.iter().find(|b| b.config.uuid == pa).expect("PA");
        assert!(bus.config.live);
        assert_eq!(bus.config.live_frames, None, "100 is not offered: 64");
        assert_eq!(bus.live, Some(false), "set live, not playing live here");
        assert_eq!(
            bus.device_note.as_deref(),
            Some("Live: waits for a mic on its own device sent to it")
        );
        engine.edit_bus(&pa, |b| b.live_frames = Some(128)).expect("PA");
        let frames = engine.config.buses.iter().find(|b| b.uuid == pa).map(live_frames_of);
        assert_eq!(frames, Some(128));
    }

    /// A live bus's mics: those sent to it, and those whose send is off but
    /// kept — so turning a send off and on is not a new feed (the device
    /// opened again); a mic never sent there is not among them.
    #[test]
    fn a_live_buses_mics_keep_a_send_turned_off() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let pa = engine.add_bus("PA");
        engine.edit_bus(&pa, |b| {
            b.device = Some("Mic".into());
            b.live = true;
        }).expect("bus");
        let a = engine.add_input("Lectern", "Mic", 0, 0);
        let b = engine.add_input("Podium", "Mic", 1, 1);
        let c = engine.add_input("Spare", "Mic", 2, 2);
        for (mic, sent, kept) in [(&a, true, true), (&b, false, true), (&c, false, false)] {
            engine
                .edit_source(mic, |s| {
                    s.sends = if sent { vec![pa.clone()] } else { Vec::new() };
                    s.sent_before = if kept { vec![pa.clone()] } else { Vec::new() };
                    // c's level set while never sent: not among the mics.
                    s.send_settings.insert(pa.clone(), SendConfig { gain_db: -6.0, ..SendConfig::default() });
                })
                .expect("mic");
        }
        let mics = |engine: &AudioEngine| -> Vec<(String, bool)> {
            engine
                .live_direct()
                .into_iter()
                .find(|(bus, _)| *bus == pa)
                .map(|(_, m)| m.into_iter().map(|(i, kept)| (i.uuid, kept)).collect())
                .unwrap_or_default()
        };
        assert_eq!(
            mics(&engine),
            [(a.clone(), false), (b.clone(), true)],
            "sent and kept; never the one never sent"
        );
        // A toggled off, b on: the same mics in the same order — the same feed.
        engine.edit_source(&a, |s| s.sends.clear()).expect("a");
        engine.edit_source(&b, |s| s.sends = vec![pa.clone()]).expect("b");
        assert_eq!(mics(&engine), [(a.clone(), true), (b.clone(), false)], "not reordered");
    }

    /// A live bus's mic plays at its send's level: before the fader on a
    /// monitor mix (the fader left out), after it otherwise.
    #[test]
    fn a_live_mics_gain_is_its_sends() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let wedge = engine.add_bus("Wedge");
        let mic = engine.add_input("Lectern", "Mic", 0, 0);
        let gain = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (_tx, frames) = crossbeam_channel::bounded(1);
        engine.live_taps.insert(
            wedge.clone(),
            LiveBusFeed {
                source_id: 1,
                token: 1,
                frames,
                counts: Arc::default(),
                mics: vec![(mic.clone(), (0, 0), Arc::clone(&gain))],
                control: Arc::default(),
                rest: crossbeam_channel::bounded(1),
            },
        );
        {
            let fifo = Arc::new(Mutex::new(std::collections::VecDeque::new()));
            engine.core.lock().unwrap().sources.push(Source::new(mic.clone(), Feed::Fifo(fifo)));
        }
        engine.edit_bus(&wedge, |b| b.pre_fader = true).expect("bus");
        engine
            .edit_source(&mic, |s| {
                s.gain_db = -20.0;
                s.muted = false;
                s.sends = vec![wedge.clone()];
                s.send_settings.insert(wedge.clone(), SendConfig { gain_db: -6.0, ..SendConfig::default() });
            })
            .expect("mic");
        let heard = || f32::from_bits(gain.load(Ordering::Relaxed));
        assert!((heard() - db_to_gain(-6.0)).abs() < 1e-6, "before the fader: {}", heard());
        engine.edit_bus(&wedge, |b| b.pre_fader = false).expect("bus");
        assert!((heard() - db_to_gain(-26.0)).abs() < 1e-6, "after it: {}", heard());
        engine
            .edit_source(&mic, |s| s.send_settings.get_mut(&wedge).unwrap().mode = SendMode::Pre)
            .expect("mic");
        assert!((heard() - db_to_gain(-6.0)).abs() < 1e-6, "the send's own say: {}", heard());
    }

    /// Shows saved before sends had levels load as they sounded: every send
    /// at unity, after the fader; a level out of range comes back at unity,
    /// its point kept; a bus removed takes its send settings with it.
    #[test]
    fn send_settings_load_and_go_with_their_bus() {
        let old: AudioStripConfig =
            serde_json::from_value(serde_json::json!({ "uuid": "mic", "sends": ["m"] })).expect("strip");
        assert!(old.send_settings.is_empty());
        let bus: AudioBusConfig =
            serde_json::from_value(serde_json::json!({ "uuid": "m", "name": "Master" })).expect("bus");
        assert!(!bus.pre_fader);
        assert_eq!(old.send_to(&bus), core::Send { bus: "m".into(), gain: 1.0, pre: false });
        let wild: AudioStripConfig = serde_json::from_value(serde_json::json!({
            "uuid": "mic",
            "send_settings": { "m": { "gain_db": 99.0, "mode": "pre" } }
        }))
        .expect("strip");
        assert_eq!(wild.send_settings["m"], SendConfig { gain_db: 0.0, mode: SendMode::Pre });

        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let wedge = engine.add_bus("Wedge");
        let mic = engine.add_input("Lectern", "Mic", 0, 0);
        engine.config.strips.iter_mut().find(|s| s.uuid == mic).unwrap().send_settings.insert(
            wedge.clone(),
            SendConfig { gain_db: -3.0, mode: SendMode::Post },
        );
        engine.remove_bus(&wedge).expect("removed");
        assert!(engine.config.strips.iter().all(|s| s.send_settings.is_empty()));
    }

    /// The mixer's own leveler gain for each live mic's strip reaches its
    /// bus's callback, for when it opens again.
    #[test]
    fn a_live_mics_leveler_is_tracked_from_the_mixers() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let control = Arc::new(live::LiveControl::default());
        let (_tx, frames) = crossbeam_channel::bounded(1);
        engine.live_taps.insert(
            "bus".into(),
            LiveBusFeed {
                source_id: 1,
                token: 1,
                frames,
                counts: Arc::default(),
                mics: vec![
                    ("mic".into(), (0, 0), Arc::default()),
                    ("gone".into(), (1, 1), Arc::default()),
                ],
                control: Arc::clone(&control),
                rest: crossbeam_channel::bounded(1),
            },
        );
        {
            let mut core = engine.core.lock().unwrap();
            let fifo = Arc::new(Mutex::new(std::collections::VecDeque::new()));
            let mut mic = Source::new("mic", Feed::Fifo(fifo));
            mic.dynamics.configure(&dynamics::SourceSettings {
                leveler: LevelerConfig::speech(),
                ..Default::default()
            });
            mic.dynamics.start_leveler_at(4.5);
            core.sources.push(mic);
        }
        engine.track_live_levelers();
        assert!((control.leveler(0) - 4.5).abs() < 1e-6);
        assert!(control.leveler(1).is_nan(), "no such strip: none");
    }

    /// The effect a bus has, asked for again, is kept as it is — its saved
    /// settings and bypass; another one starts afresh.
    #[test]
    fn the_same_effect_asked_for_again_keeps_its_settings() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let bus = engine.default_bus().expect("Master").to_string();
        engine.set_insert(&bus, Some("aufx:lpas:appl".into())).expect("set");
        engine
            .edit_bus(&bus, |b| {
                let insert = b.insert.as_mut().expect("insert");
                insert.settings = Some(Arc::from("c2F2ZWQ="));
                insert.bypass = true;
            })
            .expect("bus");
        engine.set_insert(&bus, Some("aufx:lpas:appl".into())).expect("again");
        let insert = engine.config.buses[0].insert.clone().expect("insert");
        assert_eq!(insert.settings.as_deref(), Some("c2F2ZWQ="));
        assert!(insert.bypass);
        engine.set_insert(&bus, Some("aufx:hpas:appl".into())).expect("another");
        let insert = engine.config.buses[0].insert.clone().expect("insert");
        assert!(insert.settings.is_none() && !insert.bypass, "another starts afresh");
        assert!(engine.set_insert("nosuchbus", None).is_err());
    }

    /// A bus takes one of Apple's effects as its insert: it loads (out of
    /// Nida) and plays. One not installed here fails and says why — the bus
    /// heard without it; taken out, no insert.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bus_takes_an_audio_unit_insert() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let bus = engine.default_bus().expect("Master").to_string();
        let set = |engine: &mut AudioEngine, component: Option<&str>| {
            engine
                .edit_bus(&bus, |b| {
                    b.insert = component.map(|c| AudioInsertConfig {
                        component: c.into(),
                        name: c.into(),
                        bypass: false,
                        settings: None,
                    });
                })
                .expect("bus");
        };
        let state_becomes = |engine: &AudioEngine, want: &str| {
            let end = Instant::now() + Duration::from_secs(10);
            loop {
                let status = engine.snapshot().buses[0].insert_status.clone();
                if status.as_ref().is_some_and(|s| s.state == want) || Instant::now() > end {
                    return status;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        set(&mut engine, Some("aufx:lpas:appl"));
        let playing = state_becomes(&engine, "playing").expect("a status");
        assert_eq!(playing.state, "playing", "{playing:?}");
        engine
            .edit_bus(&bus, |b| b.insert.as_mut().expect("insert").bypass = true)
            .expect("bus");
        assert_eq!(
            engine.snapshot().buses[0].insert_status.as_ref().map(|s| s.state.as_str()),
            Some("bypassed"),
            "the running insert bypassed"
        );
        assert!(engine.open_insert_window(&bus).is_ok(), "its window asked for");
        assert!(engine.open_insert_window("nosuchbus").is_err_and(|e| e.starts_with("no audio bus")));
        assert!((playing.latency_ms - 20.0).abs() < 0.5, "the handoff's 20 ms and none of its own: {}", playing.latency_ms);
        set(&mut engine, Some("aufx:none:none"));
        let failed = state_becomes(&engine, "failed").expect("a status");
        assert!(failed.why.as_deref().is_some_and(|w| w.contains("not installed on this Mac")), "{:?}", failed.why);
        assert!(engine.open_insert_window(&bus).is_err_and(|e| e.contains("not loaded")), "failed: none");
        assert_eq!(failed.state, "failed");
        assert!(failed.why.is_some(), "says why");
        set(&mut engine, None);
        assert!(engine.snapshot().buses[0].insert_status.is_none());
        assert!(engine.open_insert_window(&bus).is_err_and(|e| e.contains("has no effect")));
        // A bus removed with an effect on it: its window goes too.
        let gone = engine.add_bus("Gone");
        engine
            .edit_bus(&gone, |b| {
                b.insert = Some(AudioInsertConfig {
                    component: "aufx:lpas:appl".into(),
                    name: "AULowpass".into(),
                    bypass: false,
                    settings: None,
                });
            })
            .expect("bus");
        au_editor::CLOSED.lock().unwrap().retain(|k| *k != gone);
        engine.remove_bus(&gone).expect("removed");
        assert!(au_editor::CLOSED.lock().unwrap().contains(&gone), "its window closed");
        // A show loaded with the same effect makes it afresh (what it held
        // was the old show's); one whose bus has none loses it.
        set(&mut engine, Some("aufx:lpas:appl"));
        let running = |engine: &AudioEngine| {
            engine.core.lock().unwrap().buses[0].insert.as_ref().map(|i| Arc::clone(&i.status))
        };
        let before = running(&engine).expect("an insert");
        engine.load(engine.config.clone());
        let after = running(&engine).expect("an insert");
        assert!(!Arc::ptr_eq(&before, &after), "made afresh");
        let mut show = engine.config.clone();
        show.buses[0].insert = None;
        engine.load(show);
        assert!(running(&engine).is_none(), "the old show's insert is gone");
    }

    /// A playing insert's own settings reach the show's bus within a few
    /// seconds, as the plug-in's saved state, for the show to save.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_bus_inserts_settings_are_kept_for_the_show() {
        use base64::Engine as _;
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let bus = engine.default_bus().expect("Master").to_string();
        engine
            .edit_bus(&bus, |b| {
                b.insert = Some(AudioInsertConfig {
                    component: "aufx:lpas:appl".into(),
                    name: "AULowpass".into(),
                    bypass: false,
                    settings: None,
                });
            })
            .expect("bus");
        let end = Instant::now() + Duration::from_secs(10);
        let settings = loop {
            engine.poll_devices();
            let settings = engine.config.buses[0].insert.as_ref().and_then(|i| i.settings.clone());
            if settings.is_some() || Instant::now() > end {
                break settings;
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        let settings = settings.expect("its settings kept");
        let bytes = base64::engine::general_purpose::STANDARD.decode(settings.as_bytes()).expect("base64");
        assert!(bytes.starts_with(b"bplist"), "a binary property list");
    }

    /// A show loaded tells each live bus's callback to start its
    /// processing afresh, as the mixer's own is.
    #[test]
    fn a_show_loaded_starts_live_buses_afresh() {
        let mut engine = AudioEngine::with_opener(Box::new(|_, _, _| anyhow::bail!("no devices")));
        let control = Arc::new(live::LiveControl::default());
        let (_tx, frames) = crossbeam_channel::bounded(1);
        engine.live_taps.insert(
            "bus".into(),
            LiveBusFeed {
                source_id: 1,
                token: 1,
                frames,
                counts: Arc::default(),
                mics: Vec::new(),
                control: Arc::clone(&control),
                rest: crossbeam_channel::bounded(1),
            },
        );
        let before = control.afreshes();
        engine.load(engine.config.clone());
        assert_eq!(control.afreshes(), before + 1);
    }

    /// A strip saved before the de-esser loads with it off; one saved with
    /// it out of range loads with it off too.
    #[test]
    fn an_older_strip_loads_with_its_de_esser_off() {
        let old: AudioStripConfig =
            serde_json::from_value(serde_json::json!({ "uuid": "mic" })).expect("strip");
        assert_eq!(old.de_esser, DeEsserConfig::default());
        let wild: AudioStripConfig = serde_json::from_value(serde_json::json!({
            "uuid": "mic",
            "de_esser": { "enabled": true, "hz": 99_000.0 }
        }))
        .expect("strip");
        assert_eq!(wild.de_esser, DeEsserConfig::default(), "out of range: off");
        let kept: AudioStripConfig = serde_json::from_value(serde_json::json!({
            "uuid": "mic",
            "de_esser": { "enabled": true, "hz": 5_000.0, "threshold_db": -20.0 }
        }))
        .expect("strip");
        assert!(kept.de_esser.enabled && (kept.de_esser.hz - 5_000.0).abs() < 1e-3);
    }

    /// A strip saved before the EQ loads with it off and flat; one saved with
    /// a band out of range loses that band alone.
    #[test]
    fn an_older_strip_loads_with_its_eq_off() {
        let old: AudioStripConfig =
            serde_json::from_value(serde_json::json!({ "uuid": "mic" })).expect("strip");
        assert_eq!(old.eq, EqConfig::default());
        let wild: AudioStripConfig = serde_json::from_value(serde_json::json!({
            "uuid": "mic",
            "eq": { "enabled": true, "bands": [
                { "hz": 150.0, "gain_db": -3.0 },
                { "hz": 350.0, "gain_db": 99.0 },
                { "hz": 3000.0 },
                { "hz": 8000.0 }
            ] }
        }))
        .expect("strip");
        assert!(wild.eq.enabled);
        assert!((wild.eq.bands[0].gain_db + 3.0).abs() < 1e-6, "kept");
        assert!(wild.eq.bands[1].gain_db.abs() < 1e-6, "out of range: as new");
    }

    /// A strip saved before the gate loads with it off; one saved with a gate
    /// out of range, off too.
    #[test]
    fn an_older_strip_loads_with_its_gate_off() {
        let old: AudioStripConfig =
            serde_json::from_value(serde_json::json!({ "uuid": "mic" })).expect("strip");
        assert_eq!(old.gate, GateConfig::default());
        assert!(!old.gate.enabled);
        let wild: AudioStripConfig = serde_json::from_value(serde_json::json!({
            "uuid": "mic", "gate": { "enabled": true, "range_db": 90.0 }
        }))
        .expect("strip");
        assert_eq!(wild.gate, GateConfig::default(), "out of range: off");
        let set: AudioStripConfig = serde_json::from_value(serde_json::json!({
            "uuid": "mic", "gate": { "enabled": true, "threshold_db": -45.0 }
        }))
        .expect("strip");
        assert!(set.gate.enabled && (set.gate.threshold_db + 45.0).abs() < 1e-6);
    }

    /// A live bus's callback gets each mic's strip processing and balance
    /// in its feed's order, and the bus's own compressor, limiter and delay.
    #[test]
    fn a_live_bus_takes_its_strips_and_its_own_settings() {
        let mut config = AudioMixConfig::default();
        let mut bus = AudioBusConfig::new("PA");
        bus.delay_ms = 12.0;
        bus.limiter.ceiling_db = -3.0;
        bus.compressor.enabled = true;
        let strip = |uuid: &str, hz: f32, balance: f32| {
            let mut s: AudioStripConfig =
                serde_json::from_value(serde_json::json!({ "uuid": uuid })).expect("strip");
            s.low_cut.enabled = true;
            s.low_cut.hz = hz;
            s.balance = balance;
            s.delay_ms = 40.0;
            s
        };
        config.strips = vec![strip("a", 80.0, -0.5), strip("b", 150.0, 0.25)];
        let uuid = bus.uuid.clone();
        config.buses.push(bus);
        let got = live_settings(&config, &uuid, ["b", "a"].into_iter()).expect("the bus");
        assert_eq!(got.delay_ms, 12.0);
        assert_eq!(got.limiter.ceiling_db, -3.0);
        assert!(got.compressor.enabled);
        assert_eq!((got.mics[0].low_cut.hz, got.mics[0].balance), (150.0, 0.25), "feed order");
        assert_eq!((got.mics[1].low_cut.hz, got.mics[1].balance), (80.0, -0.5));
        assert_eq!(got.mics[2], live::LiveMicSettings::default(), "no third mic");
        assert!(live_settings(&config, "gone", std::iter::empty()).is_none());
    }

    /// A device found plays its bus; one that fails (unplugged) is let go
    /// and looked for again.
    #[test]
    fn a_device_plays_its_bus_and_is_found_again_when_it_fails() {
        let failed = Arc::new(AtomicBool::new(false));
        let tries = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (flag, counted) = (Arc::clone(&failed), Arc::clone(&tries));
        let (mut engine, bus) = engine_on(
            "Speakers",
            Box::new(move |_, _, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                flag.store(false, Ordering::Relaxed);
                Ok(Box::new(FakeDevice(Arc::clone(&flag))))
            }),
        );
        let until = |engine: &mut AudioEngine, want: Option<bool>| {
            let end = Instant::now() + Duration::from_secs(4);
            while device_ok(engine) != want && Instant::now() < end {
                engine.poll_devices();
                std::thread::sleep(Duration::from_millis(10));
            }
            device_ok(engine)
        };
        assert_eq!(until(&mut engine, Some(true)), Some(true), "playing");
        let playing_into_ring = engine
            .core
            .lock()
            .unwrap()
            .bus(&bus)
            .is_some_and(|b| !b.outputs.is_empty());
        assert!(playing_into_ring, "the bus plays into the device's ring");
        failed.store(true, Ordering::Relaxed);
        assert_eq!(until(&mut engine, Some(false)), Some(false), "let go");
        assert_eq!(until(&mut engine, Some(true)), Some(true), "found again");
        assert!(tries.load(Ordering::Relaxed) >= 2);
    }

    /// A meter moving is sound: a source playing above the floor, heard or
    /// not, or a bus heard above it; quieter, or nothing at all, is not.
    #[test]
    fn sound_is_a_meter_above_the_floor() {
        let engine = AudioEngine::new();
        assert!(!engine.sounding(-60.0), "nothing at all");
        engine.core.lock().unwrap().sources.push(Source::new(
            "mic00001",
            Feed::Fifo(Arc::new(Mutex::new(std::collections::VecDeque::new()))),
        ));
        let set_source = |level: f32| {
            engine.core.lock().unwrap().sources[0].set_signal_for_tests([level, 0.0]);
        };
        set_source(0.0005); // about −66 dBFS
        assert!(!engine.sounding(-60.0), "below the floor");
        set_source(0.01); // −40 dBFS
        assert!(engine.sounding(-60.0), "a source playing");
        set_source(0.0);
        let bus = engine.core.lock().unwrap().buses.first().map(|b| b.uuid.clone());
        assert!(bus.is_some(), "a show starts with Master");
        engine.core.lock().unwrap().buses[0].set_meter_for_tests([0.0, 0.02]);
        assert!(engine.sounding(-60.0), "a bus heard");
    }

    fn with_clip(uuid: &str) -> AudioEngine {
        let mut engine = AudioEngine::new();
        engine.sync_clips(vec![ClipSource {
            deck_uuid: uuid.into(),
            name: "intro.mp4".into(),
            shared: Arc::new(ClipAudioShared::default()),
        }]);
        engine
    }

    fn at(level: f32) -> HashMap<String, f32> {
        std::iter::once(("d1".to_string(), level)).collect()
    }

    fn strip(engine: &AudioEngine) -> AudioStripConfig {
        engine
            .config()
            .strips
            .iter()
            .find(|s| s.uuid == "d1")
            .cloned()
            .expect("the clip's strip")
    }

    fn gate(engine: &AudioEngine) -> f32 {
        engine
            .snapshot()
            .sources
            .iter()
            .find(|s| s.uuid == "d1")
            .expect("the clip")
            .gate
    }

    /// The loudest sample a bus plays over `secs`.
    fn loudest(engine: &AudioEngine, bus: &str, secs: f64) -> f32 {
        let tap = engine.tap(bus).expect("the bus");
        let end = Instant::now() + Duration::from_secs_f64(secs);
        let mut peak = 0.0_f32;
        while Instant::now() < end {
            while let Ok(chunk) = tap.receiver.try_recv() {
                peak = chunk.samples.iter().fold(peak, |p, s| p.max(s.abs()));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        engine.untap(tap.token);
        peak
    }

    #[test]
    fn an_ndi_sources_sound_is_a_strip_heard_as_it_goes_on_air() {
        let mut engine = AudioEngine::new();
        let master = engine.default_bus().expect("Master").to_string();
        let (tx, rx) = crossbeam_channel::unbounded();
        let sound = |feed| LiveSound {
            deck_uuid: "d1".into(),
            name: "STUDIO (Camera 1)".into(),
            key: 7,
            feed,
        };
        engine.sync_live(vec![sound(Some(LiveFeed {
            rx,
            rate: 44_100,
            channels: 2,
        }))]);
        assert_eq!(engine.live_key("d1"), Some(7));
        assert!(strip(&engine).auto, "Audio Auto, like a clip");
        assert!(strip(&engine).muted, "off until it goes on air");
        let shown = engine.snapshot();
        let source = shown.sources.iter().find(|s| s.uuid == "d1").unwrap();
        assert_eq!(source.name, "STUDIO (Camera 1)");
        assert!(matches!(&source.kind, AudioSourceKind::Live { deck_uuid } if deck_uuid == "d1"));
        assert_eq!(
            engine.auto_sources(),
            vec![("d1".to_string(), "d1".to_string(), None)]
        );

        // A steady level at the sender's own rate, a frame's worth (33 ms)
        // at a time, as NDI sends it.
        let sending = Arc::new(AtomicBool::new(true));
        let sender = {
            let sending = sending.clone();
            std::thread::spawn(move || {
                let frame = vec![0.5_f32; 2 * 1470];
                while sending.load(Ordering::Relaxed) {
                    let _ = tx.send(crate::audio::PcmChunk {
                        samples: frame.clone(),
                        lost_before: 0,
                    });
                    std::thread::sleep(Duration::from_micros(33_333));
                }
            })
        };
        engine.set_on_air(HashMap::new());
        let off = loudest(&engine, &master, 0.3);
        engine.set_on_air(at(1.0));
        let on = loudest(&engine, &master, 0.5);
        sending.store(false, Ordering::Relaxed);
        let _ = sender.join();
        assert!(off < 1e-4, "silent off air: peak {off}");
        assert!(on > 0.3, "heard on air: peak {on}");

        // The same key again changes nothing; gone, the strip is kept.
        engine.sync_live(vec![sound(None)]);
        assert_eq!(engine.live_key("d1"), Some(7));
        engine.sync_live(Vec::new());
        assert_eq!(engine.live_key("d1"), None);
        assert!(engine.snapshot().sources.iter().all(|s| s.uuid != "d1"));
        assert!(engine.config().strips.iter().any(|s| s.uuid == "d1"));
    }

    #[test]
    fn audio_auto_presses_and_releases_the_on_button_with_the_picture() {
        let mut engine = with_clip("d1");
        assert!(strip(&engine).auto, "on for a new clip");
        assert!(strip(&engine).muted, "off until it goes on air");
        engine.set_on_air(HashMap::new());
        assert!(strip(&engine).muted);

        // A fade starting: pressed at once, the sound coming in with it.
        engine.set_on_air(at(0.3));
        assert!(!strip(&engine).muted);
        assert!((gate(&engine) - 0.3).abs() < 1e-6);
        engine.set_on_air(at(1.0));
        assert!((gate(&engine) - 1.0).abs() < 1e-6);

        // The operator turns it off on air; it stays off while on air…
        engine.edit_source("d1", |s| s.muted = true).unwrap();
        engine.set_on_air(at(1.0));
        engine.set_on_air(at(0.5));
        assert!(strip(&engine).muted, "the operator's choice holds on air");
        // …and leaving then coming back presses it again.
        engine.set_on_air(HashMap::new());
        engine.set_on_air(at(1.0));
        assert!(!strip(&engine).muted);

        // Fading out: the sound goes with the picture, then the button.
        engine.set_on_air(at(0.4));
        assert!(!strip(&engine).muted && (gate(&engine) - 0.4).abs() < 1e-6);
        engine.set_on_air(HashMap::new());
        assert!(strip(&engine).muted);
        assert!(
            (gate(&engine) - 1.0).abs() < 1e-6,
            "steady off air: the button decides"
        );
    }

    #[test]
    fn without_audio_auto_nothing_moves_the_on_button() {
        let mut engine = with_clip("d1");
        engine
            .edit_source("d1", |s| {
                s.auto = false;
                s.muted = false;
            })
            .unwrap();
        engine.set_on_air(HashMap::new());
        assert!(!strip(&engine).muted);
        engine.set_on_air(at(0.5));
        assert!(
            (gate(&engine) - 1.0).abs() < 1e-6,
            "no fade without Audio Auto"
        );
    }

    /// A rundown step fading a clip out — music on no screen, Audio Auto
    /// off — turns its sound down, and back up once let go; with Audio Auto
    /// fading it too, the two multiply.
    #[test]
    fn a_rundown_step_fades_a_sound_out_with_or_without_audio_auto() {
        let mut engine = with_clip("d1");
        engine
            .edit_source("d1", |s| {
                s.auto = false;
                s.muted = false;
            })
            .unwrap();
        engine.set_on_air(HashMap::new());
        engine.set_step_fades(HashMap::from([("d1".to_string(), 0.25)]));
        assert!((gate(&engine) - 0.25).abs() < 1e-6, "{}", gate(&engine));
        engine.set_step_fades(HashMap::new());
        assert!((gate(&engine) - 1.0).abs() < 1e-6, "let go");

        engine.edit_source("d1", |s| s.auto = true).unwrap();
        engine.set_on_air(at(0.5));
        engine.set_step_fades(HashMap::from([("d1".to_string(), 0.5)]));
        assert!((gate(&engine) - 0.25).abs() < 1e-6, "both: {}", gate(&engine));
    }

    #[test]
    fn a_strip_saved_before_audio_auto_keeps_following_its_picture() {
        let old: AudioStripConfig =
            serde_json::from_str(r#"{"uuid":"d1","sends":["m"],"follow":"bus00001"}"#).unwrap();
        assert!(old.auto, "it followed a bus: Audio Auto");
        assert_eq!(old.follow, None, "now on air anywhere");
        let always: AudioStripConfig = serde_json::from_str(r#"{"uuid":"mic1"}"#).unwrap();
        assert!(!always.auto, "heard all the time: no Audio Auto");

        let new: AudioStripConfig =
            serde_json::from_str(r#"{"uuid":"d2","auto":true,"follow":"main","balance":-0.25}"#)
                .unwrap();
        assert!(new.auto);
        assert_eq!(new.follow.as_deref(), Some(FOLLOW_MAIN));
        let back: AudioStripConfig =
            serde_json::from_str(&serde_json::to_string(&new).unwrap()).unwrap();
        assert_eq!(back, new);
    }

    /// A source's delay is kept to 0–1000 ms, saved with the show — and left
    /// out of it at 0, so a show saved before there was one loads as it was —
    /// and reaches the mix.
    #[test]
    fn a_sources_delay_is_kept_in_range_saved_and_mixed() {
        let mut engine = with_clip("d1");
        for (asked, kept) in [
            (170.0, 170.0),
            (5_000.0, MAX_DELAY_MS),
            (-20.0, 0.0),
            (f32::NAN, 0.0),
        ] {
            engine
                .edit_source("d1", |s| s.delay_ms = asked)
                .expect("the clip");
            assert!(
                (strip(&engine).delay_ms - kept).abs() < 1e-6,
                "{asked} kept as {kept}"
            );
        }
        engine
            .edit_source("d1", |s| s.delay_ms = 170.0)
            .expect("the clip");
        let core = engine.core.lock().unwrap();
        let mixed = core
            .sources
            .iter()
            .find(|s| s.uuid == "d1")
            .expect("in the mix");
        assert!((mixed.delay_ms - 170.0).abs() < 1e-6);
        drop(core);

        let saved = serde_json::to_string(&strip(&engine)).unwrap();
        assert!(saved.contains("\"delay_ms\":170"), "{saved}");
        let back: AudioStripConfig = serde_json::from_str(&saved).unwrap();
        assert_eq!(back, strip(&engine));
        let old: AudioStripConfig = serde_json::from_str(r#"{"uuid":"d1"}"#).unwrap();
        assert_eq!(old.delay_ms, 0.0, "saved before there was one");
        engine
            .edit_source("d1", |s| s.delay_ms = 0.0)
            .expect("the clip");
        assert!(!serde_json::to_string(&strip(&engine))
            .unwrap()
            .contains("delay_ms"));
    }

    #[test]
    fn processing_settings_round_trip_and_old_shows_get_only_the_limiter() {
        let old: AudioStripConfig = serde_json::from_str(r#"{"uuid":"d1","auto":true}"#).unwrap();
        assert_eq!(old.compressor, CompressorConfig::default());
        assert!(!old.low_cut.enabled && !old.leveler.enabled && !old.compressor.enabled);
        let old_bus: AudioBusConfig =
            serde_json::from_str(r#"{"uuid":"main","name":"Master"}"#).unwrap();
        assert_eq!(old_bus.compressor, CompressorConfig::default());
        // A show saved before the limiter gets it on: it only ever acts on
        // what would otherwise be within 1 dB of clipping.
        assert_eq!(old_bus.limiter, LimiterConfig::default());
        assert!(old_bus.limiter.enabled);
        let mut engine = with_clip("d1");
        let config = CompressorConfig {
            enabled: true,
            ratio: 7.0,
            makeup_db: 3.0,
            ..CompressorConfig::default()
        };
        let limiter = LimiterConfig {
            ceiling_db: -2.0,
            ..LimiterConfig::default()
        };
        engine
            .edit_source("d1", |strip| {
                strip.compressor = config;
                strip.low_cut = LowCutConfig::speech();
                strip.leveler = LevelerConfig::speech();
            })
            .unwrap();
        let bus = engine.default_bus().unwrap().to_string();
        engine
            .edit_bus(&bus, |bus| {
                bus.compressor = config;
                bus.limiter = limiter;
            })
            .unwrap();
        let saved = serde_json::to_string(engine.config()).unwrap();
        let back: AudioMixConfig = serde_json::from_str(&saved).unwrap();
        assert_eq!(&back, engine.config());
        assert_eq!(back.strips[0].compressor, config);
        assert_eq!(back.strips[0].low_cut, LowCutConfig::speech());
        assert_eq!(back.strips[0].leveler, LevelerConfig::speech());
        assert_eq!(back.buses[0].compressor, config);
        assert_eq!(back.buses[0].limiter, limiter);
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.sources[0].dynamics.low_cut, Some(LowCutConfig::speech()));
        assert_eq!(snapshot.buses[0].dynamics.limiter, Some(limiter));
        engine
            .edit_source("d1", |strip| strip.compressor.ratio = f32::NAN)
            .unwrap();
        assert_eq!(strip(&engine).compressor, CompressorConfig::default());
    }

    #[test]
    fn unrelated_edits_preserve_compressor_history_and_reconnect_recreates_it() {
        let mut engine = with_clip("d1");
        engine.stop.store(true, Ordering::Relaxed);
        engine.thread.take().unwrap().join().unwrap();
        let config = CompressorConfig {
            enabled: true,
            threshold_db: -30.0,
            ..CompressorConfig::default()
        };
        engine
            .edit_source("d1", |strip| strip.compressor = config)
            .unwrap();
        {
            let mut core = engine.core.lock().unwrap();
            let source = core
                .sources
                .iter_mut()
                .find(|source| source.uuid == "d1")
                .unwrap();
            let mut block = vec![[0.5; 2]; BLOCK_FRAMES];
            for _ in 0..10 {
                block.fill([0.5; 2]);
                source.dynamics.process(&mut block);
            }
        }
        let before = engine.snapshot().sources[0].dynamics.reduction_db;
        assert!(before > 10.0);
        engine
            .edit_source("d1", |strip| strip.gain_db = -6.0)
            .unwrap();
        assert_eq!(engine.snapshot().sources[0].dynamics.reduction_db, before);
        engine.load(engine.config().clone());
        assert_eq!(engine.snapshot().sources[0].dynamics.reduction_db, 0.0);
        engine.sync_clips(vec![]);
        engine.sync_clips(vec![ClipSource {
            deck_uuid: "d1".into(),
            name: "reconnected".into(),
            shared: Arc::new(ClipAudioShared::default()),
        }]);
        let snapshot = engine.snapshot();
        assert_eq!(snapshot.sources[0].dynamics.config, config);
        assert_eq!(snapshot.sources[0].dynamics.reduction_db, 0.0);
    }

    #[test]
    fn peak_reset_is_per_target_and_does_not_edit_audio_settings() {
        let mut engine=with_clip("d1");
        engine.stop.store(true,Ordering::Relaxed);
        engine.thread.take().unwrap().join().unwrap();
        let bus=engine.default_bus().unwrap().to_string();
        let config=engine.config().clone();
        {
            let mut core=engine.core.lock().unwrap();
            core.sources.iter_mut().find(|source|source.uuid=="d1").unwrap().held_peak=[0.5;2];
            core.buses.iter_mut().find(|target|target.uuid==bus).unwrap().held_peak=[1.0;2];
        }
        engine.reset_meter("d1",false);
        assert_eq!(engine.snapshot().sources[0].peak_db,[-96.0;2]);
        assert_eq!(engine.snapshot().buses[0].peak_db,[0.0;2]);
        engine.reset_meter(&bus,true);
        assert_eq!(engine.snapshot().buses[0].peak_db,[-96.0;2]);
        assert_eq!(engine.config(),&config);
    }

    #[test]
    fn resetting_a_buses_peak_keeps_its_loudness_and_resetting_loudness_keeps_its_peak() {
        let mut engine = with_clip("d1");
        engine.stop.store(true, Ordering::Relaxed);
        engine.thread.take().unwrap().join().unwrap();
        let bus = engine.default_bus().unwrap().to_string();
        {
            let mut core = engine.core.lock().unwrap();
            let target = core.buses.iter_mut().find(|b| b.uuid == bus).unwrap();
            let tone: Vec<reader::Frame> = (0..BLOCK_FRAMES)
                .map(|i| {
                    let v = 0.1 * (i as f32 * 0.13).sin();
                    [v, v]
                })
                .collect();
            for _ in 0..200 {
                let mut block = tone.clone();
                target.dynamics.process_post(&mut block);
            }
            target.held_peak = [0.1; 2];
        }
        let loudness = |e: &AudioEngine| {
            e.snapshot().buses[0]
                .dynamics
                .loudness
                .unwrap()
                .integrated_lufs
        };
        assert!(loudness(&engine).is_some());
        engine.reset_meter(&bus, true);
        assert!(
            loudness(&engine).is_some(),
            "the peak reset left the loudness"
        );
        engine.core.lock().unwrap().buses[0].held_peak = [0.1; 2];
        engine.reset_loudness(&bus);
        assert_eq!(loudness(&engine), None);
        assert_eq!(engine.core.lock().unwrap().buses[0].held_peak, [0.1; 2]);
    }
}
