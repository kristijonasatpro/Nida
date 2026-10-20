//! Channel - Groups multiple decks into a composited layer with its own effect chain

use crate::arrangement::SourceDemand;
use crate::deck::{Deck, Effect};
use crate::isf::ISFShader;
use crate::mixer::GpuTimingFrame;
use crate::modulation::ModulationEngine;
use crate::params::ShaderParams;
use crate::renderer::region::{self, Pixels};
use crate::renderer::{
    BlitPipeline, CompositeBlitPipeline, GpuContext, ISFUniforms, TransitionPipeline,
};
use anyhow::{Context as _, Result};

/// Blend modes for compositing decks and channels
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Subtract,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    ColorDodge,
    ColorBurn,
    Difference,
    Exclusion,
    Darken,
    Lighten,
    LinearBurn,
}

impl BlendMode {
    /// Shader uniform index for this blend mode.
    /// Must match the constants in composite.wgsl.
    pub fn to_index(&self) -> u32 {
        match self {
            BlendMode::Normal => 0,
            BlendMode::Add => 1,
            BlendMode::Subtract => 2,
            BlendMode::Multiply => 3,
            BlendMode::Screen => 4,
            BlendMode::Overlay => 5,
            BlendMode::SoftLight => 6,
            BlendMode::HardLight => 7,
            BlendMode::ColorDodge => 8,
            BlendMode::ColorBurn => 9,
            BlendMode::Difference => 10,
            BlendMode::Exclusion => 11,
            BlendMode::Darken => 12,
            BlendMode::Lighten => 13,
            BlendMode::LinearBurn => 14,
        }
    }

    /// Full name, where there is room to spell it out.
    pub fn label(&self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Add => "Add",
            BlendMode::Subtract => "Subtract",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft light",
            BlendMode::HardLight => "Hard light",
            BlendMode::ColorDodge => "Colour dodge",
            BlendMode::ColorBurn => "Colour burn",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::LinearBurn => "Linear burn",
        }
    }

    /// Short display name for UI
    pub fn short_name(&self) -> &'static str {
        match self {
            BlendMode::Normal => "Norm",
            BlendMode::Add => "Add",
            BlendMode::Subtract => "Sub",
            BlendMode::Multiply => "Mult",
            BlendMode::Screen => "Scrn",
            BlendMode::Overlay => "Ovly",
            BlendMode::SoftLight => "SftL",
            BlendMode::HardLight => "HrdL",
            BlendMode::ColorDodge => "CDge",
            BlendMode::ColorBurn => "CBrn",
            BlendMode::Difference => "Diff",
            BlendMode::Exclusion => "Excl",
            BlendMode::Darken => "Dark",
            BlendMode::Lighten => "Lite",
            BlendMode::LinearBurn => "LBrn",
        }
    }

    /// All blend mode variants in display order
    pub fn all() -> &'static [BlendMode] {
        &[
            BlendMode::Normal,
            BlendMode::Add,
            BlendMode::Subtract,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::Overlay,
            BlendMode::SoftLight,
            BlendMode::HardLight,
            BlendMode::ColorDodge,
            BlendMode::ColorBurn,
            BlendMode::Difference,
            BlendMode::Exclusion,
            BlendMode::Darken,
            BlendMode::Lighten,
            BlendMode::LinearBurn,
        ]
    }
}

// ── Auto-Transition Types ──────────────────────────────────────────

/// Unit of a duration value.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub enum DurationUnit {
    Seconds,
    Minutes,
    Hours,
    Beats,
}

impl DurationUnit {
    /// Short label for UI display.
    pub fn label(&self) -> &'static str {
        match self {
            DurationUnit::Seconds => "s",
            DurationUnit::Minutes => "m",
            DurationUnit::Hours => "h",
            DurationUnit::Beats => "b",
        }
    }

    /// Cycle to the next unit: s → m → h → b → s
    #[must_use]
    pub fn next(&self) -> Self {
        match self {
            DurationUnit::Seconds => DurationUnit::Minutes,
            DurationUnit::Minutes => DurationUnit::Hours,
            DurationUnit::Hours => DurationUnit::Beats,
            DurationUnit::Beats => DurationUnit::Seconds,
        }
    }
}

/// Duration specified in beats or wall-clock time (seconds, minutes, or hours).
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DurationSpec {
    Beats(f64),
    Seconds(f64),
    Minutes(f64),
    Hours(f64),
}

impl DurationSpec {
    /// Resolve to seconds given the current BPM (falls back to 120 if unknown/invalid).
    pub fn to_seconds(&self, bpm: Option<f64>) -> f64 {
        match self {
            DurationSpec::Beats(b) => {
                let safe_bpm = match bpm {
                    Some(v) if v.is_finite() && v > 0.0 => v,
                    _ => 120.0,
                };
                b * 60.0 / safe_bpm
            }
            DurationSpec::Seconds(s) => *s,
            DurationSpec::Minutes(m) => m * 60.0,
            DurationSpec::Hours(h) => h * 3600.0,
        }
    }

    /// Get the raw numeric value.
    pub fn value(&self) -> f64 {
        match self {
            DurationSpec::Beats(v)
            | DurationSpec::Seconds(v)
            | DurationSpec::Minutes(v)
            | DurationSpec::Hours(v) => *v,
        }
    }

    pub fn is_beats(&self) -> bool {
        matches!(self, DurationSpec::Beats(_))
    }

    /// Get the unit of this duration.
    pub fn unit(&self) -> DurationUnit {
        match self {
            DurationSpec::Seconds(_) => DurationUnit::Seconds,
            DurationSpec::Minutes(_) => DurationUnit::Minutes,
            DurationSpec::Hours(_) => DurationUnit::Hours,
            DurationSpec::Beats(_) => DurationUnit::Beats,
        }
    }

    /// Create a `DurationSpec` from a value and unit.
    pub fn from_value_unit(value: f64, unit: DurationUnit) -> Self {
        match unit {
            DurationUnit::Seconds => DurationSpec::Seconds(value),
            DurationUnit::Minutes => DurationSpec::Minutes(value),
            DurationUnit::Hours => DurationSpec::Hours(value),
            DurationUnit::Beats => DurationSpec::Beats(value),
        }
    }

    /// Set the numeric value, preserving the unit.
    pub fn set_value(&mut self, v: f64) {
        match self {
            DurationSpec::Beats(ref mut b) => *b = v,
            DurationSpec::Seconds(ref mut s) => *s = v,
            DurationSpec::Minutes(ref mut m) => *m = v,
            DurationSpec::Hours(ref mut h) => *h = v,
        }
    }
}

/// What starts the transition countdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransitionTrigger {
    /// Timer-based: starts counting when deck becomes the active (topmost visible) deck.
    Timer,
    /// Content-aware: starts when video hits its out-point or end-of-file.
    /// Falls back to Timer for non-video sources.
    ClipEnd,
}

/// Runtime phase of a deck's auto-transition.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum DeckTransitionPhase {
    /// Not the active top deck, or auto-transition disabled.
    Inactive,
    /// Playing content, countdown running.
    Playing { elapsed: f64 },
    /// Transition shader active, progress 0.0 → 1.0.
    Transitioning { progress: f64 },
    /// Transition complete — deck is effectively invisible.
    Done,
}

/// Per-deck auto-transition configuration.
pub struct DeckAutoTransition {
    pub enabled: bool,
    pub play_duration: DurationSpec,
    pub transition_duration: DurationSpec,
    pub trigger: TransitionTrigger,
    /// Name of the transition shader (None = simple opacity fade).
    pub transition_shader_name: Option<String>,
    /// Runtime phase (not persisted).
    pub phase: DeckTransitionPhase,
}

impl Default for DeckAutoTransition {
    fn default() -> Self {
        Self::new()
    }
}

impl DeckAutoTransition {
    pub fn new() -> Self {
        Self {
            enabled: false,
            play_duration: DurationSpec::Beats(16.0),
            transition_duration: DurationSpec::Seconds(2.0),
            trigger: TransitionTrigger::Timer,
            transition_shader_name: None,
            phase: DeckTransitionPhase::Inactive,
        }
    }
}

/// Compiled transition shader for a deck (separate from config for GPU resource lifecycle).
pub struct DeckTransitionEffect {
    pub shader: ISFShader,
    pub pipeline: TransitionPipeline,
    pub params: ShaderParams,
}

/// Per-deck render FPS setting.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum DeckRenderFps {
    /// Automatic adaptive skipping based on render cost
    #[default]
    Auto,
    /// Fixed render rate (renders every Nth frame to hit target)
    Fixed(u32),
}

impl std::fmt::Display for DeckRenderFps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => write!(f, "Auto"),
            Self::Fixed(fps) => write!(f, "{fps}"),
        }
    }
}

// ── DeckSlot ───────────────────────────────────────────────────────

/// A deck slot in a channel with compositing properties
pub struct DeckSlot {
    pub deck: Deck,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub solo: bool,
    pub mute: bool,
    pub z_index: i32,
    /// Auto-transition config (None = no auto-transition).
    pub auto_transition: Option<DeckAutoTransition>,
    /// Compiled transition effect for this deck's auto-transition.
    pub transition_effect: Option<DeckTransitionEffect>,
    /// Per-deck render FPS setting (Auto = adaptive skipping)
    pub render_fps: DeckRenderFps,
    /// Smoothed render cost in microseconds (EMA)
    pub render_cost_us: f32,
    /// Frame counter for skip logic
    pub skip_counter: u32,
    /// GPU-measured render cost in microseconds (EMA, 0 = no data yet)
    pub gpu_render_cost_us: f32,
    /// True while the arrangement is driving this deck, which suspends its
    /// auto-transition. Recomputed every frame by `Mixer::apply_arrangement`
    /// and never persisted. See /spec/arrangement.md § Authority.
    pub arrangement_authority: bool,
    /// What the arrangement predicts about this deck's source: whether it is
    /// wanted soon, or far enough from any region that its decoding can stop.
    /// Recomputed every frame alongside `arrangement_authority`, never
    /// persisted. See /spec/deck-residency.md.
    pub source_demand: SourceDemand,
}

impl DeckSlot {
    pub fn new(deck: Deck) -> Self {
        Self {
            deck,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            solo: false,
            mute: false,
            z_index: 0,
            auto_transition: None,
            transition_effect: None,
            render_fps: DeckRenderFps::default(),
            render_cost_us: 0.0,
            skip_counter: 0,
            gpu_render_cost_us: 0.0,
            arrangement_authority: false,
            source_demand: SourceDemand::default(),
        }
    }

    /// Set the transition shader for this deck's auto-transition.
    /// Compiles the shader and stores the pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error if the shader's GLSL fails to compile to SPIR-V or if the
    /// transition render pipeline cannot be created from the resulting module.
    pub fn set_transition_shader(&mut self, context: &GpuContext, shader: ISFShader) -> Result<()> {
        let spirv = crate::isf::compile_glsl_to_spirv(&shader.fragment_source, &shader.name())
            .context("Failed to compile transition shader to SPIR-V")?;
        let pipeline =
            TransitionPipeline::new(&context.device, &spirv, context.compositing_format)?;
        let name = shader.name();
        let inputs = shader.metadata.inputs.as_deref().unwrap_or(&[]);
        let mut params = ShaderParams::from_inputs(inputs);
        params.ensure_buffer(&context.device);

        // Ensure auto_transition config exists
        if self.auto_transition.is_none() {
            self.auto_transition = Some(DeckAutoTransition::new());
        }
        if let Some(at) = &mut self.auto_transition {
            at.transition_shader_name = Some(name);
        }

        self.transition_effect = Some(DeckTransitionEffect {
            shader,
            pipeline,
            params,
        });
        Ok(())
    }

    /// Clear the transition shader (revert to opacity fade).
    pub fn clear_transition_shader(&mut self) {
        self.transition_effect = None;
        if let Some(at) = &mut self.auto_transition {
            at.transition_shader_name = None;
        }
    }

    /// Get the current auto-transition phase.
    pub fn transition_phase(&self) -> DeckTransitionPhase {
        self.auto_transition
            .as_ref()
            .filter(|at| at.enabled)
            .map_or(DeckTransitionPhase::Inactive, |at| at.phase)
    }
}

/// Channel - Groups multiple decks into a composited layer
// Switches set by the mixer each frame (overlay, visible parts, every
// deck, tapped): each its own question, not a state.
#[allow(clippy::struct_excessive_bools)]
pub struct Channel {
    /// Stable UUID for this channel (8-char hex, persists across saves)
    uuid: String,

    /// Channel name (A, B, C, ...)
    pub name: String,

    /// Decks in this channel
    pub decks: Vec<DeckSlot>,

    /// Per-channel effect chain (applied to composited deck output)
    pub effects: Vec<Effect>,

    /// Channel opacity (0.0–1.0) for mixing into final output
    pub opacity: f32,

    /// Channel blend mode for mixing into final output
    pub blend_mode: BlendMode,

    /// In Main mode (`crate::mixer::MixMode::Program`), an overlay: switched
    /// on and off over whatever is on Main, like vMix's overlay channels,
    /// instead of being taken to Main itself.
    pub overlay: bool,

    /// Composite output texture (all decks blended together)
    pub composite_texture: wgpu::Texture,
    pub composite_view: wgpu::TextureView,

    /// Ping-pong texture for channel effect chain
    pub effect_ping_texture: wgpu::Texture,
    pub effect_ping_view: wgpu::TextureView,

    /// Previous frame's composite, kept only while a deck taps this channel
    /// round a loop (see `Mixer::prepare_taps`) — any other tap reads this
    /// frame's composite, rendered first. Swapped with `composite_texture` at
    /// the top of each frame, so reading it costs no copy.
    /// See spec/program-tap.md.
    tap_prev: Option<(wgpu::Texture, wgpu::TextureView)>,

    /// Frame counter for uniforms
    frame_count: u32,

    /// Shader-based composite pipeline for blending decks (all blend modes via uniform)
    composite_pipeline: CompositeBlitPipeline,

    /// Simple blit pipeline for first-deck copy (Normal mode, no blend needed)
    blit_pipeline: BlitPipeline,
    /// Blend-unit pipelines for Add and Screen decks (see
    /// [`crate::renderer::blit::ADD_BLENDING`]).
    add_pipeline: BlitPipeline,
    screen_pipeline: BlitPipeline,

    /// Smoothed render time for this channel in milliseconds (EMA over recent frames)
    pub render_time_ms: f32,
    /// Number of active (rendered) decks in the last frame
    pub active_deck_count: u32,
    /// Frame time gone by since this channel was last drawn, while it is
    /// drawn only now and then (a tile, see `Mixer::tile_previews`): handed
    /// to its next draw, so what runs on time keeps real time.
    pub dt_owed: f32,
    /// Decks blended into the composite last frame — fewer than the active
    /// ones when a deck covering the frame hides those below it.
    pub composited_deck_count: usize,
    /// Decks shown on their own somewhere — alone on Main or a bus, or being
    /// edited — by UUID, set by the mixer each frame. A deck hidden under one
    /// that covers the frame still renders when it is one of these.
    pub shown_alone: Vec<String>,
    /// Draw each deck only where the decks over it leave it showing; set by
    /// the mixer each frame (see `Mixer::draw_visible_parts`).
    pub draw_visible_parts: bool,
    /// Draw every deck, and all of each — none left out as hidden under
    /// another, none only in parts — set by the mixer each frame for a
    /// channel in a merge, whose layers move and uncover one another.
    pub draw_every_deck: bool,
    /// Some deck, here or in another channel, shows this channel's picture
    /// (a tap); set by the mixer each frame. It must render then, whatever
    /// its own level on the mix.
    pub tapped: bool,
}

/// How a deck takes part in its channel's picture this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
enum CompositeRole {
    /// Drawn in its place in the stack.
    Steady,
    /// Transitioning with a shader, at this progress: drawn over the rest.
    Transitioning(f64),
}

impl Channel {
    /// Create a new channel
    /// Get the stable UUID for this channel
    pub fn uuid(&self) -> &str {
        &self.uuid
    }

    /// Set the UUID (used during scene restore to preserve identity)
    pub fn set_uuid(&mut self, uuid: String) {
        self.uuid = uuid;
    }

    /// # Errors
    ///
    /// Returns an error if the composite-blit or alpha-blend blit pipelines cannot
    /// be created on the given GPU device.
    pub fn new(name: String, context: &GpuContext, width: u32, height: u32) -> Result<Self> {
        let composite_texture = context.create_compositing_texture(width, height);
        let composite_view = composite_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let effect_ping_texture = context.create_compositing_texture(width, height);
        let effect_ping_view =
            effect_ping_texture.create_view(&wgpu::TextureViewDescriptor::default());

        let composite_pipeline =
            CompositeBlitPipeline::new(&context.device, context.compositing_format)?;
        let blit_pipeline = BlitPipeline::with_blend(
            &context.device,
            context.compositing_format,
            wgpu::BlendState::ALPHA_BLENDING,
        )?;
        let add_pipeline = BlitPipeline::with_blend(
            &context.device,
            context.compositing_format,
            crate::renderer::blit::ADD_BLENDING,
        )?;
        let screen_pipeline = BlitPipeline::with_blend(
            &context.device,
            context.compositing_format,
            crate::renderer::blit::SCREEN_BLENDING,
        )?;

        Ok(Self {
            uuid: crate::deck::generate_short_uuid(),
            name,
            decks: Vec::new(),
            effects: Vec::new(),
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            overlay: false,
            composite_texture,
            composite_view,
            effect_ping_texture,
            effect_ping_view,
            tap_prev: None,
            frame_count: 0,
            composite_pipeline,
            blit_pipeline,
            add_pipeline,
            screen_pipeline,
            render_time_ms: 0.0,
            active_deck_count: 0,
            dt_owed: 0.0,
            composited_deck_count: 0,
            shown_alone: Vec::new(),
            draw_visible_parts: true,
            draw_every_deck: false,
            tapped: false,
        })
    }

    /// Previous frame's composite, or `None` when nothing taps this channel
    /// round a loop.
    pub fn tap_view(&self) -> Option<&wgpu::TextureView> {
        self.tap_prev.as_ref().map(|(_, v)| v)
    }

    /// Allocate the tap target if it does not exist yet. Idempotent, so the
    /// per-frame sync can call it unconditionally for every tapped channel.
    pub fn ensure_tap(&mut self, context: &GpuContext, width: u32, height: u32) {
        if self.tap_prev.is_some() {
            return;
        }
        let tex = context.create_compositing_texture(width, height);
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        self.tap_prev = Some((tex, view));
    }

    /// Release the tap target so an untapped channel costs no extra memory.
    pub fn clear_tap(&mut self) {
        self.tap_prev = None;
    }

    /// Exchange the composite and tap targets. Called once per frame before
    /// any deck renders, so a tap round a loop reads last frame's composite
    /// while this frame's is written.
    pub(crate) fn swap_tap(&mut self) {
        if let Some((mut tex, mut view)) = self.tap_prev.take() {
            std::mem::swap(&mut self.composite_texture, &mut tex);
            std::mem::swap(&mut self.composite_view, &mut view);
            self.tap_prev = Some((tex, view));
        }
    }

    /// Add a deck to this channel
    pub fn add_deck(&mut self, deck: Deck) -> usize {
        let idx = self.decks.len();
        self.decks.push(DeckSlot::new(deck));
        idx
    }

    /// Remove a deck from this channel
    pub fn remove_deck(&mut self, index: usize) -> Option<Deck> {
        if index < self.decks.len() {
            Some(self.decks.remove(index).deck)
        } else {
            None
        }
    }

    /// Remove a deck slot (preserving all properties) from this channel
    pub fn remove_deck_slot(&mut self, index: usize) -> Option<DeckSlot> {
        if index < self.decks.len() {
            Some(self.decks.remove(index))
        } else {
            None
        }
    }

    /// Add a pre-configured deck slot to this channel
    pub fn add_deck_slot(&mut self, slot: DeckSlot) -> usize {
        let idx = self.decks.len();
        self.decks.push(slot);
        idx
    }

    /// Put a deck slot just before the deck `before` (by UUID), or at the end
    /// when that is `None` or not in this channel. Returns where it went.
    pub fn insert_deck_slot_before(&mut self, slot: DeckSlot, before: Option<&str>) -> usize {
        let at = before
            .and_then(|b| self.decks.iter().position(|s| s.deck.uuid() == b))
            .unwrap_or(self.decks.len());
        self.decks.insert(at, slot);
        at
    }

    /// Add a deck just before the deck `before` (by UUID), or at the end.
    pub fn insert_deck_before(&mut self, deck: Deck, before: Option<&str>) -> usize {
        self.insert_deck_slot_before(DeckSlot::new(deck), before)
    }

    /// Get number of decks
    /// Frames this channel has been drawn.
    pub fn frames_drawn(&self) -> u32 {
        self.frame_count
    }

    pub fn deck_count(&self) -> usize {
        self.decks.len()
    }

    /// Tick video frames for all decks without doing a full render.
    /// Call this every frame even for off-screen channels so video players
    /// stay in sync and don't show stale/black frames when faded back in.
    /// `target_fps` is the rate the renderer presents at (0 = uncapped); decode
    /// threads use it to avoid producing frames that can never be shown.
    /// `refresh_at` is when the refresh that called for this frame came,
    /// when a screen's refresh starts frames; `held`, whether each frame is
    /// held on screen a frame's time.
    ///
    /// The one exception is a deck the arrangement has put to sleep, which is
    /// far enough from its next region that nothing can bring it up in time to
    /// matter. It holds its last frame and resumes from there.
    /// See /spec/deck-residency.md.
    pub fn tick_video_frames(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target_fps: u32,
        refresh_at: Option<std::time::Instant>,
        held: bool,
    ) {
        for slot in &mut self.decks {
            slot.deck
                .set_video_suspended(!slot.source_demand.wants_frames());
            if !slot.source_demand.wants_frames() {
                continue;
            }
            slot.deck.set_video_output_fps(target_fps);
            slot.deck.set_frame_refresh_at(refresh_at);
            slot.deck.set_frame_held(held);
            if let Err(e) = slot.deck.update_video_frame(encoder) {
                log::warn!("Video frame update failed: {e}");
            }
        }
    }

    /// Request re-mapping of staging buffers after `queue.submit()`.
    pub fn request_video_remap(&mut self) {
        for slot in &mut self.decks {
            slot.deck.request_video_remap();
        }
    }

    /// How a deck is composited this frame: `None` when it is not (muted,
    /// soloed away, invisible, or an auto-transition deck waiting or done).
    fn composite_role(&self, idx: usize, any_solo: bool) -> Option<CompositeRole> {
        let slot = &self.decks[idx];
        let phase = slot.transition_phase();
        if slot.mute || (any_solo && !slot.solo) || slot.opacity <= 0.0 {
            return None;
        }
        // Another input as a layer, that input gone: nothing to show, and no
        // border or effect drawn round the nothing.
        if slot.deck.shows_a_lost_input() {
            return None;
        }
        // Inactive and Done auto-transition decks don't composite.
        // Inactive = waiting for turn. Done = already played, no longer needed
        // (the next deck in sequence gets re-activated to Playing when needed).
        let has_at = slot.auto_transition.as_ref().is_some_and(|at| at.enabled);
        if has_at && (phase == DeckTransitionPhase::Inactive || phase == DeckTransitionPhase::Done)
        {
            return None;
        }
        Some(match phase {
            DeckTransitionPhase::Transitioning { progress } => {
                CompositeRole::Transitioning(progress)
            }
            // Done decks composite normally (full opacity) — they serve
            // as the visible background that transitioning decks reveal.
            _ => CompositeRole::Steady,
        })
    }

    /// The decks nothing of which can show: those under the topmost deck
    /// that covers the whole frame (opaque, Normal, at full opacity), by
    /// index. Transitioning decks are drawn over everything, so they are
    /// never hidden, and neither is a deck shown on its own somewhere.
    fn hidden_decks(
        &self,
        deck_indices: &[usize],
        any_solo: bool,
        modulation: &ModulationEngine,
    ) -> std::collections::HashSet<usize> {
        let steady: Vec<usize> = deck_indices
            .iter()
            .copied()
            .filter(|&idx| self.composite_role(idx, any_solo) == Some(CompositeRole::Steady))
            .collect();
        let Some(top) = steady.iter().rposition(|&idx| {
            let slot = &self.decks[idx];
            slot.opacity >= 1.0
                && slot.blend_mode == BlendMode::Normal
                && slot.deck.covers_frame(modulation)
        }) else {
            return std::collections::HashSet::new();
        };
        steady[..top]
            .iter()
            .copied()
            .filter(|&idx| {
                let uuid = self.decks[idx].deck.uuid();
                !self.shown_alone.iter().any(|s| s == uuid)
            })
            .collect()
    }

    /// What shows of each steady deck once the opaque decks over it are
    /// drawn, as rectangles of composite pixels that do not overlap, for the
    /// decks partly or wholly covered; a deck not listed is drawn whole —
    /// as is one shown on its own somewhere, or one whose showing part
    /// would take too many rectangles to be worth it. Only steady decks
    /// over it cover a deck: a transitioning one is drawn over everything
    /// and lets it show through.
    fn visible_parts(
        &self,
        deck_indices: &[usize],
        any_solo: bool,
        modulation: &ModulationEngine,
    ) -> std::collections::HashMap<usize, Vec<Pixels>> {
        let (width, height) = (
            self.composite_texture.width(),
            self.composite_texture.height(),
        );
        let all = Pixels::all(width, height);
        let mut covers: Vec<Pixels> = Vec::new();
        let mut parts = std::collections::HashMap::new();
        for &idx in deck_indices.iter().rev() {
            if self.composite_role(idx, any_solo) != Some(CompositeRole::Steady) {
                continue;
            }
            let slot = &self.decks[idx];
            if !covers.is_empty() && !self.shown_alone.iter().any(|s| s == slot.deck.uuid()) {
                let shows = covers
                    .iter()
                    .fold(vec![all], |left, cover| region::subtract(&left, cover));
                if shows.len() <= region::MAX_PARTS {
                    parts.insert(idx, shows);
                }
            }
            if slot.opacity >= 1.0 && slot.blend_mode == BlendMode::Normal {
                let cover = if slot.deck.covers_frame(modulation) {
                    Some(all)
                } else {
                    slot.deck
                        .opaque_area(modulation)
                        .and_then(|area| area.inner_pixels(width, height, 0))
                };
                covers.extend(cover);
            }
        }
        parts
    }

    /// Render all decks in this channel and composite them, then apply channel effects
    /// `channel_idx` is used for modulation key addressing (e.g., "`ch0_deck0:paramname`")
    /// `dt` is the frame delta in seconds (for auto-transition tick).
    /// `target_fps` is the global target FPS for adaptive skip budget calculation.
    /// `total_active_decks` is the total active deck count across all channels (from last frame).
    /// `gpu_load_ratio` scales CPU-measured render costs to estimate true GPU execution time.
    /// A ratio of 1.0 means CPU and GPU costs match; >1.0 means GPU-bound (GPU takes longer).
    /// `frame_over_budget`: whether last frame's decks together ran over a
    /// frame's time — the only case a deck on `Auto` is drawn less often.
    ///
    /// # Errors
    ///
    /// Returns an error if any deck fails to render its frame (shader/pipeline or
    /// source-decode failure), propagated from `Deck::render_with_prefix`.
    // Hot-path render entry; args are distinct per-frame inputs and bundling them into a
    // struct would add indirection without a shared invariant.
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        context: &GpuContext,
        audio_data: &crate::audio::AudioData,
        modulation: &ModulationEngine,
        channel_idx: usize,
        time: f32,
        dt: f32,
        target_fps: u32,
        total_active_decks: u32,
        gpu_load_ratio: f32,
        frame_over_budget: bool,
        prefix_cmds: &mut Vec<wgpu::CommandBuffer>,
        mut timing: Option<&mut GpuTimingFrame>,
        query_set: Option<&wgpu::QuerySet>,
    ) -> Result<()> {
        struct DeckCompositeInfo {
            deck_idx: usize,
            blend_mode: BlendMode,
            opacity: f32,
            transition_progress: Option<f64>, // Some = transitioning with shader
            /// The parts of the composite it shows in, when not all of it.
            within: Option<Vec<Pixels>>,
        }

        let render_start = std::time::Instant::now();

        // Tick auto-transition state before rendering
        let bpm = audio_data.bpm.map(f64::from);
        self.tick_auto_transitions(f64::from(dt), bpm);

        // Sort decks by z-index
        let mut deck_indices: Vec<usize> = (0..self.decks.len()).collect();
        deck_indices.sort_by_key(|&i| self.decks[i].z_index);

        // Check if any deck is solo'd
        let any_solo = self.decks.iter().any(|slot| slot.solo);

        // Note: video frame updates are handled by tick_video_frames() called
        // from the mixer before render, so all channels stay in sync even when
        // faded out by the crossfader.

        // Calculate per-deck budget for adaptive skipping
        let frame_budget_us = if target_fps > 0 {
            1_000_000.0 / target_fps as f32
        } else {
            f32::MAX // uncapped
        };
        let per_deck_budget_us = if total_active_decks > 0 {
            frame_budget_us / total_active_decks as f32
        } else {
            frame_budget_us
        };

        // Decks hidden under one that covers the frame are not rendered at
        // all: nothing of them shows, and a generator hidden under a
        // full-screen picture costs a whole frame at 4K. They pick up again
        // the frame the cover goes.
        let hidden = if self.draw_every_deck {
            std::collections::HashSet::default()
        } else {
            self.hidden_decks(&deck_indices, any_solo, modulation)
        };
        // Of the rest, those partly covered are drawn only where they show:
        // a background with a camera over most of it costs the strips round
        // the camera. Nothing shows of one covered by several at once.
        let parts = if self.draw_visible_parts && !self.draw_every_deck {
            self.visible_parts(&deck_indices, any_solo, modulation)
        } else {
            std::collections::HashMap::new()
        };
        let (composite_width, composite_height) = (
            self.composite_texture.width(),
            self.composite_texture.height(),
        );

        // Render each deck to its texture (skip muted, non-solo'd, zero-opacity)
        // Done decks still render — they serve as visible background for transitioning decks above.
        // Adaptive skipping: expensive decks render less frequently, reusing stale textures.
        let t_deck_render = std::time::Instant::now();
        let mut cmd_buffers: Vec<wgpu::CommandBuffer> = Vec::new();
        let mut active_count: u32 = 0;
        let mut per_deck_timings: Vec<(String, u128, bool)> = Vec::new(); // (name, us, skipped)
        for (deck_idx, slot) in self.decks.iter_mut().enumerate() {
            let shows = parts.get(&deck_idx);
            if hidden.contains(&deck_idx) || shows.is_some_and(Vec::is_empty) {
                continue;
            }
            slot.deck.set_visible(shows.map(|rects| {
                rects
                    .iter()
                    .map(|r| r.area(composite_width, composite_height))
                    .collect()
            }));
            // A deck shown on its own somewhere — alone on Main — is drawn
            // whatever its place here: muted, left out by a solo, faded out.
            // Whether it is composited here is the composite's own question.
            let alone = self.shown_alone.iter().any(|s| s == slot.deck.uuid());
            if alone || (!slot.mute && (!any_solo || slot.solo) && slot.opacity > 0.0) {
                active_count += 1;
                slot.skip_counter += 1;

                // Determine if this deck should render this frame
                let should_render = match slot.render_fps {
                    DeckRenderFps::Fixed(fps) if fps > 0 && target_fps > 0 => {
                        // Render every Nth frame: N = target_fps / deck_fps
                        let skip_interval = (target_fps / fps).max(1);
                        slot.skip_counter % skip_interval == 0
                    }
                    DeckRenderFps::Auto => auto_draws(
                        slot.render_cost_us * gpu_load_ratio,
                        per_deck_budget_us,
                        frame_over_budget,
                        slot.skip_counter,
                    ),
                    DeckRenderFps::Fixed(_) => true, // Fixed(0) or uncapped target = always render
                };
                // A frame showing a part the last render left undrawn cannot
                // reuse it; nor can a merge, which moves the deck about.
                let should_render =
                    should_render || slot.deck.shows_undrawn_part() || self.draw_every_deck;

                if should_render {
                    // Set the fixed time step so the shader TIME uniform advances
                    // by consistent increments regardless of skip gaps.
                    let render_dt = if target_fps > 0 {
                        1.0 / target_fps as f32
                    } else {
                        dt // uncapped: use actual frame delta
                    };
                    slot.deck.set_render_dt(render_dt);

                    // Allocate GPU timing queries for this deck
                    let gpu_timing = match (&mut timing, query_set) {
                        (Some(t), Some(qs)) => {
                            t.allocate(channel_idx, deck_idx).map(|(b, e)| (qs, b, e))
                        }
                        _ => None,
                    };

                    let param_prefix = slot.deck.param_prefix().to_owned();
                    let t_single = std::time::Instant::now();
                    slot.deck.render_with_prefix(
                        context,
                        audio_data,
                        modulation,
                        &param_prefix,
                        &mut cmd_buffers,
                        gpu_timing,
                    )?;
                    let elapsed_us = t_single.elapsed().as_micros() as f32;
                    // Update EMA render cost (α = 0.2 for responsive adaptation)
                    if slot.render_cost_us > 0.0 {
                        slot.render_cost_us = 0.2 * elapsed_us + 0.8 * slot.render_cost_us;
                    } else {
                        slot.render_cost_us = elapsed_us; // first sample
                    }
                    per_deck_timings.push((
                        slot.deck.source_name().to_owned(),
                        elapsed_us as u128,
                        false,
                    ));
                } else {
                    // Skipped — stale texture reuse, compositing still includes this deck
                    per_deck_timings.push((slot.deck.source_name().to_owned(), 0, true));
                }
            }
        }
        self.active_deck_count = active_count;
        let deck_render_us = t_deck_render.elapsed().as_micros();
        // Batch submit all deck renders at once.
        // Drain any prefix command buffers (e.g. video upload copies) and
        // prepend them so they execute before deck render passes that read
        // the updated textures — all in a single queue.submit() call.
        let t_deck_submit = std::time::Instant::now();
        if !prefix_cmds.is_empty() {
            let mut all_cmds: Vec<wgpu::CommandBuffer> = std::mem::take(prefix_cmds);
            all_cmds.extend(cmd_buffers);
            context.submit(all_cmds);
        } else if !cmd_buffers.is_empty() {
            context.submit(cmd_buffers);
        }
        let deck_submit_us = t_deck_submit.elapsed().as_micros();

        // Collect render info for visible decks, including transition phase
        let deck_composite_info: Vec<DeckCompositeInfo> = deck_indices
            .iter()
            .filter_map(|&idx| {
                let transition_progress = match self.composite_role(idx, any_solo)? {
                    CompositeRole::Steady => None,
                    CompositeRole::Transitioning(progress) => Some(progress),
                };
                let slot = &self.decks[idx];
                let within = parts.get(&idx).cloned();
                if within.as_ref().is_some_and(Vec::is_empty) {
                    return None;
                }
                Some(DeckCompositeInfo {
                    deck_idx: idx,
                    blend_mode: slot.blend_mode,
                    opacity: slot.opacity,
                    transition_progress,
                    within,
                })
            })
            .collect();

        // Reorder compositing: non-transitioning decks first, then transitioning.
        // This ensures composite-so-far always contains all "revealed" content
        // before the transitioning deck is rendered.
        let mut non_transitioning: Vec<&DeckCompositeInfo> = Vec::new();
        let mut transitioning: Vec<&DeckCompositeInfo> = Vec::new();
        for info in &deck_composite_info {
            if info.transition_progress.is_some() {
                transitioning.push(info);
            } else {
                non_transitioning.push(info);
            }
        }
        // Nothing under the topmost deck that covers the frame can show, so
        // it is not composited. A stack of full-screen slides is a normal
        // show; at 4K each hidden one cost a full-frame copy and blend. The
        // result is identical: the covering deck's blit clears what was below.
        if let Some(top) = non_transitioning.iter().rposition(|info| {
            info.opacity >= 1.0
                && info.blend_mode == BlendMode::Normal
                && self.decks[info.deck_idx].deck.covers_frame(modulation)
        }) {
            non_transitioning.drain(..top);
        }
        let ordered: Vec<&DeckCompositeInfo> =
            non_transitioning.into_iter().chain(transitioning).collect();
        self.composited_deck_count = ordered.len();

        // Composite all decks to the composite texture.
        // Per-draw params are written into a persistent ring buffer (one slot per
        // deck) so all command buffers can batch into a single queue.submit().
        // This eliminates N-1 submit calls per channel — critical for Intel
        // integrated GPUs where each Metal command buffer commit is expensive.
        let t_composite = std::time::Instant::now();
        let width = self.composite_texture.width();
        let height = self.composite_texture.height();
        let mut composite_cmds: Vec<wgpu::CommandBuffer> = Vec::new();
        // One params slot per compositing deck. Sized up front, not per draw:
        // growing mid-loop would strand bind groups on the previous buffer.
        self.blit_pipeline
            .ensure_ring_slots(&context.device, ordered.len());
        self.add_pipeline
            .ensure_ring_slots(&context.device, ordered.len());
        self.screen_pipeline
            .ensure_ring_slots(&context.device, ordered.len());
        self.composite_pipeline
            .ensure_ring_slots(&context.device, ordered.len());
        let mut copy_count: u32 = 0;
        let mut bind_group_count: u32 = 0;
        let mut copy_encode_us: u128 = 0;
        let mut bind_group_us: u128 = 0;

        for (i, info) in ordered.iter().enumerate() {
            let slot = &mut self.decks[info.deck_idx];

            // Check if this deck is transitioning with a shader
            if let Some(progress) = info.transition_progress {
                if let Some(effect) = slot.transition_effect.as_mut().filter(|_| i > 0) {
                    // Snapshot composite-so-far into effect_ping_texture
                    let mut copy_encoder =
                        context
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("AT Snapshot Copy"),
                            });
                    copy_encoder.copy_texture_to_texture(
                        self.composite_texture.as_image_copy(),
                        self.effect_ping_texture.as_image_copy(),
                        self.composite_texture.size(),
                    );
                    composite_cmds.push(copy_encoder.finish());

                    // Run transition shader: start=deck (outgoing), end=composite-below (incoming)
                    let uniforms = ISFUniforms {
                        time,
                        time_delta: dt,
                        frame_index: self.frame_count,
                        pass_index: 0,
                        render_size: [width as f32, height as f32],
                        phase_times: [0.0; 4],
                        ..Default::default()
                    };

                    // Set progress on the transition shader
                    effect.params.set(
                        "progress",
                        crate::params::ParamValue::Float(progress as f32),
                    );
                    effect.params.build_buffer_data();
                    if let Some(buf) = effect.params.buffer() {
                        context.queue.write_buffer(buf, 0, effect.params.scratch());
                    }

                    let cmd = effect.pipeline.render_to_cmd(
                        context,
                        &slot.deck.texture_view, // startImage: outgoing deck
                        &self.effect_ping_view,  // endImage: composite below
                        &self.composite_view,    // output: back to composite
                        &uniforms,
                        effect.params.buffer(),
                    );
                    composite_cmds.push(cmd);
                    continue;
                }

                // Opacity fade fallback (no shader or first deck)
                let fade_opacity = info.opacity * (1.0 - progress as f32);
                if i == 0 {
                    // First deck: simple blit with alpha blending
                    self.blit_pipeline.write_params_slot(
                        &context.queue,
                        i,
                        fade_opacity,
                        [1.0, 1.0],
                        [0.0, 0.0],
                        false,
                    );
                    let bind_group = self.blit_pipeline.create_ring_bind_group(
                        &context.device,
                        &slot.deck.texture_view,
                        i,
                    );
                    let mut encoder =
                        context
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("Channel Composite Encoder (AT fade first)"),
                            });
                    {
                        let mut render_pass =
                            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("Channel Composite Pass (AT fade first)"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &self.composite_view,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                        store: wgpu::StoreOp::Store,
                                    },
                                    depth_slice: None,
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                        self.blit_pipeline
                            .render_at_slot(&mut render_pass, &bind_group);
                    }
                    composite_cmds.push(encoder.finish());
                } else {
                    // Subsequent decks: snapshot + composite shader
                    let mut copy_encoder =
                        context
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("Composite Snapshot Copy (AT fade)"),
                            });
                    copy_encoder.copy_texture_to_texture(
                        self.composite_texture.as_image_copy(),
                        self.effect_ping_texture.as_image_copy(),
                        self.composite_texture.size(),
                    );

                    self.composite_pipeline.write_params_slot(
                        &context.queue,
                        i,
                        fade_opacity,
                        info.blend_mode.to_index(),
                        [1.0, 1.0],
                        [0.0, 0.0],
                        false,
                    );
                    let bind_group = self.composite_pipeline.create_ring_bind_group(
                        &context.device,
                        &slot.deck.texture_view,
                        &self.effect_ping_view,
                        i,
                    );
                    let mut encoder =
                        context
                            .device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("Channel Composite Encoder (AT fade)"),
                            });
                    {
                        let mut render_pass =
                            encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                                label: Some("Channel Composite Pass (AT fade)"),
                                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                                    view: &self.composite_view,
                                    resolve_target: None,
                                    ops: wgpu::Operations {
                                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                        store: wgpu::StoreOp::Store,
                                    },
                                    depth_slice: None,
                                })],
                                depth_stencil_attachment: None,
                                timestamp_writes: None,
                                occlusion_query_set: None,
                                multiview_mask: None,
                            });
                        self.composite_pipeline
                            .render_at_slot(&mut render_pass, &bind_group);
                    }
                    composite_cmds.push(copy_encoder.finish());
                    composite_cmds.push(encoder.finish());
                }
                continue;
            }

            // Normal compositing
            if i == 0 {
                // First deck: simple blit with per-draw params
                self.blit_pipeline.write_params_slot(
                    &context.queue,
                    i,
                    info.opacity,
                    [1.0, 1.0],
                    [0.0, 0.0],
                    false,
                );
                let t_bg = std::time::Instant::now();
                let bind_group = self.blit_pipeline.create_ring_bind_group(
                    &context.device,
                    &slot.deck.texture_view,
                    i,
                );
                bind_group_us += t_bg.elapsed().as_micros();
                bind_group_count += 1;
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Channel Composite Encoder (first)"),
                        });
                {
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Channel Composite Pass (first)"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.composite_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    // Where it does not show, a deck over it covers the
                    // clear.
                    self.blit_pipeline.render_at_slot_within(
                        &mut render_pass,
                        &bind_group,
                        info.within.as_deref(),
                    );
                }
                composite_cmds.push(encoder.finish());
            } else if let Some(pipeline) = match info.blend_mode {
                BlendMode::Normal => Some(&self.blit_pipeline),
                BlendMode::Add => Some(&self.add_pipeline),
                BlendMode::Screen => Some(&self.screen_pipeline),
                _ => None,
            } {
                // Normal, Add and Screen are what the GPU's blend unit computes
                // itself, exactly as the composite shader does (see the blend
                // states), so the deck is drawn straight onto the composite —
                // no full-frame snapshot copy and re-read, ~130 MB at 4K.
                if info.blend_mode == BlendMode::Normal {
                    // ALPHA_BLENDING weights the straight colour by alpha itself.
                    pipeline.write_params_slot(
                        &context.queue,
                        i,
                        info.opacity,
                        [1.0, 1.0],
                        [0.0, 0.0],
                        false,
                    );
                } else {
                    pipeline.write_params_slot_premultiplying(&context.queue, i, info.opacity);
                }
                bind_group_count += 1;
                composite_cmds.push(pipeline.blend_onto_within(
                    &context.device,
                    &slot.deck.texture_view,
                    &self.composite_view,
                    i,
                    "Channel Composite (blend unit)",
                    info.within.as_deref(),
                ));
            } else {
                // Subsequent decks: snapshot composite → ping, blend src + ping → composite
                let t_copy = std::time::Instant::now();
                let mut copy_encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Composite Snapshot Copy"),
                        });
                copy_encoder.copy_texture_to_texture(
                    self.composite_texture.as_image_copy(),
                    self.effect_ping_texture.as_image_copy(),
                    self.composite_texture.size(),
                );
                copy_encode_us += t_copy.elapsed().as_micros();
                copy_count += 1;

                self.composite_pipeline.write_params_slot(
                    &context.queue,
                    i,
                    info.opacity,
                    info.blend_mode.to_index(),
                    [1.0, 1.0],
                    [0.0, 0.0],
                    false,
                );
                let t_bg = std::time::Instant::now();
                let bind_group = self.composite_pipeline.create_ring_bind_group(
                    &context.device,
                    &slot.deck.texture_view,
                    &self.effect_ping_view,
                    i,
                );
                bind_group_us += t_bg.elapsed().as_micros();
                bind_group_count += 1;
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Channel Composite Encoder"),
                        });
                {
                    // Drawn only where it shows, the rest is left as it was.
                    let load = if info.within.is_some() {
                        wgpu::LoadOp::Load
                    } else {
                        wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT)
                    };
                    let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Channel Composite Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: &self.composite_view,
                            resolve_target: None,
                            ops: wgpu::Operations {
                                load,
                                store: wgpu::StoreOp::Store,
                            },
                            depth_slice: None,
                        })],
                        depth_stencil_attachment: None,
                        timestamp_writes: None,
                        occlusion_query_set: None,
                        multiview_mask: None,
                    });
                    self.composite_pipeline.render_at_slot_within(
                        &mut render_pass,
                        &bind_group,
                        info.within.as_deref(),
                    );
                }
                composite_cmds.push(copy_encoder.finish());
                composite_cmds.push(encoder.finish());
            }
        }
        let composite_encode_us = t_composite.elapsed().as_micros();

        // Batch submit all channel composite commands at once
        let t_composite_submit = std::time::Instant::now();
        if !composite_cmds.is_empty() {
            context.submit(composite_cmds);
        }
        let composite_submit_us = t_composite_submit.elapsed().as_micros();

        // If no decks, clear the composite texture to transparent
        if deck_composite_info.is_empty() {
            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Channel Clear Encoder"),
                    });
            {
                let _render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Channel Clear Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.composite_view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            context.submit(std::iter::once(encoder.finish()));
        }

        // Apply channel effect chain (if any)
        let t_effects = std::time::Instant::now();
        let mut effects_count: u32 = 0;
        if !self.effects.is_empty() {
            let width = self.composite_texture.width();
            let height = self.composite_texture.height();

            let uniforms = ISFUniforms {
                time,
                time_delta: 1.0 / 60.0,
                frame_index: self.frame_count,
                pass_index: 0,
                render_size: [width as f32, height as f32],
                audio_level: audio_data.level,
                audio_bass: audio_data.bass(),
                audio_mid: audio_data.mid(),
                audio_treble: audio_data.treble(),
                audio_bpm: audio_data.bpm.unwrap_or(0.0),
                audio_beat_phase: audio_data.beat_phase(),
                date: crate::deck::get_current_date(),
                phase_times: [0.0; 4],
            };

            let mut read_from_composite = true;
            let mut fx_cmd_buffers: Vec<wgpu::CommandBuffer> = Vec::new();

            for (eff_idx, effect) in self.effects.iter_mut().enumerate() {
                if !effect.is_active() {
                    continue;
                }

                let (input_view, output_view) = if read_from_composite {
                    (&self.composite_view, &self.effect_ping_view)
                } else {
                    (&self.effect_ping_view, &self.composite_view)
                };

                if let Err(e) = effect.apply_with_modulation(
                    context,
                    input_view,
                    output_view,
                    &uniforms,
                    Some(modulation),
                    &mut fx_cmd_buffers,
                ) {
                    log::warn!("Effect {eff_idx} failed, skipping: {e}");
                    continue;
                }
                effects_count += 1;
                read_from_composite = !read_from_composite;
            }

            // If result is in ping texture, copy back to composite
            if !read_from_composite {
                let mut encoder =
                    context
                        .device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Channel Effect Final Copy Encoder"),
                        });
                encoder.copy_texture_to_texture(
                    self.effect_ping_texture.as_image_copy(),
                    self.composite_texture.as_image_copy(),
                    self.composite_texture.size(),
                );
                fx_cmd_buffers.push(encoder.finish());
            }

            // Batch submit all channel effects
            if !fx_cmd_buffers.is_empty() {
                context.submit(fx_cmd_buffers);
            }
        }
        let effects_us = t_effects.elapsed().as_micros();

        self.frame_count += 1;

        // Update smoothed render time (EMA, α = 0.1)
        let raw_ms = render_start.elapsed().as_secs_f32() * 1000.0;
        self.render_time_ms = 0.1 * raw_ms + 0.9 * self.render_time_ms;

        // Log detailed timing every 127 frames (~2s at 60fps).
        // Prime interval avoids systematic alignment with skip intervals
        // (e.g., 120 % 2/3/4/5/6 == 0 would always sample render frames).
        if self.frame_count.is_multiple_of(127) && active_count > 0 {
            let total_us = render_start.elapsed().as_micros();
            // Build per-deck breakdown string: "shader_name=123us" or "shader_name=SKIP"
            let deck_breakdown: String = per_deck_timings
                .iter()
                .map(|(name, us, skipped)| {
                    if *skipped {
                        format!("{name}=SKIP")
                    } else {
                        format!("{name}={us}us")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            log::debug!(
                "[PERF] ch={} decks={} | deck_render={}us [{}] deck_submit={}us | \
                 composite_encode={}us composite_submit={}us copies={} \
                 copy_encode={}us bind_groups={} bind_group={}us | \
                 effects={} effects={}us | total={}us ({:.1}ms)",
                self.name,
                active_count,
                deck_render_us,
                deck_breakdown,
                deck_submit_us,
                composite_encode_us,
                composite_submit_us,
                copy_count,
                copy_encode_us,
                bind_group_count,
                bind_group_us,
                effects_count,
                effects_us,
                total_us,
                raw_ms,
            );
        }

        Ok(())
    }

    /// Resize the channel's textures
    pub fn resize(&mut self, context: &GpuContext, width: u32, height: u32) {
        self.composite_texture = context.create_compositing_texture(width, height);
        self.composite_view = self
            .composite_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        self.effect_ping_texture = context.create_compositing_texture(width, height);
        self.effect_ping_view = self
            .effect_ping_texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        // Dropped rather than resized; the per-frame sync reallocates it at the
        // new size, costing one black frame for any deck tapping this channel.
        self.tap_prev = None;

        for slot in &mut self.decks {
            slot.deck.resize(context, width, height);
        }
    }

    /// Add a channel effect
    pub fn add_effect(&mut self, effect: Effect) {
        self.effects.push(effect);
    }

    /// Remove a channel effect by index
    pub fn remove_effect(&mut self, index: usize) -> bool {
        if index < self.effects.len() {
            self.effects.remove(index);
            true
        } else {
            false
        }
    }

    /// Set deck opacity
    pub fn set_deck_opacity(&mut self, index: usize, opacity: f32) {
        if let Some(slot) = self.decks.get_mut(index) {
            slot.opacity = if opacity.is_finite() {
                opacity.clamp(0.0, 1.0)
            } else {
                1.0
            };
        }
    }

    /// Set deck solo
    pub fn set_deck_solo(&mut self, index: usize, solo: bool) {
        if let Some(slot) = self.decks.get_mut(index) {
            slot.solo = solo;
        }
    }

    /// Set deck mute
    pub fn set_deck_mute(&mut self, index: usize, mute: bool) {
        if let Some(slot) = self.decks.get_mut(index) {
            slot.mute = mute;
        }
    }

    /// Set deck blend mode
    pub fn set_deck_blend_mode(&mut self, index: usize, blend_mode: BlendMode) {
        if let Some(slot) = self.decks.get_mut(index) {
            slot.blend_mode = blend_mode;
        }
    }

    /// Tick auto-transition state for all decks in this channel.
    /// Called once per frame before rendering.
    /// `dt` is the frame delta in seconds, `bpm` is the current detected BPM (if any).
    pub fn tick_auto_transitions(&mut self, dt: f64, bpm: Option<f64>) {
        // Determine the "active" deck — topmost visible with auto-transition enabled.
        // Sort by z-index descending to find the top deck.
        let mut sorted: Vec<usize> = (0..self.decks.len()).collect();
        sorted.sort_by_key(|&i| std::cmp::Reverse(self.decks[i].z_index));

        // Find the topmost deck that is visible and has auto-transition enabled
        let active_idx = sorted.iter().copied().find(|&i| {
            let slot = &self.decks[i];
            let has_at = slot.auto_transition.as_ref().is_some_and(|at| at.enabled);
            let phase = slot.transition_phase();
            has_at
                && !slot.mute
                && !slot.arrangement_authority
                && phase != DeckTransitionPhase::Done
        });

        // Update phase for each deck, collecting indices that just started transitioning
        let mut just_started_transitioning: Vec<usize> = Vec::new();

        for i in 0..self.decks.len() {
            let is_active = active_idx == Some(i);
            let slot = &mut self.decks[i];
            // A deck the arrangement drives keeps its auto-transition config but
            // stops advancing it. Auto-transitions are *relative* (phase depends
            // on when a deck became active, not on transport position), so they
            // cannot be resolved from an arbitrary position and would fight the
            // regions. See /spec/transport.md § Relationship to Performance Mode
            // Sequencers.
            if slot.arrangement_authority {
                continue;
            }
            let at = match &mut slot.auto_transition {
                Some(at) if at.enabled => at,
                _ => continue,
            };

            match at.phase {
                DeckTransitionPhase::Inactive => {
                    if is_active {
                        at.phase = DeckTransitionPhase::Playing { elapsed: 0.0 };
                    }
                }
                DeckTransitionPhase::Playing { ref mut elapsed } => {
                    *elapsed += dt;
                    let play_secs = at.play_duration.to_seconds(bpm);

                    // Check trigger condition
                    let should_transition = match at.trigger {
                        TransitionTrigger::Timer => *elapsed >= play_secs,
                        TransitionTrigger::ClipEnd => {
                            // Check if video reached end
                            let snap = slot.deck.playback_snapshot();
                            let clip_ended = snap.as_ref().is_some_and(|ps| ps.reached_end);
                            // Also respect timer as fallback for non-video sources
                            clip_ended || (snap.is_none() && *elapsed >= play_secs)
                        }
                    };

                    if should_transition {
                        at.phase = DeckTransitionPhase::Transitioning { progress: 0.0 };
                        just_started_transitioning.push(i);
                    }
                }
                DeckTransitionPhase::Transitioning { ref mut progress } => {
                    let trans_secs = at.transition_duration.to_seconds(bpm);
                    if trans_secs > 0.0 {
                        *progress += dt / trans_secs;
                    } else {
                        *progress = 1.0;
                    }
                    if *progress >= 1.0 {
                        at.phase = DeckTransitionPhase::Done;
                    }
                }
                DeckTransitionPhase::Done => {
                    // Stay done until loop reset
                }
            }
        }

        // Activate the next deck for each deck that just started transitioning.
        // This makes the next deck visible as the background the transition reveals.
        // First try Inactive decks; if none, wrap around to the first Done deck (loop).
        for _trigger_idx in just_started_transitioning {
            let mut activated = false;
            // Try Inactive first
            for j in 0..self.decks.len() {
                let slot = &self.decks[j];
                let is_candidate = slot
                    .auto_transition
                    .as_ref()
                    .is_some_and(|at| at.enabled && at.phase == DeckTransitionPhase::Inactive);
                if is_candidate && !slot.mute {
                    if let Some(at) = self.decks[j].auto_transition.as_mut() {
                        at.phase = DeckTransitionPhase::Playing { elapsed: 0.0 };
                    }
                    activated = true;
                    break;
                }
            }
            // If no Inactive found, wrap around: re-activate the first Done deck
            if !activated {
                for j in 0..self.decks.len() {
                    let slot = &self.decks[j];
                    let is_candidate = slot
                        .auto_transition
                        .as_ref()
                        .is_some_and(|at| at.enabled && at.phase == DeckTransitionPhase::Done);
                    if is_candidate && !slot.mute {
                        if let Some(at) = self.decks[j].auto_transition.as_mut() {
                            at.phase = DeckTransitionPhase::Playing { elapsed: 0.0 };
                        }
                        break;
                    }
                }
            }
        }

        // Check if all auto-transition decks are now Done → loop reset.
        // Done AFTER phase updates so the reset happens in the same frame
        // a deck transitions to Done, preventing a flash of stale content.
        let all_done = self.decks.iter().all(|slot| match &slot.auto_transition {
            Some(at) if at.enabled => at.phase == DeckTransitionPhase::Done,
            _ => true,
        });
        let any_at = self
            .decks
            .iter()
            .any(|slot| slot.auto_transition.as_ref().is_some_and(|at| at.enabled));

        if all_done && any_at {
            // Reset all AT decks to Inactive, then immediately activate the first one
            for slot in &mut self.decks {
                if let Some(at) = &mut slot.auto_transition {
                    if at.enabled {
                        at.phase = DeckTransitionPhase::Inactive;
                    }
                }
            }
            for slot in &mut self.decks {
                let dominated = slot.mute;
                let is_inactive_at = slot
                    .auto_transition
                    .as_ref()
                    .is_some_and(|at| at.enabled && at.phase == DeckTransitionPhase::Inactive);
                if is_inactive_at && !dominated {
                    if let Some(at) = slot.auto_transition.as_mut() {
                        at.phase = DeckTransitionPhase::Playing { elapsed: 0.0 };
                    }
                    break;
                }
            }
        }
    }
}

/// Whether a deck on [`DeckRenderFps::Auto`] is drawn this frame, its
/// estimated cost `estimated_us` (CPU-measured × the GPU load ratio) against
/// its share of a frame's time.
///
/// Drawn every frame unless last frame's decks together ran over a frame's
/// time (`frame_over_budget`); only then is one over its share drawn every
/// Nth frame, repeating its last picture between. Splitting a frame's time
/// evenly between the decks and skipping any over its share whatever the
/// total (as Varda did) had a show of a dozen decks — picture-in-pictures,
/// taps, playlists — drawing most of them every other frame, a moving picture stuttering on
/// Preview, while the frame itself kept well within its time.
fn auto_draws(
    estimated_us: f32,
    per_deck_budget_us: f32,
    frame_over_budget: bool,
    skip_counter: u32,
) -> bool {
    if !frame_over_budget
        || estimated_us <= 0.0
        || per_deck_budget_us >= f32::MAX
        || estimated_us <= per_deck_budget_us
    {
        return true;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let skip_interval = (estimated_us / per_deck_budget_us).ceil() as u32;
    skip_counter.is_multiple_of(skip_interval.max(1))
}

/// Whether decks costing `estimated_us` each (CPU-measured × the GPU load
/// ratio) together run over a frame's time at `target_fps`.
pub(crate) fn decks_over_budget(estimated_us: impl IntoIterator<Item = f32>, target_fps: u32) -> bool {
    if target_fps == 0 {
        return false;
    }
    #[allow(clippy::cast_precision_loss)]
    let frame_us = 1_000_000.0 / target_fps as f32;
    estimated_us.into_iter().sum::<f32>() > frame_us
}

#[cfg(test)]
mod tests;
