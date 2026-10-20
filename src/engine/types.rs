//! Shared value types for the engine layer.
//!
//! These types are used in engine trait signatures and snapshot structs.
//! They MUST NOT reference wgpu, egui, winit, or any GPU/UI framework types.
//!
//! Per /spec/engine-value-types.md, this module names its value vocabulary
//! from two places, and never reaches into `internal::{renderer,surface,video}`
//! directly:
//! - **Tier 1** (`engine::value::*`): plain data owned by the engine, whose
//!   *definitions* live in `crate::engine::value` — `renderer`/`surface`/
//!   `video` `pub use` them back to keep their existing call paths working.
//! - **Tier 2** (domain modules below): genuine domain entities that already
//!   lived in pure, framework-free modules (`audio`, `camera`, `channel`,
//!   `deck`, `mixer`, `modulation`, `params`) — out of scope for the Tier 1
//!   relocation; re-exported here as-is.

use serde::{Deserialize, Serialize};

// Tier 2 — pure domain modules, re-exported as-is.
pub use crate::audio::AudioSourceId;
pub use crate::bus::{AudioMixId, BusCanvas, BusInputRef};
pub use crate::camera::CameraId;
pub use crate::channel::{BlendMode, DeckRenderFps};
pub use crate::deck::{DeckRenderScale, ScalingMode};
pub use crate::depth::DepthSensorId;
pub use crate::mixer::CrossfadeEasing;
pub use crate::modulation::{
    ADSRStage, AudioBandPreset, AudioReactMode, LFOWaveform, StepInterpolation,
};
pub use crate::params::ParamValue;

// Tier 1 — engine-owned value types (see `crate::engine::value`).
pub use crate::engine::value::render::OutputSource;
pub use crate::engine::value::surface::{
    CircleHint, ContentMapping, CubicHandle, SurfaceOutputType, SurfacePath, SurfaceReorderOp,
};
pub use crate::engine::value::video::{DeckTransportSync, LoopMode, TransportSyncMode};

/// Identifies which effect chain to operate on.
///
/// Used for chain-scoped operations (append an effect, reorder within a chain).
/// Operations on an *existing* effect address it by its own UUID instead — see
/// [`/spec/api-addressing.md`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum EffectTarget {
    /// A deck's pre-composite chain, by deck UUID.
    Deck(String),
    /// A channel's post-composite chain, by channel UUID.
    Channel(String),
    /// The master output chain.
    Master,
}

/// What a copy is taken from. See [`/spec/clipboard.md`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ClipboardSource {
    Deck(String),
    Channel(String),
    Effect(String),
}

/// Where a paste lands.
///
/// The `After*` forms are what a right-click uses, so a copy arrives directly
/// below the thing the menu was opened on; the `Into*` forms append, which is
/// what the container's own menu and an API caller mean.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, utoipa::ToSchema)]
pub enum PasteTarget {
    /// This deck's channel, directly below it.
    AfterDeck(String),
    /// The end of this channel.
    IntoChannel(String),
    /// This effect's chain, directly after it.
    AfterEffect(String),
    /// The end of a deck, channel, or master chain.
    IntoChain(EffectTarget),
    /// A new channel at the end of the mixer.
    NewChannel,
}

/// What the clipboard is holding, for a menu that has to name it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ClipboardSummary {
    pub kind: ClipboardKind,
    /// The object's own name, for "Paste deck 'ripple'".
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ClipboardKind {
    Deck,
    Channel,
    Effect,
}

/// Per-frame engine state snapshot — plain data, no GPU types, no lifetimes.
///
/// Produced by `VardaApp` each frame. Distributed to consumers via watch channel.
/// `UIData` is derived from this for the egui UI consumer.
/// How evenly frames are started and shown — for telling a judder's cause:
/// frames started late (the renderer), or presented unevenly (an output).
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct PacingSnapshot {
    /// What starts each frame: a screen's refresh ("display 2 at 60.00 Hz,
    /// every refresh") or Nida's own timer ("timer at 60 fps").
    pub clock: String,
    /// Frames the GPU may still be drawing when the next starts (see the
    /// `SetFramesInFlight` command).
    pub frames_in_flight: u32,
    /// What starts each frame (see the `SetFramePacing` command).
    pub frame_pacing: crate::engine::value::render::FramePacing,
    /// Time between the starts of the last frames (about five seconds of
    /// them), in milliseconds.
    pub interval_mean_ms: f32,
    pub interval_stdev_ms: f32,
    pub interval_min_ms: f32,
    pub interval_max_ms: f32,
    /// Frames counted in those figures.
    pub intervals: u32,
    /// Of those, frames started more than half a frame late: a screen
    /// showing every frame showed the one before twice.
    pub late_frames: u32,
    /// Each output window's presents since it opened.
    pub outputs: Vec<OutputPacing>,
    /// Each live camera's and captured screen's delay inside Nida.
    #[serde(default)]
    pub inputs: Vec<InputPacing>,
    /// Where frames' time goes: each part's mean and worst over the last
    /// frames, the costliest first — "ui", "gpu wait", "mixer", a window's
    /// "acquire" (its wait for a frame to draw into) and "draw", "ui present",
    /// and "start delay" (from the refresh that called for a frame to its
    /// start).
    pub parts: Vec<PartStats>,
    /// The last frames (up to 20) that were busy for most of a frame's time,
    /// started half a frame after their refresh, or came half a frame late,
    /// part by part.
    pub slow_frames: Vec<SlowFrame>,
    /// Frames asked for by the refresh each display-link callback is
    /// stamped for, not by counting callbacks (see `SetClockTimestamps`).
    pub clock_timestamps: bool,
    /// Each frame held on screen for a frame's time (see `SetFrameHold`).
    pub frame_hold: bool,
    /// The control window presents in step with its screen's refresh (see
    /// `SetControlVsync`).
    pub control_vsync: bool,
    /// The control window's frames waiting to be shown.
    pub control_queue: ControlQueueStats,
    /// The display link's callbacks and what came of them, when a screen's
    /// refresh starts frames. Updated every tenth frame.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_link: Option<DisplayLinkSnapshot>,
    /// How evenly the control window's frames reached its screen.
    pub control_on_screen: OnScreenStats,
    /// Where the control window's frames' time goes, now that it draws on
    /// the main thread apart from the engine: each part's mean and worst,
    /// the costliest first — "ui" (laying it out), "engine wait" (for the
    /// engine thread to finish a frame), "ui acquire" (for a frame of its
    /// own to draw into), and so on.
    #[serde(default)]
    pub control_parts: Vec<PartStats>,
    /// The control window's last slow frames, part by part.
    #[serde(default)]
    pub control_slow_frames: Vec<SlowFrame>,
    /// How busy the GPU is (0–100), as the OS reports it (macOS: the
    /// accelerator's "Device Utilization %"); absent where it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_busy_pct: Option<f32>,
    /// Where Nida's GPU time went over the last second, by part of the app
    /// (macOS: when each command buffer ran on the GPU); absent elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_time: Option<GpuTimeSnapshot>,
}

/// Where Nida's GPU time went over the last second.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct GpuTimeSnapshot {
    /// The share of the second Nida's work was running on the GPU (0–100),
    /// command buffers that overlapped counted once. The whole GPU's
    /// `gpu_busy_pct` adds other apps' work and the screens' compositing.
    pub busy_pct: f32,
    /// GPU time by part of the app — the show, the control window, its
    /// previews, the outputs, wgpu's own uploads — most first.
    pub groups: Vec<GpuPartTime>,
    /// GPU time by command buffer label, most first.
    pub parts: Vec<GpuPartTime>,
}

/// One part's GPU time.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct GpuPartTime {
    pub name: String,
    /// Milliseconds of GPU time per second: its share of the time the GPU
    /// was busy with Nida's work, a moment shared equally among the runs
    /// going on then — the parts add up to `busy_pct` (×10).
    pub ms_per_s: f32,
    /// How many times it ran per second.
    pub runs_per_s: f32,
}

/// The display link that starts frames: its callbacks, the frames they
/// asked for, and the redraws the frame loop saw.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct DisplayLinkSnapshot {
    /// Callbacks since the link started.
    pub callbacks: u64,
    /// Frames they asked for.
    pub wakes: u64,
    /// Of those asks, the ones that reached the frame loop.
    pub link_events: u64,
    /// Frames drawn.
    pub redraws: u64,
    /// Redraws macOS asked for by itself (a window needing display), not
    /// the frame loop.
    pub unrequested_redraws: u64,
    /// Of those, the ones not drawn (with `clock_timestamps` on).
    pub skipped_redraws: u64,
    /// Asks that reached the frame loop while the one before was still
    /// waiting to be drawn: merged into one frame, a refresh with none.
    pub collapsed: u64,
    /// The last callbacks (about five seconds' worth), oldest first.
    pub recent: Vec<LinkTick>,
}

/// One display-link callback.
#[derive(Clone, Copy, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct LinkTick {
    /// When it came, in milliseconds after the link started.
    pub at_ms: f64,
    /// The refresh it is for — the time macOS stamped it with — in the same
    /// milliseconds; none when macOS gave none.
    pub shown_ms: Option<f64>,
}

/// One part of the frame loop's time.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct PartStats {
    pub name: String,
    pub mean_ms: f32,
    pub max_ms: f32,
}

/// A frame that ran long or started late, part by part.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct SlowFrame {
    /// When it started, in seconds after the first frame.
    pub at_secs: f64,
    /// From the start of the frame before.
    pub interval_ms: f32,
    /// From the refresh that called for it to its start, when a screen's
    /// refresh starts frames.
    pub start_delay_ms: Option<f32>,
    /// From its start to its end.
    pub busy_ms: f32,
    /// Its parts over 0.3 ms, in the order spent.
    pub parts: Vec<FramePart>,
}

/// Time a frame spent on one part.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct FramePart {
    pub name: String,
    pub ms: f32,
}

/// How evenly a window's frames reached the screen since it opened, from
/// the system's own report of when each was shown (macOS; zeros elsewhere).
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct OnScreenStats {
    /// Frames shown.
    pub shown: u64,
    /// Frames presented but never shown.
    pub dropped: u64,
    /// Frames up more than half a frame too long: the one before repeated,
    /// the hitch an eye sees in a moving slide.
    pub hitches: u64,
    /// Frames replaced less than half a frame after they came up.
    pub early: u64,
    /// How long each frame should stay up, ms.
    pub expected_ms: f32,
    /// Mean and spread of the last 300 times between frames coming up, ms.
    pub interval_mean_ms: f32,
    pub interval_sd_ms: f32,
}

/// The control window's queue of frames waiting to be shown. With every
/// frame held on screen it settles at 3 (measured on a laptop), and the
/// frame loop waits for the oldest to come off the screen — time taken from
/// the loop, not smoothness: skipping frames to drain it made the control
/// screen stutter.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct ControlQueueStats {
    /// Frames presented and not yet reported shown, most over the last ten
    /// frames.
    pub waiting: u64,
}

/// A live input's frames on their way into the show.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct InputPacing {
    /// The camera or captured screen.
    pub name: String,
    /// How long before its show frame was due on screen the frame shown was
    /// captured, milliseconds: the input's delay inside Nida, past the
    /// camera's own (see `frame_picker`). The outputs' own delay to their
    /// glass comes on top.
    pub behind_ms: f32,
}

/// One output window's presents, counted since it opened.
#[derive(Clone, Debug, Default, Serialize, utoipa::ToSchema)]
pub struct OutputPacing {
    pub name: String,
    /// Frames presented.
    pub presents: u64,
    /// Frames drawn but not presented, to keep to the display's rate.
    pub pace_skipped: u64,
    /// Frames the window had nowhere to present.
    pub unavailable: u64,
    /// On the screen whose refresh starts frames: every frame presented.
    pub follows_clock: bool,
    /// How evenly its frames reached the screen.
    pub on_screen: OnScreenStats,
}

// Serialized DTO: the flags mirror independent engine toggles, not a state enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize)]
pub struct EngineState {
    pub mixer: MixerSnapshot,
    /// Configured production buses, in order.
    pub buses: Vec<BusSnapshot>,
    /// The rundown: its steps, where the show is, what comes next.
    pub rundown: RundownSnapshot,
    pub audio: AudioSnapshot,
    /// The audio mixer: buses, source strips, meters, sound devices.
    pub audio_mix: crate::audio_mix::AudioMixSnapshot,
    /// The stream: settings with keys and passwords masked, and how each
    /// destination is doing.
    pub stream: crate::streaming::StreamSnapshot,
    /// Recording: settings and how it is doing.
    pub record: crate::record::RecordSnapshot,
    pub modulation: ModulationSnapshot,
    pub outputs: OutputSnapshot,
    pub registry: RegistrySnapshot,
    pub midi: MidiSnapshot,
    pub cameras: CameraSnapshot,
    pub depth_sensors: DepthSensorSnapshot,
    pub screen_capture: ScreenCaptureSnapshot,
    pub clock: ClockSnapshot,
    pub transport: TransportSnapshot,
    pub timecode: TimecodeSnapshot,
    /// Present only when the scene has an arrangement.
    pub arrangement: Option<ArrangementSnapshot>,
    pub fps: f32,
    pub frame_count: u64,
    /// Target FPS (0 = uncapped)
    pub target_fps: u32,
    /// How evenly frames are started and shown.
    pub pacing: PacingSnapshot,
    /// Discovered NDI sources (names)
    pub ndi_sources: Vec<String>,
    /// Whether NDI runtime is available
    pub ndi_available: bool,
    /// Discovered Syphon servers (names)
    pub syphon_sources: Vec<String>,
    /// Blackmagic devices that can put out video, and their formats (see
    /// `crate::decklink`).
    #[serde(default)]
    pub decklink_devices: Vec<crate::decklink::DeckLinkDevice>,
    /// Whether Syphon framework is available
    pub syphon_available: bool,
    /// Active stream receiver configs (url, mode, connected)
    pub stream_receivers: Vec<StreamReceiverSnapshot>,
    pub analyzers: Vec<AnalyzerTypeInfo>,
    /// User-defined macro controls (one control → many parameter targets).
    pub macros: Vec<crate::macros::Macro>,
    /// Whether the undo timeline has an undoable action (shared UI/API timeline).
    pub can_undo: bool,
    /// Whether the redo timeline has a redoable action (shared UI/API timeline).
    pub can_redo: bool,
}

/// Snapshot of an active stream receiver for UI consumption.
#[derive(Clone, Serialize)]
pub struct StreamReceiverSnapshot {
    pub url: String,
    pub mode: String,
    pub connected: bool,
}

// ── Clock Snapshot ──────────────────────────────────────────────

/// A detected MIDI clock source for UI display.
#[derive(Clone, Debug, Serialize)]
pub struct DetectedClockSourceSnapshot {
    pub device_id: crate::midi::DeviceId,
    pub device_name: String,
    pub bpm: Option<f32>,
}

/// Snapshot of one production bus.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct BusSnapshot {
    /// Stable bus UUID.
    pub uuid: String,
    /// Operator-facing label.
    pub name: String,
    /// What is staged on Preview.
    pub preview: BusInputRef,
    /// What is settled on Program. During a transition this is still the
    /// outgoing content; `transition` describes the blend actually on air.
    pub program: BusInputRef,
    /// Whether `preview` resolves against the loaded scene. When false the bus
    /// shows black, and the reference is retained only so the operator can see
    /// what the show intended.
    pub preview_resolved: bool,
    /// Whether `program` resolves against the loaded scene. When false the bus
    /// puts black on air rather than any other source.
    pub program_resolved: bool,
    /// Bus canvas size. Stored and persisted, but not yet honoured by the
    /// renderer — see docs/conference/known-upstream-defects.md.
    pub canvas: BusCanvas,
    /// Identity of the audio mix this bus owns.
    pub audio_mix: AudioMixId,
    /// Default `AUTO` duration in seconds.
    pub auto_duration: f32,
    /// Mixer layers (channel UUIDs) this bus leaves out when it shows Master.
    pub hidden_layers: Vec<String>,
    /// In-flight transition, if any.
    pub transition: Option<BusTransitionSnapshot>,
    /// Where the bus is shown from its card's Screen: picker: a monitor, by
    /// name, or a Blackmagic device ("UltraStudio Monitor 3G (Blackmagic)");
    /// none when it is on no screen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
}

/// The rundown: the show in order, as steps taken with one press.
#[derive(Clone, Default, Serialize, utoipa::ToSchema)]
pub struct RundownSnapshot {
    /// Seconds the buses take to fade to a step.
    pub fade_secs: f32,
    /// The steps, in show order.
    pub steps: Vec<RundownStepSnapshot>,
    /// The step taken last, by UUID: where the show is.
    pub last: Option<String>,
    /// The step GO takes, by UUID; none at the end of the rundown.
    pub next: Option<String>,
    /// The wait of a step that goes on to the next by itself, while it
    /// runs.
    pub wait: Option<RundownWaitSnapshot>,
}

/// One step of the rundown, and whether it can be taken now.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct RundownStepSnapshot {
    /// What it sets.
    #[serde(flatten)]
    pub step: crate::rundown::Step,
    /// Why it cannot be taken: what it sets that is not in the show, each a
    /// phrase. Empty when it can.
    pub missing: Vec<String>,
    /// What is still fading, so that a fade to it is refused just now — a
    /// cut is not.
    pub fading: Vec<String>,
    /// Whether its picture — the buses, Main and overlays it sets — is what
    /// is on now. Not whether its clips are playing.
    pub shown: bool,
}

/// A step's wait being counted out.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct RundownWaitSnapshot {
    /// The step whose wait it is, by UUID.
    pub from: String,
    /// The step taken when it is up, by UUID.
    pub to: String,
    /// Seconds left.
    pub secs_left: f32,
    /// Whether that step is faded to, or cut.
    pub fade: bool,
}

/// Snapshot of an in-flight bus transition.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct BusTransitionSnapshot {
    /// Content the bus is leaving.
    pub from: BusInputRef,
    /// Content the bus is heading to.
    pub to: BusInputRef,
    /// Total duration in seconds.
    pub duration: f32,
    /// Linear progress, 0.0 to 1.0.
    pub progress: f32,
    /// Eased mix factor, 0.0 (fully `from`) to 1.0 (fully `to`).
    pub mix: f32,
    /// Whether the outgoing endpoint resolves against the loaded scene. A
    /// renderer must show black for an endpoint that does not.
    pub from_resolved: bool,
    /// Whether the incoming endpoint resolves against the loaded scene.
    pub to_resolved: bool,
}

/// Snapshot of the unified clock state for UI display.
#[derive(Clone, Serialize)]
pub struct ClockSnapshot {
    /// Current BPM from the resolved clock source.
    pub bpm: Option<f32>,
    /// Beat phase 0.0–1.0.
    pub beat_phase: f32,
    /// Which source is active: "Audio", "MIDI", "OSC", or "None".
    pub source_label: String,
    /// Device name (for MIDI clock source).
    pub device_name: Option<String>,
    /// Whether a valid clock source is active.
    pub active: bool,
    /// All MIDI devices currently detected as sending clock ticks.
    pub detected_midi_sources: Vec<DetectedClockSourceSnapshot>,
    /// Whether OSC clock is currently active.
    pub osc_active: bool,
    /// Current OSC BPM (if active).
    pub osc_bpm: Option<f32>,
    /// Current audio BPM (always available as fallback).
    pub audio_bpm: Option<f32>,
    /// Current preference label: "Auto", "`ForceMidi`(<name>)", "`ForceOsc`", "`ForceAudio`", "`ForceManual`".
    pub preference_label: String,
    /// Device ID if preference is `ForceMidi`.
    pub preference_force_device_id: Option<crate::midi::DeviceId>,
    /// Manual BPM value (if preference is `ForceManual`).
    pub manual_bpm: Option<f32>,
    /// How many modulation sources are locked to the beat. Drives the readout's
    /// emphasis and answers "what stops if this clock goes away".
    /// See /spec/transport.md § Tempo and position are both shown.
    pub beat_followers: usize,
}

/// Snapshot of arrangement mode. See /spec/arrangement.md.
///
/// Absent when the scene has no arrangement, which is how a Performance-only
/// scene stays free of arrangement concepts entirely.
#[derive(Clone, Serialize, Default)]
pub struct ArrangementSnapshot {
    /// The authored lanes and idle behaviour, verbatim.
    pub config: crate::arrangement::ArrangementConfig,
    /// Whether the arrangement is currently driving decks. False before the
    /// transport has run, so a scene that opens in Performance mode stays there.
    pub engaged: bool,
    /// Modulation keys a performer has taken by hand. Drives the "held" badge
    /// and the re-arm affordance.
    pub overridden_params: Vec<String>,
    /// Latest position covered by any region, for the ruler's default extent.
    pub duration: f64,
}

/// Snapshot of the absolute show position. See /spec/transport.md.
///
/// Distinct from [`ClockSnapshot`], which is tempo. Both can be live at once.
#[derive(Clone, Serialize)]
pub struct TransportSnapshot {
    /// Absolute position in seconds. `f64` because shows conventionally start
    /// at hour 1.
    pub position: f64,
    pub running: bool,
    /// Whether the transport has advanced at least once this session. Until it
    /// has, position-locked features stay inert.
    pub has_run: bool,
    pub source: crate::transport::TransportSource,
    /// Why the transport is or is not moving, so idle and broken are
    /// distinguishable on a dark stage.
    pub status_label: String,
    pub loop_region: Option<crate::transport::LoopRegion>,
    /// Frame rate positions are displayed at.
    pub timecode_rate: crate::transport::TimecodeRate,
    /// Position pre-rendered as `HH:MM:SS:FF`, so every consumer shows the
    /// same string rather than each reimplementing drop-frame.
    pub timecode: String,
    /// How many modulation sources are locked to the transport. Counterpart to
    /// [`ClockSnapshot::beat_followers`].
    pub followers: usize,
    /// Whether live parameter writes are being kept as automation. Armed and
    /// recording are different states: nothing is written until the position
    /// moves. See /spec/automation-recording.md.
    pub record_armed: bool,
    /// Parameter keys with a take open right now, so the lanes catching a pass
    /// can say so. Filled in by the snapshot builder, which can see the
    /// recorder.
    pub recording_params: Vec<String>,
}

impl Default for TransportSnapshot {
    fn default() -> Self {
        Self {
            position: 0.0,
            running: false,
            has_run: false,
            source: crate::transport::TransportSource::default(),
            status_label: crate::transport::TransportStatus::Idle.label().to_string(),
            loop_region: None,
            timecode_rate: crate::transport::TimecodeRate::default(),
            timecode: crate::transport::TimecodeRate::default().format(0.0),
            followers: 0,
            record_armed: false,
            recording_params: Vec::new(),
        }
    }
}

impl From<&crate::transport::Transport> for TransportSnapshot {
    /// `followers` is left at zero here: the count lives in the modulation
    /// engine, which the transport has no reference to. The snapshot builder
    /// fills it in.
    fn from(t: &crate::transport::Transport) -> Self {
        Self {
            position: t.position(),
            running: t.running(),
            has_run: t.has_run(),
            source: t.source(),
            status_label: t.status().label().to_string(),
            loop_region: t.loop_region(),
            timecode_rate: t.timecode_rate(),
            timecode: t.formatted_position(),
            followers: 0,
            record_armed: false,
            recording_params: Vec::new(),
        }
    }
}

// ── Timecode Snapshot ──────────────────────────────────────────────

/// One timecode input, resolved or not. See /spec/timecode.md.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct TimecodeInputSnapshot {
    /// Stable name, `ltc` or `mtc:<device>`, and what `resolved` names.
    pub key: String,
    /// For a readout: "LTC (channel 2)", "MTC (Tascam Model 12)".
    pub label: String,
    pub position: f64,
    /// Position as `HH:MM:SS:FF` at this input's own rate, which is not
    /// necessarily the rate the ruler is drawn at.
    pub timecode: String,
    pub rate: crate::transport::TimecodeRate,
    pub running: bool,
    /// Coasting through a dropout rather than reading frames.
    pub freewheeling: bool,
    /// Measured against wall time; 1.0 while a master plays forwards.
    pub speed: f64,
}

/// Every timecode input being listened to, and which one is driving.
///
/// A list rather than an object even though one signal drives the transport: a
/// performer chasing a bad cable needs to see the input that is *not*
/// resolving. See /spec/timecode.md § Dual simultaneous inputs.
#[derive(Clone, Serialize, Default, utoipa::ToSchema)]
pub struct TimecodeSnapshot {
    pub inputs: Vec<TimecodeInputSnapshot>,
    /// `key` of the input driving the transport, if any.
    pub resolved: Option<String>,
    pub preference: crate::timecode::TimecodePreference,
    /// The audio input LTC is expected on, while one is patched.
    pub ltc_input: Option<crate::timecode::LtcInput>,
}

// ── Registry Snapshot ──────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct RegistrySnapshot {
    /// Generator shaders: (name, index)
    pub generators: Vec<(String, usize)>,
    /// Filter shaders: (name, index)
    pub filters: Vec<(String, usize)>,
    /// Total shader count
    pub shader_count: usize,
}

// ── Mixer Snapshot ──────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct MixerSnapshot {
    pub channels: Vec<ChannelSnapshot>,
    pub crossfader: f32,
    pub auto_crossfade_active: bool,
    pub auto_crossfade_progress: f32,
    pub master_effects: Vec<EffectSnapshot>,
    pub active_transition_name: Option<String>,
    pub transition_names: Vec<String>,
    pub sequences: Vec<SequenceSnapshot>,
    pub tonemap_mode: crate::engine::value::render::TonemapMode,
    pub active_lut: Option<String>,
    /// Main, the vMix way: the mode, what is on air, fades and overlays.
    pub main: crate::mixer::MainSnapshot,
}

#[derive(Clone, Serialize)]
pub struct ChannelSnapshot {
    pub idx: usize,
    pub uuid: String,
    pub name: String,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub decks: Vec<DeckSnapshot>,
    pub effects: Vec<EffectSnapshot>,
    /// Smoothed render time for this channel in milliseconds
    pub render_time_ms: f32,
    /// Number of active (rendered) decks in the last frame
    pub active_deck_count: u32,
    /// An overlay in Main mode.
    pub overlay: bool,
}

// Serialized DTO: the flags mirror independent deck toggles, not a state enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize)]
pub struct DeckSnapshot {
    pub idx: usize,
    pub uuid: String,
    pub name: String,
    /// True when this deck's source is an HTML/Servo instance.
    pub is_html: bool,
    /// True when the interactive window is currently open for this deck.
    pub is_html_interactive: bool,
    /// True when this deck's source is a depth sensor (point-cloud) source.
    pub is_depth_sensor: bool,
    /// Point-cloud controls (None = not a depth-sensor source).
    pub point_cloud_params: Option<PointCloudParamsSnapshot>,
    /// True when this deck has a `depth_sensor` shader preprocessor attached.
    pub has_depth_prepro: bool,
    /// Depth-preprocessor controls (None = no preprocessor attached).
    pub depth_prepro_params: Option<DepthPreproParamsSnapshot>,
    /// Screen-capture controls (None = not a screen-capture source).
    pub screen_capture: Option<ScreenCaptureDeckSnapshot>,
    /// Tap controls (None = not a tap source).
    pub tap: Option<TapDeckSnapshot>,
    /// A playlist's items and where it has got (None = not a playlist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist: Option<crate::engine::value::playlist::PlaylistSnapshot>,
    /// A timer's settings and what it reads (None = not a timer).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timer: Option<crate::engine::value::timer::TimerSnapshot>,
    /// How a camera's picture is read into colours (None = not a camera).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_colour: Option<crate::engine::value::camera::CameraColour>,
    /// What a camera's picture is coded as (None = not a camera).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<crate::engine::value::camera::CameraProfile>,
    /// Whether a camera's newest picture is shown every frame (None = not a
    /// camera).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_lowest_latency: Option<bool>,
    pub opacity: f32,
    pub effective_opacity: f32,
    pub blend_mode: BlendMode,
    pub solo: bool,
    pub mute: bool,
    /// True when this deck preserves source alpha (transparent compositing).
    pub transparent: bool,
    pub scaling_mode: Option<ScalingMode>,
    /// The source picture's size in pixels, for a source a scaling mode fits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_size: Option<[u32; 2]>,
    /// A colour layer's colour, straight RGBA 0 to 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub solid_color: Option<[f32; 4]>,
    pub generator: ShaderParamsSnapshot,
    pub effects: Vec<EffectSnapshot>,
    pub video_playback: Option<VideoPlaybackSnapshot>,
    pub auto_transition: Option<AutoTransitionSnapshot>,
    /// Configured render FPS (Auto or fixed value)
    pub render_fps: DeckRenderFps,
    /// Resolution the deck renders at, relative to the master.
    #[serde(default)]
    pub render_scale: DeckRenderScale,
    /// Effective render rate (actual FPS this deck is rendering at)
    pub effective_render_fps: f32,
    /// Smoothed render cost in microseconds
    pub render_cost_us: f32,
    /// GPU-measured render cost in microseconds (0 = not available)
    pub gpu_render_cost_us: f32,
    /// Smoothed FPS from actual deck render pipeline timing
    pub fps: f32,
    /// An image drawn again only when its settings change (see
    /// `Deck::still_frame`): its `fps` counts redraws, not motion.
    #[serde(default)]
    pub still: bool,
    /// True while the arrangement has this deck's source asleep because no
    /// region or curve will show it soon. A sleeping video holds its frame and
    /// resumes from there, which is why a frozen clip is worth reporting rather
    /// than leaving someone to wonder. See /spec/deck-residency.md.
    pub source_asleep: bool,
    pub running_analyzers: Vec<RunningAnalyzerSnapshot>,
}

/// Router-exposed `deck/<uuid>/depth/*` values, normalized to `0..1` so a
/// consumer can render faders without reaching into the engine.
/// See spec/depth-sensors.md.
#[derive(Clone, Serialize)]
pub struct PointCloudParamsSnapshot {
    pub orbit_yaw: f32,
    pub orbit_pitch: f32,
    pub zoom: f32,
    pub point_size: f32,
    pub depth_min: f32,
    pub depth_max: f32,
    pub seed: f32,
    pub drift: f32,
    pub disruption: f32,
    /// 0 = Rgb, 1 = `DepthRamp`, 2 = Solid.
    pub color_mode: u8,
}

/// Router-exposed `deck/<uuid>/depth_prepro/*` values, normalized to `0..1` so a
/// consumer can render faders without reaching into the engine.
/// See spec/depth-sensor-preprocessor.md.
#[derive(Clone, Serialize)]
pub struct DepthPreproParamsSnapshot {
    /// Name of the sensor the preprocessor acquired.
    pub sensor_name: String,
    pub near: f32,
    pub far: f32,
    pub smoothing: f32,
    pub hole_fill: f32,
    pub mask_feather: f32,
    pub motion_gain: f32,
    /// Bucketed from the normalized `mirror` fader.
    pub mirror: bool,
}

#[derive(Clone, Serialize)]
pub struct EffectSnapshot {
    pub uuid: String,
    pub name: String,
    pub enabled: bool,
    /// Pending and failed effects remain editable but do not draw.
    pub load_state: EffectLoadState,
    pub params: ShaderParamsSnapshot,
    /// Where the effect shows; everywhere when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mask: Option<crate::engine::value::mask::EffectMask>,
}

/// Runtime state only; saved shows retain the effect's settings, not a job.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectLoadState {
    #[default]
    Ready,
    Building,
    Failed {
        message: String,
    },
}

#[derive(Clone, Serialize)]
pub struct ShaderParamsSnapshot {
    pub shader_name: String,
    pub params: Vec<ParamSnapshot>,
}

#[derive(Clone, Serialize)]
pub struct ParamSnapshot {
    pub name: String,
    pub label: Option<String>,
    pub value: ParamValue,
    pub min: Option<f32>,
    pub max: Option<f32>,
}

#[derive(Clone, Serialize)]
pub struct VideoPlaybackSnapshot {
    pub playing: bool,
    pub position: f64,
    pub duration: f64,
    /// The performer's set point, untouched by modulation.
    pub speed: f64,
    /// The rate playback is actually running at, which differs from `speed` only
    /// while a modulator holds it. See /spec/video-playback-modulation.md.
    pub effective_speed: f64,
    /// How far a modulator has carried the playhead from where the clip would
    /// otherwise be. `position - position_offset` is the point it swings around.
    pub position_offset: f64,
    pub loop_mode: LoopMode,
    pub in_point: f64,
    pub out_point: f64,
    pub frame_rate: f64,
    pub transport_sync: crate::engine::value::video::DeckTransportSync,
    /// Decoded on the Mac's media engine rather than in software.
    pub hardware_decode: bool,
    /// Auto Play / Pause / Restart as it goes on and off air.
    pub auto: crate::engine::value::video::AutoPlayback,
    /// A sound file: nothing to see.
    pub sound_only: bool,
}

// Serialized DTO: each flag pairs with its own value field (beats vs seconds).
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize)]
pub struct AutoTransitionSnapshot {
    pub enabled: bool,
    pub trigger_is_clip_end: bool,
    pub play_duration_value: f64,
    pub play_duration_is_beats: bool,
    pub transition_duration_value: f64,
    pub transition_duration_is_beats: bool,
    pub transition_shader_name: Option<String>,
    pub phase: crate::channel::DeckTransitionPhase,
}

// ── Audio Snapshot ──────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct AudioSnapshot {
    pub level: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub bpm: Option<f32>,
    pub beat_phase: f32,
    pub enabled: bool,
    pub devices: Vec<AudioDeviceSnapshot>,
    pub fft: Vec<f32>,
    pub sample_rate: f32,
}

#[derive(Clone, Serialize)]
pub struct AudioDeviceSnapshot {
    pub id: AudioSourceId,
    pub name: String,
    pub active: bool,
}

// ── Modulation Snapshot ─────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct ModulationSnapshot {
    pub sources: Vec<ModulationSourceSnapshotEntry>,
    pub current_values: std::collections::HashMap<String, f32>,
    pub assignments: std::collections::HashMap<String, Vec<ModulationAssignmentSnapshot>>,
}

#[derive(Clone, Serialize)]
pub struct ModulationSourceSnapshotEntry {
    pub uuid: String,
    pub source: ModulationSourceSnapshot,
    /// Which notion of time this source follows. See /spec/timebase.md.
    pub timebase: crate::timebase::Timebase,
}

#[derive(Clone, Serialize)]
pub enum ModulationSourceSnapshot {
    LFO {
        waveform: LFOWaveform,
        frequency: f32,
        phase: f32,
        amplitude: f32,
        bipolar: bool,
    },
    Audio {
        source_id: Option<AudioSourceId>,
        freq_low: f32,
        freq_high: f32,
        gain: f32,
        smoothing: f32,
        mode: AudioReactMode,
        noise_gate: f32,
    },
    ADSR {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
        stage: ADSRStage,
    },
    StepSequencer {
        steps: Vec<f32>,
        rate: f32,
        interpolation: StepInterpolation,
        bipolar: bool,
    },
    Analyzer {
        deck_id: String,
        analyzer_type: String,
        output_name: String,
        smoothing: f32,
    },
    Envelope {
        breakpoints: Vec<crate::modulation::Breakpoint>,
    },
}

#[derive(Clone, Serialize)]
pub struct ModulationAssignmentSnapshot {
    pub source_id: String,
    pub amount: f32,
}

// ── Sequence Snapshot ───────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct SequenceSnapshot {
    pub uuid: String,
    pub name: String,
    pub enabled: bool,
    pub playing: bool,
    pub current_step: usize,
    pub step_elapsed: f64,
    pub steps: Vec<SequenceStepSnapshot>,
}

#[derive(Clone, Serialize)]
pub struct SequenceStepSnapshot {
    pub label: String,
    pub kind: SequenceStepKindSnapshot,
}

#[derive(Debug, Clone, Serialize)]
pub enum SequenceStepKindSnapshot {
    Fade {
        /// Source channel UUID. Resolve against the channel list for a name; a
        /// UUID that no longer resolves means the channel was deleted.
        from_ch: String,
        to_ch: String,
        duration_val: f64,
        duration_unit: crate::channel::DurationUnit,
        easing: String,
        transition_shader: Option<String>,
        target_amount: f32,
    },
    Wait {
        duration_val: f64,
        duration_unit: crate::channel::DurationUnit,
    },
    GoTo {
        step_index: usize,
    },
}

// ── Output Snapshot ─────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct OutputSnapshot {
    pub windows: Vec<OutputWindowSnapshot>,
    pub surfaces: Vec<SurfaceSnapshot>,
    pub monitors: Vec<MonitorSnapshot>,
}

#[derive(Clone, Serialize)]
pub struct OutputWindowSnapshot {
    pub uuid: String,
    pub name: String,
    /// Full output target (carries `audio_device` for ffmpeg-backed outputs).
    pub target: crate::engine::value::render::OutputTarget,
    pub target_label: String,
    pub is_on_display: bool,
    /// Whether a headless output is actively recording/streaming.
    pub is_active: bool,
    pub surface_assignments: Vec<SurfaceAssignmentSnapshot>,
    pub calibration_mode: crate::engine::value::render::CalibrationMode,
    /// Persisted precision and dithering request.
    pub presentation_request: crate::engine::value::render::PresentationRequest,
    /// Runtime format selected by the active output adapter.
    pub resolved_presentation: crate::engine::value::render::ResolvedPresentation,
    /// Live audio passthrough health for an active ffmpeg output (None = video-only).
    pub audio_passthrough: Option<AudioPassthroughSnapshot>,
    /// Live ffmpeg video health (None = this output has no subprocess).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliveryHealthSnapshot>,
    /// What a headless output sends: `Master` or `BusProgram(uuid)`. Absent
    /// for a window, whose picture comes from its surfaces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<crate::engine::value::render::OutputSource>,
    /// The LED screens the output's feed carries, when it is an LED feed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub led: Option<crate::engine::value::led::LedLayout>,
}

#[derive(Clone, Serialize)]
pub struct AudioPassthroughSnapshot {
    /// Selected capture device name.
    pub device: String,
    /// PCM chunks written to ffmpeg so far.
    pub frames_written: u64,
    /// PCM chunks dropped on backpressure.
    pub frames_dropped: u64,
}

/// Video frames offered to an ffmpeg subprocess, split by fate.
#[derive(Clone, Serialize)]
pub struct DeliveryHealthSnapshot {
    /// Frames the writer actually put down the pipe from the renderer.
    pub frames_written: u64,
    /// Frames dropped because the writer channel was full.
    pub frames_dropped: u64,
    /// Repeated frames inserted to cover renderer gaps.
    pub frames_padded: u64,
}

#[derive(Clone, Serialize)]
pub struct SurfaceAssignmentSnapshot {
    pub surface_uuid: String,
    pub surface_name: String,
    pub enabled: bool,
}

#[derive(Clone, Serialize)]
pub struct SurfaceSnapshot {
    pub uuid: String,
    pub name: String,
    pub vertices: Vec<[f32; 2]>,
    pub extra_contours: Vec<Vec<[f32; 2]>>,
    pub source: OutputSource,
    pub content_mapping: ContentMapping,
    pub output_type: SurfaceOutputType,
    pub circle_hint: Option<CircleHint>,
    /// Effective warp (auto-conforming to the shape while `warp_bound`).
    pub warp: Option<crate::engine::value::warp::WarpMode>,
    /// Whether the warp auto-conforms to the surface shape (auto-warp).
    pub warp_bound: bool,
    /// Curve authoring path, when the surface is bezier-edited.
    pub path: Option<SurfacePath>,
    /// Subtractive cut-out holes (8i.7).
    pub holes: Vec<SurfacePath>,
    /// Flattened hole contours (canvas coords), derived from `holes`.
    pub hole_contours: Vec<Vec<[f32; 2]>>,
}

#[derive(Clone, Serialize)]
pub struct MonitorSnapshot {
    /// What outputs address it by.
    pub name: String,
    pub index: usize,
    pub width: u32,
    pub height: u32,
    /// How it is shown: the name given to it, or what the platform calls it,
    /// with its size and rate (see `app::screens::label`).
    pub label: String,
    /// What the platform calls it ("Color LCD").
    pub model: Option<String>,
    /// The name given to it on this computer.
    pub given_name: Option<String>,
    pub refresh_hz: Option<f64>,
}

// ── MIDI Snapshot ───────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct MidiSnapshot {
    pub devices: Vec<MidiDeviceSnapshot>,
    pub mappings: Vec<MidiMappingSnapshot>,
    pub learn_active: bool,
    pub learn_target: Option<String>,
}

#[derive(Clone, Serialize)]
pub struct MidiDeviceSnapshot {
    pub id: crate::midi::DeviceId,
    pub name: String,
    pub enabled: bool,
    pub has_output: bool,
    pub profile: String,
}

#[derive(Clone, Serialize)]
pub struct MidiMappingSnapshot {
    pub key: crate::midi::MidiKey,
    pub key_display: String,
    pub device_name: String,
    pub param_path: String,
}

// ── Camera Snapshot ─────────────────────────────────────────────────

#[derive(Clone, Serialize)]
pub struct CameraSnapshot {
    pub devices: Vec<(String, CameraId)>,
    /// The sizes and rates each camera offers, largest and fastest first.
    pub modes: Vec<(CameraId, Vec<crate::engine::value::camera::CameraMode>)>,
    /// The OS refuses this app the cameras (Camera access off on a Mac).
    pub access_denied: bool,
    /// macOS camera effects on for this app (Portrait, Studio Light, Edge
    /// Light, Background, Reactions gestures): each changes every camera's
    /// picture, on air too.
    pub video_effects: Vec<String>,
}

// ── Depth Sensor Snapshot ───────────────────────────────────────────

/// Per-sensor runtime state for GUI/API/WS consumers. Plain data — no GPU
/// types. See spec/depth-sensors.md.
#[derive(Clone, Serialize)]
pub struct DepthSensorSnapshot {
    /// Detected sensors as `(name, id)`, for the Library panel.
    pub devices: Vec<(String, DepthSensorId)>,
}

/// Per-deck screen-capture controls, for the deck detail panel and the API.
// The flags are independent facts about one capture, not a state machine:
// cursor and Varda-exclusion are user settings, bound and connected are two
// distinct failure modes a performer needs told apart.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct ScreenCaptureDeckSnapshot {
    /// The target this deck captures, in its persisted display form.
    pub target_label: String,
    /// Display targets get the `exclude_varda` toggle; window targets do not.
    pub is_display: bool,
    /// Capture rate as a 0–1 fraction of the 1–120 fps range, matching what
    /// `deck/<uuid>/capture/rate` accepts.
    pub rate_norm: f32,
    /// The same rate in fps, for display.
    pub rate_fps: f32,
    /// Normalized crop as `[x, y, w, h]`.
    pub crop: [f32; 4],
    pub show_cursor: bool,
    pub exclude_varda: bool,
    /// False when a restored scene named a target that is not currently on
    /// screen. The deck keeps its effects and mappings and renders black.
    pub bound: bool,
    /// Whether frames are currently arriving.
    pub connected: bool,
}

/// Per-deck tap controls, for the deck detail panel and the API.
/// See spec/program-tap.md.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct TapDeckSnapshot {
    /// `"master_program"` or `"channel"`.
    pub kind: String,
    /// Channel UUID when `kind` is `"channel"`.
    pub channel_uuid: Option<String>,
    /// Resolved display name for the tap point.
    pub label: String,
    /// False when the tapped channel no longer exists. The deck keeps its
    /// effects and mappings and renders black.
    pub bound: bool,
}

// ── Screen Capture Snapshot ─────────────────────────────────────────

/// One capturable display or window, for the Library panel and the API.
/// Plain data — no platform handles, since the UI addresses targets by name.
/// See spec/screen-capture.md.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct CaptureTargetSnapshot {
    /// `"display"` or `"window"`.
    pub kind: String,
    /// Human-readable name, e.g. `"Display 1"` or `"Ableton Live — Set 3"`.
    pub label: String,
    /// Owning application bundle id or process name. Windows only.
    pub app: Option<String>,
    /// Window title at enumeration time. Windows only.
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
    /// This target is one of Varda's own windows — capturing it is a deliberate
    /// self-capture, which the UI marks so it is an informed choice.
    pub is_varda: bool,
}

/// Screen-capture subsystem state for GUI/API/WS consumers.
#[derive(Clone, Serialize, utoipa::ToSchema)]
pub struct ScreenCaptureSnapshot {
    /// Targets found by the last scan. Manual — never polled.
    pub targets: Vec<CaptureTargetSnapshot>,
    /// `granted` / `denied` / `not_determined` / `not_required`. This is why a
    /// capture deck can be black, so it is reported rather than inferred.
    pub permission: String,
    /// False when built without the `screen-capture` feature or started with
    /// `--no-screen-capture`.
    pub available: bool,
    /// Platform backend in use, e.g. `"ScreenCaptureKit"`.
    pub backend: String,
    /// Number of live capture sessions (one per target, shared by N decks).
    pub active_captures: usize,
}

impl Default for ScreenCaptureSnapshot {
    /// The state a build without a capture backend reports: nothing to scan,
    /// and no permission to ask for.
    fn default() -> Self {
        Self {
            targets: vec![],
            permission: "not_required".into(),
            available: false,
            backend: "none".into(),
            active_captures: 0,
        }
    }
}

// ── Analyzer Snapshot ──────────────────────────────────────────────

/// Info about an available analyzer type (for UI discovery).
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AnalyzerTypeInfo {
    pub analyzer_type: String,
    pub scalar_outputs: Vec<AnalyzerScalarInfo>,
    pub texture_outputs: Vec<String>,
}

/// Info about a scalar output an analyzer produces.
#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AnalyzerScalarInfo {
    pub name: String,
    pub description: String,
    pub range: (f32, f32),
    pub default_smoothing: f32,
}

#[derive(Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunningAnalyzerSnapshot {
    pub analyzer_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── EffectTarget tests ───────────────────────────────────────────

    #[test]
    fn effect_target_deck_equality() {
        let a = EffectTarget::Deck("deck-a".into());
        let b = EffectTarget::Deck("deck-a".into());
        assert_eq!(a, b);
    }

    #[test]
    fn effect_target_deck_inequality() {
        assert_ne!(
            EffectTarget::Deck("deck-a".into()),
            EffectTarget::Deck("deck-b".into())
        );
        assert_ne!(
            EffectTarget::Deck("uuid-1".into()),
            EffectTarget::Channel("uuid-1".into())
        );
        assert_ne!(EffectTarget::Channel("ch-a".into()), EffectTarget::Master);
    }

    #[test]
    fn effect_target_debug() {
        assert!(format!("{:?}", EffectTarget::Master).contains("Master"));
        assert!(format!("{:?}", EffectTarget::Channel("ch-2".into())).contains("ch-2"));
        assert!(format!("{:?}", EffectTarget::Deck("deck-1".into())).contains("deck-1"));
    }

    #[test]
    fn effect_target_clone() {
        let original = EffectTarget::Deck("deck-5".into());
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn effect_target_hash() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(EffectTarget::Master);
        set.insert(EffectTarget::Channel("ch-a".into()));
        set.insert(EffectTarget::Channel("ch-a".into())); // duplicate
        assert_eq!(set.len(), 2);
    }

    // ── Snapshot struct construction ─────────────────────────────────

    #[test]
    fn engine_state_can_be_constructed() {
        let state = EngineState {
            buses: Vec::new(),
            rundown: RundownSnapshot::default(),
            mixer: MixerSnapshot {
                main: crate::mixer::MainSnapshot::default(),
                channels: vec![],
                crossfader: 0.0,
                auto_crossfade_active: false,
                auto_crossfade_progress: 0.0,
                master_effects: vec![],
                active_transition_name: None,
                transition_names: vec![],
                sequences: vec![],
                tonemap_mode: crate::engine::value::render::TonemapMode::default(),
                active_lut: None,
            },
            audio_mix: crate::audio_mix::AudioMixSnapshot::default(),
            stream: crate::streaming::StreamSnapshot::default(),
            record: crate::record::RecordSnapshot::default(),
            audio: AudioSnapshot {
                level: 0.0,
                bass: 0.0,
                mid: 0.0,
                treble: 0.0,
                bpm: None,
                beat_phase: 0.0,
                enabled: false,
                devices: vec![],
                fft: vec![],
                sample_rate: 48000.0,
            },
            modulation: ModulationSnapshot {
                sources: vec![],
                current_values: std::collections::HashMap::default(),
                assignments: std::collections::HashMap::default(),
            },
            outputs: OutputSnapshot {
                windows: vec![],
                surfaces: vec![],
                monitors: vec![],
            },
            registry: RegistrySnapshot {
                generators: vec![],
                filters: vec![],
                shader_count: 0,
            },
            midi: MidiSnapshot {
                devices: vec![],
                mappings: vec![],
                learn_active: false,
                learn_target: None,
            },
            cameras: CameraSnapshot {
                devices: vec![],
                modes: vec![],
                access_denied: false,
                video_effects: Vec::new(),
            },
            depth_sensors: DepthSensorSnapshot { devices: vec![] },
            screen_capture: ScreenCaptureSnapshot::default(),
            transport: TransportSnapshot::default(),
            timecode: TimecodeSnapshot::default(),
            arrangement: None,
            clock: ClockSnapshot {
                bpm: None,
                beat_phase: 0.0,
                source_label: "None".into(),
                device_name: None,
                active: false,
                detected_midi_sources: vec![],
                osc_active: false,
                osc_bpm: None,
                audio_bpm: None,
                preference_label: "Auto".into(),
                preference_force_device_id: None,
                manual_bpm: None,
                beat_followers: 0,
            },
            fps: 60.0,
            frame_count: 0,
            target_fps: 60,
            pacing: crate::engine::PacingSnapshot::default(),
            ndi_sources: vec![],
            ndi_available: false,
            syphon_sources: vec![],
            decklink_devices: vec![],
            syphon_available: false,
            stream_receivers: vec![],
            analyzers: vec![],
            can_undo: false,
            can_redo: false,
            macros: vec![],
        };
        assert!((state.fps - 60.0).abs() < 1e-5);
        assert_eq!(state.frame_count, 0);
    }

    #[test]
    fn engine_state_clone() {
        let state = EngineState {
            buses: Vec::new(),
            rundown: RundownSnapshot::default(),
            mixer: MixerSnapshot {
                main: crate::mixer::MainSnapshot::default(),
                channels: vec![],
                crossfader: 0.5,
                auto_crossfade_active: false,
                auto_crossfade_progress: 0.0,
                master_effects: vec![],
                active_transition_name: None,
                transition_names: vec![],
                sequences: vec![],
                tonemap_mode: crate::engine::value::render::TonemapMode::default(),
                active_lut: None,
            },
            audio_mix: crate::audio_mix::AudioMixSnapshot::default(),
            stream: crate::streaming::StreamSnapshot::default(),
            record: crate::record::RecordSnapshot::default(),
            audio: AudioSnapshot {
                level: 0.0,
                bass: 0.0,
                mid: 0.0,
                treble: 0.0,
                bpm: Some(120.0),
                beat_phase: 0.0,
                enabled: true,
                devices: vec![],
                fft: vec![],
                sample_rate: 48000.0,
            },
            modulation: ModulationSnapshot {
                sources: vec![],
                current_values: std::collections::HashMap::default(),
                assignments: std::collections::HashMap::default(),
            },
            outputs: OutputSnapshot {
                windows: vec![],
                surfaces: vec![],
                monitors: vec![],
            },
            registry: RegistrySnapshot {
                generators: vec![("Sine".into(), 0)],
                filters: vec![],
                shader_count: 1,
            },
            midi: MidiSnapshot {
                devices: vec![],
                mappings: vec![],
                learn_active: false,
                learn_target: None,
            },
            cameras: CameraSnapshot {
                devices: vec![],
                modes: vec![],
                access_denied: false,
                video_effects: Vec::new(),
            },
            depth_sensors: DepthSensorSnapshot { devices: vec![] },
            screen_capture: ScreenCaptureSnapshot::default(),
            transport: TransportSnapshot::default(),
            timecode: TimecodeSnapshot::default(),
            arrangement: None,
            clock: ClockSnapshot {
                bpm: Some(120.0),
                beat_phase: 0.0,
                source_label: "Audio".into(),
                device_name: None,
                active: true,
                detected_midi_sources: vec![],
                osc_active: false,
                osc_bpm: None,
                audio_bpm: Some(120.0),
                preference_label: "Auto".into(),
                preference_force_device_id: None,
                manual_bpm: None,
                beat_followers: 0,
            },
            fps: 59.9,
            frame_count: 42,
            target_fps: 60,
            pacing: crate::engine::PacingSnapshot::default(),
            ndi_sources: vec![],
            ndi_available: false,
            syphon_sources: vec![],
            decklink_devices: vec![],
            syphon_available: false,
            stream_receivers: vec![],
            analyzers: vec![],
            can_undo: false,
            can_redo: false,
            macros: vec![],
        };
        let cloned = state.clone();
        assert!((cloned.mixer.crossfader - 0.5).abs() < 1e-5);
        assert_eq!(cloned.audio.bpm, Some(120.0));
        assert_eq!(cloned.registry.shader_count, 1);
        assert_eq!(cloned.frame_count, 42);
    }

    // ── EngineCommand construction ───────────────────────────────────

    #[test]
    fn engine_command_debug() {
        let cmd = crate::engine::EngineCommand::SetCrossfader(0.5);
        assert!(format!("{cmd:?}").contains("SetCrossfader"));
    }

    #[test]
    fn engine_command_add_deck() {
        let cmd = crate::engine::EngineCommand::AddDeck {
            channel_uuid: "ch-0".into(),
            shader_name: "Color Bars".into(),
        };
        match cmd {
            crate::engine::EngineCommand::AddDeck {
                channel_uuid,
                shader_name,
            } => {
                assert_eq!(channel_uuid, "ch-0");
                assert_eq!(shader_name, "Color Bars");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn engine_command_set_param() {
        let cmd = crate::engine::EngineCommand::SetParam {
            path: "ch0:deck0:brightness".into(),
            value: ParamValue::Float(0.8),
        };
        match cmd {
            crate::engine::EngineCommand::SetParam { path, value } => {
                assert_eq!(path, "ch0:deck0:brightness");
                match value {
                    ParamValue::Float(v) => assert!((v - 0.8).abs() < 1e-5),
                    _ => panic!("Expected Float"),
                }
            }
            _ => panic!("Wrong variant"),
        }
    }

    // ── Snapshot field access ────────────────────────────────────────

    #[test]
    fn channel_snapshot_fields() {
        let ch = ChannelSnapshot {
            overlay: false,
            idx: 0,
            uuid: "test0001".into(),
            name: "Ch 0".into(),
            opacity: 0.75,
            blend_mode: BlendMode::Add,
            decks: vec![],
            effects: vec![],
            render_time_ms: 1.5,
            active_deck_count: 2,
        };
        assert_eq!(ch.idx, 0);
        assert!((ch.opacity - 0.75).abs() < 1e-5);
        assert_eq!(ch.blend_mode, BlendMode::Add);
        assert!((ch.render_time_ms - 1.5).abs() < 1e-5);
        assert_eq!(ch.active_deck_count, 2);
    }

    #[test]
    fn deck_snapshot_fields() {
        let d = DeckSnapshot {
            idx: 0,
            uuid: "test0002".into(),
            name: "Sine Wave".into(),
            is_html: false,
            is_html_interactive: false,
            is_depth_sensor: true,
            point_cloud_params: Some(PointCloudParamsSnapshot {
                orbit_yaw: 0.5,
                orbit_pitch: 0.5,
                zoom: 0.25,
                point_size: 0.1,
                depth_min: 0.05,
                depth_max: 0.5,
                seed: 0.0,
                drift: 0.25,
                disruption: 0.75,
                color_mode: 1,
            }),
            has_depth_prepro: true,
            depth_prepro_params: Some(DepthPreproParamsSnapshot {
                sensor_name: "Kinect".into(),
                near: 0.0625,
                far: 0.5,
                smoothing: 0.5,
                hole_fill: 0.5,
                mask_feather: 0.375,
                motion_gain: 0.4,
                mirror: true,
            }),
            screen_capture: None,
            tap: None,
            playlist: None,
            timer: None,
            camera_colour: None,
            camera_profile: None,
            camera_lowest_latency: None,
            opacity: 1.0,
            effective_opacity: 0.5,
            blend_mode: BlendMode::Normal,
            solo: false,
            mute: true,
            transparent: false,
            scaling_mode: Some(ScalingMode::default()),
            source_size: None,
            solid_color: None,
            generator: ShaderParamsSnapshot {
                shader_name: "Sine".into(),
                params: vec![],
            },
            effects: vec![],
            video_playback: None,
            auto_transition: None,
            render_fps: DeckRenderFps::Auto,
            render_scale: DeckRenderScale::default(),
            effective_render_fps: 0.0,
            render_cost_us: 0.0,
            gpu_render_cost_us: 0.0,
            fps: 59.5,
            still: false,
            source_asleep: false,
            running_analyzers: vec![],
        };
        assert!(d.mute);
        assert!(!d.solo);
        assert!((d.effective_opacity - 0.5).abs() < 1e-5);
        assert!((d.fps - 59.5).abs() < 1e-5);
        let prepro = d.depth_prepro_params.expect("preprocessor params present");
        assert_eq!(prepro.sensor_name, "Kinect");
        assert!(prepro.mirror);
        assert!((prepro.far - 0.5).abs() < 1e-5);
        let pc = d.point_cloud_params.expect("point-cloud params present");
        assert_eq!(pc.color_mode, 1);
        assert!((pc.disruption - 0.75).abs() < 1e-5);
    }
}
