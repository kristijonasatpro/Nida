//! `UIActions` — everything a frame of UI emits, plus the drag payload types.
//!
//! Split into the outbound `EngineCommand` stream and UI-local [`UISession`]
//! state (/spec/ui-engine-boundary.md WS4).

use super::{DomeAction, ParamUIInfo, UISession};
use crate::ShaderParams;

/// All UI output collected during a frame, split into two buckets (WS4):
///
/// - [`commands`](Self::commands): the outbound engine-mutation stream. Panels
///   push `EngineCommand`s directly (the single mutation vocabulary shared with
///   the HTTP/CLI consumers); the app-layer drain runs each through the same
///   dispatch as the command bus.
/// - [`session`](Self::session): UI-local ephemeral state (selection, panel
///   visibility, learn mode, dialog triggers, undo/redo/save). See [`UISession`].
pub struct UIActions {
    /// Outbound engine mutations (see /spec/ui-engine-boundary.md WS2).
    pub commands: Vec<crate::engine::EngineCommand>,
    /// UI-local session/ephemeral state (see [`UISession`], WS4).
    pub session: UISession,
}

impl Default for UIActions {
    fn default() -> Self {
        Self::new()
    }
}

/// Run the UI one pass a frame. egui runs a frame's UI again when a pass
/// asks it to — a grid or a resizable area seen for the first time — but
/// gives the frame's input to the first pass only, while a frame's actions
/// are applied once it is done: the last pass's, run from the same data
/// without the clicks, undid them (a layout picked from the menu went back to
/// the one before whenever its panels were new), and keeping the first would
/// lose what a later pass finishes (a drop). One pass sees each input once;
/// a new grid is sized a frame late.
pub fn one_pass_a_frame(ctx: &egui::Context) {
    ctx.options_mut(|o| o.max_passes = std::num::NonZeroUsize::MIN);
}

/// Keep Nida dark whatever the Mac's appearance. Its colours are made for a
/// dark window, and it starts dark: the window says nothing of the Mac's
/// appearance until it changes. When it changed while Nida ran, egui took
/// it up — to light, its own light look, whitish around the dark tiles,
/// until Nida was opened again.
pub fn always_dark(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
}

impl UIActions {
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
            session: UISession::new(),
        }
    }

    /// Whether this frame's actions include any undoable mutation carried by a
    /// non-command field. Source-deck adds, deck remove/move/reorder, channel
    /// add, effects, presets, and mixer edits now flow through `commands` and
    /// are gated by `batch_has_undoable`. Only two irreducible residuals remain
    /// here: the async shader load (`shader_to_add`, resolved off-frame and thus
    /// never on `commands`) and channel removal (`remove_channel`, kept a field
    /// so the runner can fix up UI selection with the removed index).
    pub fn has_undoable_action(&self) -> bool {
        self.session.shader_to_add.is_some()
            || self.session.remove_channel.is_some()
            // Files dropped beside the inputs make inputs: in the frame's one
            // undo with a PowerPoint deck dropped with them, made by its
            // command. Not when none can make one.
            || self
                .session
                .files_as_inputs
                .iter()
                .any(|f| crate::deck::media::MediaKind::of(f).is_some())
    }

    /// Whether this frame's actions include any undoable *stage* mutation
    /// (surface geometry/warp/holes/combine/reorder, surface→output
    /// assignments, or authored dome changes).
    ///
    /// Deliberately excludes non-authored actions on the same collections:
    /// output-window lifecycle (create/remove/reposition) and dome preview
    /// camera navigation (rotate/zoom/reset) are live/venue controls, not
    /// stage-editor edits, so they must not create history entries.
    ///
    /// Does NOT distinguish continuous vs discrete edits — gesture collapsing is
    /// handled by the `gesture_active` edge in the runner.
    pub fn has_undoable_stage_action(&self) -> bool {
        self.session.dome_actions.iter().any(|a| {
            !matches!(
                a,
                DomeAction::RotateCamera { .. }
                    | DomeAction::ZoomCamera { .. }
                    | DomeAction::ResetCamera
            )
        })
    }
}

/// Drag payload types for library drag-and-drop
#[derive(Debug, Clone)]
pub enum LibraryDrag {
    /// Generator shader from library (registry index)
    Generator(usize),
    /// Effect/filter shader from library (registry index)
    Effect(usize),
    /// Camera device from library (`CameraId`)
    Camera(crate::camera::CameraId),
    /// Depth sensor from library (`DepthSensorId`)
    DepthSensor(crate::depth::DepthSensorId),
    /// Screen or window capture target, addressed by name rather than by
    /// platform handle so the payload stays valid across a rescan.
    ScreenCapture(crate::scene::CaptureTargetConfig),
    /// Varda's own program or a channel composite. See spec/program-tap.md.
    Tap(crate::scene::TapSourceConfig),
    /// NDI network source (source name)
    Ndi(String),
    /// Syphon server (server name)
    Syphon(String),
    /// SRT network source (url, mode)
    Srt(String, crate::stream::SrtMode),
    /// HLS stream source (url)
    Hls(String),
    /// DASH stream source (url)
    Dash(String),
    /// RTMP stream source (url, mode)
    Rtmp(String, crate::stream::RtmpMode),
    /// HTML content source (url)
    Html(String),
    /// Deck preset from library (index into `preset_library.deck_presets`)
    DeckPreset(usize),
    /// Channel preset from library (index into `preset_library.channel_presets`)
    ChannelPreset(usize),
}

/// Drag payload for moving a deck to another channel or reordering it within
/// its own. Named by UUID because the payload is set on drag start and read on
/// release: the deck's index can shift in between.
#[derive(Debug, Clone, PartialEq)]
pub struct DeckDrag {
    pub deck_uuid: String,
}

/// Drag payload for effect reordering within a chain. The chain is named by
/// UUID because the payload outlives the frame it was created in — the drop is
/// applied after release, by which point an index could name another entity.
#[derive(Debug, Clone, PartialEq)]
pub enum EffectDrag {
    /// Deck effect: (`deck_uuid`, `effect_idx`)
    Deck(String, usize),
    /// Channel effect: (`channel_uuid`, `effect_idx`)
    Channel(String, usize),
    /// Master effect: (`effect_idx`)
    Master(usize),
}

/// Drag payload for reordering steps within a sequence (bottom bar only)
#[derive(Debug, Clone, PartialEq)]
pub struct SequenceStepDrag {
    pub sequence_uuid: String,
    pub step_idx: usize,
}

/// Helper to extract params from `ShaderParams` for UI display
pub fn collect_params(params: &ShaderParams) -> Vec<ParamUIInfo> {
    params
        .param_order
        .iter()
        .filter_map(|name| {
            let value = params.values.get(name)?;
            let def = params.definitions.get(name);
            Some(ParamUIInfo {
                name: name.clone(),
                label: def.and_then(|d| d.label.clone()),
                value: *value,
                min: def.and_then(|d| d.min),
                max: def.and_then(|d| d.max),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The theme a context takes after the Mac's appearance turns light.
    fn after_a_switch_to_light(ctx: &egui::Context) -> egui::Theme {
        let input = egui::RawInput {
            system_theme: Some(egui::Theme::Light),
            ..Default::default()
        };
        ctx.run_ui(input, |_| {}).textures_delta.clear();
        ctx.theme()
    }

    /// Files dropped beside the inputs are in the frame's one undo — with
    /// the input a PowerPoint deck dropped with them makes by its command —
    /// unless none of them can make an input.
    #[test]
    fn files_dropped_as_inputs_are_undone_with_the_frame() {
        let mut actions = UIActions::new();
        actions.session.files_as_inputs = vec!["/show/notes.pdf".into()];
        assert!(!actions.has_undoable_action(), "nothing it can make");
        actions
            .session
            .files_as_inputs
            .push("/show/slide 1.png".into());
        assert!(actions.has_undoable_action());
    }

    #[test]
    fn nida_stays_dark_when_the_mac_turns_light() {
        let plain = egui::Context::default();
        assert_eq!(
            after_a_switch_to_light(&plain),
            egui::Theme::Light,
            "precondition: egui follows the Mac"
        );
        let ctx = egui::Context::default();
        always_dark(&ctx);
        assert_eq!(after_a_switch_to_light(&ctx), egui::Theme::Dark);
        assert!(ctx.global_style().visuals.dark_mode);
    }
}
