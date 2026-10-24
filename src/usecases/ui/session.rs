//! `UISession` — the half of a frame's UI output that is *not* an engine
//! mutation: selection focus, panel visibility, learn-mode targeting, dialog
//! triggers, and the undo/redo/save flags.
//!
//! See /spec/ui-engine-boundary.md WS4 / Decision #11.

use super::{CameraDetectAction, DomeAction};

/// What the Show menu asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShowAction {
    /// Save this show and start an empty one, keeping the room setup.
    New,
    /// Save this show and pick another to open.
    Open,
    /// Save this show and open this one.
    OpenRecent(std::path::PathBuf),
    /// Save this show under a new name, and carry on in it.
    SaveAs,
    /// Show the show's folder in Finder.
    Reveal,
}

/// Where a new deck goes: a channel, and the deck in it that it goes
/// before — `None` for the end. Both by UUID, since a load can outlive the
/// positions it was aimed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeckPlace {
    pub channel_uuid: String,
    pub before_deck_uuid: Option<String>,
}

impl DeckPlace {
    /// The end of a channel.
    pub fn end_of(channel_uuid: impl Into<String>) -> Self {
        Self {
            channel_uuid: channel_uuid.into(),
            before_deck_uuid: None,
        }
    }
}

/// UI-local session/ephemeral state accumulated during a frame (Population 2).
///
/// This is the half of the frame's UI output that is **not** an engine mutation:
/// selection focus, panel visibility, learn-mode targeting, dialog-open triggers,
/// notification dismissals, gesture continuation, the async shader-load request,
/// and the layout-coupled undo/redo/save triggers. None of it belongs on the
/// command bus (the HTTP/CLI/MIDI consumers neither can nor should express it);
/// it targets UI-local state (`UILayoutState`) or the runner, not the engine.
///
/// See /spec/ui-engine-boundary.md WS4 / Decision #11 — the deliberate split of
/// "what I tell the engine" (`UIActions::commands`) from "my local view state".
// Per-frame request flags that are independently set and cleared; an enum cannot express them.
#[allow(clippy::struct_excessive_bools)]
pub struct UISession {
    /// (`channel_uuid`, `generator_registry_idx`) — add a shader as a new deck to a
    /// channel. Resolved off-frame via `spawn_deck_loads` (not a command), so the
    /// channel is held by UUID: a channel index captured at click time can name a
    /// different channel by the time the shader finishes compiling. See
    /// [`/spec/api-addressing.md`].
    pub shader_to_add: Option<(DeckPlace, usize)>,
    /// Channel UUID to open an image file dialog for (deferred to outside egui
    /// frame). A UUID rather than an index because the dialog outlives the
    /// frame that requested it — see [`/spec/api-addressing.md`].
    pub open_image_dialog_for_channel: Option<String>,
    /// Channel UUID to open a video file dialog for (deferred to outside egui frame)
    pub open_video_dialog_for_channel: Option<String>,
    /// Pick pictures, a new input each (Add Input's Image).
    pub open_image_dialog_for_new_inputs: bool,
    /// Pick clips and sound files, a new input each (Add Input's Video).
    pub open_video_dialog_for_new_inputs: bool,
    /// Files dropped in from Finder, each batch with where it goes, in the
    /// order they were dropped.
    pub files_to_add: Vec<(DeckPlace, Vec<std::path::PathBuf>)>,
    /// Files dropped where no channel is, in the order they were dropped:
    /// an input each.
    pub files_as_inputs: Vec<std::path::PathBuf>,
    /// Open a folder picker for where recordings go (outside the frame, like
    /// the file dialogs).
    pub choose_record_folder: bool,
    /// Show a file in Finder (or the system's file manager).
    pub reveal_file: Option<std::path::PathBuf>,
    /// Notifications to put away, by id (see `Notification::id`).
    pub notifications_to_dismiss: Vec<u64>,
    /// Info notifications to push (e.g. "Copied URL to clipboard")
    pub info_notifications: Vec<String>,
    /// MIDI learn: toggle learn mode on/off
    pub midi_learn_toggle: bool,
    /// MIDI learn: select a parameter as learn target (in learn mode, clicking a param)
    pub midi_learn_select: Option<String>,
    /// Keyboard learn: toggle learn mode on/off
    pub keyboard_learn_toggle: bool,
    /// Keyboard learn: select a target (Action or `ParamPath`)
    pub keyboard_learn_select: Option<crate::keymap::KeyTarget>,
    /// Keyboard learn: bind a key combo to current target
    pub keyboard_learn_bind: Option<crate::keymap::KeyCombo>,
    /// Select a deck for detail view in bottom bar (`ch_idx`, `deck_idx`)
    pub select_deck: Option<(usize, usize)>,
    /// Select a channel for detail view in bottom bar (`ch_idx`)
    pub select_channel: Option<usize>,
    /// Select master output for detail view in bottom bar
    pub select_master: bool,
    /// Select a sequence for detail view in bottom bar (`seq_idx`)
    pub select_sequence: Option<usize>,
    /// Select a step within a sequence for editing in bottom bar (`seq_idx`, `step_idx`)
    pub select_sequence_step: Option<(usize, usize)>,
    /// Select a macro (by UUID) for detail view in bottom bar
    pub select_macro: Option<String>,
    /// Clear the macro selection (e.g. its macro was deleted, or Close pressed)
    pub deselect_macro: bool,
    /// Remove a channel from the mixer (by index). Retained as a non-command
    /// field because the runner needs the removed index to fix up UI selection
    /// state (`layout.fixup_channel_removal`).
    pub remove_channel: Option<usize>,
    /// Toggle stage editor open/closed
    pub toggle_stage_editor: bool,
    /// Swap the central area between Performance and Arrangement. Purely a view
    /// change: the arrangement drives decks whenever the transport runs, in
    /// either mode. See /spec/arrangement.md § UI.
    pub toggle_arrangement_mode: bool,
    /// Timeline horizontal zoom, in pixels per second of show time.
    pub set_arrangement_zoom: Option<f32>,
    /// Timeline horizontal scroll, as the show position at the left edge.
    pub set_arrangement_scroll: Option<f64>,
    /// Timeline vertical scroll, in pixels of rows above the top edge.
    pub set_arrangement_scroll_y: Option<f32>,
    /// Round timeline edits to whole frames. A property of the gesture, never of
    /// the stored position. See /spec/arrangement.md § Does the ruler own a
    /// frame rate?
    pub toggle_arrangement_snap: bool,
    /// Mark the stretch of show being worked on. See /spec/arrangement.md § The
    /// focus area.
    pub set_arrangement_focus: Option<super::state::FocusRange>,
    /// Unmark it. Separate from setting so a clear is not a range nobody can
    /// express.
    pub clear_arrangement_focus: bool,
    /// Toggle 3D dome preview in stage editor
    pub toggle_dome_preview: bool,
    /// Dome mode actions (camera, config, mode toggle). UI-local layout state
    /// (`DomeLayoutFields`); only committed to the engine via `GenerateDomeSlices`.
    pub dome_actions: Vec<DomeAction>,
    /// Camera detection actions (preview state machine; only `Accept` becomes a command)
    pub camera_detect_actions: Vec<CameraDetectAction>,
    /// Set stage editor grid size (normalized)
    pub set_grid_size: Option<f32>,
    /// Toggle snap-to-grid
    pub toggle_snap: bool,
    /// Toggle library panel open/closed
    pub toggle_library_panel: bool,
    /// Panels to open (true) or close (false) — a timer's control panel,
    /// from its buttons.
    pub panel_requests: Vec<(crate::usecases::ui::screen_layout::Pane, bool)>,
    /// Toggle right panel open/closed
    pub toggle_right_panel: bool,
    /// The panel arrangement after this frame — dragged, resized, opened,
    /// closed, or a layout chosen. Sent every frame the dock is drawn.
    pub screen_layout: Option<crate::usecases::ui::screen_layout::ScreenLayout>,
    /// Save the arrangement in use under this name, for the user rather than
    /// the workspace.
    pub save_layout_as: Option<String>,
    /// Forget this saved layout.
    pub delete_saved_layout: Option<String>,
    /// Choices made in windows to keep for every show — where the Input
    /// Settings window puts its controls (see `window_choices`).
    pub window_choices: Option<crate::usecases::ui::window_choices::WindowChoices>,
    /// Fold this channel to a strip in the Inputs panel, or open it out.
    pub toggle_collapsed_channel: Option<String>,
    /// Save workspace requested (Ctrl+S / Cmd+S). Layout-coupled runner trigger
    /// (accepted deviation, /spec/ui-engine-boundary.md Decision #10).
    pub save_requested: bool,
    /// Something to do with shows, from the Show menu: layout-coupled (the
    /// layout is saved with the show) and needing file dialogs, so the runner's.
    pub show_action: Option<ShowAction>,
    /// Undo last undoable action. Layout-coupled runner trigger (accepted deviation).
    pub undo_requested: bool,
    /// Redo last undone action. Layout-coupled runner trigger (accepted deviation).
    pub redo_requested: bool,
    /// A mutating stage/warp pointer drag is in progress this frame. Set by the
    /// stage editor and warp editor while dragging a vertex, warp point, bezier
    /// handle, or gizmo, and by any scene param/opacity slider drag. Used to
    /// collapse a continuous drag into a single undo step (snapshot on gesture
    /// start, suppressed while held).
    pub gesture_active: bool,
}

impl Default for UISession {
    fn default() -> Self {
        Self::new()
    }
}

impl UISession {
    pub fn new() -> Self {
        Self {
            shader_to_add: None,
            open_image_dialog_for_channel: None,
            open_video_dialog_for_channel: None,
            open_image_dialog_for_new_inputs: false,
            open_video_dialog_for_new_inputs: false,
            files_to_add: Vec::new(),
            files_as_inputs: Vec::new(),
            choose_record_folder: false,
            reveal_file: None,
            notifications_to_dismiss: Vec::new(),
            info_notifications: Vec::new(),
            midi_learn_toggle: false,
            midi_learn_select: None,
            keyboard_learn_toggle: false,
            keyboard_learn_select: None,
            keyboard_learn_bind: None,
            select_deck: None,
            select_channel: None,
            select_master: false,
            select_sequence: None,
            select_sequence_step: None,
            select_macro: None,
            deselect_macro: false,
            remove_channel: None,
            toggle_stage_editor: false,
            toggle_arrangement_mode: false,
            set_arrangement_zoom: None,
            set_arrangement_scroll: None,
            set_arrangement_scroll_y: None,
            toggle_arrangement_snap: false,
            set_arrangement_focus: None,
            clear_arrangement_focus: false,
            toggle_dome_preview: false,
            dome_actions: Vec::new(),
            camera_detect_actions: Vec::new(),
            set_grid_size: None,
            toggle_snap: false,
            toggle_library_panel: false,
            panel_requests: Vec::new(),
            toggle_right_panel: false,
            screen_layout: None,
            save_layout_as: None,
            delete_saved_layout: None,
            window_choices: None,
            toggle_collapsed_channel: None,
            save_requested: false,
            show_action: None,
            undo_requested: false,
            redo_requested: false,
            gesture_active: false,
        }
    }
}
