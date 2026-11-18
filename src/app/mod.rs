//! Application layer — concrete engine implementation.
//!
//! `VardaApp` owns all engine subsystems (Mixer, Audio, Cameras, MIDI, OSC,
//! `ShaderRegistry`, `SurfaceManager`) and implements the engine traits.
//!
//! The main.rs `App` struct owns window/egui state and holds a `VardaApp`.

mod actions;
pub(crate) mod app_id;
mod bus_dissolve;
mod bus_overlay;
mod commands;
mod engine_impl;
pub(crate) mod frame_parts;
pub(crate) mod history;
mod inputs;
/// Interactive mode for HTML decks (feature `html`). See /spec/html-source.md §4.
#[cfg(feature = "html")]
pub(crate) mod interactive;
mod keep_awake;
pub mod log_file;
mod media_adds;
pub(crate) mod monitors;
mod outputs;
pub(crate) mod render;
mod render_hint;
pub use render_hint::RenderHint;
pub mod shows;
pub use render::{DeckFile, DeckLoadResult, DeckLoadToken};
pub(crate) mod resolve;
pub mod screens;
mod snapshot;
pub(crate) mod state;
mod surfaces;
pub mod tool_path;
mod workspace;

pub use workspace::WorkspaceLoad;

/// Default render resolution for all decks and stage output (Full HD 1080p)
pub const DEFAULT_RENDER_WIDTH: u32 = 1920;
/// Default render resolution for all decks and stage output (Full HD 1080p)
pub const DEFAULT_RENDER_HEIGHT: u32 = 1080;

/// Clamp a requested render resolution to what the GPU can allocate.
///
/// Varda imposes no artificial resolution cap; the only bound is the GPU's
/// `max_texture_dimension_2d` (spec/resolution-and-scaling.md, "No Maximum
/// Resolution"). Each dimension is independently clamped to `max_dim`. Zero is
/// left untouched — callers reject it separately so this stays a pure clamp.
pub(crate) fn clamp_resolution_to_gpu(width: u32, height: u32, max_dim: u32) -> (u32, u32) {
    (width.min(max_dim), height.min(max_dim))
}

/// Session configuration derived from CLI flags + workspace defaults.
/// CLI flags override persisted config for the session without modifying files.
// CLI flag struct: each bool is an independent `--flag`, not a state enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, clap::Parser)]
#[command(name = "varda", version, about = "Live visuals engine")]
pub struct AppConfig {
    /// Run without main UI window (API-only control)
    #[arg(long)]
    pub headless: bool,

    /// Measure the Mac's media engine — one and two 4K encoders at once —
    /// print what it found and quit. Keeps the engine busy for about a
    /// minute and a half: run it with nothing on air.
    #[arg(long = "bench-encoders", hide = true)]
    pub bench_encoders: bool,

    /// Measure how late a Blackmagic box's sound comes through macOS (its
    /// sound device) against its sound through Blackmagic's own API — the
    /// box named like this — print it and quit. Twelve seconds, sound going
    /// into the box, no input on it open.
    #[arg(long = "blackmagic-sound-lag", hide = true, value_name = "BOX")]
    pub blackmagic_sound_lag: Option<String>,

    /// HTTP API port (or `VARDA_PORT`, which an app bundle can set)
    #[arg(long = "port", env = "VARDA_PORT", default_value_t = 8080)]
    pub api_port: u16,

    /// Target render FPS (default: 60, 0 = uncapped)
    #[arg(long = "fps", default_value_t = 60)]
    pub target_fps: u32,

    /// Workspace root directory (default: current working directory; or
    /// `VARDA_WORKSPACE`)
    #[arg(long = "workspace", env = "VARDA_WORKSPACE")]
    pub workspace_root: Option<std::path::PathBuf>,

    /// Scene file to load (overrides workspace default)
    #[arg(long = "scene")]
    pub scene_path: Option<std::path::PathBuf>,

    /// Stage file to load (overrides workspace default)
    #[arg(long = "stage")]
    pub stage_path: Option<std::path::PathBuf>,

    /// OSC input port (overrides osc.json config)
    #[arg(long = "osc-port")]
    pub osc_port: Option<u16>,

    /// OSC feedback target host:port (repeatable)
    #[arg(long = "osc-out")]
    pub osc_targets: Vec<String>,

    /// Disable OSC input entirely
    #[arg(long = "no-osc")]
    pub osc_disabled: bool,

    /// Disable NDI discovery and sending
    #[arg(long = "no-ndi")]
    pub ndi_disabled: bool,

    /// Disable Syphon (macOS only)
    #[arg(long = "no-syphon")]
    pub syphon_disabled: bool,

    /// Disable HTML deck sources (skips Servo rendering)
    #[arg(long = "no-html")]
    pub html_disabled: bool,

    /// Disable screen / window capture deck sources (skips OS capture entirely,
    /// so no Screen Recording permission is ever requested)
    #[arg(long = "no-screen-capture")]
    pub screen_capture_disabled: bool,

    /// Disable PowerPoint inputs (never opens or drives PowerPoint, so the
    /// Automation permission is never asked for)
    #[arg(long = "no-powerpoint")]
    pub powerpoint_disabled: bool,

    /// Disable camera deck sources (skips device enumeration entirely, so no
    /// Camera permission is ever requested)
    ///
    /// Enumeration is what raises the OS permission dialog, and it happens when
    /// the app is constructed — before any camera is opened, and whether or not
    /// a camera is ever used. On macOS an unanswered dialog does not block the
    /// next one, so anything that constructs the app repeatedly (a test suite,
    /// a batch of headless runs) stacks them, and a backlog of unanswered TCC
    /// prompts can stop unrelated permission requests from being presented at
    /// all. Every other source already has an off switch; this completes the
    /// set.
    #[arg(long = "no-camera")]
    pub camera_disabled: bool,

    /// Additional shader library directory (repeatable). Scanned and
    /// hot-reloaded alongside the built-in locations. Added last, so a
    /// shader here overrides a built-in shader of the same name — useful
    /// for pointing at show- or rig-specific shader folders in the field.
    /// `VARDA_SHADER_DIRS` gives them too, separated by `:`.
    #[arg(long = "shader-dir", env = "VARDA_SHADER_DIRS", value_delimiter = ':')]
    pub shader_dirs: Vec<std::path::PathBuf>,
}

impl AppConfig {
    /// Resolve workspace root with three-tier fallback:
    /// 1. Explicit `--workspace` flag (highest priority)
    /// 2. CWD if it contains a `.varda/` directory (project-local workspace)
    /// 3. Home directory as root → uses `~/.varda/` (default workspace)
    pub fn effective_workspace_root(&self) -> std::path::PathBuf {
        Self::resolve_workspace_root(
            self.workspace_root.as_deref(),
            std::env::current_dir().ok().as_deref(),
            dirs::home_dir().as_deref(),
        )
    }

    /// Pure workspace resolution — testable without touching environment.
    fn resolve_workspace_root(
        explicit: Option<&std::path::Path>,
        cwd: Option<&std::path::Path>,
        home: Option<&std::path::Path>,
    ) -> std::path::PathBuf {
        // Tier 1: explicit --workspace flag
        if let Some(ws) = explicit {
            return ws.to_path_buf();
        }
        // Tier 2: CWD has .varda/
        if let Some(cwd) = cwd {
            if cwd.join(".varda").is_dir() {
                return cwd.to_path_buf();
            }
        }
        // Tier 3: home directory (workspace data lives at ~/.varda/)
        if let Some(home) = home {
            return home.to_path_buf();
        }
        // Ultimate fallback
        std::path::PathBuf::from(".")
    }
}

use crate::audio::AudioManager;
use crate::camera::CameraManager;
use crate::depth::DepthSensorManager;
use crate::keymap::KeymapStore;
use crate::midi;
use crate::mixer::Mixer;
use crate::notifications::NotificationSystem;
use crate::osc::{OscConfig, OscFeedbackSender, OscReceiver};
use crate::persistence::Workspace;
use crate::registry::ShaderRegistry;
use crate::renderer::context::{GpuContext, UnifiedOutput};
use crate::screen_capture::ScreenCaptureManager;
use crate::surface::SurfaceManager;

use crate::engine::{CommandEnvelope, CommandResult, EngineState, ErrorCode};

/// Where a command's answer goes: an HTTP request waiting, say.
pub(crate) type Reply = tokio::sync::oneshot::Sender<CommandResult>;

// ── Domain sub-structs ──────────────────────────────────────────

/// Input subsystem: OSC, MIDI, keyboard shortcuts, clock.
pub(crate) struct InputSubsystem {
    pub osc_receiver: Option<OscReceiver>,
    pub osc_feedback: Option<OscFeedbackSender>,
    pub osc_config: OscConfig,
    pub midi_devices: Option<midi::MidiDeviceManager>,
    pub midi_mappings: midi::MidiMappingStore,
    pub controller_led_mgr: midi::ControllerLedManager,
    pub auto_map_engine: midi::AutoMapEngine,
    pub keymap: KeymapStore,
    pub clock_manager: crate::clock::ClockManager,
    /// Absolute position from an external master. Distinct from the clock
    /// beside it, which resolves tempo; see /spec/timecode.md § Why This Is Not
    /// the Clock Either.
    pub timecode: crate::timecode::TimecodeManager,
    /// The PCM tap LTC is decoded from, while one is patched.
    pub ltc_tap: Option<LtcTap>,
    /// The source whose device went (unplugged) while LTC was read from it,
    /// and when it was last looked for: taken again once it is back, the
    /// patch kept.
    pub ltc_lost: Option<(crate::audio::AudioSourceId, std::time::Instant)>,
    /// The source being opened for LTC, if one is: taken once it is open
    /// (see `App::tick_ltc`).
    pub ltc_opening: Option<crate::audio::AudioSourceId>,
}

/// A live subscription to the raw PCM of the audio input carrying LTC.
///
/// Rides the same tee as the recording path, so listening for timecode costs
/// no change to the capture callback. See /spec/audio-passthrough.md.
pub(crate) struct LtcTap {
    pub source_id: crate::audio::AudioSourceId,
    pub token: crate::audio::PcmToken,
    pub receiver: crossbeam_channel::Receiver<crate::audio::PcmChunk>,
    pub sample_rate: u32,
    pub channels: u16,
}

/// Output and surface subsystem: windows, headless outputs, surface layout, dome.
pub(crate) struct OutputSubsystem {
    pub outputs: Vec<UnifiedOutput>,
    pub surface_manager: SurfaceManager,
    pub calibration_textures: Vec<(wgpu::Texture, wgpu::TextureView)>,
    /// Test cards of the LED feeds showing them.
    pub led_cards: crate::renderer::led_card::LedCards,
    /// Test cards of the outputs showing theirs (calibration `Projector`),
    /// each at its own pixels.
    pub output_cards: crate::renderer::led_card::OutputCards,
    pub domemaster: Option<crate::renderer::dome::DomemasterRenderer>,
    /// Size the domemaster renderer is built at. Held alongside the renderer
    /// rather than inside it because it outlives it: the setting is restored
    /// from the stage before any dome surface exists, and `ensure_domemaster`
    /// builds at whatever it says at the time.
    pub domemaster_resolution: crate::renderer::dome::DomemasterResolution,
    pub pending_output_creates: Vec<crate::scene::OutputConfig>,
    pub cached_monitors: Vec<(String, winit::monitor::MonitorHandle)>,
    /// Each of `cached_monitors`' size in pixels, read with it on the main
    /// thread: asked from another, macOS answers through the main thread.
    pub monitor_sizes: Vec<winit::dpi::PhysicalSize<u32>>,
    /// What else is known of each of `cached_monitors`, read with it (see
    /// `screens`) — again only when the screens change, or a second on.
    pub screen_facts: Vec<screens::ScreenFacts>,
    /// The screens, by name and place, `screen_facts` were read for, and when.
    screen_facts_for: Vec<(String, (i32, i32))>,
    screen_facts_at: Option<std::time::Instant>,
    /// The names given to screens on this computer.
    pub screen_names: screens::ScreenNames,
    /// Render targets for buses fading between two pictures.
    pub bus_dissolves: bus_dissolve::BusDissolves,
    /// Lower thirds drawn over the buses they are sent to.
    pub bus_overlays: bus_overlay::BusOverlays,
    /// Keeps the computer and displays awake while Varda runs.
    keep_awake: keep_awake::KeepAwake,
    /// The screen the control window is on, by monitor name.
    control_screen: Option<String>,
    /// When output screens' refresh rates are next checked against the show's.
    rate_check_at: Option<std::time::Instant>,
    /// (output, screen millihertz, show fps) already warned about.
    rate_warned: std::collections::HashSet<(String, u32, u32)>,
    /// Output windows made again because their screen stopped showing their
    /// frames, by output uuid (see `reopen_unshown_outputs`).
    reopened: std::collections::HashMap<String, outputs::Reopened>,
    /// When the Blackmagic devices connected were last looked at, and which
    /// they were (see `poll_blackmagic_outputs`).
    blackmagic_looked: Option<std::time::Instant>,
    blackmagic_seen: Option<Vec<String>>,
}

/// External I/O managers: NDI, Syphon, SRT/HLS/DASH/RTMP streams.
pub(crate) struct ExternalIO {
    pub ndi_manager: crate::ndi::NdiManager,
    #[cfg(target_os = "macos")]
    pub syphon_manager: crate::syphon::SyphonManager,
    /// Syphon decks restored from the workspace whose server is not yet
    /// published. The render thread auto-binds them once the server appears
    /// (`VardaApp::reconcile_syphon`). See `persistence::PendingSyphonDeck`.
    #[cfg(target_os = "macos")]
    pub pending_syphon: Vec<crate::persistence::PendingSyphonDeck>,
    /// Throttle for periodic Syphon re-discovery on the render thread.
    #[cfg(target_os = "macos")]
    pub last_syphon_scan: std::time::Instant,
    pub stream_manager: crate::stream::StreamManager,
    pub stream_library: Vec<(String, crate::stream::SrtMode)>,
    pub hls_library: Vec<String>,
    pub dash_library: Vec<String>,
    pub rtmp_library: Vec<(String, crate::stream::RtmpMode)>,
    pub html_manager: crate::html::HtmlManager,
    pub html_library: Vec<String>,
}

/// Frame timing and system monitoring.
pub(crate) struct FrameStats {
    pub last_frame_instant: std::time::Instant,
    pub fps_history: std::collections::VecDeque<f32>,
    pub fps_smoothed: f32,
    pub frame_count: u64,
    pub system_monitor: crate::sysmon::SystemMonitor,
    /// Command buffer commits issued during the previous frame, sampled once
    /// per frame in `update_frame_timing`. Commits are not free — see
    /// `renderer::submit_stats`.
    pub last_frame_submits: u32,
    /// When the process started, the origin of [`FrameStats::frame_time_secs`].
    pub start_instant: std::time::Instant,
    /// Monotonic frame clock: seconds since `start_instant`, sampled once at
    /// the top of each frame.
    ///
    /// Time-based engine state reads this rather than a per-frame delta.
    /// Commands run at different points relative to the frame tick — queued API
    /// commands before it, native UI commands after — so a delta cannot be
    /// attributed correctly without knowing which side a command landed on.
    /// A shared timestamp needs no such knowledge.
    pub frame_time_secs: f64,
    /// Milliseconds between the starts of the last frames, the latest last.
    pub intervals: std::collections::VecDeque<f32>,
    /// What starts frames: a screen's refresh, or the timer.
    pub clock: String,
    /// The screen whose refresh starts frames, when one does: outputs on it
    /// present every frame.
    pub clock_display: Option<u32>,
    /// Where each frame's time goes (see [`frame_parts`]).
    pub parts: frame_parts::FrameParts,
    /// An output window showed the last frame: its screen's refresh can
    /// hold the loop back when frames are drawn ahead.
    pub windows_presented: bool,
    /// When the screen's refresh that called for this frame came, when a
    /// refresh starts frames (see `FrameTiming::refresh_at`).
    pub refresh_at: Option<std::time::Instant>,
    /// When the frame being drawn was drawn, for the recording and stream
    /// to time it by: set as the mixer draws it — which takes
    /// `refresh_at` — and taken by the outputs (see `VardaApp::frame_drawn`).
    pub drawn_at: Option<std::time::Instant>,
    /// The display link's figures, from the frame loop.
    pub display_link: Option<crate::engine::DisplayLinkSnapshot>,
    /// How evenly the control window's frames reached its screen, from the
    /// frame loop.
    pub control_on_screen: crate::engine::OnScreenStats,
    /// The control window's queue, from the frame loop: frames waiting to be
    /// shown (most over the last ten frames).
    pub control_queue: crate::engine::ControlQueueStats,
    /// Where the control window's frames' time goes, and its slow frames.
    pub control_parts: Vec<crate::engine::PartStats>,
    pub control_slow_frames: Vec<crate::engine::SlowFrame>,
    /// How the control window's monitors stepped through the show's frames.
    pub control_monitors: crate::engine::ControlMonitorsStats,
    /// How often the control window is drawn, and why.
    pub control_rate: crate::engine::ControlRateStats,
    /// Where the GPU's time goes, by part of the app, a second at a time.
    pub gpu_time: crate::renderer::gpu_time::GpuTime,
}

/// Frame intervals the performance figures cover: five seconds at 60.
pub(crate) const PACING_FRAMES: usize = 300;

/// What a bus is putting on air this frame, and which part of the master it is.
///
/// `source` is the composited image the bus's program resolves to; `uv_scale`
/// and `uv_offset` select the bus's region out of it. A destination following
/// the bus uses this mapping instead of its own, so the operator configures the
/// crop once on the bus rather than on every destination that shows it.
#[derive(Debug, Clone)]
pub struct BusFeed {
    /// What the bus has on air.
    pub picture: FeedPicture,
    /// UV scale selecting the bus's region of that source.
    pub uv_scale: [f32; 2],
    /// UV offset selecting the bus's region of that source.
    pub uv_offset: [f32; 2],
}

/// The picture a bus puts on air this frame.
#[derive(Debug, Clone)]
pub enum FeedPicture {
    /// A composited source, shown as it is.
    Source(crate::engine::value::render::OutputSource),
    /// An `AUTO` in flight: `from` dissolving into `to` by the eased `mix`
    /// (0.0 = all `from`). `None` is black.
    Dissolve {
        from: Option<crate::engine::value::render::OutputSource>,
        to: Option<crate::engine::value::render::OutputSource>,
        mix: f32,
    },
    /// That dissolve, rendered this frame into the bus's own target.
    Rendered(wgpu::TextureView),
}

impl FeedPicture {
    /// The concrete sources this picture reads.
    pub fn sources(&self) -> impl Iterator<Item = &crate::engine::value::render::OutputSource> {
        let (a, b) = match self {
            Self::Source(source) => (Some(source), None),
            Self::Dissolve { from, to, .. } => (from.as_ref(), to.as_ref()),
            Self::Rendered(_) => (None, None),
        };
        a.into_iter().chain(b)
    }
}

/// The external sessions a deck holds open.
///
/// Read out of a deck before its owner is dropped, so the release can run
/// without holding a borrow on the mixer. Dropping a `DeckSlot` frees its GPU
/// textures, but camera, depth-sensor, screen-capture, SRT, NDI and Syphon
/// sessions belong to managers outside the deck: without this, a capture thread
/// and its USB handle outlive the last deck that used them.
pub(crate) struct HeldResources {
    camera: Option<crate::camera::CameraId>,
    depth: Vec<crate::depth::DepthSensorId>,
    screen_capture: Option<crate::screen_capture::CaptureId>,
    srt: Option<usize>,
    ndi: Option<usize>,
    #[cfg(target_os = "macos")]
    syphon: Option<usize>,
}

/// What a set of live decks still holds, for holder reconciliation.
///
/// Only the shared-uncounted kinds are tracked: see [`HeldResources::minus`]
/// for why reference-counted devices must not be filtered this way.
#[derive(Default)]
pub(crate) struct HeldSet {
    srt: std::collections::HashSet<usize>,
}

#[cfg(test)]
impl HeldResources {
    /// Build a holding set directly, for policy tests.
    pub(crate) fn for_test(
        camera: Option<crate::camera::CameraId>,
        depth: Vec<crate::depth::DepthSensorId>,
        screen_capture: Option<crate::screen_capture::CaptureId>,
        srt: Option<usize>,
        ndi: Option<usize>,
    ) -> Self {
        Self {
            camera,
            depth,
            screen_capture,
            srt,
            ndi,
            #[cfg(target_os = "macos")]
            syphon: None,
        }
    }

    pub(crate) fn clone_for_test(&self) -> Self {
        Self::for_test(
            self.camera,
            self.depth.clone(),
            self.screen_capture,
            self.srt,
            self.ndi,
        )
    }

    pub(crate) fn camera_for_test(&self) -> Option<crate::camera::CameraId> {
        self.camera
    }
    pub(crate) fn depth_for_test(&self) -> Vec<crate::depth::DepthSensorId> {
        self.depth.clone()
    }
    pub(crate) fn screen_capture_for_test(&self) -> Option<crate::screen_capture::CaptureId> {
        self.screen_capture
    }
    pub(crate) fn srt_for_test(&self) -> Option<usize> {
        self.srt
    }
    pub(crate) fn ndi_for_test(&self) -> Option<usize> {
        self.ndi
    }
}

impl HeldResources {
    /// Everything held across `all`.
    pub(crate) fn union(all: &[HeldResources]) -> HeldSet {
        let mut set = HeldSet::default();
        for h in all {
            set.srt.extend(h.srt);
        }
        set
    }

    /// This deck's holdings, with sessions a survivor still owns removed.
    ///
    /// The policy differs by manager, and getting it uniform in either
    /// direction is a bug:
    ///
    /// - **Cameras and depth sensors are reference counted** — every
    ///   acquisition increments, so every retiring holder must release exactly
    ///   once. Suppressing a release because a survivor holds the same device
    ///   leaves the count permanently high and the capture thread running
    ///   forever. These pass through untouched.
    /// - **Stream receivers are shared and uncounted** —
    ///   `StreamManager::start_receive` hands back the index of an existing
    ///   receiver for the same URL, and `stop_receive` stops it globally. Only
    ///   this one is suppressed when a survivor still holds it; stopping it
    ///   would freeze an on-air deck.
    /// - **NDI and Syphon create a fresh receiver per deck**, so an index has
    ///   exactly one holder and there is nothing to suppress.
    /// - **Screen capture** is reference counted like cameras, so it passes
    ///   through and is released here too. It additionally has a per-frame
    ///   `reconcile_holders` pass, which repairs any holder it misses.
    pub(crate) fn minus(self, keep: &HeldSet) -> Self {
        Self {
            camera: self.camera,
            depth: self.depth,
            screen_capture: self.screen_capture,
            srt: self.srt.filter(|idx| !keep.srt.contains(idx)),
            ndi: self.ndi,
            #[cfg(target_os = "macos")]
            syphon: self.syphon,
        }
    }

    /// Snapshot what this deck currently holds.
    pub(crate) fn of(deck: &crate::deck::Deck) -> Self {
        Self {
            camera: deck.camera_id(),
            // Depth sensors were once missing from teardown, so the capture
            // thread and USB handle outlived the last deck using them. Covers
            // both point-cloud sources and shader preprocessors.
            // See spec/depth-sensors.md § Known defect.
            depth: deck.held_depth_sensors(),
            screen_capture: deck.screen_capture_id(),
            srt: deck.srt_receiver_idx(),
            ndi: deck.ndi_receiver_idx(),
            #[cfg(target_os = "macos")]
            syphon: deck.syphon_client_idx(),
        }
    }
}

impl FrameStats {
    /// The monotonic clock right now, in seconds since `start_instant`.
    ///
    /// Distinct from [`FrameStats::frame_time_secs`], which is the reading
    /// taken at the top of the current frame. Anything that starts or advances
    /// timed state must read live: commands execute at different points
    /// relative to the frame tick, and a cached reading would charge a take for
    /// time that elapsed before it began.
    pub fn now_secs(&self) -> f64 {
        self.start_instant.elapsed().as_secs_f64()
    }
}

/// Session persistence: workspace, presets, undo/redo, notifications.
pub(crate) struct SessionState {
    pub workspace: Workspace,
    pub preset_library: crate::persistence::presets::PresetLibrary,
    pub history: history::HistoryManager,
    pub notifications: NotificationSystem,
    /// Last UI layout the engine has seen, from either a load or a GUI-driven
    /// save. `EngineCommand::SaveWorkspace` has no layout of its own to pass, so
    /// without this a save from the API or a headless process would overwrite
    /// `stage.json` with default panel state.
    pub last_layout: crate::usecases::ui::UILayoutState,
    /// What copy is holding, as configs rather than live objects. Session state:
    /// it survives no further than the process. See /spec/clipboard.md.
    pub clipboard: Option<state::clipboard::ClipboardPayload>,
    /// Where the last cue jump left the playhead, so repeated presses of an
    /// arrow walk the list instead of returning to the same cue while playback
    /// carries the position past it. Cleared by any other move of the playhead.
    /// See /spec/arrangement.md § Cue points.
    pub cue_anchor: Option<f64>,
    /// When the transport last had timecode to chase, so silence can be
    /// reported once rather than every frame of it.
    pub chase_silent_since: Option<std::time::Instant>,
    pub chase_silence_reported: bool,
    /// Last position republished over OSC, so the show is sent at the rate
    /// frames exist rather than at the rate the renderer runs.
    pub published_timecode: Option<String>,
    /// Whether live parameter writes are being kept as automation, and what is
    /// being written right now. Session state: an arm is a mode the performer
    /// is in. See /spec/automation-recording.md.
    pub recorder: state::recorder::Recorder,
    /// Show files that did not load and of which no copy could be kept:
    /// not written over by a save (see `VardaApp::keep_unloaded`).
    pub unwritable: std::collections::HashSet<std::path::PathBuf>,
}

/// Cross-thread message passing.
pub(crate) struct MessageBus {
    pub command_rx: tokio::sync::mpsc::UnboundedReceiver<CommandEnvelope>,
    pub command_tx: tokio::sync::mpsc::UnboundedSender<CommandEnvelope>,
    pub state_tx: std::sync::Arc<crate::engine::types::PublishedState>,
}

// ── Main application struct ─────────────────────────────────────

/// Core engine application. Owns all subsystems except window/egui.
///
/// Implements all engine traits (`MixerCommands`, `AudioCommands`, etc.)
/// for direct same-thread access. Also processes `EngineCommands` from
/// cross-thread consumers via mpsc channel.
// Aggregate root: the flags are unrelated subsystem toggles, not a state enum.
#[allow(clippy::struct_excessive_bools)]
pub struct VardaApp {
    // ── Engine core ──────────────────────────────────────────────
    mixer: Mixer,
    /// Independent production buses (PVW/PGM switching per destination).
    ///
    /// Named in full to keep it distinct from `bus`, which is the cross-thread
    /// `MessageBus` and an entirely unrelated concept.
    production_buses: crate::bus::BusBank,
    /// The show in order, as steps taken with one press (see
    /// `state/rundown.rs`).
    rundown: crate::rundown::Rundown,
    /// Clips the rundown step taken last paused or stopped — by deck UUID,
    /// with the decoder they had then and the on-air updates they are still
    /// held for: Auto Play does not start them as that step brings them on
    /// air (see `state/on_air.rs`, `state/rundown.rs`).
    clips_held: Vec<(String, usize, u8)>,
    /// Clips and playlists a rundown step is fading out — their sound — to
    /// stop or pause when the fade is done (see `state/rundown.rs`).
    rundown_fades: Vec<state::rundown::SoundFade>,
    /// The clips and playlists the step taken last plays till the next
    /// step, as it was taken: kept should that step be removed before then.
    rundown_till_next: Vec<String>,
    /// The time of day the rundown last looked at, seconds after local
    /// midnight, with the step GO took next then: a step's time is due as
    /// the clock comes to it while it is next.
    rundown_day_was: Option<(f64, Option<String>)>,
    /// Writes the show's files on a thread of their own, so the outputs never
    /// wait for the disk (see `persistence::writer`).
    show_writer: crate::persistence::writer::ShowWriter,
    /// Clips and pictures asked for over the bus, being made on threads of
    /// their own (see `media_adds`).
    media_adds: media_adds::MediaAdds,
    /// Moves on each time another show replaces the mixer: what was asked
    /// of the one before is not put into this one.
    show_generation: u64,
    /// A Save As on its way: the show moves once its copy has landed.
    saving_as: Option<shows::SavingAs>,
    /// Presets whose files are on their way, by the writer's number (see
    /// `state/presets.rs`).
    presets_on_way: Vec<(u64, state::presets::PresetOnWay)>,
    audio_manager: AudioManager,
    /// The audio mixer: clips and inputs on strips, sent to buses that play on
    /// sound devices and feed recordings and streams.
    audio_mix: crate::audio_mix::AudioEngine,
    /// Which clips were on air last frame, by deck UUID, with their decode
    /// thread's identity, to see them go on and off (see `state::on_air`).
    clips_on_air: std::collections::HashMap<String, (bool, usize)>,
    /// Whether sound devices have been listed. Done once a real event loop
    /// runs, not in every test engine.
    audio_devices_listed: bool,
    /// The sound inputs' listing, when it had not finished as the app
    /// started (see `AudioManager::new_waiting_at_most`).
    audio_inputs_pending: Option<crate::audio::InputListing>,
    /// When the sound inputs are to be listed again, the devices having
    /// changed (see `audio::device_watch`).
    audio_rescan_due: Option<std::time::Instant>,
    /// The sound inputs being listed again.
    audio_inputs_rescan: Option<crate::audio::InputListing>,
    /// When ATEM Setup was last looked for (see `state::atem`).
    atem_checked: Option<std::time::Instant>,
    /// The stream: settings, and its encoder and destinations while on.
    streaming: state::streaming::Streaming,
    /// Recording: settings, and its encoder while recording.
    recording: state::record::Recording,
    camera_manager: CameraManager,
    /// Each camera's frame rate watched (see `camera::RateWatch`), and when
    /// it was last looked at.
    camera_rates: std::collections::HashMap<crate::camera::CameraId, crate::camera::RateWatch>,
    camera_rates_at: Option<std::time::Instant>,
    /// Depth-sensor capture manager (Kinect/LIDAR point-cloud sources).
    depth_manager: DepthSensorManager,
    /// Screen / window capture manager. See spec/screen-capture.md.
    screen_capture_manager: ScreenCaptureManager,
    /// PowerPoint inputs' worker, started with the first of them.
    presentations: state::presentations::Presentations,
    /// Slides inputs' web talks: `WebKit`, made with the first of them.
    web_talks: state::slides::WebTalks,
    /// Talks' files looked over for what may go wrong in the show.
    talk_checks: crate::presentation::check::Checker,
    registry: ShaderRegistry,
    registry_snapshot: std::cell::RefCell<Option<(u64, crate::engine::RegistrySnapshot)>>,
    analyzer_registry: crate::analyzer::AnalyzerRegistry,
    context: GpuContext,
    /// Absolute show position. Distinct from the tempo clock in
    /// `input.clock_manager`; see /spec/transport.md.
    pub(crate) transport: crate::transport::Transport,
    /// Edge-detect for the arrangement blackout notice, so a deliberate
    /// blackout reports once rather than every frame it lasts.
    pub(crate) arrangement_blackout_reported: bool,

    // ── Domain sub-structs ───────────────────────────────────────
    pub(crate) input: InputSubsystem,
    pub(crate) output: OutputSubsystem,
    pub(crate) external_io: ExternalIO,
    /// Interactive HTML window state (feature `html`). See /spec/html-source.md §4.
    #[cfg(feature = "html")]
    pub(crate) interactive: interactive::InteractiveHtmlState,
    pub(crate) frame_stats: FrameStats,
    pub(crate) session: SessionState,
    pub(crate) bus: MessageBus,

    // ── Audio textures (GPU resource, owned here) ──────────────
    audio_textures: crate::audio::AudioTextures,

    // ── Render resolution (configurable, scene-level) ───────
    render_width: u32,
    render_height: u32,
    /// Auto: the master follows the largest screen an output is on.
    render_auto: bool,
    /// A size Auto wants, and for how many frames running it has wanted it.
    auto_resolution_pending: Option<((u32, u32), u32)>,

    // ── Frame pacing (global, runtime-mutable) ──────────
    target_fps: u32,
    /// Frames the GPU may still be drawing when the next starts (1–3).
    frames_in_flight: u32,
    /// What starts each frame.
    frame_pacing: crate::engine::value::render::FramePacing,
    /// Frames asked for by the refresh each display-link callback is stamped
    /// for, rather than by counting callbacks.
    clock_timestamps: bool,
    /// Each frame held on screen for a frame's time (see `set_frame_hold`).
    frame_hold: bool,
    /// The control window presents in step with its screen's refresh.
    control_vsync: bool,
    /// The control window's screen set to the show's rate while Nida runs
    /// (see `screen_rate`).
    control_screen_rate: bool,
    /// Night Shift kept off while an output window is on a screen (see
    /// `night_shift`).
    night_shift_off: bool,

    // ── Channel preview / cue (ephemeral, published by UI each frame) ──
    // Channels force-rendered for off-air preview. Never persisted; affects the
    // render gate only, never the compositor. See /spec/channel-preview.md.
    preview_channels: Vec<usize>,
    /// The channels whose tiles the control window shows (see
    /// `set_tile_channels`).
    tile_channels: Vec<usize>,
    /// The decks whose thumbnails the control window drew last frame, by
    /// UUID (see `set_previewed_decks`).
    previewed_decks: Vec<String>,
    /// The rate frames really come at (see `set_frame_cadence`).
    frame_cadence: Option<u32>,
    /// A screen's mode changed: the show's thread looks at its clock again
    /// now (see `take_clock_recheck`).
    clock_recheck: bool,
    /// The deck selected in the UI (channel, deck), whose own picture the
    /// settings show while it is edited. Set by the runner each frame.
    ui_selected_deck: Option<(usize, usize)>,
    /// When decks waiting for their camera are next looked for.
    camera_retry_at: Option<std::time::Instant>,
    /// The display inputs that go by their name alone, as last looked: the
    /// operator is told when one is new (see `warn_captures_by_name`).
    captures_by_name: Vec<String>,
    /// The lower thirds' list files, looked at for changes off the frame.
    lower_third_lists: state::lower_thirds::ListWatch,
    /// The control window is up, showing every channel's picture as a tile:
    /// off-air channels are drawn now and then for it (see
    /// `Mixer::tile_previews`), their cameras kept live.
    ui_tiles: bool,

    // ── Pending MIDI-triggered actions (consumed by runner) ──
    pub(crate) midi_pending_undo: bool,
    pub(crate) midi_pending_redo: bool,
    pub(crate) midi_pending_save: bool,

    // ── Shutdown request flag ──────────────────────────────────
    /// Set by `EngineCommand::Shutdown` (the API's): shared with the
    /// runner, which quits the app — windowed or headless — when it is set.
    pub(crate) shutdown_requested: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

// In the windowed app the engine runs on a thread of its own and is handed
// to the main thread and back (see `usecases::ui::runner::engine_thread`), so
// it must be `Send`: a part that is not stops the build here.
const _: () = {
    const fn assert_send<T: Send>() {}
    assert_send::<VardaApp>();
};

impl VardaApp {
    /// Create a new `VardaApp` with all subsystems initialized.
    ///
    /// Requires a fully initialized `GpuContext` — the engine cannot exist
    /// without a GPU. A default two-channel mixer is always created.
    /// `config` provides session settings from CLI flags + workspace defaults.
    ///
    /// # Errors
    /// Returns an error if the default mixer's GPU render targets cannot be
    /// allocated, or if a subsystem required by `config` (MIDI, OSC, HTTP API)
    /// fails to start.
    pub fn new(gpu: GpuContext, config: &AppConfig) -> anyhow::Result<Self> {
        log::info!("[STARTUP]   Audio init...");
        crate::audio::device_watch::start();
        let (audio_manager, audio_inputs_pending) =
            AudioManager::new_waiting_at_most(std::time::Duration::from_secs(3));

        log::info!("[STARTUP]   Workspace init...");
        let workspace = Workspace::new(config.effective_workspace_root());

        // Build shader registry with all library paths. Order is precedence:
        // later paths override earlier ones by shader name, and that ordering is
        // held across the whole session (hot-reload and removal re-resolve the
        // winner), not just at the initial scan.
        // 1. Bundled shaders (exe-relative, for packaged .app / AppImage)
        // 2. CWD shaders/ (dev builds / cargo run)
        // 3. Workspace .varda/shaders/ (per-show user shaders)
        // 4. Platform user dir (global user shader collection)
        // 5. Any --shader-dir flags (added last, override built-ins by name)
        let mut registry = ShaderRegistry::new();
        if let Some(bundled) = crate::registry::get_bundled_shader_path() {
            if let Err(e) = registry.add_library_path(&bundled) {
                log::warn!("Failed to add bundled shaders path: {e}");
            }
        }
        if let Err(e) = registry.add_library_path("shaders") {
            log::warn!("Failed to add shaders path: {e}");
        }
        registry.mark_show_slot();
        registry.set_show_library(&workspace.shaders_dir());
        for path in crate::registry::get_default_library_paths() {
            if path.is_dir() {
                if let Err(e) = registry.add_library_path(&path) {
                    log::warn!(
                        "Failed to add user shader library path {}: {}",
                        path.display(),
                        e
                    );
                }
            }
        }
        for path in &config.shader_dirs {
            if let Err(e) = registry.add_library_path(path) {
                log::warn!("Failed to add --shader-dir {}: {}", path.display(), e);
            }
        }
        match registry.scan() {
            Ok(count) => log::info!("Loaded {count} shaders"),
            Err(e) => log::error!("Failed to scan shaders: {e}"),
        }
        if let Err(e) = registry.start_watching() {
            log::warn!("Failed to start shader hot-reload: {e}");
        }

        // Load OSC config from workspace (or use defaults), then apply CLI overrides
        let mut osc_config = if workspace.has_osc() {
            OscConfig::load(workspace.osc_path()).unwrap_or_else(|e| {
                log::warn!("Failed to load OSC config: {e}, using defaults");
                OscConfig::default()
            })
        } else {
            OscConfig::default()
        };

        // CLI overrides for OSC
        if config.osc_disabled {
            osc_config.enabled = false;
        }
        if let Some(port) = config.osc_port {
            osc_config.in_port = port;
        }
        for target in &config.osc_targets {
            if !osc_config.feedback_targets.contains(target) {
                osc_config.feedback_targets.push(target.clone());
            }
        }

        log::info!("[STARTUP]   OSC init...");
        let osc_receiver = if osc_config.enabled {
            match OscReceiver::new(osc_config.in_port) {
                Ok(osc) => {
                    log::info!("OSC receiver started on port {}", osc_config.in_port);
                    Some(osc)
                }
                Err(e) => {
                    log::warn!(
                        "Failed to start OSC receiver on port {}: {}",
                        osc_config.in_port,
                        e
                    );
                    None
                }
            }
        } else {
            log::info!("OSC input disabled by config");
            None
        };

        let osc_feedback = match OscFeedbackSender::new() {
            Ok(mut sender) => {
                for target in &osc_config.feedback_targets {
                    if let Err(e) = sender.add_target(target) {
                        log::warn!("Failed to add OSC feedback target '{target}': {e}");
                    }
                }
                Some(sender)
            }
            Err(e) => {
                log::warn!("Failed to create OSC feedback sender: {e}");
                None
            }
        };

        log::info!("[STARTUP]   MIDI init...");
        let mut controller_led_mgr = midi::ControllerLedManager::new();
        let mut auto_map_engine = midi::AutoMapEngine::new();
        let midi_devices = match midi::MidiDeviceManager::new() {
            Ok(mut mgr) => {
                mgr.load_user_profiles(&workspace.controller_profiles_dir());
                if workspace.controller_profiles_dir().is_dir() {
                    let _ = mgr.scan_devices();
                }
                log::info!("MIDI initialized: {} device(s)", mgr.devices.len());
                controller_led_mgr.sync_devices(&mgr);
                auto_map_engine.sync_devices(&mgr);
                Some(mgr)
            }
            Err(e) => {
                log::warn!("Failed to initialize MIDI: {e}");
                None
            }
        };

        let (command_tx, command_rx) = tokio::sync::mpsc::unbounded_channel();
        let state_tx = std::sync::Arc::new(std::sync::RwLock::new(None));

        // Always create GPU-dependent resources up front
        log::info!("[STARTUP]   GPU resources (textures, mixer)...");
        let audio_textures = crate::audio::AudioTextures::new(&gpu.device);
        let calibration_textures =
            crate::renderer::context::create_calibration_textures(&gpu.device, &gpu.queue, 8);
        let mixer = Mixer::new(&gpu, DEFAULT_RENDER_WIDTH, DEFAULT_RENDER_HEIGHT)?;
        let preset_library = crate::persistence::presets::PresetLibrary::load(&workspace);

        let waiting_for_sound = audio_inputs_pending.is_some();
        let mut app = Self {
            mixer,
            audio_manager,
            audio_mix: crate::audio_mix::AudioEngine::new(),
            clips_on_air: std::collections::HashMap::new(),
            audio_devices_listed: false,
            audio_inputs_pending,
            audio_rescan_due: None,
            audio_inputs_rescan: None,
            atem_checked: None,
            streaming: state::streaming::Streaming::default(),
            recording: state::record::Recording::default(),
            camera_rates: std::collections::HashMap::new(),
            camera_rates_at: None,
            camera_manager: if config.camera_disabled {
                CameraManager::disabled()
            } else {
                CameraManager::new()
            },
            depth_manager: DepthSensorManager::new(),
            // Constructed disabled rather than merely inert, so `--no-screen-capture`
            // never triggers the macOS TCC prompt.
            screen_capture_manager: if config.screen_capture_disabled {
                ScreenCaptureManager::new_disabled()
            } else {
                // Captures drawn as coded only where the GPU reads a
                // texture two ways (see `ScreenCaptureManager::on_gpu`).
                ScreenCaptureManager::new()
                    .on_gpu(gpu.adapter.get_downlevel_capabilities().flags)
            },
            presentations: state::presentations::Presentations::new(config.powerpoint_disabled),
            web_talks: state::slides::WebTalks::new(config.headless),
            talk_checks: crate::presentation::check::Checker::new(),
            registry,
            registry_snapshot: std::cell::RefCell::new(None),
            analyzer_registry: crate::analyzer::default_registry(),
            context: gpu,
            transport: crate::transport::Transport::new(),
            arrangement_blackout_reported: false,
            input: InputSubsystem {
                osc_receiver,
                osc_feedback,
                osc_config,
                midi_devices,
                keymap: KeymapStore::with_defaults(),
                midi_mappings: midi::MidiMappingStore::new(),
                controller_led_mgr,
                auto_map_engine,
                clock_manager: crate::clock::ClockManager::new(),
                timecode: crate::timecode::TimecodeManager::new(),
                ltc_tap: None,
                ltc_lost: None,
                ltc_opening: None,
            },
            output: OutputSubsystem {
                outputs: Vec::new(),
                surface_manager: SurfaceManager::new(),
                calibration_textures,
                led_cards: crate::renderer::led_card::LedCards::default(),
                output_cards: crate::renderer::led_card::OutputCards::default(),
                domemaster: None,
                domemaster_resolution: crate::renderer::dome::DomemasterResolution::default(),
                pending_output_creates: Vec::new(),
                cached_monitors: Vec::new(),
                monitor_sizes: Vec::new(),
                screen_facts: Vec::new(),
                screen_facts_for: Vec::new(),
                screen_facts_at: None,
                // A test engine never reads or writes this computer's names.
                screen_names: if cfg!(test) {
                    screens::ScreenNames::load_from(None)
                } else {
                    screens::ScreenNames::load()
                },
                bus_dissolves: bus_dissolve::BusDissolves::default(),
                bus_overlays: bus_overlay::BusOverlays::default(),
                keep_awake: keep_awake::KeepAwake::default(),
                control_screen: None,
                rate_check_at: None,
                rate_warned: std::collections::HashSet::new(),
                reopened: std::collections::HashMap::new(),
                blackmagic_looked: None,
                blackmagic_seen: None,
            },
            external_io: ExternalIO {
                ndi_manager: if config.ndi_disabled {
                    log::info!("NDI disabled by CLI flag");
                    crate::ndi::NdiManager::new_disabled()
                } else {
                    crate::ndi::NdiManager::new()
                },
                #[cfg(target_os = "macos")]
                syphon_manager: if config.syphon_disabled {
                    log::info!("Syphon disabled by CLI flag");
                    crate::syphon::SyphonManager::new_disabled()
                } else {
                    crate::syphon::SyphonManager::new()
                },
                #[cfg(target_os = "macos")]
                pending_syphon: Vec::new(),
                #[cfg(target_os = "macos")]
                last_syphon_scan: std::time::Instant::now(),
                stream_manager: crate::stream::StreamManager::new(),
                stream_library: Vec::new(),
                hls_library: Vec::new(),
                dash_library: Vec::new(),
                rtmp_library: Vec::new(),
                html_manager: if config.html_disabled {
                    crate::html::HtmlManager::new_disabled()
                } else {
                    crate::html::HtmlManager::new()
                },
                html_library: Vec::new(),
            },
            frame_stats: FrameStats {
                last_frame_instant: std::time::Instant::now(),
                fps_history: std::collections::VecDeque::with_capacity(60),
                fps_smoothed: 0.0,
                frame_count: 0,
                system_monitor: crate::sysmon::SystemMonitor::new(),
                last_frame_submits: 0,
                start_instant: std::time::Instant::now(),
                frame_time_secs: 0.0,
                intervals: std::collections::VecDeque::with_capacity(PACING_FRAMES),
                clock: String::new(),
                clock_display: None,
                parts: frame_parts::FrameParts::default(),
                windows_presented: false,
                refresh_at: None,
                drawn_at: None,
                display_link: None,
                control_on_screen: crate::engine::OnScreenStats::default(),
                control_parts: Vec::new(),
                control_slow_frames: Vec::new(),
                control_monitors: crate::engine::ControlMonitorsStats::default(),
                control_rate: crate::engine::ControlRateStats::default(),
                control_queue: crate::engine::ControlQueueStats::default(),
                gpu_time: crate::renderer::gpu_time::GpuTime::default(),
            },
            session: SessionState {
                workspace,
                preset_library,
                history: history::HistoryManager::new(),
                notifications: NotificationSystem::new(),
                last_layout: crate::usecases::ui::UILayoutState::default(),
                clipboard: None,
                cue_anchor: None,
                chase_silent_since: None,
                chase_silence_reported: false,
                published_timecode: None,
                recorder: state::recorder::Recorder::default(),
                unwritable: std::collections::HashSet::new(),
            },
            bus: MessageBus {
                command_rx,
                command_tx,
                state_tx,
            },
            audio_textures,
            #[cfg(feature = "html")]
            interactive: interactive::InteractiveHtmlState::default(),
            production_buses: crate::bus::BusBank::with_legacy_default(),
            rundown: crate::rundown::Rundown::default(),
            clips_held: Vec::new(),
            rundown_fades: Vec::new(),
            rundown_till_next: Vec::new(),
            rundown_day_was: None,
            show_writer: crate::persistence::writer::ShowWriter::default(),
            media_adds: media_adds::MediaAdds::default(),
            show_generation: 0,
            saving_as: None,
            presets_on_way: Vec::new(),
            render_width: DEFAULT_RENDER_WIDTH,
            render_height: DEFAULT_RENDER_HEIGHT,
            render_auto: false,
            auto_resolution_pending: None,
            target_fps: config.target_fps,
            frames_in_flight: 1,
            frame_pacing: crate::engine::value::render::FramePacing::default(),
            clock_timestamps: false,
            // On, where there is a hold to give (macOS): measured on a Mac
            // mini, presents not held let the display link skip one refresh
            // in ten or twenty — a slow slide stuttered — and on a ProMotion
            // MacBook they pulled the screen's own refresh about.
            frame_hold: cfg!(target_os = "macos"),
            control_vsync: false,
            control_screen_rate: true,
            night_shift_off: true,
            preview_channels: Vec::new(),
            tile_channels: Vec::new(),
            previewed_decks: Vec::new(),
            frame_cadence: None,
            clock_recheck: false,
            ui_selected_deck: None,
            camera_retry_at: None,
            captures_by_name: Vec::new(),
            lower_third_lists: state::lower_thirds::ListWatch::default(),
            ui_tiles: false,
            midi_pending_undo: false,
            midi_pending_redo: false,
            midi_pending_save: false,
            shutdown_requested: std::sync::Arc::default(),
        };
        if waiting_for_sound {
            app.session.notifications.warn(
                "Sound inputs are not ready: macOS may be asking whether Nida may use the \
                 microphone — look for its prompt, which can hide behind full-screen windows",
            );
        }
        Ok(app)
    }

    /// Get a command sender for cross-thread consumers (HTTP API, CLI).
    pub fn command_sender(&self) -> tokio::sync::mpsc::UnboundedSender<CommandEnvelope> {
        self.bus.command_tx.clone()
    }

    /// Get a shared reference to the latest engine state (for cross-thread consumers).
    pub fn state_reader(&self) -> std::sync::Arc<crate::engine::types::PublishedState> {
        self.bus.state_tx.clone()
    }

    /// Process all queued cross-thread commands. Called once per frame.
    ///
    /// Exhaustive match — the compiler enforces that every `EngineCommand` variant
    /// is handled. Adding a new variant requires wiring it here.
    pub fn process_commands(&mut self) {
        self.place_made_media();
        self.take_started_record();
        self.take_started_stream();
        while let Ok((cmd, reply_tx)) = self.bus.command_rx.try_recv() {
            // A clip or picture is made on a thread of its own, its undo step
            // taken as it is put in (see `media_adds`).
            let Some((cmd, reply_tx)) = self.add_media_for_command(cmd, reply_tx) else {
                continue;
            };
            // Record pre-mutation state so bus-driven (HTTP API / WebSocket /
            // CLI / MIDI-issued) edits are undoable on the same timeline the
            // windowed UI uses. In-process UI mutations do NOT flow through the
            // bus — the windowed runner records those itself — so there is no
            // double-record here. See `makes_undo_step`.
            // A pass under way has already pushed the one entry it gets, and a
            // performer playing a fader through the API would otherwise fill
            // the stack a frame at a time.
            if self.makes_undo_step(&cmd) && !self.is_recording() {
                let snapshot = self.history_snapshot_default();
                self.push_history(snapshot);
            }
            // Saves are written on a thread of their own: the reply waits
            // for the files, the engine does not.
            let result = match cmd {
                crate::engine::EngineCommand::SaveWorkspace => {
                    self.save_for_command(reply_tx);
                    continue;
                }
                // Answered once its encoder runs (see `EncoderStart`).
                crate::engine::EngineCommand::StartRecording => {
                    self.start_recording_for_command(reply_tx);
                    continue;
                }
                crate::engine::EngineCommand::StartStream => {
                    self.start_stream_for_command(reply_tx);
                    continue;
                }
                crate::engine::EngineCommand::SaveShowAs { folder } => {
                    self.save_show_as_for_command(&folder, reply_tx);
                    continue;
                }
                crate::engine::EngineCommand::SaveDeckPreset { deck_uuid, name } => {
                    match self.deck_preset_file(&deck_uuid, &name) {
                        Ok(save) => {
                            self.save_preset(save, reply_tx);
                            continue;
                        }
                        Err(refused) => refused,
                    }
                }
                crate::engine::EngineCommand::SaveChannelPreset { channel_uuid, name } => {
                    match self.channel_preset_file(&channel_uuid, &name) {
                        Ok(save) => {
                            self.save_preset(save, reply_tx);
                            continue;
                        }
                        Err(refused) => refused,
                    }
                }
                crate::engine::EngineCommand::RenameScreen { monitor_name, name } => {
                    match self.screen_name_file(&monitor_name, name.as_deref()) {
                        Ok(Some(file)) => {
                            self.write_for_command(file, reply_tx);
                            continue;
                        }
                        Ok(None) => CommandResult::Ok,
                        Err(refused) => refused,
                    }
                }
                cmd => self.execute_command(cmd),
            };
            if let Some(tx) = reply_tx {
                let _ = tx.send(result);
            }
        }
    }

    /// Nida is quitting, its engine stopped: a command still waiting is
    /// answered that it is, and none is taken from now. An HTTP request left
    /// waiting for an answer would keep the API server from closing, and
    /// Nida from quitting. On the main thread as Nida quits — not inside an
    /// async runtime, where the blocking receive would panic.
    pub fn refuse_commands(&mut self) {
        self.refuse_media();
        self.refuse_record_start();
        self.refuse_stream_start();
        self.bus.command_rx.close();
        // After the close a receive hands over every command sent before
        // it, one still on its way in included, then says there are no
        // more. (`try_recv` would call the channel empty while a send is
        // half way in, and that request would wait for ever.)
        while let Some((_, reply)) = self.bus.command_rx.blocking_recv() {
            if let Some(reply) = reply {
                let _ = reply.send(CommandResult::Err {
                    code: ErrorCode::Unavailable,
                    message: "Nida is quitting".into(),
                });
            }
        }
    }

    /// Helper: access auto-transition on a deck, creating if needed, then apply mutation.
    fn exec_auto_transition(
        &mut self,
        deck_uuid: &str,
        f: impl FnOnce(&mut crate::channel::DeckAutoTransition),
    ) -> CommandResult {
        let (ch_idx, deck_idx) = match self.resolve_deck(deck_uuid) {
            Ok(loc) => loc,
            Err(e) => {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: e.to_string(),
                }
            }
        };
        let slot = &mut self.mixer.channels_mut()[ch_idx].decks[deck_idx];
        f(slot
            .auto_transition
            .get_or_insert_with(crate::channel::DeckAutoTransition::new));
        CommandResult::Ok
    }

    /// Resolve `deck_uuid` and apply `f` to the deck. `f` returns false when the
    /// deck exists but does not support the operation (for example a video
    /// command on a shader deck) — that is `InvalidInput`, distinct from the
    /// `NotFound` a stale UUID produces.
    fn exec_on_deck(
        &mut self,
        deck_uuid: &str,
        f: impl FnOnce(&mut crate::deck::Deck) -> bool,
    ) -> CommandResult {
        let (ch_idx, deck_idx) = match self.resolve_deck(deck_uuid) {
            Ok(loc) => loc,
            Err(e) => {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: e.to_string(),
                }
            }
        };
        if f(&mut self.mixer.channels_mut()[ch_idx].decks[deck_idx].deck) {
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: format!("Deck '{deck_uuid}' does not support this operation"),
            }
        }
    }

    /// A deck's playback state, or `None` if the UUID is stale or the deck has
    /// no video source.
    fn video_playback_snapshot(&self, deck_uuid: &str) -> Option<crate::video::PlaybackSnapshot> {
        let (ch_idx, deck_idx) = self.mixer.find_deck_by_uuid(deck_uuid)?;
        self.mixer.channels()[ch_idx].decks[deck_idx]
            .deck
            .playback_snapshot()
    }

    /// Helper: update a modulation source by UUID.
    fn exec_modulation_update(
        &mut self,
        uuid: &str,
        f: impl FnOnce(&mut crate::modulation::ModulationSource),
    ) -> CommandResult {
        if let Some(source) = self.mixer.modulation_mut().source_mut(uuid) {
            f(source);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("Modulation source {uuid} not found"),
            }
        }
    }

    /// Build a domain-neutral engine state snapshot for cross-thread consumers.
    pub fn build_engine_state(&self) -> EngineState {
        snapshot::build_engine_state(self)
    }

    /// Publish the latest engine state for cross-thread consumers.
    pub fn publish_state(&self) {
        let state = std::sync::Arc::new(self.build_engine_state());
        // A reader holds the lock only to take the pointer, so it is seldom
        // held; when it is, this state goes unpublished rather than the
        // show waiting — the next comes a few frames on. The one replaced
        // goes outside the lock.
        let replaced = match self.bus.state_tx.try_write() {
            Ok(mut guard) => guard.replace(state),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => {
                let replaced = poisoned.into_inner().replace(state);
                // A whole state in place again: readers may read it.
                self.bus.state_tx.clear_poison();
                replaced
            }
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        drop(replaced);
    }

    // ── Public accessors (controlled access for delivery layers) ─────

    /// Read-only access to the GPU context.
    pub fn gpu_context(&self) -> &GpuContext {
        &self.context
    }

    /// Command buffer commits issued during the previous frame.
    pub fn last_frame_submits(&self) -> u32 {
        self.frame_stats.last_frame_submits
    }

    /// Read-only access to the mixer.
    pub fn mixer_ref(&self) -> &crate::mixer::Mixer {
        &self.mixer
    }

    /// Release the external sessions a deck was holding.
    ///
    /// Shared by normal deck removal and by the structural restore that retires
    /// pooled runtimes, so both free managers-owned resources identically.
    pub(crate) fn release_held_resources(&mut self, held: HeldResources) {
        if let Some(cam_id) = held.camera {
            self.camera_manager.release_camera(cam_id);
        }
        for sensor_id in held.depth {
            self.depth_manager.release(sensor_id);
        }
        if let Some(capture_id) = held.screen_capture {
            self.screen_capture_manager.release(capture_id);
        }
        if let Some(idx) = held.srt {
            self.external_io.stream_manager.stop_receive(idx);
        }
        if let Some(idx) = held.ndi {
            self.external_io.ndi_manager.stop_receive(idx);
        }
        #[cfg(target_os = "macos")]
        if let Some(idx) = held.syphon {
            self.external_io.syphon_manager.stop_receive(idx);
        }
    }

    /// Read-only access to the production buses.
    pub fn production_buses(&self) -> &crate::bus::BusBank {
        &self.production_buses
    }

    /// Read-only access to the camera manager.
    pub fn camera_manager(&self) -> &CameraManager {
        &self.camera_manager
    }

    /// Mutable access to the camera manager (open/release cameras).
    pub fn camera_manager_mut(&mut self) -> &mut CameraManager {
        &mut self.camera_manager
    }

    /// Open a camera, returning resolution on success. Avoids split-borrow issues
    /// by accessing both `camera_manager` and `context` internally.
    ///
    /// # Errors
    /// Returns an error if no camera with `id` is present, if the OS refuses
    /// access to the device, or if its GPU textures cannot be allocated.
    pub fn open_camera(&mut self, id: crate::camera::CameraId) -> anyhow::Result<(u32, u32)> {
        self.camera_manager
            .open_camera(id, &self.context.device, None)
    }

    /// Read-only access to the screen-capture manager.
    /// `(uuid, name)` for every channel, which is what resolves a tap's
    /// channel UUID to something a performer can read.
    pub fn channel_labels(&self) -> Vec<(String, String)> {
        self.mixer
            .channels()
            .iter()
            .map(|ch| (ch.uuid().to_string(), ch.name.clone()))
            .collect()
    }

    pub fn screen_capture_manager(&self) -> &ScreenCaptureManager {
        &self.screen_capture_manager
    }

    /// Mutable access to the screen-capture manager (scan/open/release captures).
    pub fn screen_capture_manager_mut(&mut self) -> &mut ScreenCaptureManager {
        &mut self.screen_capture_manager
    }

    /// Read-only access to the depth-sensor manager.
    pub fn depth_manager(&self) -> &DepthSensorManager {
        &self.depth_manager
    }

    /// Mutable access to the depth-sensor manager (open/release sensors).
    pub fn depth_manager_mut(&mut self) -> &mut DepthSensorManager {
        &mut self.depth_manager
    }

    /// The audio mixer.
    pub fn audio_mix(&self) -> &crate::audio_mix::AudioEngine {
        &self.audio_mix
    }

    /// Read-only access to the outputs.
    pub fn outputs_ref(&self) -> &[crate::renderer::context::UnifiedOutput] {
        &self.output.outputs
    }

    /// Outputs loaded from the stage file that open on the next turn of the
    /// event loop.
    pub fn pending_outputs(&self) -> &[crate::scene::OutputConfig] {
        &self.output.pending_output_creates
    }

    /// Mutable access to the mixer (for deck insertion from background loads).
    pub fn mixer_mut(&mut self) -> &mut crate::mixer::Mixer {
        &mut self.mixer
    }

    /// Read-only access to the domemaster renderer output view (if enabled).
    pub fn domemaster_view(&self) -> Option<&wgpu::TextureView> {
        self.output
            .domemaster
            .as_ref()
            .map(super::internal::renderer::dome::DomemasterRenderer::output_view)
    }

    /// Edge length of the live domemaster texture, or `None` when no renderer
    /// has been built. Reports what the GPU actually holds, which can lag
    /// [`Self::domemaster_resolution`] if a rebuild failed.
    pub fn domemaster_output_size(&self) -> Option<u32> {
        self.output
            .domemaster
            .as_ref()
            .map(super::internal::renderer::dome::DomemasterRenderer::output_size)
    }

    /// Ensure the domemaster renderer exists and is enabled.
    /// Creates it lazily on first call; subsequent calls just ensure `enabled = true`.
    pub fn ensure_domemaster(&mut self) {
        if let Some(dome) = &mut self.output.domemaster {
            dome.enabled = true;
        } else {
            let config = crate::renderer::dome::DomemasterConfig {
                resolution: self.output.domemaster_resolution,
                ..crate::renderer::dome::DomemasterConfig::default()
            };
            match crate::renderer::dome::DomemasterRenderer::new(
                &self.context.device,
                self.context.compositing_format,
                config,
            ) {
                Ok(mut dome) => {
                    dome.enabled = true;
                    self.output.domemaster = Some(dome);
                    log::info!("Domemaster renderer created and enabled");
                }
                Err(e) => {
                    log::error!("Failed to create domemaster renderer: {e}");
                }
            }
        }
    }

    /// Size the domemaster is rendered at.
    pub fn domemaster_resolution(&self) -> crate::renderer::dome::DomemasterResolution {
        self.output.domemaster_resolution
    }

    /// Set the domemaster output size, rebuilding the renderer if one is live.
    ///
    /// Every texture in the renderer is sized at construction, so there is no
    /// resize path — the old renderer is dropped and a new one built in its
    /// place. A failed rebuild leaves no renderer rather than one at the old
    /// size, so the dome goes black instead of silently ignoring the setting.
    pub fn set_domemaster_resolution(
        &mut self,
        resolution: crate::renderer::dome::DomemasterResolution,
    ) {
        if resolution == self.output.domemaster_resolution {
            return;
        }
        log::info!(
            "Domemaster resolution: {} → {}",
            self.output.domemaster_resolution,
            resolution
        );
        self.output.domemaster_resolution = resolution;

        let Some(existing) = self.output.domemaster.take() else {
            return;
        };
        let config = crate::renderer::dome::DomemasterConfig {
            resolution,
            ..existing.config.clone()
        };
        let was_enabled = existing.enabled;
        let content_rotation = existing.content_rotation;
        drop(existing);

        match crate::renderer::dome::DomemasterRenderer::new(
            &self.context.device,
            self.context.compositing_format,
            config,
        ) {
            Ok(mut dome) => {
                dome.enabled = was_enabled;
                dome.content_rotation = content_rotation;
                self.output.domemaster = Some(dome);
            }
            Err(e) => {
                log::error!("Failed to rebuild domemaster renderer at {resolution}: {e}");
            }
        }
    }

    /// Set domemaster content rotation (azimuth, elevation, roll) in radians.
    /// Called each frame from the UI layer so content rotation is applied
    /// in real-time by the domemaster shader, not baked into warp meshes.
    pub fn set_domemaster_content_rotation(&mut self, az: f32, el: f32, roll: f32) {
        if let Some(dome) = &mut self.output.domemaster {
            dome.set_content_rotation(az, el, roll);
        }
    }

    /// Publish the set of channels to force-render for off-air preview.
    /// Called by the runner each frame from the UI selection. Ephemeral — never
    /// persisted; affects the render gate only. See /spec/channel-preview.md.
    pub fn set_preview_channels(&mut self, channels: Vec<usize>) {
        self.preview_channels = channels;
    }

    /// The channels whose pictures the control window (and its panels'
    /// windows) shows as tiles this frame: an off-air one is drawn for its
    /// tile only while it is seen (see `Mixer::tile_channels`).
    pub fn set_tile_channels(&mut self, channels: Vec<usize>) {
        self.tile_channels = channels;
    }

    /// The decks whose thumbnails the control window (and its panels'
    /// windows) drew last frame: they draw their own frames, which the
    /// thumbnails show, rather than only onto their input from their source
    /// (see `Channel::previewed`).
    pub fn set_previewed_decks(&mut self, decks: Vec<String>) {
        self.previewed_decks = decks;
    }

    /// The rate frames really come at, as the show's thread knows it — a
    /// screen's refreshes, which follow a new target rate only after a
    /// moment — for the recordings and streams that count frames by it.
    pub fn set_frame_cadence(&mut self, fps: Option<u32>) {
        self.frame_cadence = fps;
    }

    /// The channels whose tiles are seen now (see `set_tile_channels`).
    pub fn tile_channels(&self) -> &[usize] {
        &self.tile_channels
    }

    /// The control window is up and shows every channel's picture as a tile
    /// (see `Mixer::tile_previews`). Set once by the windowed app.
    pub fn set_ui_tiles(&mut self, on: bool) {
        self.ui_tiles = on;
    }

    /// Publish the deck selected in the UI, (channel, deck) by position:
    /// its own picture keeps rendering while it is edited, even under a
    /// deck that covers its channel. Called by the runner each frame.
    pub fn set_ui_selected_deck(&mut self, deck: Option<(usize, usize)>) {
        self.ui_selected_deck = deck;
    }

    /// Number of loaded shaders.
    pub fn shader_count(&self) -> usize {
        self.registry.count()
    }

    /// Resolve a generator index to a cloned `ISFShader`.
    /// Returns None if the index is out of bounds.
    pub fn resolve_generator(&self, gen_idx: usize) -> Option<crate::isf::ISFShader> {
        self.registry
            .generators()
            .get(gen_idx)
            .map(|s| (*s).clone())
    }

    /// Tick notification expiry timers.
    pub fn update_notifications(&mut self) {
        self.report_saves();
        self.session.notifications.update();
    }

    /// Push an info-level notification.
    pub fn notify_info(&mut self, message: impl Into<String>) {
        self.session.notifications.info(message);
    }

    /// Surface an error to the performer. Used by consumers that handle a
    /// failure outside the command bus (which toasts errors on their behalf).
    pub fn notify_error(&mut self, message: impl Into<String>) {
        self.session.notifications.error(message);
    }

    /// Close an output window by its winit `WindowId`. Returns the name if found.
    pub fn close_output_window_by_id(
        &mut self,
        window_id: winit::window::WindowId,
    ) -> Option<String> {
        if let Some(idx) = self.output.outputs.iter().position(|o| {
            if let UnifiedOutput::Window(w) = o {
                w.window.id() == window_id
            } else {
                false
            }
        }) {
            let name = self.output.outputs[idx].name().to_string();
            if let UnifiedOutput::Window(w) = self.output.outputs.remove(idx) {
                w.destroy();
            }
            Some(name)
        } else {
            None
        }
    }

    /// Resize an output window by its winit `WindowId`.
    /// Take the output in `window_id` off its screen, back to an ordinary
    /// window — what Esc on an output does, so one sent to the wrong screen
    /// (the control screen above all) can always be got out of. Returns its
    /// name if it was on a screen.
    pub fn take_output_off_screen_by_id(
        &mut self,
        window_id: winit::window::WindowId,
    ) -> Option<String> {
        let idx = self.output.outputs.iter().position(|o| {
            matches!(o, UnifiedOutput::Window(w)
                if w.window.id() == window_id
                    && matches!(w.configured_target(), crate::renderer::context::OutputTarget::Display { .. }))
        })?;
        let name = self.output.outputs[idx].name().to_string();
        let _ = self.cmd_set_output_target(idx, crate::renderer::context::OutputTarget::Windowed);
        self.session.notifications.info(format!(
            "Output '{name}' taken off its screen (Esc) — pick a screen for it in the Switcher"
        ));
        Some(name)
    }

    /// Record which screen the control window is on, so the screen pickers
    /// can mark it. Called by the runner every frame.
    pub fn note_control_window(&mut self, monitor: Option<winit::monitor::MonitorHandle>) {
        self.output.control_screen = monitor.and_then(|m| {
            self.output
                .cached_monitors
                .iter()
                .find(|(_, handle)| *handle == m)
                .map(|(name, _)| name.clone())
        });
    }

    /// The screen the control window is on, by monitor name.
    pub fn control_screen(&self) -> Option<&str> {
        self.output.control_screen.as_deref()
    }

    /// The screen the control window is on, as the main thread last saw it.
    pub fn control_monitor(&self) -> Option<winit::monitor::MonitorHandle> {
        let name = self.output.control_screen.as_deref()?;
        monitors::find(&self.output.cached_monitors, name).map(|(_, handle)| handle.clone())
    }

    /// An output window was hidden or shown again (main thread): its surface
    /// is told, as the engine thread may not ask the window.
    pub fn occlude_output_window_by_id(&mut self, window_id: winit::window::WindowId, hidden: bool) {
        for o in &self.output.outputs {
            if let UnifiedOutput::Window(w) = o {
                if w.window.id() == window_id {
                    crate::renderer::context::set_occluded(&w.surface, Some(hidden));
                    return;
                }
            }
        }
    }

    /// An output window moved on the desktop (main thread): kept for saving.
    pub fn move_output_window_by_id(
        &mut self,
        window_id: winit::window::WindowId,
        position: winit::dpi::PhysicalPosition<i32>,
    ) {
        for o in &mut self.output.outputs {
            if let UnifiedOutput::Window(w) = o {
                if w.window.id() == window_id {
                    w.position = Some(position);
                    return;
                }
            }
        }
    }

    pub fn resize_output_window_by_id(
        &mut self,
        window_id: winit::window::WindowId,
        new_size: winit::dpi::PhysicalSize<u32>,
    ) {
        for o in &mut self.output.outputs {
            if let UnifiedOutput::Window(w) = o {
                if w.window.id() == window_id {
                    w.resize(&self.context, new_size);
                    return;
                }
            }
        }
    }

    /// Current render width.
    pub fn render_width(&self) -> u32 {
        self.render_width
    }

    /// Current render height.
    pub fn render_height(&self) -> u32 {
        self.render_height
    }

    /// Whether the render resolution follows the output screens.
    pub fn render_auto(&self) -> bool {
        self.render_auto
    }

    /// Turn Auto on or off. On, the master follows the largest screen an
    /// output is on (see `follow_output_screens`); off, it stays where it is.
    pub fn set_render_auto(&mut self, enabled: bool) {
        self.render_auto = enabled;
        self.auto_resolution_pending = None;
    }

    /// Maximum render dimension (width or height) the GPU can allocate a
    /// texture for. Varda imposes no artificial cap — this hardware limit is
    /// the only bound on render resolution (see spec/resolution-and-scaling.md).
    pub fn max_render_dimension(&self) -> u32 {
        self.context.device.limits().max_texture_dimension_2d
    }

    /// Change the master render resolution. Resizes all textures in the pipeline.
    ///
    /// Zero dimensions are rejected. Larger-than-hardware requests are clamped to
    /// the GPU's `max_texture_dimension_2d` so both the UI and the HTTP API share
    /// the same bound and neither can trigger a GPU allocation failure.
    pub fn set_render_resolution(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            log::warn!("Ignoring zero render resolution {width}×{height}");
            return;
        }
        let max_dim = self.max_render_dimension();
        let (width, height) = clamp_resolution_to_gpu(width, height, max_dim);
        if width == self.render_width && height == self.render_height {
            return;
        }
        log::info!(
            "Changing render resolution: {}×{} → {}×{}",
            self.render_width,
            self.render_height,
            width,
            height
        );
        let from = (self.render_width, self.render_height);
        self.render_width = width;
        self.render_height = height;
        let buses_were = self.production_buses.picture_sizes(from);
        self.production_buses
            .rescale_canvases(from, (width, height));
        let buses_now = self.production_buses.picture_sizes((width, height));
        self.rescale_led_parts(from, (width, height), &buses_were, &buses_now);
        self.mixer.resize(&self.context, width, height);
        // Screen captures follow the show's size, as when they were added:
        // each capture once, whatever number of inputs share it. A PowerPoint
        // show's is its own, never shared with these (see
        // `ScreenCaptureManager::adopt`), and keeps its cap.
        let mut captures = std::collections::BTreeSet::new();
        for channel in self.mixer.channels_mut() {
            for slot in &mut channel.decks {
                captures.extend(slot.deck.cap_capture_to_show(width, height));
            }
        }
        for id in captures {
            self.screen_capture_manager.cap_to(id, Some((width, height)));
        }
        // Clear sub-mix cache since textures were recreated
        self.mixer.clear_sub_mix_cache();
        let stopped = self.resize_headless_outputs(width, height);
        self.session
            .notifications
            .info(format!("📐 Resolution changed to {width}×{height}"));
        if !stopped.is_empty() {
            self.session.notifications.warn(format!(
                "⏹ Stopped {} to resize: {}",
                if stopped.len() == 1 {
                    "an output".to_string()
                } else {
                    format!("{} outputs", stopped.len())
                },
                stopped.join(", ")
            ));
        }
    }

    /// Current target FPS (0 = uncapped).
    pub fn target_fps(&self) -> u32 {
        self.target_fps
    }

    /// How many frames the GPU may still be drawing when the next starts:
    /// at 1 a frame starts once the GPU has finished the one before, at 2
    /// the next is prepared while the GPU draws the last.
    pub fn frames_in_flight(&self) -> u32 {
        self.frames_in_flight
    }

    /// What starts each frame (see [`crate::engine::value::render::FramePacing`]).
    pub fn frame_pacing(&self) -> crate::engine::value::render::FramePacing {
        self.frame_pacing
    }

    pub fn set_frame_pacing(&mut self, pacing: crate::engine::value::render::FramePacing) {
        if pacing != self.frame_pacing {
            log::info!("Frame pacing: {pacing:?}");
        }
        self.frame_pacing = pacing;
    }

    /// Whether frames are asked for by the refresh each display-link
    /// callback is stamped for (see the `SetClockTimestamps` command).
    pub fn clock_timestamps(&self) -> bool {
        self.clock_timestamps
    }

    pub fn set_clock_timestamps(&mut self, on: bool) {
        if on != self.clock_timestamps {
            log::info!("Frames asked for by refresh timestamps: {on}");
        }
        self.clock_timestamps = on;
    }

    /// Whether each frame is held on screen for a frame's time — macOS's own
    /// pacing (Metal's `presentDrawable:afterMinimumDuration:`), which on a
    /// variable-refresh screen keeps every frame up equally long whatever its
    /// refresh signal does — and what moves by whole frames steps a frame
    /// per frame.
    pub fn frame_hold(&self) -> bool {
        self.frame_hold
    }

    pub fn set_frame_hold(&mut self, on: bool) {
        if on != self.frame_hold {
            log::info!("Each frame held on screen a frame's time: {on}");
        }
        self.frame_hold = on;
    }

    /// Whether the control window presents in step with its screen's refresh
    /// (see the `SetControlVsync` command).
    pub fn control_vsync(&self) -> bool {
        self.control_vsync
    }

    pub fn set_control_vsync(&mut self, on: bool) {
        self.control_vsync = on;
    }

    /// Whether the control window's screen is set to the show's rate while
    /// Nida runs (see `crate::screen_rate` and `SetControlScreenRate`).
    pub fn control_screen_rate(&self) -> bool {
        self.control_screen_rate
    }

    pub fn set_control_screen_rate(&mut self, on: bool) {
        if on != self.control_screen_rate {
            log::info!("The control window's screen set to the show's rate: {on}");
        }
        self.control_screen_rate = on;
    }

    /// Whether Night Shift is kept off while an output window is on a
    /// screen (see `crate::night_shift` and `SetNightShiftOff`).
    pub fn night_shift_off(&self) -> bool {
        self.night_shift_off
    }

    pub fn set_night_shift_off(&mut self, on: bool) {
        if on != self.night_shift_off {
            log::info!("Night Shift kept off while an output is on a screen: {on}");
        }
        self.night_shift_off = on;
    }

    /// Tell the log, and the operator the first time, what was done to
    /// Night Shift (see `crate::night_shift`).
    pub fn note_night_shift(&mut self, kept: crate::night_shift::Kept) {
        use crate::night_shift::Kept;
        match kept {
            Kept::TurnedOff { first: true } => {
                let text = "Night Shift is off while outputs are on screens: macOS warms every \
                     screen with it, the outputs too, and lets no app leave one out, so this \
                     screen is not warmed either. Nida turns it back on when no output is on a \
                     screen, if it is due on then. Turn this off in the frame-rate menu (Keep \
                     Night Shift off for the outputs).";
                log::info!("{text}");
                self.session.notifications.info(text);
            }
            Kept::TurnedOff { first: false } => {
                log::info!("Night Shift came on while outputs were on screens: turned off again");
            }
            Kept::GivenBack { on: true } => log::info!("Night Shift given back: on"),
            Kept::GivenBack { on: false } => {
                log::info!("Night Shift given back: off until its schedule turns it on");
            }
        }
    }

    /// Whether a screen's mode changed since asked: the show's thread then
    /// looks at its clock again at once — a screen now refreshing at another
    /// rate may be the one frames follow.
    pub fn take_clock_recheck(&mut self) -> bool {
        std::mem::take(&mut self.clock_recheck)
    }

    /// Tell the log, and the operator once, what was done to the control
    /// window's screen (see `crate::screen_rate`).
    pub fn note_screen_rate(&mut self, kept: &crate::screen_rate::Kept) {
        self.clock_recheck = true;
        match kept {
            crate::screen_rate::Kept::Set { display, from, to } => {
                let text = format!(
                    "This screen now refreshes at {to:.0} Hz, the show's rate (it was {from:.0} Hz), \
                     for as long as Nida runs: at a faster rate macOS missed frames on every \
                     screen while a screen was captured. macOS puts it back when Nida quits. \
                     Turn it off in the frame-rate menu (Keep this screen at the show's rate)."
                );
                log::info!("Control window's screen (display {display}): {text}");
                self.session.notifications.info(text);
            }
            crate::screen_rate::Kept::SetAgain { display, found, to } => {
                log::info!(
                    "Control window's screen (display {display}) found at {found:.0} Hz, set to \
                     {to:.0} Hz again (macOS gives screens back their own refresh as screens \
                     come and go)"
                );
            }
            crate::screen_rate::Kept::GivenBack { display, rate } => {
                let at = rate.map(|hz| format!(", {hz:.0} Hz")).unwrap_or_default();
                log::info!(
                    "The control window's screen (display {display}) was given back its own \
                     refresh{at}"
                );
            }
        }
    }

    /// Whether a screen's refresh starts each frame (see `check_display_clock`):
    /// then each is shown once, in turn, and what moves with time — a
    /// playlist's slide — moves one frame a frame. Not whether frames are
    /// held on screen: turning the hold off once brought back a slow slide's
    /// double steps (a jittery clock rounded to two frames, then none) while
    /// it cut an LED wall's missed refreshes (a laptop, 2026-10-02).
    pub fn frames_follow_a_screen(&self) -> bool {
        self.target_fps > 0 && self.frame_stats.clock_display.is_some()
    }

    /// How long each frame is held on screen, when it is: a frame's time —
    /// only while a screen's refresh starts frames, the rate a whole number
    /// of its refreshes. Held a frame's time where the rate does not fit (50
    /// on a 60 Hz screen), each frame would stay up two refreshes: half the
    /// rate.
    pub fn frame_hold_secs(&self) -> Option<f64> {
        (self.frame_hold && self.target_fps > 0 && self.frame_stats.clock_display.is_some())
            .then(|| 1.0 / f64::from(self.target_fps))
    }

    /// The display link's figures, for the performance figures.
    pub fn set_display_link_stats(&mut self, stats: Option<crate::engine::DisplayLinkSnapshot>) {
        self.frame_stats.display_link = stats;
    }

    /// How evenly the control window's frames reached its screen, and its
    /// queue of frames waiting to be shown.
    pub fn set_control_on_screen(
        &mut self,
        stats: crate::engine::OnScreenStats,
        queue: crate::engine::ControlQueueStats,
    ) {
        self.frame_stats.control_on_screen = stats;
        self.frame_stats.control_queue = queue;
    }

    /// Where the control window's frames' time goes, and its slow frames
    /// (it draws on the main thread, apart from the engine).
    /// How the control window's monitors stepped through the show's frames.
    pub fn set_control_monitors(&mut self, monitors: crate::engine::ControlMonitorsStats) {
        self.frame_stats.control_monitors = monitors;
    }

    pub fn set_control_parts(
        &mut self,
        parts: Vec<crate::engine::PartStats>,
        slow: Vec<crate::engine::SlowFrame>,
    ) {
        self.frame_stats.control_parts = parts;
        self.frame_stats.control_slow_frames = slow;
    }

    /// How often the control window is drawn now — the runner says the
    /// wait, the hands and the widgets — with what moves in the show and
    /// why, worked out here.
    pub fn set_control_rate(&mut self, period_ms: f32, hands_on: bool, widgets_ms: Option<f32>) {
        let (motion, why) = self.ui_motion_why();
        self.frame_stats.control_rate = crate::engine::ControlRateStats {
            motion: match motion {
                crate::app::state::ui_motion::UiMotion::Pictures => "pictures",
                crate::app::state::ui_motion::UiMotion::Sound => "sound",
                crate::app::state::ui_motion::UiMotion::Still => "still",
            }
            .to_string(),
            why,
            period_ms,
            hands_on,
            widgets_ms,
        };
    }

    /// Whether an output window showed the last frame: its screen's refresh
    /// can then hold the frame loop back when frames are drawn ahead.
    pub fn output_windows_presenting(&self) -> bool {
        self.frame_stats.windows_presented
    }

    pub fn set_frames_in_flight(&mut self, frames: u32) {
        let frames = frames.clamp(1, 3);
        if frames != self.frames_in_flight {
            log::info!("Frames in flight: {frames}");
        }
        self.frames_in_flight = frames;
    }

    /// Set the target FPS. 0 = uncapped.
    pub fn set_target_fps(&mut self, fps: u32) {
        if fps == self.target_fps {
            return;
        }
        log::info!("Target FPS: {} → {}", self.target_fps, fps);
        self.target_fps = fps;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Parse `AppConfig` from a simulated CLI invocation.
    fn parse_args(args: &[&str]) -> AppConfig {
        AppConfig::parse_from(std::iter::once("varda").chain(args.iter().copied()))
    }

    #[test]
    fn app_config_defaults() {
        let config = parse_args(&[]);
        assert!(!config.headless);
        assert_eq!(config.api_port, 8080);
        assert_eq!(config.target_fps, 60);
        assert!(config.workspace_root.is_none());
        assert!(config.scene_path.is_none());
        assert!(config.stage_path.is_none());
        assert!(config.osc_port.is_none());
        assert!(config.osc_targets.is_empty());
        assert!(!config.osc_disabled);
        assert!(!config.ndi_disabled);
        assert!(!config.syphon_disabled);
        assert!(config.shader_dirs.is_empty());
    }

    #[test]
    fn app_config_shader_dirs_repeatable() {
        let config = parse_args(&[
            "--shader-dir",
            "/srv/shaders/show-a",
            "--shader-dir",
            "/media/usb/shaders",
        ]);
        assert_eq!(
            config.shader_dirs,
            vec![
                std::path::PathBuf::from("/srv/shaders/show-a"),
                std::path::PathBuf::from("/media/usb/shaders"),
            ]
        );
    }

    /// An app bundle, opened from the Finder, passes no flags: it sets these
    /// in its environment instead.
    #[test]
    fn port_workspace_and_shader_folders_can_come_from_the_environment() {
        use clap::CommandFactory;
        let command = AppConfig::command();
        let env_of = |id: &str| {
            command
                .get_arguments()
                .find(|a| a.get_id() == id)
                .and_then(|a| a.get_env())
                .map(|e| e.to_string_lossy().into_owned())
        };
        assert_eq!(env_of("api_port").as_deref(), Some("VARDA_PORT"));
        assert_eq!(env_of("workspace_root").as_deref(), Some("VARDA_WORKSPACE"));
        assert_eq!(env_of("shader_dirs").as_deref(), Some("VARDA_SHADER_DIRS"));
        // Several shader folders in one variable, as a PATH lists them.
        let config = parse_args(&["--shader-dir", "/a/shaders:/b/shaders"]);
        assert_eq!(
            config.shader_dirs,
            vec![
                std::path::PathBuf::from("/a/shaders"),
                std::path::PathBuf::from("/b/shaders")
            ]
        );
    }

    #[test]
    fn app_config_headless_with_port() {
        let config = parse_args(&["--headless", "--port", "3030", "--fps", "30"]);
        assert!(config.headless);
        assert_eq!(config.api_port, 3030);
        assert_eq!(config.target_fps, 30);
    }

    #[test]
    fn app_config_osc_flags() {
        let config = parse_args(&[
            "--osc-port",
            "7000",
            "--osc-out",
            "192.168.1.1:8000",
            "--osc-out",
            "10.0.0.1:9000",
        ]);
        assert_eq!(config.osc_port, Some(7000));
        assert_eq!(
            config.osc_targets,
            vec!["192.168.1.1:8000", "10.0.0.1:9000"]
        );
        assert!(!config.osc_disabled);
    }

    #[test]
    fn app_config_disable_flags() {
        let config = parse_args(&["--no-osc", "--no-ndi", "--no-syphon"]);
        assert!(config.osc_disabled);
        assert!(config.ndi_disabled);
        assert!(config.syphon_disabled);
    }

    #[test]
    fn workspace_resolution_explicit_flag_wins() {
        let explicit = std::path::PathBuf::from("/tmp/show");
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join(".varda")).unwrap();
        let home = tempfile::tempdir().unwrap();

        let result = AppConfig::resolve_workspace_root(
            Some(explicit.as_path()),
            Some(cwd.path()),
            Some(home.path()),
        );
        assert_eq!(result, explicit);
    }

    #[test]
    fn workspace_resolution_cwd_with_varda_dir() {
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join(".varda")).unwrap();
        let home = tempfile::tempdir().unwrap();

        let result = AppConfig::resolve_workspace_root(None, Some(cwd.path()), Some(home.path()));
        assert_eq!(result, cwd.path());
    }

    #[test]
    fn workspace_resolution_falls_back_to_home() {
        let cwd = tempfile::tempdir().unwrap(); // no .varda/ dir
        let home = tempfile::tempdir().unwrap();

        let result = AppConfig::resolve_workspace_root(None, Some(cwd.path()), Some(home.path()));
        assert_eq!(result, home.path());
    }

    #[test]
    fn workspace_resolution_no_cwd_no_home() {
        let result = AppConfig::resolve_workspace_root(None, None, None);
        assert_eq!(result, std::path::PathBuf::from("."));
    }

    #[test]
    fn workspace_resolution_explicit_overrides_cwd_with_varda() {
        let explicit = std::path::PathBuf::from("/tmp/custom");
        let cwd = tempfile::tempdir().unwrap();
        std::fs::create_dir(cwd.path().join(".varda")).unwrap();

        let result =
            AppConfig::resolve_workspace_root(Some(explicit.as_path()), Some(cwd.path()), None);
        assert_eq!(result, explicit);
    }

    #[test]
    fn app_config_clone() {
        let config = parse_args(&["--headless", "--port", "3030", "--no-ndi"]);
        let cloned = config.clone();
        assert!(cloned.headless);
        assert_eq!(cloned.api_port, 3030);
        assert!(cloned.ndi_disabled);
    }

    // ── Engine smoke tests ──────────────────────────────────────

    fn headless_app() -> Option<VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        let config = crate::testing::headless_config();
        VardaApp::new(gpu, &config).ok()
    }

    fn channel_uuid(app: &VardaApp, idx: usize) -> String {
        app.build_engine_state().mixer.channels[idx].uuid.clone()
    }

    /// A show opened at 4K caps its screen captures at 4K, whatever the
    /// screen's size in points: a 4K screen set to "looks like 1920×1080" is
    /// 1920×1080 points, and capped at those it was captured at 1920×1080
    /// and blurred on the wall.
    #[test]
    fn a_capture_restored_in_a_4k_show_is_capped_at_4k() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        app.screen_capture_manager =
            crate::screen_capture::ScreenCaptureManager::disabled_with_targets(vec![
                crate::screen_capture::backend::CaptureTargetInfo {
                    kind: crate::screen_capture::backend::CaptureTargetKind::Display,
                    platform_id: 1,
                    label: "Display 1".into(),
                    app: None,
                    title: None,
                    width: 1920,
                    height: 1080,
                    is_varda: false,
                    uuid: None,
                },
            ]);
        let config: crate::scene::DeckConfig = serde_json::from_value(serde_json::json!({
            "name": "Display 1",
            "source": {
                "type": "ScreenCapture",
                "target": { "kind": "display", "name": "Display 1" }
            }
        }))
        .expect("a saved capture");
        let deck = crate::persistence::restore_deck(
            &config,
            &app.context,
            &app.registry,
            &mut app.camera_manager,
            &mut app.screen_capture_manager,
            &mut app.depth_manager,
            &mut app.external_io.ndi_manager,
            &mut app.external_io.stream_manager,
            &mut app.external_io.html_manager,
            3840,
            2160,
        )
        .expect("restored");
        let state = deck.screen_capture.as_ref().expect("a capture");
        assert_eq!(state.config.scale_to, Some((3840, 2160)));
    }

    /// A desk's screens as macOS lists them — each `(uuid, width)` under
    /// "Display N" by its place — every one captured already (a mock for
    /// macOS's capture), so an input opening one shares it.
    fn with_screens(app: &mut VardaApp, screens: &[(&str, u32)]) {
        use crate::screen_capture::backend::{CaptureTargetInfo, CaptureTargetKind};
        let targets: Vec<_> = screens
            .iter()
            .zip(1u32..)
            .map(|((uuid, width), n)| CaptureTargetInfo {
                kind: CaptureTargetKind::Display,
                platform_id: u64::from(n),
                label: format!("Display {n}"),
                app: None,
                title: None,
                width: *width,
                height: width * 9 / 16,
                is_varda: false,
                uuid: Some((*uuid).to_string()),
            })
            .collect();
        with_targets(app, &targets);
    }

    /// A desk with `targets` to capture, each captured already.
    fn with_targets(
        app: &mut VardaApp,
        targets: &[crate::screen_capture::backend::CaptureTargetInfo],
    ) {
        use crate::screen_capture::backend::CaptureConfig;
        app.screen_capture_manager =
            crate::screen_capture::ScreenCaptureManager::enabled_with_targets(targets.to_vec());
        for target in targets {
            app.screen_capture_manager
                .open_mock(target, CaptureConfig::default(), &app.context.device)
                .expect("open");
        }
    }

    /// What the input a show saved as capturing `target` captures once the
    /// show is opened: the screen's name now, and what the show saves of it
    /// from then on.
    fn restored_capture(
        app: &mut VardaApp,
        target: &serde_json::Value,
    ) -> (String, serde_json::Value) {
        let deck = restored_capture_deck(app, target);
        let state = deck.screen_capture.as_ref().expect("a capture");
        let captured = app
            .screen_capture_manager
            .label(state.capture_id)
            .expect("bound")
            .to_string();
        (captured, saved_target(&deck))
    }

    /// What a show saves of the screen or window `deck` captures.
    fn saved_target(deck: &crate::deck::Deck) -> serde_json::Value {
        let state = deck.screen_capture.as_ref().expect("a capture");
        serde_json::to_value(crate::scene::CaptureTargetConfig::from(&state.identity))
            .expect("serialize")
    }

    /// The input a show saved as capturing `target`, as the show opened
    /// gives it.
    fn restored_capture_deck(
        app: &mut VardaApp,
        target: &serde_json::Value,
    ) -> crate::deck::Deck {
        restored_input(
            app,
            "Screen",
            &serde_json::json!({ "type": "ScreenCapture", "target": target }),
        )
    }

    /// The input a show saved under `name` with `source`, as the show
    /// opened gives it.
    fn restored_input(
        app: &mut VardaApp,
        name: &str,
        source: &serde_json::Value,
    ) -> crate::deck::Deck {
        let config: crate::scene::DeckConfig =
            serde_json::from_value(serde_json::json!({ "name": name, "source": source }))
                .expect("a saved input");
        crate::persistence::restore_deck(
            &config,
            &app.context,
            &app.registry,
            &mut app.camera_manager,
            &mut app.screen_capture_manager,
            &mut app.depth_manager,
            &mut app.external_io.ndi_manager,
            &mut app.external_io.stream_manager,
            &mut app.external_io.html_manager,
            1920,
            1080,
        )
        .expect("restored")
    }

    /// A show saved its capture of the 4K screen as "Display 1". Opened with
    /// one more screen plugged in — macOS lists that one first, the 4K as
    /// "Display 2" (an M5 Max, 2026-11-09) — the input still captures the
    /// 4K, and the show saves it under the name it has now. Its own screen
    /// not plugged in, the one of its name stands in, and the show goes on
    /// saving the screen it was made with.
    #[test]
    fn a_captured_screen_restored_is_the_one_saved_whatever_its_number() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);

        let saved = serde_json::json!({ "kind": "display", "name": "Display 1", "uuid": "4k" });
        let (captured, saved) = restored_capture(&mut app, &saved);
        assert_eq!(captured, "Display 2", "the 4K, wherever it is listed");
        assert_eq!(
            saved,
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );

        let away = serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "led" });
        let (captured, saved) = restored_capture(&mut app, &away);
        assert_eq!(captured, "Display 2");
        assert_eq!(saved, away);
    }

    /// A show saved before screens were kept by identity has the name alone:
    /// it captures the screen of that name, and goes on saving the name
    /// alone. Made of the 4K when that was "Display 1", opened with a
    /// capture box plugged in and listed first, it must not become the
    /// box's own screen for good by being opened and closed (a copy of a
    /// show, an M5 Max, 2026-11-10).
    #[test]
    fn a_screen_saved_by_name_alone_stays_so_until_its_screen_is_picked() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let old = serde_json::json!({ "kind": "display", "name": "Display 1" });
        let (captured, saved) = restored_capture(&mut app, &old);
        assert_eq!(captured, "Display 1", "the one of that name today");
        assert_eq!(saved, old, "and no screen's identity written for it");
    }

    /// The input of `uuid`.
    fn deck_of<'a>(app: &'a VardaApp, uuid: &str) -> &'a crate::internal::deck::Deck {
        let (ch, dk) = app.mixer.find_deck_by_uuid(uuid).expect("the input");
        &app.mixer.channels()[ch].decks[dk].deck
    }

    /// The capture of the input at (channel, place).
    fn capture_state_at(
        app: &mut VardaApp,
        at: (usize, usize),
    ) -> &mut crate::internal::deck::ScreenCaptureState {
        app.mixer.channels_mut()[at.0].decks[at.1]
            .deck
            .screen_capture
            .as_mut()
            .expect("a capture")
    }

    /// The screen capture inputs of `app`, as the window and the API are
    /// told of them, by layer.
    fn capture_snapshot(app: &VardaApp, uuid: &str) -> crate::engine::ScreenCaptureDeckSnapshot {
        snapshot::build_mixer_snapshot(app)
            .channels
            .into_iter()
            .flat_map(|channel| channel.decks)
            .find(|deck| deck.uuid == uuid)
            .and_then(|deck| deck.screen_capture)
            .expect("a capture layer")
    }

    /// The same, as the control window's settings are given it.
    fn capture_in_the_window(app: &VardaApp, uuid: &str) -> crate::usecases::ui::ScreenCaptureUI {
        let none = std::collections::HashMap::new();
        crate::usecases::ui::build_ui_data(
            app,
            &crate::usecases::ui::UILayoutState::default(),
            &std::collections::HashMap::new(),
            &none,
            &std::collections::HashMap::new(),
            None,
        )
        .channels
        .into_iter()
        .flat_map(|channel| channel.decks)
        .find(|deck| deck.uuid == uuid)
        .and_then(|deck| deck.screen_capture)
        .expect("a capture layer")
    }

    /// An input a show has by its display's name alone says so, and its
    /// screen picked — the 4K, "Display 2" today — it captures that one and
    /// is kept by its identity; named after its screen, it is named after
    /// the new one. Picked by name alone, as an API client may, it is kept
    /// by identity too.
    #[test]
    fn a_screen_picked_again_is_captured_and_kept_by_its_identity() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let old = serde_json::json!({ "kind": "display", "name": "Display 1" });
        let deck = restored_capture_deck(&mut app, &old);
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);
        let captured = |app: &VardaApp| {
            let id = deck_of(app, &uuid).screen_capture_id().expect("a capture");
            app.screen_capture_manager
                .label(id)
                .map(str::to_string)
        };
        let before = capture_snapshot(&app, &uuid);
        assert!(before.kept_by_name, "the show has it by its number");
        assert_eq!(before.target_uuid, None);
        let shown = capture_in_the_window(&app, &uuid);
        assert!(shown.kept_by_name, "and its settings say so");
        assert_eq!(shown.target_uuid, None);

        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&uuid, &the_4k).expect("picked");
        assert_eq!(captured(&app).as_deref(), Some("Display 2"));
        assert_eq!(
            saved_target(deck_of(&app, &uuid)),
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );
        assert_eq!(deck_of(&app, &uuid).source_name(), "🖥 Display 2");
        assert_eq!(deck_of(&app, &uuid).source_size(), Some([3840, 2160]), "the 4K's picture");
        let after = capture_snapshot(&app, &uuid);
        assert!(!after.kept_by_name);
        assert_eq!(after.target_uuid.as_deref(), Some("4k"));
        let shown = capture_in_the_window(&app, &uuid);
        assert!(!shown.kept_by_name);
        assert_eq!(shown.target_uuid.as_deref(), Some("4k"));

        // By its name alone: the one of that name now, kept as itself.
        let by_name = crate::scene::CaptureTargetConfig::Display {
            name: "Display 1".into(),
            uuid: None,
        };
        app.set_screen_capture_target(&uuid, &by_name).expect("picked");
        assert_eq!(captured(&app).as_deref(), Some("Display 1"));
        assert_eq!(
            saved_target(deck_of(&app, &uuid)),
            serde_json::json!({ "kind": "display", "name": "Display 1", "uuid": "capture" })
        );
    }

    /// How many times it has been said that inputs go by a display's
    /// number, and the last of them.
    fn said_by_number(app: &VardaApp) -> (usize, String) {
        let said: Vec<_> = app
            .session
            .notifications
            .history()
            .iter()
            .filter(|n| n.message.contains("by a display's number"))
            .collect();
        let last = said.last().map(|n| n.message.clone()).unwrap_or_default();
        (said.len(), last)
    }

    /// A show's inputs kept by a display's number alone are said once, as a
    /// warning, with the numbers they go by — not every frame, not when one
    /// has its screen picked, and again when another such input comes. One
    /// kept by its screen's identity is not among them.
    #[test]
    fn inputs_kept_by_a_displays_number_are_said_once() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let old = serde_json::json!({ "kind": "display", "name": "Display 1" });
        let add = |app: &mut VardaApp, target: &serde_json::Value| {
            let deck = restored_capture_deck(app, target);
            let uuid = deck.uuid().to_string();
            app.mixer.channels_mut()[0].add_deck(deck);
            uuid
        };
        let first = add(&mut app, &old);
        add(&mut app, &old);
        add(
            &mut app,
            &serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" }),
        );
        assert_eq!(said_by_number(&app).0, 0);

        app.warn_captures_by_name();
        let (times, said) = said_by_number(&app);
        assert_eq!(times, 1);
        assert!(
            said.starts_with(
                "2 screen inputs go by a display's number (Display 1), not the screen itself: \
                 they show whichever screen has that number today."
            ),
            "{said}"
        );
        let level = app.session.notifications.history().last().map(|n| n.level);
        assert_eq!(level, Some(crate::notifications::NotificationLevel::Warning));
        for _ in 0..3 {
            app.warn_captures_by_name();
        }
        assert_eq!(said_by_number(&app).0, 1, "not every frame");

        // One's screen picked: one fewer, and nothing to say of that.
        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&first, &the_4k).expect("picked");
        app.warn_captures_by_name();
        assert_eq!(said_by_number(&app).0, 1, "one picked");

        // Another comes — a show opened into this one: said, of those there
        // are now.
        add(
            &mut app,
            &serde_json::json!({ "kind": "display", "name": "Display 2" }),
        );
        app.warn_captures_by_name();
        let (times, said) = said_by_number(&app);
        assert_eq!(times, 2, "a new one");
        assert!(
            said.starts_with("2 screen inputs go by a display's number (Display 1, Display 2)"),
            "{said}"
        );
    }

    /// With one screen there is no other to have its number: nothing is
    /// said of an input kept by it until a second screen is there.
    #[test]
    fn an_input_by_a_displays_number_is_not_spoken_of_with_one_screen() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("laptop", 1728)]);
        let old = serde_json::json!({ "kind": "display", "name": "Display 1" });
        let deck = restored_capture_deck(&mut app, &old);
        app.mixer.channels_mut()[0].add_deck(deck);
        app.warn_captures_by_name();
        assert_eq!(said_by_number(&app).0, 0, "the only screen there is");

        with_screens(&mut app, &[("laptop", 1728), ("projector", 1920)]);
        app.warn_captures_by_name();
        let (times, said) = said_by_number(&app);
        assert_eq!(times, 1);
        assert!(
            said.starts_with(
                "1 screen input goes by a display's number (Display 1), not the screen itself: \
                 it shows whichever screen has that number today. Check it, and pick its screen"
            ),
            "{said}"
        );
    }

    /// A screen picked for an input lets go of the capture it held, and
    /// leaves a name given it as it was. Its own screen picked again holds
    /// its capture once, not twice, a setting not yet sent down still its
    /// own; another screen's capture, running already, is shared as it runs.
    #[test]
    fn a_screen_picked_lets_go_of_the_capture_the_input_held() {
        use crate::engine::MixerCommands;
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let display = |name: &str, uuid: &str| crate::scene::CaptureTargetConfig::Display {
            name: name.into(),
            uuid: Some(uuid.into()),
        };
        let ch0 = channel_uuid(&app, 0);
        let uuid = app
            .add_screen_capture_deck(&ch0, &display("Display 1", "capture"), CaptureConfig::default())
            .expect("added");
        let (ch, dk) = app.mixer.find_deck_by_uuid(&uuid).expect("the input");
        {
            let deck = &mut app.mixer.channels_mut()[ch].decks[dk].deck;
            deck.set_source_name("Slides".into());
            assert!(deck.set_capture_param("crop_w", 0.5));
        }
        let first = app.mixer.channels()[ch].decks[dk]
            .deck
            .screen_capture_id()
            .expect("a capture");
        // The desk's mock, and the input.
        assert_eq!(app.screen_capture_manager.holders(first), 2);

        app.set_screen_capture_target(&uuid, &display("Display 1", "capture"))
            .expect("picked");
        let deck = &app.mixer.channels()[ch].decks[dk].deck;
        assert_eq!(deck.screen_capture_id(), Some(first), "its own");
        assert_eq!(app.screen_capture_manager.holders(first), 2, "held once");
        let state = deck.screen_capture.as_ref().expect("a capture");
        assert!((state.config.crop.w - 0.5).abs() < 1e-6, "its crop");
        assert_eq!(state.given.len(), 1, "still to be sent down");

        app.set_screen_capture_target(&uuid, &display("Display 2", "4k"))
            .expect("picked");
        let deck = &app.mixer.channels()[ch].decks[dk].deck;
        let second = deck.screen_capture_id().expect("a capture");
        assert_ne!(second, first);
        assert_eq!(app.screen_capture_manager.holders(first), 1, "let go");
        assert_eq!(app.screen_capture_manager.holders(second), 2);
        let state = deck.screen_capture.as_ref().expect("a capture");
        let runs = app
            .screen_capture_manager
            .config(second)
            .expect("running");
        assert_eq!(
            state.config,
            CaptureConfig {
                scale_to: Some((app.render_width, app.render_height)),
                ..runs
            },
            "as the capture it shares runs, capped as the show is"
        );
        assert!(state.given.is_empty(), "nothing of its own to send down");
        assert_eq!(deck.source_name(), "Slides", "a name given it stays");

        // Its own picked again with nothing changed: nothing to send.
        app.set_screen_capture_target(&uuid, &display("Display 2", "4k"))
            .expect("picked");
        let state = app.mixer.channels()[ch].decks[dk]
            .deck
            .screen_capture
            .as_ref()
            .expect("a capture");
        assert!(state.given.is_empty(), "nothing changed");
        // And nothing goes down: the crop it had on the first is not the
        // second's.
        draw_a_frame(&mut app);
        let runs = app
            .screen_capture_manager
            .config(second)
            .expect("running");
        assert!(runs.crop.is_full_frame(), "as it ran: {:?}", runs.crop);
    }

    /// An input given a screen another input captures already sends none
    /// of its own settings down to it: a crop that one had set the same
    /// frame, not yet sent, was written over by the one that joined, and
    /// the first showed the whole screen with half of it set. It shows and
    /// saves that crop as its own from then on, so that something else
    /// changed on it — its whole settings sent — does not put the whole
    /// screen back either.
    #[test]
    fn a_screen_shared_keeps_the_crop_its_first_input_set() {
        use crate::engine::MixerCommands;
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let display = |name: &str, uuid: &str| crate::scene::CaptureTargetConfig::Display {
            name: name.into(),
            uuid: Some(uuid.into()),
        };
        let ch0 = channel_uuid(&app, 0);
        let first = app
            .add_screen_capture_deck(&ch0, &display("Display 1", "capture"), CaptureConfig::default())
            .expect("added");
        let joins = app
            .add_screen_capture_deck(&ch0, &display("Display 2", "4k"), CaptureConfig::default())
            .expect("added");
        let (ch, dk) = app.mixer.find_deck_by_uuid(&first).expect("the input");
        let (_, joins_at) = app.mixer.find_deck_by_uuid(&joins).expect("the input");
        assert!(dk < joins_at, "the first is sent down first");
        let deck = &mut app.mixer.channels_mut()[ch].decks[dk].deck;
        assert!(deck.set_capture_param("crop_w", 0.5));
        let capture = deck.screen_capture_id().expect("a capture");

        app.set_screen_capture_target(&joins, &display("Display 1", "capture"))
            .expect("picked");
        app.update_frame_timing();
        app.render_mixer_frame();
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!((runs.crop.w - 0.5).abs() < 1e-6, "{:?}", runs.crop);

        let joined = capture_state_at(&mut app, (ch, joins_at));
        assert_eq!(joined.capture_id, capture);
        assert!(
            (joined.config.crop.w - 0.5).abs() < 1e-6,
            "the crop it is captured with: {:?}",
            joined.config.crop
        );
        set_capture(&mut app, (ch, joins_at), "cursor", 1.0);
        app.update_frame_timing();
        app.render_mixer_frame();
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!((runs.crop.w - 0.5).abs() < 1e-6, "{:?}", runs.crop);
        assert!(runs.show_cursor, "what it changed is taken");
        let first = capture_state_at(&mut app, (ch, dk));
        assert!(first.config.show_cursor, "and the first shows it too");
    }

    /// Set the capture setting `name` of the input at (channel, place), as
    /// its parameter path does.
    fn set_capture(app: &mut VardaApp, at: (usize, usize), name: &str, value: f32) {
        let deck = &mut app.mixer.channels_mut()[at.0].decks[at.1].deck;
        assert!(deck.set_capture_param(name, value), "a capture setting");
    }

    /// An input of the only screen of a desk (`with_screens`) added to the
    /// first channel with settings `asked`: where it is.
    fn screen_input(
        app: &mut VardaApp,
        asked: crate::screen_capture::backend::CaptureConfig,
    ) -> (usize, usize) {
        use crate::engine::MixerCommands;
        let screen = crate::scene::CaptureTargetConfig::Display {
            name: "Display 1".into(),
            uuid: Some("capture".into()),
        };
        let ch0 = channel_uuid(app, 0);
        let uuid = app
            .add_screen_capture_deck(&ch0, &screen, asked)
            .expect("added");
        app.mixer.find_deck_by_uuid(&uuid).expect("the input")
    }

    /// Two inputs of one screen changed the same frame — one its crop, the
    /// other its cursor: the capture they share takes both, and both show
    /// and save the same. Each sent its settings whole, and the second put
    /// back the whole screen the first had just cropped.
    #[test]
    fn two_inputs_of_one_screen_changed_at_once_keep_both_changes() {
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let first = screen_input(&mut app, CaptureConfig::default());
        let second = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, first).capture_id;
        assert_eq!(capture_state_at(&mut app, second).capture_id, capture);

        // Each input's cap is the show's; the capture's, as the desk's own
        // opened it: none.
        let show = Some((app.render_width, app.render_height));
        for at in [first, second] {
            assert_eq!(capture_state_at(&mut app, at).config.scale_to, show);
        }
        set_capture(&mut app, first, "crop_w", 0.5);
        set_capture(&mut app, second, "cursor", 1.0);
        app.update_frame_timing();
        app.render_mixer_frame();
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!(
            (runs.crop.w - 0.5).abs() < 1e-6,
            "the first's crop: {:?}",
            runs.crop
        );
        assert!(runs.show_cursor, "the second's cursor");
        assert_eq!(runs.scale_to, None, "its cap not an input's to set");
        for at in [first, second] {
            let state = capture_state_at(&mut app, at);
            assert!(state.given.is_empty(), "sent");
            assert_eq!(state.config.scale_to, show, "its cap kept: the show's");
            assert_eq!(
                state.config,
                CaptureConfig {
                    scale_to: show,
                    ..runs.clone()
                },
                "as its capture runs"
            );
        }
    }

    /// Two inputs of one screen given the same setting the same frame —
    /// its cursor shown on one, then hidden on the other: the capture they
    /// share is left as the last given says, whichever input that was, and
    /// both show and save so. Each input's settings were compared with
    /// what the capture ran with to find what it had changed, and a cursor
    /// hidden on a capture that ran without one was nothing changed: it
    /// stayed shown.
    #[test]
    fn two_inputs_of_one_screen_given_one_setting_keep_the_last_given() {
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let first = screen_input(&mut app, CaptureConfig::default());
        let second = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, first).capture_id;
        // A frame drawn: whether the capture shows the cursor, as both its
        // inputs then do.
        let cursor = |app: &mut VardaApp| {
            draw_a_frame(app);
            let runs = app
                .screen_capture_manager
                .config(capture)
                .expect("running")
                .show_cursor;
            for at in [first, second] {
                assert_eq!(
                    capture_state_at(app, at).config.show_cursor,
                    runs,
                    "as its capture runs"
                );
            }
            runs
        };
        // Either input the later given: the later counts, not its place.
        for (one, other) in [(first, second), (second, first)] {
            set_capture(&mut app, one, "cursor", 1.0);
            set_capture(&mut app, other, "cursor", 0.0);
            assert!(!cursor(&mut app), "hidden last");
            set_capture(&mut app, one, "cursor", 0.0);
            set_capture(&mut app, other, "cursor", 1.0);
            assert!(cursor(&mut app), "shown last");
            set_capture(&mut app, other, "cursor", 0.0);
            assert!(!cursor(&mut app), "hidden again");
        }
    }

    /// Two inputs of one screen given its crop the same frame — a width on
    /// one, a left edge on the other: the capture takes both as given, and
    /// both inputs show so. The input given the edge narrowed its own,
    /// older width to fit, which went down as a width changed: it took
    /// away the one the first was given.
    #[test]
    fn two_inputs_of_one_screen_given_its_crop_keep_each_side_given() {
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let first = screen_input(&mut app, CaptureConfig::default());
        let second = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, first).capture_id;

        set_capture(&mut app, first, "crop_w", 0.5);
        set_capture(&mut app, second, "crop_x", 0.25);
        draw_a_frame(&mut app);
        let crop = app
            .screen_capture_manager
            .config(capture)
            .expect("running")
            .crop;
        assert!(
            (crop.x - 0.25).abs() < 1e-6 && (crop.w - 0.5).abs() < 1e-6,
            "the edge and the width, each as given: {crop:?}"
        );
        for at in [first, second] {
            assert_eq!(
                capture_state_at(&mut app, at).config.crop,
                crop,
                "as its capture runs"
            );
        }
    }

    /// A setting given twice before its capture has it is given it twice,
    /// not once as last given: a crop's left edge moved far right narrows
    /// its width, and moved back leaves it narrowed — as on one input
    /// alone, a frame apart. On one input, and on one then the other.
    #[test]
    fn a_crop_edge_given_twice_in_a_frame_is_given_its_capture_twice() {
        use crate::screen_capture::backend::{CaptureConfig, CropRect};
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let first = screen_input(&mut app, CaptureConfig::default());
        let second = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, first).capture_id;
        for (one, other) in [(first, first), (first, second), (second, first)] {
            // The whole screen again.
            set_capture(&mut app, one, "crop_x", 0.0);
            set_capture(&mut app, one, "crop_w", 1.0);
            draw_a_frame(&mut app);
            let whole = app
                .screen_capture_manager
                .config(capture)
                .expect("running")
                .crop;
            assert!(whole.is_full_frame(), "{whole:?}");

            set_capture(&mut app, one, "crop_x", 0.75);
            set_capture(&mut app, other, "crop_x", 0.25);
            draw_a_frame(&mut app);
            let crop = app
                .screen_capture_manager
                .config(capture)
                .expect("running")
                .crop;
            assert_eq!(
                crop,
                CropRect {
                    x: 0.25,
                    y: 0.0,
                    w: 0.25,
                    h: 1.0
                },
                "narrowed, then moved back"
            );
            for at in [first, second] {
                assert_eq!(
                    capture_state_at(&mut app, at).config.crop,
                    crop,
                    "as its capture runs"
                );
            }
        }
    }

    /// A setting given an input whose screen is not there stays on the
    /// input, nothing of it kept to go down to whatever it is given next;
    /// and the screen it is then given, which nothing captures yet, is
    /// opened as the input is set.
    #[test]
    fn a_setting_given_an_input_whose_screen_is_gone_stays_on_it() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let gone = serde_json::json!({ "kind": "display", "name": "Display 5" });
        let deck = restored_capture_deck(&mut app, &gone);
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);
        let at = app.mixer.find_deck_by_uuid(&uuid).expect("the input");
        set_capture(&mut app, at, "crop_w", 0.5);
        // The desk's own captures, which no input shows, close with it.
        draw_a_frame(&mut app);
        let state = capture_state_at(&mut app, at);
        assert!(state.given.is_empty(), "nothing to go down");
        assert!(
            (state.config.crop.w - 0.5).abs() < 1e-6,
            "kept on it: {:?}",
            state.config.crop
        );

        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&uuid, &the_4k).expect("picked");
        let id = capture_of(&app, &uuid);
        assert_eq!(app.screen_capture_manager.holders(id), 1, "opened for it");
        let runs = app.screen_capture_manager.config(id).expect("running");
        assert!(
            (runs.crop.w - 0.5).abs() < 1e-6,
            "as the input is set: {:?}",
            runs.crop
        );
    }

    /// An input added of a screen another input captures already shares
    /// that capture as it runs, and shows and saves its rate, crop and
    /// cursor — not those asked for it, which were never sent: they went
    /// down whole at its first change, the other input's crop with them.
    #[test]
    fn an_input_added_of_a_screen_captured_takes_its_captures_settings() {
        use crate::screen_capture::backend::{CaptureConfig, DEFAULT_CAPTURE_RATE};
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let first = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, first).capture_id;
        set_capture(&mut app, first, "crop_w", 0.5);
        app.update_frame_timing();
        app.render_mixer_frame();

        let asked = CaptureConfig {
            rate: 24.0,
            ..CaptureConfig::default()
        };
        let second = screen_input(&mut app, asked);
        let show = (app.render_width, app.render_height);
        let state = capture_state_at(&mut app, second);
        assert_eq!(state.capture_id, capture, "the one capture of that screen");
        assert!(
            (state.config.crop.w - 0.5).abs() < 1e-6,
            "the crop it is captured with: {:?}",
            state.config.crop
        );
        assert!(
            (state.config.rate - DEFAULT_CAPTURE_RATE).abs() < 1e-6,
            "and the rate: {}",
            state.config.rate
        );
        assert_eq!(state.config.scale_to, Some(show), "capped as the show is");

        // Changed before a frame is drawn: only that goes down.
        set_capture(&mut app, second, "cursor", 1.0);
        app.update_frame_timing();
        app.render_mixer_frame();
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!((runs.crop.w - 0.5).abs() < 1e-6, "{:?}", runs.crop);
        assert!((runs.rate - DEFAULT_CAPTURE_RATE).abs() < 1e-6, "{}", runs.rate);
        assert!(runs.show_cursor);
    }

    /// A show opened captures a screen the show before captured too in the
    /// one capture there is of it, and runs it as it saved it — not with
    /// the crop the show before left on it, which its input would then
    /// show and save for its own.
    #[test]
    fn a_show_opened_runs_its_captures_as_it_saved_them() {
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let at = screen_input(&mut app, CaptureConfig::default());
        let uuid = app.mixer.channels()[at.0].decks[at.1].deck.uuid().to_string();
        let capture = capture_state_at(&mut app, at).capture_id;
        app.update_frame_timing();
        app.render_mixer_frame();
        let saved = crate::persistence::snapshot_scene(
            &app.mixer,
            None,
            app.render_width,
            app.render_height,
        );

        // Cropped since: the show there is, which the one saved replaces.
        set_capture(&mut app, at, "crop_w", 0.5);
        app.update_frame_timing();
        app.render_mixer_frame();
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!((runs.crop.w - 0.5).abs() < 1e-6, "{:?}", runs.crop);

        app.replace_scene(&saved).expect("opened");
        let at = app.mixer.find_deck_by_uuid(&uuid).expect("its input");
        assert_eq!(
            capture_state_at(&mut app, at).capture_id,
            capture,
            "the capture there was of that screen"
        );
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!(runs.crop.is_full_frame(), "as saved: {:?}", runs.crop);
        app.update_frame_timing();
        app.render_mixer_frame();
        let state = capture_state_at(&mut app, at);
        assert!(
            state.config.crop.is_full_frame(),
            "and its input shows so: {:?}",
            state.config.crop
        );
    }

    /// An input of the screen `name`, kept as `uuid`, added to the first
    /// channel: its layer.
    fn input_of_screen(app: &mut VardaApp, name: &str, uuid: &str) -> String {
        use crate::engine::MixerCommands;
        let screen = crate::scene::CaptureTargetConfig::Display {
            name: name.into(),
            uuid: Some(uuid.into()),
        };
        let ch0 = channel_uuid(app, 0);
        app.add_screen_capture_deck(
            &ch0,
            &screen,
            crate::screen_capture::backend::CaptureConfig::default(),
        )
        .expect("added")
    }

    /// The capture the input `uuid` shows.
    fn capture_of(app: &VardaApp, uuid: &str) -> crate::screen_capture::CaptureId {
        deck_of(app, uuid).screen_capture_id().expect("a capture")
    }

    /// A frame drawn: who holds each capture is counted again by it, and a
    /// capture no input holds — a desk's own (`with_targets`) — closed.
    fn draw_a_frame(app: &mut VardaApp) {
        app.update_frame_timing();
        app.render_mixer_frame();
    }

    /// A screen picked for an input that alone held its capture — a frame
    /// drawn since it was added, the holders counted — closes that capture
    /// at once: a screen no input shows is not captured on until the next
    /// frame finds it so. The one picked is held by both its inputs, and
    /// stays so.
    #[test]
    fn a_screen_picked_closes_the_capture_only_its_input_held() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let moves = input_of_screen(&mut app, "Display 1", "capture");
        let stays = input_of_screen(&mut app, "Display 2", "4k");
        draw_a_frame(&mut app);
        let first = capture_of(&app, &moves);
        let second = capture_of(&app, &stays);
        let holders = |app: &VardaApp| {
            (
                app.screen_capture_manager.holders(first),
                app.screen_capture_manager.holders(second),
            )
        };
        assert_eq!(holders(&app), (1, 1), "each its input's alone");

        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&moves, &the_4k)
            .expect("picked");
        assert_eq!(capture_of(&app, &moves), second);
        assert!(
            !app.screen_capture_manager.is_active(first),
            "closed: no input shows it"
        );
        assert_eq!(holders(&app), (0, 2), "the one picked, held by both");
        draw_a_frame(&mut app);
        assert_eq!(holders(&app), (0, 2), "and so it stays");
        assert_eq!(capture_of(&app, &moves), second);
    }

    /// A window picked for an input that showed a screen, and a screen for
    /// it again: it captures each in turn and holds it once, is saved and
    /// told of as what it captures, and — named after its screen — is
    /// named after each.
    #[test]
    fn a_window_picked_for_a_screens_input_is_captured_and_a_screen_again() {
        use crate::engine::MixerCommands;
        use crate::scene::CaptureTargetConfig;
        use crate::screen_capture::backend::{CaptureTargetInfo, CaptureTargetKind};
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let mut targets = app.screen_capture_manager.targets().to_vec();
        targets.push(CaptureTargetInfo {
            kind: CaptureTargetKind::Window,
            platform_id: 900,
            label: "Keynote — Opening".into(),
            app: Some("com.apple.Keynote".into()),
            title: Some("Opening".into()),
            width: 1280,
            height: 720,
            is_varda: false,
            uuid: None,
        });
        with_targets(&mut app, &targets);
        let uuid = input_of_screen(&mut app, "Display 1", "capture");
        let screen = capture_of(&app, &uuid);
        assert_eq!(deck_of(&app, &uuid).source_name(), "🖥 Display 1");

        let window = CaptureTargetConfig::Window {
            app: "com.apple.Keynote".into(),
            title: "Opening".into(),
        };
        app.set_screen_capture_target(&uuid, &window)
            .expect("picked");
        let deck = deck_of(&app, &uuid);
        let shown = capture_of(&app, &uuid);
        assert_ne!(shown, screen);
        assert_eq!(
            app.screen_capture_manager.label(shown),
            Some("Keynote — Opening")
        );
        // The desk's own, and the input.
        assert_eq!(app.screen_capture_manager.holders(shown), 2);
        assert_eq!(app.screen_capture_manager.holders(screen), 1, "let go");
        assert_eq!(
            saved_target(deck),
            serde_json::json!({ "kind": "window", "app": "com.apple.Keynote", "title": "Opening" })
        );
        assert_eq!(deck.source_name(), "🖥 Keynote — Opening");
        let told = capture_snapshot(&app, &uuid);
        assert!(told.bound && !told.is_display, "a window");
        assert!(!told.kept_by_name && told.target_uuid.is_none());

        // By its name, as the window lists it: kept by its identity.
        let display = CaptureTargetConfig::Display {
            name: "Display 1".into(),
            uuid: None,
        };
        app.set_screen_capture_target(&uuid, &display)
            .expect("picked");
        let deck = deck_of(&app, &uuid);
        assert_eq!(capture_of(&app, &uuid), screen);
        assert_eq!(app.screen_capture_manager.holders(screen), 2);
        assert_eq!(app.screen_capture_manager.holders(shown), 1, "let go");
        assert_eq!(
            saved_target(deck),
            serde_json::json!({ "kind": "display", "name": "Display 1", "uuid": "capture" })
        );
        assert_eq!(deck.source_name(), "🖥 Display 1");
        let told = capture_snapshot(&app, &uuid);
        assert!(told.bound && told.is_display && !told.kept_by_name);
        assert_eq!(told.target_uuid.as_deref(), Some("capture"));
    }

    /// A window as macOS lists it, for a desk (`with_targets`).
    fn keynote_window() -> crate::screen_capture::backend::CaptureTargetInfo {
        crate::screen_capture::backend::CaptureTargetInfo {
            kind: crate::screen_capture::backend::CaptureTargetKind::Window,
            platform_id: 900,
            label: "Keynote — Opening".into(),
            app: Some("com.apple.Keynote".into()),
            title: Some("Opening".into()),
            width: 1280,
            height: 720,
            is_varda: false,
            uuid: None,
        }
    }

    /// A desk of `screens` and a window, an input on the first screen, and
    /// a frame drawn: the rest, which no input shows, captured by nothing.
    /// The input's layer.
    fn a_screens_input_beside_a_window(app: &mut VardaApp, screens: &[(&str, u32)]) -> String {
        with_screens(app, screens);
        let mut targets = app.screen_capture_manager.targets().to_vec();
        targets.push(keynote_window());
        with_targets(app, &targets);
        let uuid = input_of_screen(app, "Display 1", screens[0].0);
        draw_a_frame(app);
        uuid
    }

    /// A window nothing captures yet, picked for a screen's input, is
    /// opened for it as the input is set — its rate and crop kept, Nida's
    /// own windows no longer left out, as of a window added — and the
    /// screen's capture, the input's alone, closed. The screen picked back
    /// is opened so again, Nida's own windows left out of it once more.
    #[test]
    fn a_window_nothing_captures_is_opened_as_its_input_is_set() {
        use crate::engine::MixerCommands;
        use crate::scene::CaptureTargetConfig;
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let uuid = a_screens_input_beside_a_window(&mut app, &[("capture", 1920)]);
        let at = app.mixer.find_deck_by_uuid(&uuid).expect("the input");
        set_capture(&mut app, at, "rate", 0.5);
        set_capture(&mut app, at, "crop_w", 0.5);
        draw_a_frame(&mut app);
        let screen = capture_of(&app, &uuid);
        let set = capture_state_at(&mut app, at).config.clone();
        assert!((set.rate - 60.5).abs() < 1e-4, "{}", set.rate);
        assert!((set.crop.w - 0.5).abs() < 1e-6, "{:?}", set.crop);
        assert!(set.exclude_varda, "left out of a screen");
        assert_eq!(set.scale_to, Some((app.render_width, app.render_height)));

        let window = CaptureTargetConfig::Window {
            app: "com.apple.Keynote".into(),
            title: "Opening".into(),
        };
        app.set_screen_capture_target(&uuid, &window)
            .expect("picked");
        let shown = capture_of(&app, &uuid);
        assert_ne!(shown, screen);
        assert_eq!(
            app.screen_capture_manager.label(shown),
            Some("Keynote — Opening")
        );
        assert_eq!(app.screen_capture_manager.holders(shown), 1, "opened for it");
        assert!(
            !app.screen_capture_manager.is_active(screen),
            "closed: no input shows it"
        );
        let as_a_window = CaptureConfig {
            exclude_varda: false,
            ..set.clone()
        };
        assert_eq!(
            app.screen_capture_manager.config(shown),
            Some(as_a_window.clone()),
            "opened as the input is set"
        );
        assert_eq!(capture_state_at(&mut app, at).config, as_a_window);

        let display = CaptureTargetConfig::Display {
            name: "Display 1".into(),
            uuid: None,
        };
        app.set_screen_capture_target(&uuid, &display)
            .expect("picked");
        let again = capture_of(&app, &uuid);
        assert_eq!(app.screen_capture_manager.label(again), Some("Display 1"));
        assert_eq!(app.screen_capture_manager.holders(again), 1, "opened for it");
        assert!(!app.screen_capture_manager.is_active(shown), "closed");
        assert_eq!(
            app.screen_capture_manager.config(again),
            Some(set.clone()),
            "left out of a screen again"
        );
        assert_eq!(capture_state_at(&mut app, at).config, set);
        // And so it stays once the holders are counted.
        draw_a_frame(&mut app);
        assert_eq!(capture_of(&app, &uuid), again);
        assert_eq!(app.screen_capture_manager.holders(again), 1);
        assert_eq!(capture_state_at(&mut app, at).config, set);
    }

    /// A window listed that does not open — closed since — and a screen
    /// macOS will not let be captured leave the input as it was: on the
    /// screen it showed, whose capture runs on, still its alone and as it
    /// was set, the input named and saved as before; and each says why.
    #[test]
    fn a_window_that_does_not_open_leaves_the_input_as_it_was() {
        use crate::scene::CaptureTargetConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let uuid = a_screens_input_beside_a_window(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let at = app.mixer.find_deck_by_uuid(&uuid).expect("the input");
        set_capture(&mut app, at, "crop_w", 0.5);
        draw_a_frame(&mut app);
        let held = capture_of(&app, &uuid);
        let set = capture_state_at(&mut app, at).config.clone();
        let runs = app.screen_capture_manager.config(held).expect("running");
        let saved = saved_target(deck_of(&app, &uuid));
        app.screen_capture_manager.opening_fails();

        let does_not_open = [
            (
                CaptureTargetConfig::Window {
                    app: "com.apple.Keynote".into(),
                    title: "Opening".into(),
                },
                "Window not picked: ",
            ),
            (
                CaptureTargetConfig::Display {
                    name: "Display 2".into(),
                    uuid: Some("4k".into()),
                },
                "Screen not picked: ",
            ),
        ];
        for (target, said) in does_not_open {
            let mut actions = crate::usecases::ui::UIActions::new();
            actions
                .commands
                .push(crate::engine::EngineCommand::SetScreenCaptureTarget {
                    deck_uuid: uuid.clone(),
                    target,
                });
            let _ = app.apply_engine_actions(&mut actions);
            let shown: Vec<&str> = app
                .session
                .notifications
                .visible()
                .iter()
                .map(|n| n.message.as_str())
                .collect();
            assert!(
                shown
                    .iter()
                    .any(|m| m.starts_with(said) && m.contains("did not open")),
                "{said} {shown:?}"
            );
            assert_eq!(capture_of(&app, &uuid), held, "on the screen it showed");
            assert!(app.screen_capture_manager.is_active(held), "running on");
            assert_eq!(app.screen_capture_manager.holders(held), 1, "its alone");
            assert_eq!(
                app.screen_capture_manager.config(held),
                Some(runs.clone()),
                "as it ran"
            );
            assert_eq!(capture_state_at(&mut app, at).config, set, "as it was set");
            let deck = deck_of(&app, &uuid);
            assert_eq!(saved_target(deck), saved, "saved as before");
            assert_eq!(deck.source_name(), "🖥 Display 1");
        }
        // Nothing of either pick left behind for a frame to find.
        draw_a_frame(&mut app);
        assert_eq!(capture_of(&app, &uuid), held);
        assert_eq!(app.screen_capture_manager.holders(held), 1);
    }

    /// A screen picked is undone and done again as any change is: the input
    /// goes back to the screen it showed, under the name it had, and on to
    /// the one picked; and each screen's capture is held by the inputs that
    /// show it, once a frame is drawn.
    #[test]
    fn a_screen_picked_is_undone_and_done_again() {
        use crate::engine::{CommandResult, EngineCommand};
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        // An input stays on each screen: its capture runs on, to go back to.
        let on_first = input_of_screen(&mut app, "Display 1", "capture");
        let on_second = input_of_screen(&mut app, "Display 2", "4k");
        let moves = input_of_screen(&mut app, "Display 1", "capture");
        draw_a_frame(&mut app);
        let first = capture_of(&app, &on_first);
        let second = capture_of(&app, &on_second);
        let holders = |app: &VardaApp| {
            (
                app.screen_capture_manager.holders(first),
                app.screen_capture_manager.holders(second),
            )
        };
        assert_eq!(holders(&app), (2, 1));
        let saved = |app: &VardaApp| saved_target(deck_of(app, &moves));
        let was = serde_json::json!({ "kind": "display", "name": "Display 1", "uuid": "capture" });
        let picked = serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" });

        let pick = EngineCommand::SetScreenCaptureTarget {
            deck_uuid: moves.clone(),
            target: crate::scene::CaptureTargetConfig::Display {
                name: "Display 2".into(),
                uuid: Some("4k".into()),
            },
        };
        assert!(app.makes_undo_step(&pick), "undone as any change");
        // As the command bus does for an undoable command.
        let before = app.history_snapshot_default();
        app.push_history(before);
        assert!(matches!(app.execute_command(pick), CommandResult::Ok));
        assert_eq!(capture_of(&app, &moves), second);
        assert_eq!(deck_of(&app, &moves).source_name(), "🖥 Display 2");
        draw_a_frame(&mut app);
        assert_eq!(holders(&app), (1, 2));

        assert!(matches!(
            app.execute_command(EngineCommand::Undo),
            CommandResult::Ok
        ));
        assert_eq!(capture_of(&app, &moves), first, "undone");
        assert_eq!(saved(&app), was);
        assert_eq!(deck_of(&app, &moves).source_name(), "🖥 Display 1");
        draw_a_frame(&mut app);
        assert_eq!(holders(&app), (2, 1), "each held by those that show it");

        assert!(matches!(
            app.execute_command(EngineCommand::Redo),
            CommandResult::Ok
        ));
        assert_eq!(capture_of(&app, &moves), second, "done again");
        assert_eq!(saved(&app), picked);
        assert_eq!(deck_of(&app, &moves).source_name(), "🖥 Display 2");
        draw_a_frame(&mut app);
        assert_eq!(holders(&app), (1, 2));
    }

    /// A screen or window picked that cannot be captured — a window closed
    /// since it was listed, say — says why, as an input that cannot be
    /// added does: the input just stays on what it showed.
    #[test]
    fn a_screen_or_window_that_cannot_be_picked_says_why() {
        use crate::engine::MixerCommands;
        use crate::scene::CaptureTargetConfig;
        use crate::screen_capture::backend::CaptureConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let ch0 = channel_uuid(&app, 0);
        let screen = CaptureTargetConfig::Display {
            name: "Display 1".into(),
            uuid: Some("capture".into()),
        };
        let uuid = app
            .add_screen_capture_deck(&ch0, &screen, CaptureConfig::default())
            .expect("added");
        let held = deck_of(&app, &uuid).screen_capture_id();

        let gone = [
            (
                CaptureTargetConfig::Window {
                    app: "com.apple.Keynote".into(),
                    title: "Opening".into(),
                },
                "Window not picked: ",
            ),
            (
                CaptureTargetConfig::Display {
                    name: "Display 5".into(),
                    uuid: None,
                },
                "Screen not picked: ",
            ),
        ];
        for (target, said) in gone {
            let mut actions = crate::usecases::ui::UIActions::new();
            actions
                .commands
                .push(crate::engine::EngineCommand::SetScreenCaptureTarget {
                    deck_uuid: uuid.clone(),
                    target,
                });
            let _ = app.apply_engine_actions(&mut actions);
            let shown: Vec<&str> = app
                .session
                .notifications
                .visible()
                .iter()
                .map(|n| n.message.as_str())
                .collect();
            assert!(shown.iter().any(|m| m.starts_with(said)), "{said} {shown:?}");
            assert_eq!(deck_of(&app, &uuid).screen_capture_id(), held, "as it was");
        }
    }

    /// An input whose screen is not there — "Target not found", black — is
    /// given another: it captures it, and holds it once.
    #[test]
    fn an_input_whose_screen_is_gone_is_given_another() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let gone = serde_json::json!({ "kind": "display", "name": "Display 5" });
        let deck = restored_capture_deck(&mut app, &gone);
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "nothing to capture"
        );
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);
        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&uuid, &the_4k).expect("picked");
        let (ch, dk) = app.mixer.find_deck_by_uuid(&uuid).expect("the input");
        let deck = &app.mixer.channels()[ch].decks[dk].deck;
        let id = deck.screen_capture_id().expect("a capture now");
        assert_eq!(app.screen_capture_manager.label(id), Some("Display 2"));
        assert_eq!(app.screen_capture_manager.holders(id), 2);
        assert_eq!(
            saved_target(deck),
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );
    }

    /// The name a show saves for the input `uuid`.
    fn saved_name(app: &VardaApp, uuid: &str) -> String {
        crate::persistence::snapshot_scene(&app.mixer, None, app.render_width, app.render_height)
            .channels
            .into_iter()
            .flat_map(|channel| channel.decks)
            .find(|deck| deck.uuid == uuid)
            .expect("saved")
            .name
    }

    /// What the input `deck` was last named after
    /// (`ScreenCaptureState::listed_as`).
    fn listed_as(deck: &crate::deck::Deck) -> Option<&str> {
        deck.screen_capture.as_ref()?.listed_as.as_deref()
    }

    /// What a show saves of what the input `uuid` shows, as it is written.
    fn saved_source(app: &VardaApp, uuid: &str) -> serde_json::Value {
        let saved = crate::persistence::snapshot_scene(
            &app.mixer,
            None,
            app.render_width,
            app.render_height,
        )
        .channels
        .into_iter()
        .flat_map(|channel| channel.decks)
        .find(|deck| deck.uuid == uuid)
        .expect("saved");
        serde_json::to_value(saved.source).expect("serialize")
    }

    /// A show saved with an app's window for an input, opened with that app
    /// closed: the input, capturing nothing, keeps its name, and the show
    /// saves it under that name again. Its window is kept by the app's
    /// bundle id, and named from that it read "🖥 com.apple.Keynote —
    /// Opening" from then on (an M5 Max, 2026-11-10). A screen picked for
    /// it, it is named after the screen: its name was what it captured, not
    /// one given it.
    #[test]
    fn an_input_whose_app_is_closed_keeps_its_name() {
        use crate::engine::MixerCommands;
        use crate::scene::CaptureTargetConfig;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let uuid = a_screens_input_beside_a_window(&mut app, &[("capture", 1920)]);
        let window = CaptureTargetConfig::Window {
            app: "com.apple.Keynote".into(),
            title: "Opening".into(),
        };
        app.set_screen_capture_target(&uuid, &window)
            .expect("picked");
        assert_eq!(saved_name(&app, &uuid), "🖥 Keynote — Opening");
        let saved = crate::persistence::snapshot_scene(
            &app.mixer,
            None,
            app.render_width,
            app.render_height,
        );
        drop(app);

        // Another day at the same desk, Keynote not open.
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        app.replace_scene(&saved).expect("opened");
        let deck = deck_of(&app, &uuid);
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "nothing to capture"
        );
        assert_eq!(deck.source_name(), "🖥 Keynote — Opening");
        assert_eq!(saved_name(&app, &uuid), "🖥 Keynote — Opening", "saved so again");
        assert_eq!(
            saved_target(deck),
            serde_json::json!({ "kind": "window", "app": "com.apple.Keynote", "title": "Opening" }),
            "its window still the one looked for"
        );

        let screen = CaptureTargetConfig::Display {
            name: "Display 1".into(),
            uuid: Some("capture".into()),
        };
        app.set_screen_capture_target(&uuid, &screen)
            .expect("picked");
        assert_eq!(deck_of(&app, &uuid).source_name(), "🖥 Display 1");
    }

    /// An input a show saved as capturing a screen not there now.
    fn a_gone_screen() -> serde_json::Value {
        serde_json::json!({
            "type": "ScreenCapture",
            "target": { "kind": "display", "name": "Display 5" }
        })
    }

    /// An input the show has no name for, its screen not there, is named
    /// after the screen it was to capture.
    #[test]
    fn an_input_the_show_has_no_name_for_is_named_after_what_it_was_to_capture() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let deck = restored_input(&mut app, "", &a_gone_screen());
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "nothing to capture"
        );
        assert_eq!(deck.source_name(), "🖥 Display 5");
    }

    /// The name a show has for an input is the input's only while it
    /// captures nothing: one that captures is named as its screen is listed
    /// now. The 4K, "Display 1" when the show was saved, is "Display 2" with
    /// one more screen plugged in.
    #[test]
    fn an_input_that_captures_is_named_as_its_screen_is_listed_now() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let deck = restored_input(
            &mut app,
            "🖥 Display 1",
            &serde_json::json!({
                "type": "ScreenCapture",
                "target": { "kind": "display", "name": "Display 1", "uuid": "4k" }
            }),
        );
        let id = deck.screen_capture_id().expect("a capture");
        assert_eq!(app.screen_capture_manager.label(id), Some("Display 2"), "the 4K");
        assert_eq!(deck.source_name(), "🖥 Display 2");
        assert_eq!(listed_as(&deck), Some("Display 2"), "what it is named after now");
    }

    /// The name of the input a show saved under `name` with `source` — what
    /// it was to capture not there — once the show is opened and the 4K
    /// picked for it. Nothing with no GPU to run on.
    fn named_once_the_4k_is_picked(name: &str, source: &serde_json::Value) -> Option<String> {
        use crate::engine::MixerCommands;
        let mut app = headless_app()?;
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let deck = restored_input(&mut app, name, source);
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "nothing to capture"
        );
        assert_eq!(deck.source_name(), name, "the show's name for it");
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);
        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&uuid, &the_4k).expect("picked");
        let deck = deck_of(&app, &uuid);
        assert_eq!(
            saved_target(deck),
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );
        Some(deck.source_name().to_string())
    }

    /// A name given an input — one not its screen's — is its name in the
    /// show opened with its screen not there, and stays when a screen is
    /// picked for it.
    #[test]
    fn a_name_given_an_input_that_captures_nothing_stays() {
        let Some(name) = named_once_the_4k_is_picked("Slides", &a_gone_screen()) else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        assert_eq!(name, "Slides", "a name given it stays");
    }

    /// So does one that reads as a capture's: a screen's input is named
    /// after the screen picked only when its name is the one the show has
    /// its screen under.
    #[test]
    fn a_name_given_a_screens_input_stays_though_it_reads_as_a_captures() {
        let Some(name) = named_once_the_4k_is_picked("🖥 Slides", &a_gone_screen()) else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        assert_eq!(name, "🖥 Slides", "not its screen's name: one given it");
    }

    /// An input a show saved as capturing Keynote's window.
    fn keynotes_window_saved() -> serde_json::Value {
        serde_json::json!({
            "type": "ScreenCapture",
            "target": { "kind": "window", "app": "com.apple.Keynote", "title": "Opening" }
        })
    }

    /// A window's input, its app closed, keeps a name given it when a
    /// screen is picked for it.
    #[test]
    fn a_name_given_a_windows_input_stays_when_a_screen_is_picked_for_it() {
        let Some(name) = named_once_the_4k_is_picked("Slides", &keynotes_window_saved()) else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        assert_eq!(name, "Slides", "a name given it stays");
    }

    /// A show saved by a Nida that named a window's input by its app's
    /// bundle id has that for its name. Opened with the app running, the
    /// input captures its window and is named as the window is listed: the
    /// show's name for it is its name only while it captures nothing.
    #[test]
    fn an_input_named_by_its_apps_id_is_named_as_its_window_is_listed_once_it_captures() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_targets(&mut app, &[keynote_window()]);
        let deck = restored_input(
            &mut app,
            "🖥 com.apple.Keynote — Opening",
            &keynotes_window_saved(),
        );
        let id = deck.screen_capture_id().expect("a capture");
        assert_eq!(app.screen_capture_manager.label(id), Some("Keynote — Opening"));
        assert_eq!(deck.source_name(), "🖥 Keynote — Opening");
        assert_eq!(listed_as(&deck), Some("Keynote — Opening"), "what it is named after now");
    }

    /// Opened with the app closed, such a show's input keeps that name — it
    /// is the show's for it — until its window is picked again: then it is
    /// named as the window is listed.
    #[test]
    fn an_input_named_by_its_apps_id_is_named_after_the_window_picked_for_it() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let deck = restored_input(
            &mut app,
            "🖥 com.apple.Keynote — Opening",
            &keynotes_window_saved(),
        );
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "nothing to capture"
        );
        assert_eq!(deck.source_name(), "🖥 com.apple.Keynote — Opening");
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);

        // Keynote opened, and its window picked for the input.
        let mut targets = app.screen_capture_manager.targets().to_vec();
        targets.push(keynote_window());
        with_targets(&mut app, &targets);
        let window = crate::scene::CaptureTargetConfig::Window {
            app: "com.apple.Keynote".into(),
            title: "Opening".into(),
        };
        app.set_screen_capture_target(&uuid, &window)
            .expect("picked");
        assert_eq!(deck_of(&app, &uuid).source_name(), "🖥 Keynote — Opening");
    }

    /// What an input is named after is kept with it from when it is added
    /// — its screen as it is listed — and with the show: its name is held
    /// against that when another screen or window is picked for it, and an
    /// input whose app is closed has nothing else to hold it against.
    #[test]
    fn what_an_input_is_named_after_is_kept_with_it_and_with_the_show() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let uuid = input_of_screen(&mut app, "Display 1", "capture");
        let deck = deck_of(&app, &uuid);
        assert_eq!(deck.source_name(), "🖥 Display 1");
        assert_eq!(listed_as(deck), Some("Display 1"), "named after its screen");
        assert_eq!(
            saved_source(&app, &uuid)["listed_as"],
            serde_json::json!("Display 1"),
            "and the show keeps that"
        );
    }

    /// An input a show saved as capturing a window of Keynote's with no
    /// title — one listed by its app alone, "Keynote", where the show has
    /// the app by its bundle id — and, with `listed_as`, as named after it
    /// so listed.
    fn keynotes_untitled_window_saved(listed_as: Option<&str>) -> serde_json::Value {
        let mut source = serde_json::json!({
            "type": "ScreenCapture",
            "target": { "kind": "window", "app": "com.apple.Keynote", "title": "" }
        });
        if let Some(listed_as) = listed_as {
            source["listed_as"] = listed_as.into();
        }
        source
    }

    /// An input named after a window with no title, its app closed, is
    /// named after the screen picked for it: the show kept what it was
    /// named after, and nothing else tells — its name is what the app is
    /// called, and the show has the app by its bundle id.
    #[test]
    fn an_input_named_after_a_window_with_no_title_is_named_after_the_screen_picked() {
        let saved = keynotes_untitled_window_saved(Some("Keynote"));
        let Some(name) = named_once_the_4k_is_picked("🖥 Keynote", &saved) else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        assert_eq!(name, "🖥 Display 2", "its name was its window's");
    }

    /// In a show saved before that was kept, the name of such an input may
    /// as well be one given it — nothing in the show tells — and stays.
    #[test]
    fn a_name_no_record_tells_of_stays_on_an_input_of_a_window_with_no_title() {
        let saved = keynotes_untitled_window_saved(None);
        let Some(name) = named_once_the_4k_is_picked("🖥 Keynote", &saved) else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        assert_eq!(name, "🖥 Keynote", "nothing tells it is its window's");
    }

    /// So does one that ends in its window's title, as a listing of the
    /// window does: its window's as it was listed, or one given the input
    /// ("🖥 My — Opening") — the end of a name does not tell which, and a
    /// name given is not to be replaced.
    #[test]
    fn a_name_that_ends_in_its_windows_title_stays_in_a_show_with_no_record() {
        for given in ["🖥 Keynote — Opening", "🖥 My — Opening"] {
            let Some(name) = named_once_the_4k_is_picked(given, &keynotes_window_saved()) else {
                eprintln!("Skipping: no headless GPU available");
                return;
            };
            assert_eq!(name, given, "nothing tells it is its window's");
        }
    }

    /// The name of the input a preset named `name` has with `source` — what
    /// it was to capture not there — once the preset is loaded, as the
    /// Library loads it, and the 4K picked for the input. Nothing with no
    /// GPU to run on.
    fn a_presets_input_named_once_the_4k_is_picked(
        name: &str,
        source: &serde_json::Value,
    ) -> Option<String> {
        use crate::engine::{CommandResult, EngineCommand, MixerCommands};
        let mut app = headless_app()?;
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let config: crate::scene::DeckConfig =
            serde_json::from_value(serde_json::json!({ "name": name, "source": source }))
                .expect("a saved input");
        app.session
            .preset_library
            .deck_presets
            .push(crate::persistence::presets::DeckPreset {
                name: name.into(),
                config,
            });
        let ch0 = channel_uuid(&app, 0);
        let loaded = app.execute_command(EngineCommand::LoadDeckPreset {
            channel_uuid: ch0,
            preset_name: name.into(),
        });
        assert!(matches!(loaded, CommandResult::Ok), "loaded: {loaded:?}");
        let decks = &app.mixer.channels()[0].decks;
        assert_eq!(decks.len(), 1, "the preset's input, alone");
        let deck = &decks[0].deck;
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "nothing to capture"
        );
        assert_eq!(deck.source_name(), name, "the preset's name for it");
        let uuid = deck.uuid().to_string();
        let the_4k = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: Some("4k".into()),
        };
        app.set_screen_capture_target(&uuid, &the_4k).expect("picked");
        let deck = deck_of(&app, &uuid);
        assert_eq!(
            saved_target(deck),
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );
        Some(deck.source_name().to_string())
    }

    /// A preset's name for a window's input is one given it, though it
    /// reads as a capture's: "🖥 Slides", the preset saved of an input named
    /// after Keynote's window. Loaded with Keynote closed and a screen
    /// picked for it, the input keeps the name — it is not what the input
    /// was named after.
    #[test]
    fn a_presets_name_that_reads_as_a_captures_stays_when_a_screen_is_picked() {
        let mut saved = keynotes_window_saved();
        saved["listed_as"] = "Keynote — Opening".into();
        let Some(name) = a_presets_input_named_once_the_4k_is_picked("🖥 Slides", &saved) else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        assert_eq!(name, "🖥 Slides", "the preset's name: one given it");
    }

    /// So does the name of a preset with no record of what its input was
    /// named after — one saved before that was kept, or of an input that
    /// had none — also one that ends in its window's title.
    #[test]
    fn a_presets_name_no_record_tells_of_stays_when_a_screen_is_picked() {
        for given in ["🖥 Slides", "🖥 My — Opening"] {
            let Some(name) =
                a_presets_input_named_once_the_4k_is_picked(given, &keynotes_window_saved())
            else {
                eprintln!("Skipping: no headless GPU available");
                return;
            };
            assert_eq!(name, given, "the preset's name: one given it");
        }
    }

    /// The PowerPoint input a show saved under `name`, its show's window
    /// last titled `title`, as the show opened gives it: capturing nothing,
    /// its show not yet playing.
    fn restored_powerpoint_input(
        app: &mut VardaApp,
        name: &str,
        title: &str,
    ) -> crate::deck::Deck {
        let deck = restored_input(
            app,
            name,
            &serde_json::json!({
                "type": "ScreenCapture",
                "target": {
                    "kind": "window",
                    "app": crate::presentation::powerpoint::POWERPOINT_ID,
                    "title": title
                },
                "presentation": { "path": "/talks/talk.pptx" }
            }),
        );
        assert_eq!(
            deck.screen_capture_id(),
            Some(crate::screen_capture::UNBOUND_CAPTURE_ID),
            "its show not yet playing"
        );
        assert!(deck.screen_capture.as_ref().is_some_and(|s| s.presentation.is_some()));
        deck
    }

    /// A PowerPoint input in a show opened is named after its file, as when
    /// it was added: its show's window is found once PowerPoint plays it,
    /// and until then it captures nothing. A name given it is kept as well.
    #[test]
    fn a_powerpoint_input_in_a_show_opened_keeps_its_name() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        for name in ["📽 talk", "Opening talk"] {
            let deck = restored_powerpoint_input(&mut app, name, "talk");
            assert_eq!(deck.source_name(), name);
        }
    }

    /// A show saved by a Nida that named a PowerPoint input by its window's
    /// app at every open has that for its name: "🖥 com.microsoft.Powerpoint
    /// — …" with the title its window had then — its file's, or its show's
    /// once PowerPoint had played it. Opened, the input is named after its
    /// file again: no window is ever picked for it, which mends another
    /// window's input.
    #[test]
    fn a_powerpoint_input_named_by_its_windows_app_is_named_after_its_talk_again() {
        use crate::presentation::powerpoint::POWERPOINT_ID;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        for (name, title) in [
            (format!("🖥 {POWERPOINT_ID} — talk"), "talk"),
            (format!("🖥 {POWERPOINT_ID} — talk"), "PowerPoint Slide Show - [talk]"),
            (format!("🖥 {POWERPOINT_ID}"), ""),
        ] {
            let deck = restored_powerpoint_input(&mut app, &name, title);
            assert_eq!(deck.source_name(), "📽 talk", "saved as {name}, its window {title:?}");
        }
    }

    /// A PowerPoint input the show has no name for is named after its file
    /// too.
    #[test]
    fn a_powerpoint_input_the_show_has_no_name_for_is_named_after_its_talk() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920)]);
        let deck = restored_powerpoint_input(&mut app, "", "talk");
        assert_eq!(deck.source_name(), "📽 talk");
    }

    /// Only a screen capture input has a screen to pick, and only one there
    /// is: a layer that is no capture, a screen not listed and a layer not
    /// there are each refused as what they are, the input left as it was.
    #[test]
    fn a_screen_is_picked_only_for_a_capture_and_only_one_there_is() {
        use crate::engine::{CommandResult, EngineCommand, ErrorCode};
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let old = serde_json::json!({ "kind": "display", "name": "Display 1" });
        let deck = restored_capture_deck(&mut app, &old);
        let uuid = deck.uuid().to_string();
        app.mixer.channels_mut()[0].add_deck(deck);
        let pick = |app: &mut VardaApp, deck_uuid: &str, name: &str| {
            app.execute_command(EngineCommand::SetScreenCaptureTarget {
                deck_uuid: deck_uuid.to_string(),
                target: crate::scene::CaptureTargetConfig::Display {
                    name: name.into(),
                    uuid: None,
                },
            })
        };
        let refused = |result: CommandResult, as_code: ErrorCode| {
            matches!(result, CommandResult::Err { code, .. } if code == as_code)
        };
        assert!(refused(
            pick(&mut app, &uuid, "Display 7"),
            ErrorCode::InvalidInput
        ));
        let (ch, dk) = app.mixer.find_deck_by_uuid(&uuid).expect("the input");
        assert_eq!(
            saved_target(&app.mixer.channels()[ch].decks[dk].deck),
            old,
            "as it was"
        );
        assert!(refused(
            pick(&mut app, "no-such-layer", "Display 2"),
            ErrorCode::NotFound
        ));
        let CommandResult::OkWithId { uuid: colour } =
            app.execute_command(EngineCommand::AddSolidColorDeck {
                channel_uuid: channel_uuid(&app, 0),
                color: [1.0, 0.0, 0.0, 1.0],
            })
        else {
            panic!("a colour layer");
        };
        assert!(refused(
            pick(&mut app, &colour, "Display 2"),
            ErrorCode::InvalidInput
        ));
        // A PowerPoint input's window is its show's.
        let talk: crate::presentation::PresentationConfig =
            serde_json::from_value(serde_json::json!({ "path": "/talks/opening.pptx" }))
                .expect("a talk");
        capture_state_at(&mut app, (ch, dk)).presentation = Some(talk);
        assert!(refused(
            pick(&mut app, &uuid, "Display 2"),
            ErrorCode::InvalidInput
        ));
        capture_state_at(&mut app, (ch, dk)).presentation = None;
        assert!(matches!(
            pick(&mut app, &uuid, "Display 2"),
            CommandResult::Ok
        ));
        assert_eq!(
            saved_target(&app.mixer.channels()[ch].decks[dk].deck),
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );
    }

    /// A screen picked by its name — in the window, or by an API client
    /// that sends no more — is kept by its identity: the input added saves
    /// the screen itself, not its place among the screens.
    #[test]
    fn a_screen_added_by_name_is_kept_by_its_identity() {
        use crate::engine::MixerCommands;
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        with_screens(&mut app, &[("capture", 1920), ("4k", 3840)]);
        let ch0 = channel_uuid(&app, 0);
        let picked = crate::scene::CaptureTargetConfig::Display {
            name: "Display 2".into(),
            uuid: None,
        };
        let options = crate::screen_capture::backend::CaptureConfig::default();
        let deck = app
            .add_screen_capture_deck(&ch0, &picked, options)
            .expect("added");
        let (ch, dk) = app.mixer.find_deck_by_uuid(&deck).expect("the deck");
        let state = app.mixer.channels()[ch].decks[dk]
            .deck
            .screen_capture
            .as_ref()
            .expect("a capture");
        assert_eq!(
            serde_json::to_value(crate::scene::CaptureTargetConfig::from(&state.identity))
                .expect("serialize"),
            serde_json::json!({ "kind": "display", "name": "Display 2", "uuid": "4k" })
        );
    }

    /// A PowerPoint show's capture keeps its own cap — its presentation's,
    /// not the show's — through a show size change, beside an
    /// input capturing the same window, which is capped; and through a
    /// setting given its input, which goes down to it alone.
    #[test]
    fn a_powerpoint_shows_capture_keeps_its_cap() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        app.set_render_resolution(1920, 1080);
        app.screen_capture_manager =
            crate::screen_capture::ScreenCaptureManager::enabled_for_tests();
        let target = crate::screen_capture::backend::mock_targets().remove(0);
        let shows = crate::screen_capture::backend::CaptureConfig {
            scale_to: Some((1920, 1146)),
            ..Default::default()
        };
        let session = Box::new(crate::screen_capture::backend::MockBackend::new(
            target.label.clone(),
            target.width,
            target.height,
            shows.clone(),
        ));
        let show = app
            .screen_capture_manager
            .adopt(&target, session, shows.clone(), &app.context.device)
            .expect("adopt");
        let at_1080 = crate::screen_capture::backend::CaptureConfig {
            scale_to: Some((1920, 1080)),
            ..Default::default()
        };
        let own = app
            .screen_capture_manager
            .open_mock(&target, at_1080.clone(), &app.context.device)
            .expect("open");
        for ((id, w, h), config, presentation) in [(show, shows, true), (own, at_1080, false)] {
            let deck = crate::deck::Deck::new_from_screen_capture(
                &app.context,
                crate::deck::ScreenCaptureState {
                    capture_id: id,
                    identity: target.identity(),
                    config,
                    given: Vec::new(),
                    listed_as: None,
                    presentation: presentation.then(|| crate::presentation::PresentationConfig {
                        path: "/talks/a.pptx".into(),
                        slide: None,
                        plays_on: crate::engine::value::presentation::PlaysOn::default(),
                    }),
                },
                &target.label,
                w,
                h,
                1920,
                1080,
            )
            .expect("a capture deck");
            app.mixer.channel_mut(0).expect("a channel").add_deck(deck);
        }
        app.set_render_resolution(3840, 2160);
        let config = |id| app.screen_capture_manager.config(id).expect("live").scale_to;
        assert_eq!(config(show.0), Some((1920, 1146)));
        assert_eq!(config(own.0), Some((3840, 2160)));

        // Its cursor shown: its settings go down whole to its own capture,
        // whose cap — the presentation's, moved on since the input was
        // made — stays.
        let slides = (0, app.mixer.channels()[0].decks.len() - 2);
        app.screen_capture_manager
            .cap_to(show.0, Some((2560, 1528)));
        set_capture(&mut app, slides, "cursor", 1.0);
        app.send_capture_settings();
        let runs = app.screen_capture_manager.config(show.0).expect("live");
        assert!(runs.show_cursor, "its cursor");
        assert_eq!(runs.scale_to, Some((2560, 1528)), "its cap kept");
        assert!(capture_state_at(&mut app, slides).given.is_empty(), "sent");
        let beside = app.screen_capture_manager.config(own.0).expect("live");
        assert!(!beside.show_cursor, "not the input's own capture of it");
    }

    /// The show going from 1080p to 4K takes its screen captures with it:
    /// one added at 1080p stayed capped at 1080p, blurred on a 4K wall. Two
    /// inputs on one screen share its capture: capped once, the crop one of
    /// them set on it kept.
    #[test]
    fn a_show_size_change_takes_its_captures_along() {
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        app.set_render_resolution(1920, 1080);
        app.screen_capture_manager =
            crate::screen_capture::ScreenCaptureManager::enabled_for_tests();
        let target = crate::screen_capture::backend::mock_targets().remove(0);
        let at_1080 = crate::screen_capture::backend::CaptureConfig {
            scale_to: Some((1920, 1080)),
            ..Default::default()
        };
        let mut cropped = at_1080.clone();
        cropped.crop.w = 0.5;
        for config in [at_1080.clone(), cropped.clone()] {
            let (id, w, h) = app
                .screen_capture_manager
                .open_mock(&target, config.clone(), &app.context.device)
                .expect("open");
            let deck = crate::deck::Deck::new_from_screen_capture(
                &app.context,
                crate::deck::ScreenCaptureState {
                    capture_id: id,
                    identity: target.identity(),
                    config,
                    given: Vec::new(),
                    listed_as: None,
                    presentation: None,
                },
                &target.label,
                w,
                h,
                1920,
                1080,
            )
            .expect("a capture deck");
            app.mixer.channel_mut(0).expect("a channel").add_deck(deck);
        }
        let id = app.mixer.channels()[0].decks[0]
            .deck
            .screen_capture_id()
            .expect("bound");
        app.screen_capture_manager.set_config(id, cropped);
        app.set_render_resolution(3840, 2160);
        for slot in &app.mixer.channels()[0].decks {
            let state = slot.deck.screen_capture.as_ref().expect("a capture");
            assert_eq!(state.config.scale_to, Some((3840, 2160)));
        }
        let live = app.screen_capture_manager.config(id).expect("live");
        assert_eq!(live.scale_to, Some((3840, 2160)));
        assert!((live.crop.w - 0.5).abs() < 1e-6, "the crop kept");
    }

    /// A screen's capture comes as big as the most of its inputs draw it —
    /// one fitted and zoomed into with its Transform, beside one that fills
    /// the frame — bigger at once, smaller after a while. The inputs' own
    /// settings say nothing of it, and a setting given one leaves it so.
    #[test]
    fn a_capture_comes_as_big_as_its_inputs_draw_it() {
        use crate::deck::ScalingMode;
        use crate::params::ParamValue::Float;
        use crate::screen_capture::backend::{CaptureConfig, Drawn};
        use std::time::{Duration, Instant};
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        app.set_render_resolution(1920, 1080);
        with_screens(&mut app, &[("capture", 3840)]);
        let first = screen_input(&mut app, CaptureConfig::default());
        let second = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, first).capture_id;
        assert_eq!(capture_state_at(&mut app, second).capture_id, capture);
        // Capped as a show's own is (the desk's mock opened it with none).
        let show = Some((1920, 1080));
        app.screen_capture_manager.cap_to(capture, show);
        let drawn = |app: &VardaApp| app.screen_capture_manager.drawn(capture).expect("running");
        let sized = |fitted, fills| Drawn {
            fitted,
            across: fills,
            down: fills,
        };
        let fit = |app: &mut VardaApp, at: (usize, usize)| {
            let deck = &mut app.mixer.channels_mut()[at.0].decks[at.1].deck;
            deck.set_scaling_mode(ScalingMode::Fit);
        };
        let zoom = |app: &mut VardaApp, at: (usize, usize), times: f32| {
            let deck = &mut app.mixer.channels_mut()[at.0].decks[at.1].deck;
            let transform = deck
                .effects
                .iter_mut()
                .find(|e| e.shader.name() == "transform")
                .expect("its Transform");
            transform.params.set("scale", Float(times));
        };
        assert_eq!(drawn(&app), Drawn::default(), "fitted, until a frame is drawn");

        // A frame drawn: both fill the frame, as inputs added do.
        draw_a_frame(&mut app);
        assert_eq!(drawn(&app), sized(1.0, 1.0));
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);

        // The first fitted and zoomed into three times over: at once.
        fit(&mut app, first);
        zoom(&mut app, first, 3.0);
        app.size_captures_as_drawn_at(at(600));
        assert_eq!(drawn(&app), sized(3.0, 1.0), "the most of each");

        // A setting given an input: the capture sized as it was, and no
        // input's settings saying how.
        set_capture(&mut app, first, "cursor", 1.0);
        app.send_capture_settings();
        assert_eq!(drawn(&app), sized(3.0, 1.0), "as it was");
        let runs = app
            .screen_capture_manager
            .config(capture)
            .expect("running");
        assert!(runs.show_cursor, "the setting taken");
        for input in [first, second] {
            let state = capture_state_at(&mut app, input);
            assert_eq!(state.config.drawn, Drawn::default());
            assert_eq!(
                state.config,
                CaptureConfig {
                    scale_to: show,
                    ..runs.clone()
                },
                "as its capture runs"
            );
        }

        // The second fitted too: nothing fills the frame, once that has
        // been so a while.
        fit(&mut app, second);
        app.size_captures_as_drawn_at(at(1000));
        app.size_captures_as_drawn_at(at(2999));
        assert_eq!(drawn(&app), sized(3.0, 1.0), "not yet");
        app.size_captures_as_drawn_at(at(3000));
        assert_eq!(drawn(&app), sized(3.0, 0.0), "smaller");
        zoom(&mut app, first, 1.5);
        app.size_captures_as_drawn_at(at(4000));
        app.size_captures_as_drawn_at(at(6000));
        assert_eq!(drawn(&app), sized(1.5, 0.0));

        // And so it comes: a 4K screen, half as big again as fits a 1080p
        // show.
        let needed = std::collections::HashSet::from([capture]);
        let until = Instant::now() + Duration::from_secs(10);
        let comes = |app: &VardaApp| app.screen_capture_manager.resolution(capture);
        while comes(&app) != Some((2880, 1620)) && Instant::now() < until {
            app.screen_capture_manager.update_selective(
                &app.context,
                &needed,
                crate::host_clock::host_seconds(),
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(comes(&app), Some((2880, 1620)), "as it is drawn");

        // The second centred beside it: placed as it came fitted — whole —
        // not by the pixels the first is zoomed into.
        let deck = &mut app.mixer.channels_mut()[second.0].decks[second.1].deck;
        deck.set_scaling_mode(ScalingMode::Center);
        draw_a_frame(&mut app);
        assert_eq!(drawn(&app), sized(1.5, 0.0), "centred: as one fitted");
        let placed = |app: &mut VardaApp, at: (usize, usize)| {
            app.mixer.channels_mut()[at.0].decks[at.1]
                .deck
                .source_size()
        };
        assert_eq!(placed(&mut app, first), Some([2880, 1620]), "zoomed into");
        assert_eq!(placed(&mut app, second), Some([1920, 1080]), "as it came");

        // Zoomed into itself, it wants what one fitted does.
        zoom(&mut app, second, 2.0);
        app.size_captures_as_drawn_at(at(7000));
        assert_eq!(drawn(&app), sized(2.0, 0.0), "centred, zoomed into");
    }

    /// Given room, a screen's capture comes twice as big as its input draws
    /// it — a 4K screen whole, for a 1080p show's input that fits it — and
    /// the input draws it from its view as coded, of the texture it has now.
    /// Centred, the input places it as it came fitted, not by the pixels
    /// its room gave it.
    #[test]
    fn a_capture_comes_with_room_and_is_drawn_as_coded() {
        use crate::deck::ScalingMode;
        use crate::screen_capture::backend::{CaptureConfig, Drawn};
        use std::time::{Duration, Instant};
        fn deck_at(app: &mut VardaApp, at: (usize, usize)) -> &mut crate::deck::Deck {
            &mut app.mixer.channels_mut()[at.0].decks[at.1].deck
        }
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        if !app.context.reads_a_texture_two_ways() {
            eprintln!("Skipping: this GPU reads a texture one way");
            return;
        }
        app.set_render_resolution(1920, 1080);
        with_screens(&mut app, &[("capture", 3840)]);
        let input = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, input).capture_id;
        // Capped as a show's own is (the desk's mock opened it with none).
        app.screen_capture_manager
            .cap_to(capture, Some((1920, 1080)));
        deck_at(&mut app, input).set_scaling_mode(ScalingMode::Fit);
        let needed = std::collections::HashSet::from([capture]);
        let comes = |app: &mut VardaApp, size: (u32, u32)| {
            let until = Instant::now() + Duration::from_secs(10);
            while app.screen_capture_manager.resolution(capture) != Some(size)
                && Instant::now() < until
            {
                app.screen_capture_manager.update_selective(
                    &app.context,
                    &needed,
                    crate::host_clock::host_seconds(),
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            app.screen_capture_manager.resolution(capture)
        };
        let fitted = |fitted| Drawn {
            fitted,
            ..Drawn::default()
        };
        let drawn = |app: &VardaApp| app.screen_capture_manager.drawn(capture).expect("running");
        // The view the input draws its capture from as coded.
        let coded = |app: &mut VardaApp| deck_at(app, input).external_source_coded_view.clone();

        // No room, as a test's desk has it: as big as it is drawn.
        draw_a_frame(&mut app);
        assert_eq!(drawn(&app), fitted(1.0));
        assert_eq!(comes(&mut app, (1920, 1080)), Some((1920, 1080)));
        draw_a_frame(&mut app);
        let small = coded(&mut app).expect("drawn as coded");
        assert_eq!(
            Some(&small),
            app.screen_capture_manager.coded_view(capture),
            "its capture's view of it as coded"
        );
        assert_eq!(small.texture().width(), 1920);

        // Room for twice: all the screen has.
        app.screen_capture_manager.give_room(2.0);
        app.size_captures_as_drawn_at(Instant::now() + Duration::from_secs(1));
        assert_eq!(drawn(&app), fitted(2.0), "twice what it is drawn at");
        assert_eq!(comes(&mut app, (3840, 2160)), Some((3840, 2160)));
        draw_a_frame(&mut app);
        let big = coded(&mut app).expect("drawn as coded");
        assert_eq!(
            Some(&big),
            app.screen_capture_manager.coded_view(capture),
            "of the texture it has now"
        );
        assert_eq!(big.texture().width(), 3840);
        assert_ne!(
            deck_at(&mut app, input).external_source_view.as_ref(),
            Some(&big),
            "beside its view as light"
        );

        // Centred: placed as it came fitted, its room not seen.
        deck_at(&mut app, input).set_scaling_mode(ScalingMode::Center);
        draw_a_frame(&mut app);
        assert_eq!(drawn(&app), fitted(2.0), "centred: as one fitted");
        assert_eq!(
            deck_at(&mut app, input).source_size(),
            Some([1920, 1080]),
            "as it came fitted"
        );
    }

    /// A PowerPoint show's capture is left as it was: its input draws it
    /// from its light, where a screen's or a window's is drawn as coded.
    #[test]
    fn a_shows_capture_is_not_drawn_as_coded() {
        use crate::screen_capture::backend::CaptureConfig;
        fn deck_at(app: &mut VardaApp, at: (usize, usize)) -> &mut crate::deck::Deck {
            &mut app.mixer.channels_mut()[at.0].decks[at.1].deck
        }
        let Some(mut app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        if !app.context.reads_a_texture_two_ways() {
            eprintln!("Skipping: this GPU reads a texture one way");
            return;
        }
        app.set_render_resolution(1920, 1080);
        with_screens(&mut app, &[("capture", 1920)]);
        let input = screen_input(&mut app, CaptureConfig::default());
        let capture = capture_state_at(&mut app, input).capture_id;
        draw_a_frame(&mut app);
        assert!(
            deck_at(&mut app, input).external_source_coded_view.is_some(),
            "a screen's: as coded"
        );

        // Its show's from here on (no PowerPoint in a test: nothing plays).
        capture_state_at(&mut app, input).presentation =
            Some(crate::presentation::PresentationConfig {
                path: "/talks/a.pptx".into(),
                slide: None,
                plays_on: crate::engine::value::presentation::PlaysOn::default(),
            });
        draw_a_frame(&mut app);
        let deck = deck_at(&mut app, input);
        assert_eq!(deck.screen_capture_id(), Some(capture), "the same capture");
        assert!(deck.external_source_view.is_some(), "drawn");
        assert!(deck.external_source_coded_view.is_none(), "from its light");
    }

    #[test]
    fn smoke_engine_starts_with_two_channels() {
        let Some(app) = headless_app() else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let state = app.build_engine_state();
        assert_eq!(
            state.mixer.channels.len(),
            2,
            "default mixer has 2 channels"
        );
        assert_eq!(state.mixer.crossfader, 0.0, "crossfader starts at A");
    }

    #[test]
    fn smoke_add_channel_via_command() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let tx = app.command_sender();
        tx.send((crate::engine::EngineCommand::AddChannel, None))
            .unwrap();
        app.process_commands();
        let state = app.build_engine_state();
        assert_eq!(state.mixer.channels.len(), 3);
    }

    #[test]
    fn smoke_set_crossfader_via_command() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let tx = app.command_sender();
        tx.send((crate::engine::EngineCommand::SetCrossfader(0.75), None))
            .unwrap();
        app.process_commands();
        let state = app.build_engine_state();
        assert!((state.mixer.crossfader - 0.75).abs() < 1e-5);
    }

    #[test]
    fn smoke_render_frame_no_crash() {
        let Some(mut app) = headless_app() else {
            return;
        };
        // Render several frames — verify no panics and FPS stabilizes
        for _ in 0..5 {
            app.update_frame_timing();
            app.render_mixer_frame();
        }
        let state = app.build_engine_state();
        assert!(state.fps >= 0.0, "FPS should be non-negative");
    }

    #[test]
    fn smoke_add_solid_color_deck() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let before = app.build_engine_state().mixer.channels[0].decks.len();
        let tx = app.command_sender();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send((
            crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: ch0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
            Some(reply_tx),
        ))
        .unwrap();
        app.process_commands();
        let result = reply_rx.blocking_recv().unwrap();
        // Deck-creating commands report the new deck's UUID (ui-engine-boundary.md WS1).
        assert!(
            matches!(result, crate::engine::CommandResult::OkWithId { .. }),
            "command should succeed: {result:?}"
        );
        let after = app.build_engine_state().mixer.channels[0].decks.len();
        assert_eq!(after, before + 1, "should have one more deck");
    }

    #[test]
    fn smoke_set_channel_opacity() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let tx = app.command_sender();
        tx.send((
            crate::engine::EngineCommand::SetChannelOpacity {
                channel_uuid: ch0,
                opacity: 0.5,
            },
            None,
        ))
        .unwrap();
        app.process_commands();
        let state = app.build_engine_state();
        assert!((state.mixer.channels[0].opacity - 0.5).abs() < 1e-5);
    }

    /// Regression test for the previously-dead API undo/redo path: bus-driven
    /// (HTTP API / headless) commands must record onto the shared timeline so
    /// `Undo`/`Redo` sent over the same bus actually restore state.
    #[test]
    fn api_command_undo_redo_roundtrip() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let tx = app.command_sender();

        // Baseline: no history yet, nothing to undo.
        assert!(!app.history_can_undo(), "fresh app has empty undo stack");
        let original = app.build_engine_state().mixer.channels[0].opacity;

        // An undoable, bus-driven edit records a pre-mutation snapshot.
        let new_opacity = if (original - 0.5).abs() < 1e-3 {
            0.25
        } else {
            0.5
        };
        tx.send((
            crate::engine::EngineCommand::SetChannelOpacity {
                channel_uuid: ch0,
                opacity: new_opacity,
            },
            None,
        ))
        .unwrap();
        app.process_commands();
        assert!(
            app.history_can_undo(),
            "undoable API command should record history"
        );
        assert!((app.build_engine_state().mixer.channels[0].opacity - new_opacity).abs() < 1e-5);

        // Undo over the bus restores the pre-edit opacity.
        tx.send((crate::engine::EngineCommand::Undo, None)).unwrap();
        app.process_commands();
        assert!(
            (app.build_engine_state().mixer.channels[0].opacity - original).abs() < 1e-5,
            "API undo must restore the pre-command state"
        );
        assert!(app.history_can_redo(), "after undo, redo is available");

        // Redo re-applies the edit.
        tx.send((crate::engine::EngineCommand::Redo, None)).unwrap();
        app.process_commands();
        assert!(
            (app.build_engine_state().mixer.channels[0].opacity - new_opacity).abs() < 1e-5,
            "API redo must re-apply the command"
        );
    }

    /// Live-control commands (crossfader) must NOT pollute the undo timeline.
    #[test]
    fn live_control_commands_do_not_record_history() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let tx = app.command_sender();
        tx.send((crate::engine::EngineCommand::SetCrossfader(0.5), None))
            .unwrap();
        app.process_commands();
        assert!(
            !app.history_can_undo(),
            "crossfader is a live control and must not record history"
        );
    }

    #[test]
    fn stage_diff_restores_surface_geometry() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let tx = app.command_sender();
        tx.send((
            crate::engine::EngineCommand::AddSurface {
                name: "S".into(),
                source: crate::engine::OutputSource::Master,
            },
            None,
        ))
        .unwrap();
        app.process_commands();

        // Capture the added surface's uuid + original geometry.
        let (uuid, orig) = {
            let s = app
                .output
                .surface_manager
                .surfaces
                .last()
                .expect("surface added");
            (s.uuid.clone(), s.vertices.clone())
        };

        // Snapshot the stage state before mutating.
        let snap = app.history_snapshot_default();

        // Move the surface.
        tx.send((
            crate::engine::EngineCommand::MoveSurface {
                uuid: uuid.clone(),
                dx: 0.2,
                dy: 0.1,
            },
            None,
        ))
        .unwrap();
        app.process_commands();
        let moved = app
            .output
            .surface_manager
            .surfaces
            .last()
            .unwrap()
            .vertices
            .clone();
        assert_ne!(orig, moved, "surface should have moved");

        // Restore the stage snapshot — geometry returns to pre-move state.
        app.apply_stage_diff(&snap.stage);
        let restored = app
            .output
            .surface_manager
            .surfaces
            .last()
            .unwrap()
            .vertices
            .clone();
        assert_eq!(
            orig, restored,
            "apply_stage_diff should restore pre-move surface geometry"
        );
    }

    #[test]
    fn smoke_add_lfo_modulation() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let tx = app.command_sender();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send((
            crate::engine::EngineCommand::AddLfo {
                waveform: crate::modulation::LFOWaveform::Sine,
                frequency: 2.0,
            },
            Some(reply_tx),
        ))
        .unwrap();
        app.process_commands();
        let result = reply_rx.blocking_recv().unwrap();
        assert!(
            matches!(result, crate::engine::CommandResult::Ok),
            "AddLfo failed: {result:?}"
        );
        let state = app.build_engine_state();
        assert!(!state.modulation.sources.is_empty());
    }

    #[test]
    fn smoke_remove_channel() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let tx = app.command_sender();
        // Add a third channel first
        tx.send((crate::engine::EngineCommand::AddChannel, None))
            .unwrap();
        app.process_commands();
        assert_eq!(app.build_engine_state().mixer.channels.len(), 3);
        // Remove the third channel
        let ch2 = channel_uuid(&app, 2);
        tx.send((
            crate::engine::EngineCommand::RemoveChannel { channel_uuid: ch2 },
            None,
        ))
        .unwrap();
        app.process_commands();
        assert_eq!(app.build_engine_state().mixer.channels.len(), 2);
    }

    #[test]
    fn smoke_set_deck_blend_mode() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let added = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: ch0,
                color: [0.0, 1.0, 0.0, 1.0],
            },
        );
        let crate::engine::CommandResult::OkWithId { uuid } = added else {
            panic!("expected OkWithId, got {added:?}");
        };
        fire(
            &mut app,
            crate::engine::EngineCommand::SetDeckBlendMode {
                deck_uuid: uuid,
                mode: crate::engine::BlendMode::Add,
            },
        );
        let state = app.build_engine_state();
        assert_eq!(
            state.mixer.channels[0].decks[0].blend_mode,
            crate::engine::BlendMode::Add
        );
    }

    #[test]
    fn clamp_resolution_leaves_in_range_untouched() {
        // Within the GPU limit: returned unchanged.
        assert_eq!(clamp_resolution_to_gpu(1920, 1080, 16384), (1920, 1080));
        assert_eq!(clamp_resolution_to_gpu(16384, 16384, 16384), (16384, 16384));
    }

    #[test]
    fn clamp_resolution_caps_each_dimension_to_gpu_max() {
        // Beyond the GPU limit: each dimension clamped independently.
        assert_eq!(clamp_resolution_to_gpu(20000, 12000, 16384), (16384, 12000));
        assert_eq!(clamp_resolution_to_gpu(30000, 30000, 16384), (16384, 16384));
        assert_eq!(clamp_resolution_to_gpu(1024, 99999, 8192), (1024, 8192));
    }

    #[test]
    fn smoke_set_render_resolution() {
        let Some(mut app) = headless_app() else {
            return;
        };
        app.set_render_resolution(1280, 720);
        assert_eq!(app.render_width(), 1280);
        assert_eq!(app.render_height(), 720);
    }

    #[test]
    fn smoke_set_render_resolution_zero_ignored() {
        let Some(mut app) = headless_app() else {
            return;
        };
        app.set_render_resolution(0, 0);
        // Should keep previous resolution
        assert!(app.render_width() > 0);
        assert!(app.render_height() > 0);
    }

    /// A headless output used to take the render resolution once, at creation,
    /// so switching a project to portrait afterwards left the recording buffer
    /// landscape and ffmpeg was spawned with the stale `-s WxH`. The saved file
    /// came out the old shape with the portrait composite squashed into it.
    #[test]
    fn headless_outputs_follow_the_render_resolution() {
        let Some(mut app) = headless_app() else {
            return;
        };
        fire(
            &mut app,
            crate::engine::EngineCommand::CreateHeadlessOutput {
                target: crate::engine::value::render::OutputTarget::Recording {
                    path: "unused.mp4".into(),
                    codec: crate::renderer::context::RecordingCodec::H264,
                    audio_device: None,
                },
            },
        );

        let dimensions = |app: &VardaApp| {
            app.output
                .outputs
                .iter()
                .filter_map(|o| match o {
                    UnifiedOutput::Headless(h) => Some((h.width, h.height)),
                    UnifiedOutput::Window(_) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            dimensions(&app),
            vec![(app.render_width(), app.render_height())],
            "a new output should start at the current render resolution"
        );

        app.set_render_resolution(1080, 1920);
        assert_eq!(
            dimensions(&app),
            vec![(1080, 1920)],
            "the output should have followed the resolution change"
        );
    }

    #[test]
    fn smoke_publish_and_read_state() {
        let Some(app) = headless_app() else {
            return;
        };
        let reader = app.state_reader();
        app.publish_state();
        let guard = reader.read().unwrap();
        let state = guard.as_ref().expect("state should be published");
        assert_eq!(state.mixer.channels.len(), 2);
    }

    /// A reader holding the published state — an API client slow to
    /// serialize a big show — never holds up publishing: the engine goes on.
    #[test]
    fn a_reader_holding_the_state_never_holds_up_publishing() {
        let Some(app) = headless_app() else {
            return;
        };
        let reader = app.state_reader();
        app.publish_state();
        let (holding, held) = std::sync::mpsc::channel();
        let holder = std::thread::spawn(move || {
            let guard = reader.read().unwrap();
            holding.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(2));
            drop(guard);
        });
        held.recv().unwrap();
        let started = std::time::Instant::now();
        app.publish_state();
        assert!(
            started.elapsed() < std::time::Duration::from_millis(500),
            "publishing waited for the reader: {:?}",
            started.elapsed()
        );
        holder.join().unwrap();
        app.publish_state();
        assert!(app.state_reader().read().unwrap().is_some());
    }

    #[test]
    fn smoke_notifications() {
        let Some(mut app) = headless_app() else {
            return;
        };
        app.notify_info("Test notification");
        app.update_notifications();
        // Verify no crash
    }

    /// Add Input that fails says why, rather than adding nothing silently —
    /// a camera the OS will not open, say.
    #[test]
    fn an_input_that_cannot_be_added_says_why() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let mut actions = crate::usecases::ui::UIActions::new();
        actions
            .commands
            .push(crate::engine::EngineCommand::AddInput {
                source: crate::engine::InputSource::Camera {
                    camera_id: 3,
                    mode: None,
                },
                name: None,
                audio: None,
            });
        let _ = app.apply_engine_actions(&mut actions);
        let shown: Vec<&str> = app
            .session
            .notifications
            .visible()
            .iter()
            .map(|n| n.message.as_str())
            .collect();
        assert!(
            shown.iter().any(|m| m.starts_with("Input not added:")),
            "{shown:?}"
        );
    }

    /// A stream that cannot start says why, as an input that cannot be
    /// added does: the STREAM button alone just stays off.
    #[test]
    fn a_stream_that_cannot_start_says_why() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let mut actions = crate::usecases::ui::UIActions::new();
        // No destination switched on.
        actions
            .commands
            .push(crate::engine::EngineCommand::StartStream);
        let _ = app.apply_engine_actions(&mut actions);
        let shown: Vec<&str> = app
            .session
            .notifications
            .visible()
            .iter()
            .map(|n| n.message.as_str())
            .collect();
        assert!(
            shown
                .iter()
                .any(|m| m.starts_with("Stream not started:") && m.contains("destination")),
            "{shown:?}"
        );
    }

    // ── Extended smoke tests ───────────────────────────────────────

    /// Helper: send command with reply, process, return result.
    fn send_cmd(
        app: &mut VardaApp,
        cmd: crate::engine::EngineCommand,
    ) -> crate::engine::CommandResult {
        let tx = app.command_sender();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        tx.send((cmd, Some(reply_tx))).unwrap();
        crate::testing::reply_of(app, reply_rx)
    }

    /// Helper: fire-and-forget command.
    fn fire(app: &mut VardaApp, cmd: crate::engine::EngineCommand) {
        app.command_sender().send((cmd, None)).unwrap();
        app.process_commands();
    }

    #[test]
    fn smoke_auto_crossfade_lifecycle() {
        let Some(mut app) = headless_app() else {
            return;
        };
        fire(
            &mut app,
            crate::engine::EngineCommand::AutoCrossfade {
                target: 1.0,
                duration_secs: 0.05,
                easing: crate::mixer::CrossfadeEasing::Linear,
            },
        );
        // Tick enough frames for it to complete
        for _ in 0..60 {
            app.update_frame_timing();
            app.render_mixer_frame();
        }
        // Verify the auto crossfade was started and no crash occurred.
        // Timing in headless mode is unpredictable, so just verify no panic.
    }

    #[test]
    fn smoke_add_video_deck_no_crash() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        // Bad path — should not panic
        let _ = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddVideoDeck {
                channel_uuid: ch0,
                path: std::path::PathBuf::from("/nonexistent/video.mp4"),
            },
        );
    }

    #[test]
    fn smoke_video_toggle_play() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let added = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: ch0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        );
        let crate::engine::CommandResult::OkWithId { uuid } = added else {
            panic!("expected OkWithId, got {added:?}");
        };
        // Toggle play on a non-video deck — should handle gracefully
        let _ = send_cmd(
            &mut app,
            crate::engine::EngineCommand::VideoTogglePlay { deck_uuid: uuid },
        );
    }

    #[test]
    fn smoke_video_set_speed() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let added = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: ch0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        );
        let crate::engine::CommandResult::OkWithId { uuid } = added else {
            panic!("expected OkWithId, got {added:?}");
        };
        let _ = send_cmd(
            &mut app,
            crate::engine::EngineCommand::VideoSetSpeed {
                deck_uuid: uuid,
                speed: 2.0,
            },
        );
    }

    #[test]
    fn video_transport_sync_rejects_a_non_video_deck() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let added = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: ch0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        );
        let crate::engine::CommandResult::OkWithId { uuid } = added else {
            panic!("expected OkWithId, got {added:?}");
        };

        let result = send_cmd(
            &mut app,
            crate::engine::EngineCommand::VideoSetTransportSync {
                deck_uuid: uuid,
                sync: crate::video::DeckTransportSync::default(),
            },
        );
        assert!(matches!(
            result,
            crate::engine::CommandResult::Err {
                code: crate::engine::ErrorCode::InvalidInput,
                ..
            }
        ));
    }

    #[test]
    fn smoke_create_sequence() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let r = send_cmd(&mut app, crate::engine::EngineCommand::CreateSequence);
        let crate::engine::CommandResult::OkWithId { uuid } = r else {
            panic!("expected OkWithId, got {r:?}");
        };
        let state = app.build_engine_state();
        assert_eq!(state.mixer.sequences.len(), 1);
        assert_eq!(state.mixer.sequences[0].uuid, uuid);
    }

    #[test]
    fn smoke_sequence_add_steps() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let ch1 = channel_uuid(&app, 1);
        let created = send_cmd(&mut app, crate::engine::EngineCommand::CreateSequence);
        let crate::engine::CommandResult::OkWithId { uuid: seq } = created else {
            panic!("expected OkWithId, got {created:?}");
        };
        send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddFadeStep {
                sequence_uuid: seq.clone(),
                from_channel_uuid: ch0,
                to_channel_uuid: ch1,
            },
        );
        send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddWaitStep { sequence_uuid: seq },
        );
        let state = app.build_engine_state();
        assert_eq!(state.mixer.sequences[0].steps.len(), 2);
    }

    #[test]
    fn smoke_effect_chain_operations() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        let added = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddSolidColorDeck {
                channel_uuid: ch0,
                color: [1.0, 0.0, 0.0, 1.0],
            },
        );
        let crate::engine::CommandResult::OkWithId { uuid: deck } = added else {
            panic!("expected OkWithId, got {added:?}");
        };
        let r = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddEffect {
                target: crate::engine::EffectTarget::Deck(deck),
                shader_name: "invert".into(),
            },
        );
        let crate::engine::CommandResult::OkWithId { uuid: effect } = r else {
            panic!("expected OkWithId, got {r:?}");
        };
        let toggled = send_cmd(
            &mut app,
            crate::engine::EngineCommand::ToggleEffect {
                effect_uuid: effect.clone(),
            },
        );
        assert!(matches!(toggled, crate::engine::CommandResult::Ok));
        let removed = send_cmd(
            &mut app,
            crate::engine::EngineCommand::RemoveEffect {
                effect_uuid: effect,
            },
        );
        assert!(matches!(removed, crate::engine::CommandResult::Ok));
    }

    #[test]
    fn smoke_multiple_modulation_sources() {
        let Some(mut app) = headless_app() else {
            return;
        };
        send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddLfo {
                waveform: crate::modulation::LFOWaveform::Sine,
                frequency: 1.0,
            },
        );
        send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddStepSequencer {
                num_steps: 8,
                rate: 2.0,
            },
        );
        send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddAdsr {
                attack: 0.1,
                decay: 0.2,
                sustain: 0.7,
                release: 0.3,
            },
        );
        let state = app.build_engine_state();
        assert_eq!(state.modulation.sources.len(), 3);
    }

    #[test]
    fn smoke_adsr_trigger_release() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let r = send_cmd(
            &mut app,
            crate::engine::EngineCommand::AddAdsr {
                attack: 0.01,
                decay: 0.01,
                sustain: 0.5,
                release: 0.01,
            },
        );
        assert!(matches!(r, crate::engine::CommandResult::Ok));
        let state = app.build_engine_state();
        let uuid = state.modulation.sources[0].uuid.clone();
        // Trigger
        fire(
            &mut app,
            crate::engine::EngineCommand::TriggerAdsr { uuid: uuid.clone() },
        );
        for _ in 0..5 {
            app.update_frame_timing();
            app.render_mixer_frame();
        }
        // Release
        fire(&mut app, crate::engine::EngineCommand::ReleaseAdsr { uuid });
        for _ in 0..5 {
            app.update_frame_timing();
            app.render_mixer_frame();
        }
    }

    #[test]
    fn smoke_set_channel_blend_mode() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let ch0 = channel_uuid(&app, 0);
        fire(
            &mut app,
            crate::engine::EngineCommand::SetChannelBlendMode {
                channel_uuid: ch0,
                mode: crate::engine::BlendMode::Add,
            },
        );
        let state = app.build_engine_state();
        assert_eq!(
            state.mixer.channels[0].blend_mode,
            crate::engine::BlendMode::Add
        );
    }

    /// Auto moves the master only once a screen size has held for the settle
    /// window, and starts counting again whenever the answer changes, so a
    /// display renegotiating its mode does not rebuild every texture twice.
    #[test]
    fn auto_resolution_waits_for_the_screen_size_to_settle() {
        let Some(mut app) = headless_app() else {
            return;
        };
        let hd = (1920, 1080);
        let uhd = (3840, 2160);
        assert_eq!((app.render_width, app.render_height), hd);
        let settle = outputs::AUTO_RESOLUTION_SETTLE_FRAMES;

        for _ in 1..settle {
            app.settle_auto_resolution(Some(uhd));
        }
        assert_eq!((app.render_width, app.render_height), hd, "not yet settled");
        // A flicker to another size restarts the count.
        app.settle_auto_resolution(Some((2560, 1440)));
        for _ in 1..settle {
            app.settle_auto_resolution(Some(uhd));
        }
        assert_eq!(
            (app.render_width, app.render_height),
            hd,
            "the count restarted"
        );
        app.settle_auto_resolution(Some(uhd));
        assert_eq!((app.render_width, app.render_height), uhd, "settled on 4K");

        // Nothing wanted (Auto off, or no output on a screen) holds the size.
        for _ in 0..settle * 2 {
            app.settle_auto_resolution(None);
        }
        assert_eq!((app.render_width, app.render_height), uhd);
    }
}
