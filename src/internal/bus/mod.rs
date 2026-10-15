//! Production buses — independent Preview/Program switching.
//!
//! A [`Bus`] is the conference-switcher analogue of an M/E bus on a hardware
//! vision mixer (vMix calls the same idea a Mix). Each bus carries its own
//! Preview (`PVW`) and Program (`PGM`) selection, its own in-flight transition,
//! its own canvas size, and the identity of the audio mix it owns. Taking a
//! source on one bus never mutates another.
//!
//! # Scope
//!
//! This module delivers *switching and routing* independence: buses select
//! among shared, globally mutable feeds. It is deliberately **not** independent
//! bus *composition* — two buses pointing at the same channel still share that
//! channel's decks, effects and opacity, so editing it affects both. Per-bus
//! composition requires the deck-product split described in docs/conference/known-upstream-defects.md.
//!
//! # Addressing
//!
//! [`BusInputRef`] addresses channels and decks by their stable 8-char hex
//! UUID, never by index. Index-based references shift when a channel is
//! removed, which would silently re-point persisted on-air routing at the wrong
//! source.
//!
//! A bus cannot reference another bus. That is enforced by the type: there is
//! no bus variant in [`BusInputRef`], so cycles are impossible by construction
//! rather than something to validate at runtime. [`BusInputRef::Master`] does
//! not break this: the master mix is composited from channels only and never
//! reads a bus.

use crate::mixer::CrossfadeEasing;

mod together;
pub use together::{TakenTogether, Together, TogetherRejected};

/// Default canvas width for a new bus.
pub const DEFAULT_BUS_WIDTH: u32 = 1920;
/// Default canvas height for a new bus.
pub const DEFAULT_BUS_HEIGHT: u32 = 1080;
/// Default `AUTO` transition duration in seconds.
pub const DEFAULT_AUTO_DURATION: f32 = 1.0;

/// What a bus's Preview or Program points at.
///
/// Channels and decks are addressed by stable UUID. There is deliberately no
/// bus variant — see the module docs on cycles.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub enum BusInputRef {
    /// Nothing on air. What a bus shows when its source is gone, and what a
    /// route held by quarantine shows; a new bus starts on Master instead
    /// (see [`Bus::new_on_main`]).
    #[default]
    Black,
    /// The master mix: every mixer layer composited, as the main output shows
    /// it. A bus can leave some layers out of it (see [`Bus::hidden_layers`]),
    /// the way vMix leaves chosen overlays off one output.
    Master,
    /// A channel's composited output, by stable channel UUID.
    Channel(String),
    /// A single deck's output, by stable deck UUID.
    Deck(String),
}

impl BusInputRef {
    /// The UUID this reference addresses, or `None` for [`BusInputRef::Black`]
    /// and [`BusInputRef::Master`].
    pub fn uuid(&self) -> Option<&str> {
        match self {
            BusInputRef::Black | BusInputRef::Master => None,
            BusInputRef::Channel(uuid) | BusInputRef::Deck(uuid) => Some(uuid),
        }
    }

    /// Whether this reference addresses the given UUID.
    pub fn addresses(&self, uuid: &str) -> bool {
        self.uuid() == Some(uuid)
    }
}

impl std::fmt::Display for BusInputRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BusInputRef::Black => write!(f, "Black"),
            BusInputRef::Master => write!(f, "Master"),
            BusInputRef::Channel(uuid) => write!(f, "Ch {uuid}"),
            BusInputRef::Deck(uuid) => write!(f, "Deck {uuid}"),
        }
    }
}

/// The region of the master raster a bus represents.
///
/// **Not** a separate render target. Everything still composites once at the
/// master resolution, and a bus declares which rectangle of that image is its
/// picture. A destination following the bus samples exactly that rectangle.
///
/// This is how LED processors expect to be fed and how comparable tools (the
/// Resolume "advanced output" model) work: send a standard display mode — 1080p
/// or 4K — and tell the processor which region of it maps onto the panels.
/// Driving a bespoke 1920x480 display mode is the unusual path, and the one
/// that fights the OS display system.
///
/// Sampling a sub-rectangle is **lossless**: a 1920x480 band of a 1920x1080
/// master is those pixels 1:1, with no resampling. Only scaling the whole
/// master into a differently shaped region resamples.
///
/// The cost is that a bus cannot exceed the master raster — a wall wider than
/// the master needs a larger master — and content is authored inside the master
/// canvas rather than in its own coordinate space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct BusCanvas {
    /// Region width in master-raster pixels. Always >= 1.
    pub width: u32,
    /// Region height in master-raster pixels. Always >= 1.
    pub height: u32,
    /// Left edge of the region within the master raster, in pixels.
    #[serde(default)]
    pub origin_x: u32,
    /// Top edge of the region within the master raster, in pixels.
    #[serde(default)]
    pub origin_y: u32,
}

// Deserialization goes through `BusCanvas::new` so a hand-edited or corrupt
// stage file cannot introduce a zero dimension. A derived impl would write the
// fields directly and bypass the clamp, and a 0-sized canvas becomes an invalid
// GPU texture allocation once per-bus composition lands.
impl<'de> serde::Deserialize<'de> for BusCanvas {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(serde::Deserialize)]
        struct Raw {
            width: u32,
            height: u32,
            #[serde(default)]
            origin_x: u32,
            #[serde(default)]
            origin_y: u32,
        }
        let raw = Raw::deserialize(deserializer)?;
        Ok(Self::new(raw.width, raw.height).at(raw.origin_x, raw.origin_y))
    }
}

impl BusCanvas {
    /// A region of the given size at the master's origin, clamped to >= 1x1.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width: width.max(1),
            height: height.max(1),
            origin_x: 0,
            origin_y: 0,
        }
    }

    /// The same region moved to `(x, y)` within the master raster.
    #[must_use]
    pub fn at(self, x: u32, y: u32) -> Self {
        Self {
            origin_x: x,
            origin_y: y,
            ..self
        }
    }

    /// UV scale and offset for sampling this region out of a `master` raster.
    ///
    /// Returned in the same form the surface path already uses, so a bus feed
    /// substitutes for a surface's own mapping. Clamped to the master, because
    /// a region hanging off the edge would sample outside the texture and read
    /// whatever the sampler's address mode gives — repeated or smeared edge
    /// pixels, on air, with nothing to indicate why.
    pub fn uv(self, master_width: u32, master_height: u32) -> ([f32; 2], [f32; 2]) {
        let mw = master_width.max(1) as f32;
        let mh = master_height.max(1) as f32;
        let w = (self.width as f32 / mw).clamp(0.0, 1.0);
        let h = (self.height as f32 / mh).clamp(0.0, 1.0);
        let x = (self.origin_x as f32 / mw).clamp(0.0, 1.0 - w);
        let y = (self.origin_y as f32 / mh).clamp(0.0, 1.0 - h);
        ([w, h], [x, y])
    }

    /// Whether this region fits inside a `master` raster without clamping.
    pub fn fits_within(self, master_width: u32, master_height: u32) -> bool {
        // Widened so an origin near `u32::MAX` cannot wrap back inside the master.
        u64::from(self.origin_x) + u64::from(self.width) <= u64::from(master_width)
            && u64::from(self.origin_y) + u64::from(self.height) <= u64::from(master_height)
    }

    /// The same part of the picture on a master resized from `from` to `to`
    /// (each a width and height).
    ///
    /// Each axis scales on its own, so a full-frame bus stays full frame when
    /// the master goes from 1080p to 4K, and a strip stays the same strip. The
    /// result is kept at least 1x1 and inside the new master.
    #[must_use]
    pub fn rescaled(self, from: (u32, u32), to: (u32, u32)) -> Self {
        let scale = |v: u32, from: u32, to: u32| {
            let from = u64::from(from.max(1));
            u32::try_from((u64::from(v) * u64::from(to) + from / 2) / from).unwrap_or(u32::MAX)
        };
        let width = scale(self.width, from.0, to.0).clamp(1, to.0.max(1));
        let height = scale(self.height, from.1, to.1).clamp(1, to.1.max(1));
        let x = scale(self.origin_x, from.0, to.0).min(to.0.saturating_sub(width));
        let y = scale(self.origin_y, from.1, to.1).min(to.1.saturating_sub(height));
        Self::new(width, height).at(x, y)
    }

    /// Aspect ratio (width / height).
    pub fn aspect(self) -> f32 {
        self.width as f32 / self.height as f32
    }
}

impl Default for BusCanvas {
    fn default() -> Self {
        Self::new(DEFAULT_BUS_WIDTH, DEFAULT_BUS_HEIGHT)
    }
}

/// Identity of the audio mix a bus owns.
///
/// Reserved seam. Audio mixing is not implemented (see docs/conference/known-upstream-defects.md),
/// but destinations must follow their bus's mix identity rather than binding a
/// capture device of their own — otherwise video buses and audio routing end up
/// with incompatible ownership models that are painful to reconcile later.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct AudioMixId(String);

impl AudioMixId {
    /// Wrap an existing identity string.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The identity as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The sources a bus is allowed to reference, split by kind.
///
/// Kept variant-aware on purpose: a single flat UUID set would let
/// `Channel(deck_uuid)` validate successfully, which is exactly the class of
/// mistake stable UUIDs are supposed to prevent.
#[derive(Debug, Clone, Default)]
pub struct KnownSources {
    /// Channel UUIDs present in the loaded scene.
    pub channels: std::collections::BTreeSet<String>,
    /// Deck UUIDs present in the loaded scene.
    pub decks: std::collections::BTreeSet<String>,
    /// Whether the master mix counts as a source. True for any loaded scene;
    /// false only in [`KnownSources::EMPTY`], so a quarantined bus on Master
    /// holds black like every other route.
    pub master: bool,
}

impl KnownSources {
    /// Resolves nothing. Used while routing is quarantined.
    pub const EMPTY: Self = Self {
        channels: std::collections::BTreeSet::new(),
        decks: std::collections::BTreeSet::new(),
        master: false,
    };

    /// Whether `input` names a source of the right kind that actually exists.
    pub fn resolves(&self, input: &BusInputRef) -> bool {
        match input {
            BusInputRef::Black => true,
            BusInputRef::Master => self.master,
            BusInputRef::Channel(uuid) => self.channels.contains(uuid),
            BusInputRef::Deck(uuid) => self.decks.contains(uuid),
        }
    }
}

/// Why an `AUTO` take was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoRejected {
    /// A take is already in flight on this bus.
    AlreadyTransitioning,
    /// The requested duration was not finite and positive.
    InvalidDuration,
}

impl std::fmt::Display for AutoRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutoRejected::AlreadyTransitioning => {
                write!(f, "a transition is already running on this bus")
            }
            AutoRejected::InvalidDuration => {
                write!(f, "transition duration must be finite and positive")
            }
        }
    }
}

/// An in-flight `AUTO` transition from one input to another.
#[derive(Debug, Clone, PartialEq)]
pub struct BusTransition {
    /// The program content the bus is leaving.
    pub from: BusInputRef,
    /// The program content the bus is heading to.
    pub to: BusInputRef,
    /// Total duration in seconds. Always > 0.
    pub duration: f64,
    /// Monotonic frame clock reading, in seconds, when the take started.
    ///
    /// Elapsed time is derived from this rather than accumulated from `dt`.
    /// `dt` is measured once at the top of a frame, but commands run at
    /// different points relative to that measurement — queued API commands
    /// before the tick, native UI commands after it — so no per-frame delta can
    /// be attributed correctly to a take without knowing which side of the tick
    /// it started on. A start timestamp needs no such knowledge, and cannot
    /// drift or stall on accumulator precision.
    pub started_at: f64,
    /// Seconds elapsed, as of the last tick.
    pub elapsed: f64,
    /// Easing curve applied to progress.
    pub easing: CrossfadeEasing,
    /// Whether the outgoing endpoint resolves against the loaded scene.
    pub from_resolved: bool,
    /// Whether the incoming endpoint resolves against the loaded scene.
    pub to_resolved: bool,
}

impl BusTransition {
    /// Linear progress through the transition, 0.0 to 1.0.
    pub fn progress(&self) -> f32 {
        (self.elapsed / self.duration).clamp(0.0, 1.0) as f32
    }

    /// Eased mix factor, 0.0 (fully `from`) to 1.0 (fully `to`).
    pub fn mix(&self) -> f32 {
        self.easing.apply(self.progress())
    }
}

/// One independent production bus.
#[derive(Debug, Clone)]
pub struct Bus {
    uuid: String,
    /// Operator-facing label, e.g. `STREAM`, `LED`, `TV`.
    pub name: String,
    preview: BusInputRef,
    program: BusInputRef,
    canvas: BusCanvas,
    audio_mix: AudioMixId,
    /// Whether `preview` / `program` resolve against the loaded scene.
    ///
    /// An unresolvable reference is kept rather than erased so a later load of
    /// the correct scene restores the routing; it simply renders black until
    /// then. Erasing it would silently discard the operator's patch on the next
    /// save.
    preview_resolved: bool,
    program_resolved: bool,
    /// Bumped on every explicit Preview change.
    ///
    /// Comparing the *value* of Preview at completion cannot tell "untouched"
    /// from "changed away and back again", and would then discard the
    /// operator's second, deliberate choice. The revision distinguishes them.
    preview_rev: u64,
    /// `preview_rev` as it was when the running take started.
    preview_rev_at_take: u64,
    transition: Option<BusTransition>,
    /// Duration in seconds used by [`Bus::auto`] when none is given.
    pub auto_duration: f32,
    /// Easing curve used by [`Bus::auto`].
    pub easing: CrossfadeEasing,
    /// Mixer layers (channel UUIDs) this bus leaves out when it shows
    /// [`BusInputRef::Master`]. See [`Bus::hidden_layers`].
    hidden_layers: Vec<String>,
}

impl Bus {
    /// A new bus with the given name and canvas, black on both PVW and PGM.
    ///
    /// The bus owns an audio mix whose identity matches its own UUID.
    pub fn new(name: impl Into<String>, canvas: BusCanvas) -> Self {
        let uuid = crate::deck::generate_short_uuid();
        let audio_mix = AudioMixId::new(uuid.clone());
        Self {
            uuid,
            name: name.into(),
            preview: BusInputRef::Black,
            program: BusInputRef::Black,
            canvas,
            audio_mix,
            preview_resolved: true,
            program_resolved: true,
            preview_rev: 0,
            preview_rev_at_take: 0,
            transition: None,
            auto_duration: DEFAULT_AUTO_DURATION,
            easing: CrossfadeEasing::EaseInOut,
            hidden_layers: Vec::new(),
        }
    }

    /// A new bus showing Main — Master on Preview and Program — until
    /// something else is taken to it: sent to a screen, it shows the show.
    /// Started on black, a bus sent to a screen looked broken (an operator,
    /// 2026-10-06: "i try sending to led but black only").
    pub fn new_on_main(name: impl Into<String>, canvas: BusCanvas) -> Self {
        Self {
            preview: BusInputRef::Master,
            program: BusInputRef::Master,
            ..Self::new(name, canvas)
        }
    }

    /// Stable UUID for this bus.
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// What is staged on Preview.
    pub fn preview(&self) -> &BusInputRef {
        &self.preview
    }

    /// What is settled on Program.
    ///
    /// During a transition this is still the outgoing content; the on-air image
    /// is the blend described by [`Bus::transition`].
    pub fn program(&self) -> &BusInputRef {
        &self.program
    }

    /// Whether the Preview reference resolves against the loaded scene.
    pub fn preview_resolved(&self) -> bool {
        self.preview_resolved
    }

    /// Whether the Program reference resolves against the loaded scene.
    pub fn program_resolved(&self) -> bool {
        self.program_resolved
    }

    /// What Program should actually put on air.
    ///
    /// Black when the configured reference does not resolve. The configured
    /// value is still readable via [`Bus::program`] so the operator can see
    /// what the show intended and recover it by loading the right scene.
    pub fn effective_program(&self) -> &BusInputRef {
        if self.program_resolved {
            &self.program
        } else {
            &BusInputRef::Black
        }
    }

    /// What Preview should actually show. Black when unresolvable.
    pub fn effective_preview(&self) -> &BusInputRef {
        if self.preview_resolved {
            &self.preview
        } else {
            &BusInputRef::Black
        }
    }

    /// The bus canvas.
    pub fn canvas(&self) -> BusCanvas {
        self.canvas
    }

    /// Resize the bus canvas.
    pub fn set_canvas(&mut self, canvas: BusCanvas) {
        self.canvas = canvas;
    }

    /// The audio mix this bus owns.
    pub fn audio_mix(&self) -> &AudioMixId {
        &self.audio_mix
    }

    /// The in-flight transition, if any.
    pub fn transition(&self) -> Option<&BusTransition> {
        self.transition.as_ref()
    }

    /// Whether an `AUTO` is currently running.
    pub fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }

    /// The mixer layers (channel UUIDs) this bus leaves out of the master mix.
    ///
    /// Only [`BusInputRef::Master`] is affected: a bus showing one channel or
    /// deck shows it whole. This is vMix's per-output overlay switch — the
    /// stream keeps the lower third while the recording goes without — and it
    /// belongs to the bus, so switching the bus to another source and back
    /// keeps it.
    pub fn hidden_layers(&self) -> &[String] {
        &self.hidden_layers
    }

    /// Hide or show one mixer layer on this bus, returning whether that
    /// changed anything.
    pub fn set_layer_hidden(&mut self, channel_uuid: &str, hidden: bool) -> bool {
        let present = self.hidden_layers.iter().any(|c| c == channel_uuid);
        if hidden == present {
            return false;
        }
        if hidden {
            self.hidden_layers.push(channel_uuid.to_string());
        } else {
            self.hidden_layers.retain(|c| c != channel_uuid);
        }
        true
    }

    /// Stage `input` on Preview. Never touches Program.
    ///
    /// The caller is responsible for having resolved `input` first; the command
    /// layer rejects unresolvable references before reaching here, so anything
    /// staged this way is resolved by construction.
    pub fn set_preview(&mut self, input: BusInputRef) {
        self.preview = input;
        self.preview_resolved = true;
        self.preview_rev = self.preview_rev.wrapping_add(1);
    }

    /// Take Preview to Program instantly, cancelling any in-flight transition.
    ///
    /// Preview and Program swap (flip-flop), matching hardware switcher
    /// behaviour: after a cut, what was on air is staged and ready to come back.
    pub fn cut(&mut self) {
        self.transition = None;
        std::mem::swap(&mut self.preview, &mut self.program);
        std::mem::swap(&mut self.preview_resolved, &mut self.program_resolved);
    }

    /// Begin a timed transition from Program to Preview.
    ///
    /// `duration` falls back to [`Bus::auto_duration`] when `None`.
    ///
    /// # Errors
    ///
    /// [`AutoRejected::InvalidDuration`] if the duration is not finite and
    /// positive. A zero or negative duration is *not* silently downgraded to a
    /// cut: `CUT` is its own command, and quietly taking a source to air when
    /// the operator asked for a timed take is a destructive surprise.
    ///
    /// [`AutoRejected::AlreadyTransitioning`] if a take is in flight. The
    /// transition is a two-endpoint blend, so it cannot be continuously
    /// retargeted at a third source; restarting from the settled program would
    /// visibly snap back to the outgoing image mid-take.
    pub fn auto(&mut self, duration: Option<f32>, now: f64) -> Result<(), AutoRejected> {
        if self.transition.is_some() {
            return Err(AutoRejected::AlreadyTransitioning);
        }
        let duration = duration.unwrap_or(self.auto_duration);
        if !duration.is_finite() || duration <= 0.0 {
            return Err(AutoRejected::InvalidDuration);
        }
        self.start_take(f64::from(duration), self.easing, now);
        Ok(())
    }

    /// Start the transition from Program to Preview, replacing any running.
    /// `duration` is finite and positive: the callers have seen to it.
    fn start_take(&mut self, duration: f64, easing: CrossfadeEasing, now: f64) {
        self.transition = Some(BusTransition {
            from: self.program.clone(),
            to: self.preview.clone(),
            duration,
            started_at: now,
            elapsed: 0.0,
            easing,
            // Endpoint resolution is captured with the endpoints. Taking an
            // unresolved Preview must not launder it into a resolved Program.
            from_resolved: self.program_resolved,
            to_resolved: self.preview_resolved,
        });
        self.preview_rev_at_take = self.preview_rev;
    }

    /// Advance an in-flight transition by `dt` seconds.
    ///
    /// Returns `true` when a transition completed on this tick. On completion
    /// Program becomes the incoming content and Preview holds the outgoing one,
    /// matching the flip-flop of [`Bus::cut`].
    pub fn tick(&mut self, now: f64) -> bool {
        let Some(transition) = self.transition.as_mut() else {
            return false;
        };
        // Derived, not accumulated: correct no matter where in the frame the
        // take was started, and immune to a non-monotonic or absent `dt`.
        transition.elapsed = (now - transition.started_at).max(0.0);
        if transition.elapsed < transition.duration {
            return false;
        }
        let Some(finished) = self.transition.take() else {
            return false;
        };
        // If the operator staged anything during the take, that choice wins —
        // overwriting it would silently discard a deliberate action. Compared
        // by revision, not value, so staging away and back still counts.
        if self.preview_rev == self.preview_rev_at_take {
            self.preview = finished.from;
            self.preview_resolved = finished.from_resolved;
        }
        self.program = finished.to;
        self.program_resolved = finished.to_resolved;
        true
    }

    /// Whether this bus references `uuid` on Preview, Program, or mid-transition.
    ///
    /// Used to decide which sources a frame must actually render.
    pub fn references(&self, uuid: &str) -> bool {
        if self.preview.addresses(uuid) || self.program.addresses(uuid) {
            return true;
        }
        self.transition.as_ref().is_some_and(|transition| {
            transition.from.addresses(uuid) || transition.to.addresses(uuid)
        })
    }
}

/// Persisted form of a [`Bus`], stored in `.varda/stage.json`.
///
/// Buses are stage data rather than scene data: a bus canvas is dictated by the
/// LED processor or screen it feeds, which belongs to the venue and not to the
/// show playing on it. This mirrors `domemaster_resolution`.
///
/// An in-flight transition is deliberately **not** persisted. It is transient,
/// and restoring a half-completed take would put indeterminate content on air.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BusConfig {
    /// Stable bus UUID.
    pub uuid: String,
    /// Operator-facing label.
    pub name: String,
    /// Bus canvas size.
    #[serde(default)]
    pub canvas: BusCanvas,
    /// What was staged on Preview.
    #[serde(default)]
    pub preview: BusInputRef,
    /// What was on Program.
    #[serde(default)]
    pub program: BusInputRef,
    /// Audio mix identity. Absent in files written before a bus owned one, in
    /// which case it is derived from the bus UUID on load.
    #[serde(default)]
    pub audio_mix: Option<AudioMixId>,
    /// Default `AUTO` duration in seconds.
    #[serde(default = "default_auto_duration")]
    pub auto_duration: f32,
    /// Easing curve for `AUTO`.
    #[serde(default = "default_easing")]
    pub easing: CrossfadeEasing,
    /// Mixer layers (channel UUIDs) left out when the bus shows Master.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hidden_layers: Vec<String>,
}

fn default_auto_duration() -> f32 {
    DEFAULT_AUTO_DURATION
}

fn default_easing() -> CrossfadeEasing {
    CrossfadeEasing::EaseInOut
}

impl Bus {
    /// Capture this bus as persisted configuration.
    pub fn to_config(&self) -> BusConfig {
        BusConfig {
            uuid: self.uuid.clone(),
            name: self.name.clone(),
            canvas: self.canvas,
            preview: self.preview.clone(),
            program: self.program.clone(),
            audio_mix: Some(self.audio_mix.clone()),
            auto_duration: self.auto_duration,
            easing: self.easing,
            hidden_layers: self.hidden_layers.clone(),
        }
    }

    /// Rebuild a bus from persisted configuration.
    ///
    /// A non-finite or non-positive persisted duration is replaced with the
    /// default rather than rejected, so one bad field cannot make a show
    /// unloadable.
    pub fn from_config(config: BusConfig) -> Self {
        let audio_mix = config
            .audio_mix
            .unwrap_or_else(|| AudioMixId::new(config.uuid.clone()));
        let auto_duration = if config.auto_duration.is_finite() && config.auto_duration > 0.0 {
            config.auto_duration
        } else {
            DEFAULT_AUTO_DURATION
        };
        Self {
            uuid: config.uuid,
            name: config.name,
            preview: config.preview,
            program: config.program,
            canvas: config.canvas,
            audio_mix,
            // Assumed resolvable until `reconcile` checks against the scene,
            // which the workspace loader runs once both files are loaded.
            preview_resolved: true,
            program_resolved: true,
            preview_rev: 0,
            preview_rev_at_take: 0,
            transition: None,
            auto_duration,
            easing: config.easing,
            hidden_layers: config.hidden_layers,
        }
    }

    /// Let go of a channel or deck the operator deleted: Preview or Program
    /// showing it goes to black, and it leaves the hidden layers. Unlike a
    /// source a load cannot find (kept, so loading the right scene restores
    /// it), a deleted one is gone for good — kept, it would be saved and
    /// reported missing on every load after. Whether anything changed.
    fn forget(&mut self, uuid: &str) -> bool {
        let mut changed = false;
        if self.preview.addresses(uuid) {
            self.preview = BusInputRef::Black;
            self.preview_resolved = true;
            self.preview_rev += 1;
            changed = true;
        }
        if self.program.addresses(uuid) {
            self.program = BusInputRef::Black;
            self.program_resolved = true;
            changed = true;
        }
        let before = self.hidden_layers.len();
        self.hidden_layers.retain(|c| c != uuid);
        changed || self.hidden_layers.len() != before
    }

    /// Mark any Preview/Program reference that `known` cannot resolve,
    /// returning a description of each.
    ///
    /// Two deliberate properties:
    ///
    /// - The reference is **marked, not erased**. It still renders black (see
    ///   [`Bus::effective_program`]), so nothing unintended reaches air, but
    ///   loading the correct scene later restores the routing instead of
    ///   permanently losing the operator's patch on the next save.
    /// - Matching is **variant-aware**: a `Channel` reference must name a
    ///   channel, not merely some UUID that happens to exist somewhere.
    fn reconcile(&mut self, known: &KnownSources) -> Vec<String> {
        let mut unresolved = Vec::new();
        for (slot, input, flag) in [
            ("PVW", &self.preview, &mut self.preview_resolved),
            ("PGM", &self.program, &mut self.program_resolved),
        ] {
            *flag = known.resolves(input);
            if !*flag {
                unresolved.push(format!("bus '{}' {slot} -> {input}", self.name));
            }
        }
        // A take in flight holds its own copies of the endpoints, so its
        // resolution has to be refreshed too — otherwise a source removed
        // mid-take would still land as a resolved Program on completion.
        if let Some(transition) = self.transition.as_mut() {
            transition.from_resolved = known.resolves(&transition.from);
            transition.to_resolved = known.resolves(&transition.to);
            if !transition.to_resolved {
                unresolved.push(format!(
                    "bus '{}' in-flight take -> {}",
                    self.name, transition.to
                ));
            }
        }
        unresolved
    }
}

/// Authored, undoable configuration of a bus — everything except what is
/// currently on air.
#[derive(Debug, Clone, PartialEq)]
pub struct BusStructure {
    /// Operator-facing label.
    pub name: String,
    /// Bus canvas size.
    pub canvas: BusCanvas,
    /// Default `AUTO` duration in seconds.
    pub auto_duration: f32,
    /// Easing curve for `AUTO`.
    pub easing: CrossfadeEasing,
}

impl Bus {
    /// The authored configuration of this bus.
    pub fn structure(&self) -> BusStructure {
        BusStructure {
            name: self.name.clone(),
            canvas: self.canvas,
            auto_duration: self.auto_duration,
            easing: self.easing,
        }
    }

    /// Adopt authored configuration without touching anything on air.
    ///
    /// Undo/redo restores what the operator *authored*. PVW, PGM, their
    /// resolution, and any take in flight are live state: rewinding them would
    /// change the picture going out because someone undid an unrelated edit.
    pub fn apply_structure(&mut self, structure: BusStructure) {
        self.name = structure.name;
        self.canvas = structure.canvas;
        self.auto_duration = structure.auto_duration;
        self.easing = structure.easing;
    }
}

/// The set of configured production buses.
///
/// The number of buses is configurable; three is the initial real-world target,
/// not a fixed limit.
#[derive(Debug, Clone, Default)]
pub struct BusBank {
    buses: Vec<Bus>,
    /// Routing is quarantined: nothing resolves until a trusted load clears it.
    ///
    /// Set when a workspace load could not establish that the buses and the
    /// scene came from the same place. Marking the routes unresolved once is
    /// not enough, because any later reconciliation recomputes resolution from
    /// whatever mixer is live — so removing a deck, or an undo, would silently
    /// re-arm a stale route. The quarantine outlives those events and only a
    /// load that establishes provenance lifts it.
    routing_quarantined: bool,
}

impl BusBank {
    /// An empty bank.
    pub fn new() -> Self {
        Self::default()
    }

    /// A bank with one 1080p bus named `PGM`, showing Main as the single
    /// program did — for a new show, and scenes saved before buses existed.
    pub fn with_legacy_default() -> Self {
        Self {
            buses: vec![Bus::new_on_main("PGM", BusCanvas::default())],
            routing_quarantined: false,
        }
    }

    /// Whether routing is quarantined pending a trusted load.
    pub fn routing_quarantined(&self) -> bool {
        self.routing_quarantined
    }

    /// Quarantine routing until a load establishes provenance.
    ///
    /// While quarantined every route reads as unresolved, so buses hold black
    /// however often reconciliation runs in between. The configured routes are
    /// untouched and come back when a trusted load clears it.
    pub fn quarantine_routing(&mut self) {
        self.routing_quarantined = true;
    }

    /// A bank with no buses, distinct from [`BusBank::with_legacy_default`].
    ///
    /// A stage file that persisted an empty bank must reload as empty rather
    /// than silently regaining the legacy bus.
    pub fn empty() -> Self {
        Self {
            buses: Vec::new(),
            routing_quarantined: false,
        }
    }

    /// Number of configured buses.
    pub fn len(&self) -> usize {
        self.buses.len()
    }

    /// Whether no buses are configured.
    pub fn is_empty(&self) -> bool {
        self.buses.is_empty()
    }

    /// Append a bus, returning its UUID.
    ///
    /// A bus joining a quarantined bank is held black with the rest: its
    /// routes read unresolved until a trusted load, so a new bus on Master
    /// cannot put the stale mixer on air.
    ///
    /// # Errors
    ///
    /// Returns the rejected bus if its UUID is already in the bank. Two buses
    /// sharing an ID would make every bus-addressed command ambiguous, which is
    /// exactly what UUID addressing exists to prevent. Boxed because `Bus` is
    /// large enough that an unboxed `Err` bloats every `Ok` too.
    pub fn add(&mut self, mut bus: Bus) -> Result<String, Box<Bus>> {
        if self.contains(bus.uuid()) {
            return Err(Box::new(bus));
        }
        if self.routing_quarantined {
            bus.reconcile(&KnownSources::EMPTY);
        }
        let uuid = bus.uuid().to_string();
        self.buses.push(bus);
        Ok(uuid)
    }

    /// Whether a bus with this UUID exists.
    pub fn contains(&self, uuid: &str) -> bool {
        self.buses.iter().any(|bus| bus.uuid() == uuid)
    }

    /// Remove the bus with this UUID, returning it if it existed.
    pub fn remove(&mut self, uuid: &str) -> Option<Bus> {
        let idx = self.buses.iter().position(|bus| bus.uuid() == uuid)?;
        Some(self.buses.remove(idx))
    }

    /// Borrow the bus with this UUID.
    pub fn get(&self, uuid: &str) -> Option<&Bus> {
        self.buses.iter().find(|bus| bus.uuid() == uuid)
    }

    /// Mutably borrow the bus with this UUID.
    ///
    /// The only *UUID-addressed* mutable path, so a command naming one bus
    /// cannot reach another. [`BusBank::tick`] and [`BusBank::reconcile`] are
    /// deliberate bank-wide operations and are not addressed this way.
    pub fn get_mut(&mut self, uuid: &str) -> Option<&mut Bus> {
        self.buses.iter_mut().find(|bus| bus.uuid() == uuid)
    }

    /// Iterate the buses in configured order.
    pub fn iter(&self) -> std::slice::Iter<'_, Bus> {
        self.buses.iter()
    }

    /// Keep every bus on the same part of the picture across a master resize
    /// (see [`BusCanvas::rescaled`]).
    pub fn rescale_canvases(&mut self, from: (u32, u32), to: (u32, u32)) {
        for bus in &mut self.buses {
            bus.set_canvas(bus.canvas().rescaled(from, to));
        }
    }

    /// Advance every in-flight transition, returning the UUIDs that completed.
    pub fn tick(&mut self, now: f64) -> Vec<String> {
        let mut completed = Vec::new();
        for bus in &mut self.buses {
            if bus.tick(now) {
                completed.push(bus.uuid().to_string());
            }
        }
        completed
    }

    /// Every bus that references `uuid` on Preview, Program, or mid-transition.
    pub fn referencing<'a>(&'a self, uuid: &'a str) -> impl Iterator<Item = &'a Bus> {
        self.buses.iter().filter(move |bus| bus.references(uuid))
    }

    /// Restore authored bus configuration from `configs`, preserving live
    /// state for buses that still exist.
    ///
    /// Used by undo/redo. Buses only in `configs` are recreated; buses only in
    /// the bank are removed (undoing an add); buses in both keep their PVW,
    /// PGM, resolution and in-flight take while adopting the authored fields.
    pub fn apply_structural_diff(&mut self, configs: &[BusConfig]) {
        self.buses.retain(|bus| {
            let kept = configs.iter().any(|c| c.uuid == bus.uuid());
            if !kept {
                log::debug!("Undo removed bus '{}' ({})", bus.name, bus.uuid());
            }
            kept
        });
        for (idx, config) in configs.iter().enumerate() {
            if let Some(live) = self.buses.iter().position(|b| b.uuid() == config.uuid) {
                self.buses[live].apply_structure(BusStructure {
                    name: config.name.clone(),
                    canvas: config.canvas,
                    auto_duration: config.auto_duration,
                    easing: config.easing,
                });
                if live != idx && idx < self.buses.len() {
                    self.buses.swap(live, idx);
                }
            } else {
                self.buses.push(Bus::from_config(config.clone()));
                let last = self.buses.len() - 1;
                if idx < last {
                    self.buses.swap(idx, last);
                }
            }
        }
    }

    /// Capture every bus as persisted configuration, in order.
    pub fn to_configs(&self) -> Vec<BusConfig> {
        self.buses.iter().map(Bus::to_config).collect()
    }

    /// Rebuild a bank from persisted configuration.
    ///
    /// Duplicate UUIDs are dropped rather than merged: two buses sharing an ID
    /// would make every bus-addressed command ambiguous.
    pub fn from_configs(configs: Vec<BusConfig>) -> Self {
        Self::from_configs_reporting(configs).0
    }

    /// [`BusBank::from_configs`], also returning what it had to reject.
    ///
    /// Rejection is enforced here rather than left to validation warnings: a
    /// bus with an empty UUID cannot be named by any command or HTTP route, and
    /// a duplicate makes every bus-addressed command ambiguous. Admitting
    /// either would produce a bank the operator cannot control.
    pub fn from_configs_reporting(configs: Vec<BusConfig>) -> (Self, Vec<String>) {
        let mut bank = Self::empty();
        let mut rejected = Vec::new();
        let mut seen_mix: std::collections::HashSet<String> = std::collections::HashSet::new();
        for config in configs {
            let uuid = config.uuid.clone();
            let name = config.name.clone();
            if uuid.trim().is_empty() {
                rejected.push(format!(
                    "bus '{name}' has an empty uuid and cannot be addressed"
                ));
                continue;
            }
            // Effective identity: an absent `audio_mix` derives from the bus
            // UUID, so two buses can collide without either naming the other's
            // value. First identity wins, matching the UUID policy.
            let effective_mix = config
                .audio_mix
                .as_ref()
                .map_or_else(|| uuid.clone(), |m| m.as_str().to_string());
            if seen_mix.contains(&effective_mix) {
                rejected.push(format!(
                    "bus '{name}' shares audio mix identity '{effective_mix}' with an earlier bus"
                ));
                continue;
            }
            // Both identities are committed only once the bus is admitted. A
            // rejected bus must not reserve a mix identity a later, valid bus
            // legitimately owns.
            if bank.add(Bus::from_config(config)).is_err() {
                rejected.push(format!("bus '{name}' duplicates uuid '{uuid}'"));
                continue;
            }
            seen_mix.insert(effective_mix);
        }
        (bank, rejected)
    }

    /// Drop every Preview/Program reference that names a source outside
    /// `known`, returning a description of each drop for the caller to report.
    /// Re-check every route against `known`, honouring any quarantine.
    ///
    /// A quarantined bank reconciles against nothing, so routes stay black no
    /// matter what the live mixer happens to contain. Use
    /// [`BusBank::reconcile_trusted`] from a load that has established
    /// provenance; it is the only thing that lifts the quarantine.
    pub fn reconcile(&mut self, known: &KnownSources) -> Vec<String> {
        let effective = if self.routing_quarantined {
            &KnownSources::EMPTY
        } else {
            known
        };
        self.buses
            .iter_mut()
            .flat_map(|bus| bus.reconcile(effective))
            .collect()
    }

    /// Let go of a channel or deck the operator deleted, on every bus (see
    /// [`Bus::forget`]). Whether anything changed.
    pub fn forget(&mut self, uuid: &str) -> bool {
        self.buses
            .iter_mut()
            .fold(false, |changed, bus| bus.forget(uuid) | changed)
    }

    /// Reconcile from a load that knows the buses and scene belong together.
    ///
    /// Lifts any quarantine, because provenance has been established.
    pub fn reconcile_trusted(&mut self, known: &KnownSources) -> Vec<String> {
        self.routing_quarantined = false;
        self.buses
            .iter_mut()
            .flat_map(|bus| bus.reconcile(known))
            .collect()
    }
}

impl<'a> IntoIterator for &'a BusBank {
    type Item = &'a Bus;
    type IntoIter = std::slice::Iter<'a, Bus>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(test)]
mod tests;
