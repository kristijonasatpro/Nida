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
mod clip;
mod core;
mod device;
mod dsp;
mod dynamics;
mod outputs;
pub mod reader;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use self::core::{Bus, Feed, InputFeed, MixCore, OutputRing, Source};
pub use self::core::{BusTap, BLOCK_FRAMES, MAX_DELAY_MS, SAMPLE_RATE};
pub use clip::{ClipAudio, ClipAudioShared};
pub use dsp::{CompressorConfig, LevelerConfig, LimiterConfig, LoudnessSnapshot, LowCutConfig};
pub use dynamics::DynamicsSnapshot;
use reader::gain_to_db;

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
    leveler: LevelerConfig,
    uuid: String,
    #[serde(default)]
    gain_db: f32,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    sends: Vec<String>,
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
            leveler: s.leveler.sanitized(),
            gain_db: s.gain_db,
            muted: s.muted,
            sends: s.sends,
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
}

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
    pub input_devices: Vec<AudioInputDevice>,
    /// Apps whose sound can be taken as an input, making sound first.
    pub apps: Vec<crate::audio::app_sound::RunningApp>,
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
struct LiveInput {
    source_id: crate::audio::AudioSourceId,
    token: crate::audio::PcmToken,
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
    input_devices: Vec<AudioInputDevice>,
    /// A listing of the sound devices being made (see [`Self::rescan_devices`]).
    listing: Option<std::sync::mpsc::Receiver<(Vec<String>, Vec<AudioInputDevice>)>>,
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
            apps: apps::Apps::spawn(taps),
            apps_refused: HashMap::new(),
            apps_generation: HashMap::new(),
            running_apps: Vec::new(),
            on_air: HashMap::new(),
            step_fades: HashMap::new(),
            solos: std::collections::HashSet::new(),
            applied_buses: std::collections::HashSet::new(),
            output_devices: Vec::new(),
            input_devices: Vec::new(),
            listing: None,
        };
        engine.load(AudioMixConfig::with_master());
        engine
    }

    /// The saved settings.
    pub fn config(&self) -> &AudioMixConfig {
        &self.config
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
        for uuid in stale {
            // Its refusal was the old app's: another app under the UUID,
            // not running, would show it.
            self.apps_refused.remove(&uuid);
            *self.apps_generation.entry(uuid).or_default() += 1;
        }
        self.config = config;
        // Inputs are re-subscribed on the next `connect_inputs`.
        self.inputs_tried = None;
        self.apply();
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
            leveler: LevelerConfig::default(),
            uuid: uuid.clone(),
            gain_db: 0.0,
            muted: deck.is_some(),
            sends,
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
                    leveler: LevelerConfig::default(),
                    uuid: clip.deck_uuid.clone(),
                    gain_db: 0.0,
                    // Off air until it is taken there.
                    muted: true,
                    sends: default_send.clone(),
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
                    leveler: LevelerConfig::default(),
                    uuid: sound.deck_uuid.clone(),
                    gain_db: 0.0,
                    muted: true,
                    sends: default_send.clone(),
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
    /// seconds, so a USB mixer plugged in mid-show comes back by itself.
    pub fn connect_inputs(&mut self, capture: &mut crate::audio::AudioManager) {
        // Before the apps' sound is taken: the sound released with a
        // subscription is then always the device's, never an app's.
        self.release_inputs(|id, token| capture.unsubscribe_pcm(id, token));
        self.forget_lost_inputs(|id, token| capture.serves(id, token));
        self.keep_apps();
        // An app's sound is taken by `keep_apps`, not through the capture
        // manager.
        let devices = self.config.inputs.iter().filter(|i| i.app.is_none()).count();
        if self.inputs.len() == devices {
            return;
        }
        if self.inputs_tried.is_some_and(|t| t.elapsed() < RETRY) {
            return;
        }
        self.inputs_tried = Some(Instant::now());
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
            let Some(sub) = capture.subscribe_pcm(id) else {
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
                    device: input.device.clone(),
                    left: input.left,
                    right: input.right,
                },
            );
        }
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
        let wanted = wanted
            .into_iter()
            .map(|(key, device, first_channel)| {
                let ring = self.rings.entry(key.clone()).or_default().clone();
                outputs::Want {
                    key,
                    device,
                    first_channel,
                    ring,
                }
            })
            .collect();
        self.outputs.want(wanted);
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
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("audio-device-listing".into())
            .spawn(move || {
                let outputs = device::output_device_names();
                let inputs = device::input_devices()
                    .into_iter()
                    .map(|(name, channels)| AudioInputDevice { name, channels })
                    .collect();
                let _ = tx.send((outputs, inputs));
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
            Ok((outputs, inputs)) => {
                self.output_devices = outputs;
                self.input_devices = inputs;
                self.listing = None;
                self.outputs.retry_now();
                self.inputs_tried = None;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.listing = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }
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
            soloing: core.soloing(),
            meter_db: meter_db(core.monitor.meter()),
        };
        AudioMixSnapshot {
            buses,
            sources,
            monitor,
            output_devices: self.output_devices.clone(),
            input_devices: self.input_devices.clone(),
            apps: self.running_apps.clone(),
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
            bus.outputs = match (&cfg.device, self.outputs.is_playing(&cfg.uuid)) {
                (Some(_), true) => self.rings.get(&cfg.uuid).cloned().into_iter().collect(),
                _ => Vec::new(),
            };
            core.buses.push(bus);
        }
        for source in &mut core.sources {
            if let Some(strip) = self.config.strips.iter().find(|s| s.uuid == source.uuid) {
                source.gain_db = strip.gain_db;
                source.muted = strip.muted;
                source.balance = strip.balance;
                source.delay_ms = strip.delay_ms;
                source
                    .dynamics
                    .configure(strip.low_cut, strip.leveler, strip.compressor);
                source.sends.clone_from(&strip.sends);
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
        self.apply_gates();
    }

    fn apply_gates(&mut self) {
        let Ok(mut core) = self.core.lock() else {
            return;
        };
        let buses: Vec<&str> = self.config.buses.iter().map(|b| b.uuid.as_str()).collect();
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
            if let Feed::Clip(shared) = &source.feed {
                let heard = source.solo
                    || source.gate > 0.0
                        && !source.muted
                        && source.sends.iter().any(|s| buses.contains(&s.as_str()));
                shared.set_wanted(heard);
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
