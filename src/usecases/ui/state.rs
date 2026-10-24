//! UI-owned layout and selection state.
//!
//! Presentation concerns the engine never sees. Each UI consumer keeps its own
//! instance; the egui consumer's copy is persisted in `stage.json` via
//! `StagePrefs`. See /spec/ui-engine-boundary.md.

use super::{DomeAction, UIActions};
use crate::camera::CameraId;
use crate::renderer::slicer::{DomeGeometry, DomePreset};
use crate::surface::detect::{DetectedContour, DetectionParams};

/// Zoom bounds for the arrangement timeline, in pixels per second.
///
/// The floor keeps an hour-long show legible on one screen; the ceiling is about
/// one pixel per frame at 60fps, past which there is nothing left to resolve.
pub const MIN_PIXELS_PER_SECOND: f32 = 0.5;
pub const MAX_PIXELS_PER_SECOND: f32 = 400.0;

/// A marked stretch of show, in seconds. The thing a performer is dialling in.
///
/// Ordered on construction, so a range drawn right to left is the same range as
/// one drawn left to right rather than an empty one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FocusRange {
    pub start: f64,
    pub end: f64,
}

impl FocusRange {
    pub fn new(a: f64, b: f64) -> Self {
        Self {
            start: a.min(b).max(0.0),
            end: a.max(b).max(0.0),
        }
    }

    pub fn span(self) -> f64 {
        self.end - self.start
    }

    /// Whether the range is long enough to be worth keeping. A click that
    /// happened to land on the strip is not a range.
    pub fn is_usable(self) -> bool {
        self.span() > f64::EPSILON
    }
}

/// UI-consumer-owned layout and selection state.
///
/// These fields are presentation concerns that don't belong in the engine.
/// Each UI consumer (egui, CLI, HTTP API) maintains its own instance.
/// Persisted in `stage.json` via the `StagePrefs` struct.
// Independent panel/toggle flags; grouping them into enums would not model reality.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug)]
pub struct UILayoutState {
    /// Currently selected deck for detail view in bottom bar (`ch_idx`, `deck_idx`)
    pub selected_deck: Option<(usize, usize)>,
    /// Currently selected channel for detail view in bottom bar (`ch_idx`)
    pub selected_channel: Option<usize>,
    /// Whether the master output is selected for detail view in bottom bar
    pub selected_master: bool,
    /// Currently selected sequence for detail view in bottom bar (`seq_idx`)
    pub selected_sequence: Option<usize>,
    /// Currently selected step within the selected sequence (`seq_idx`, `step_idx`)
    pub selected_sequence_step: Option<(usize, usize)>,
    /// Currently selected macro (by UUID) for detail view in bottom bar
    pub selected_macro: Option<String>,
    /// Whether the full-screen stage editor is open (replaces deck view)
    pub stage_editor_open: bool,
    /// Stage editor grid size (normalized, e.g. 0.05 = 20 divisions)
    pub stage_editor_grid_size: f32,
    /// Whether snap-to-grid is enabled in the stage editor
    pub stage_editor_snap: bool,
    /// Whether the central area shows the arrangement timeline instead of the
    /// mixer. See /spec/arrangement.md § UI.
    pub arrangement_mode_open: bool,
    /// Timeline horizontal zoom, in pixels per second of show time.
    pub arrangement_pixels_per_second: f32,
    /// Show position at the timeline's left edge.
    pub arrangement_scroll: f64,
    /// Rows scrolled off the top of the timeline, in pixels. A show with more
    /// channels than fit has to stay reachable.
    pub arrangement_scroll_y: f32,
    /// Whether timeline edits round to whole frames at the ruler's rate.
    pub arrangement_snap: bool,
    /// The stretch of show being worked on, if one is marked. Looping is one
    /// thing done to it rather than the thing it is, so it outlives the loop it
    /// sets. See /spec/arrangement.md § The focus area.
    pub arrangement_focus: Option<FocusRange>,
    /// Whether the library panel (left sidebar) is open
    pub library_panel_open: bool,
    /// Whether the right panel (master output sidebar) is open
    pub right_panel_open: bool,
    /// Whether the 3D dome preview is open in the stage editor
    pub dome_preview_open: bool,
    /// Whether the stage editor is in 3D Dome mode (vs 2D Polygon mode)
    pub dome_mode_active: bool,
    /// Active dome preset
    pub dome_preset: DomePreset,
    /// Active dome geometry (radius, truncation, tilt)
    pub dome_geometry: DomeGeometry,
    /// Camera detection mode state
    pub camera_detect_mode: CameraDetectMode,
    /// Which panels the window shows, and where.
    pub screen: crate::usecases::ui::screen_layout::ScreenLayout,
    /// Channels folded to a strip in the Inputs panel, by UUID.
    pub collapsed_channels: Vec<String>,
}

impl Default for UILayoutState {
    fn default() -> Self {
        Self {
            selected_deck: None,
            selected_channel: None,
            selected_master: false,
            selected_sequence: None,
            selected_sequence_step: None,
            selected_macro: None,
            stage_editor_open: false,
            stage_editor_grid_size: 0.05,
            stage_editor_snap: true,
            arrangement_mode_open: false,
            // Ten seconds across a 400px area: wide enough to see a structure,
            // tight enough that a fade handle is grabbable.
            arrangement_pixels_per_second: 40.0,
            arrangement_scroll: 0.0,
            arrangement_scroll_y: 0.0,
            // On by default: a show cut to picture wants frame-aligned edits,
            // and a music-led one can turn it off in one click.
            arrangement_snap: true,
            arrangement_focus: None,
            library_panel_open: true,
            right_panel_open: true,
            dome_preview_open: false,
            dome_mode_active: false,
            dome_preset: DomePreset::Quad,
            dome_geometry: DomeGeometry::default(),
            camera_detect_mode: CameraDetectMode::Off,
            screen: crate::usecases::ui::screen_layout::ScreenLayout::default(),
            collapsed_channels: Vec::new(),
        }
    }
}

/// The channels by UUID, front to back as the mixer holds them, each with
/// its decks' UUIDs: where a selection held by UUID is found again (see
/// [`UILayoutState::reanchor`]).
#[derive(Clone, Debug, Default)]
pub struct ChannelOrder(Vec<(String, Vec<String>)>);

impl ChannelOrder {
    pub fn of(channels: &[crate::channel::Channel]) -> Self {
        Self(
            channels
                .iter()
                .map(|ch| {
                    let decks = ch.decks.iter().map(|s| s.deck.uuid().to_string());
                    (ch.uuid().to_string(), decks.collect())
                })
                .collect(),
        )
    }

    fn channel_at(&self, ch: usize) -> Option<&str> {
        self.0.get(ch).map(|(uuid, _)| uuid.as_str())
    }

    fn deck_at(&self, ch: usize, deck: usize) -> Option<&str> {
        self.0.get(ch)?.1.get(deck).map(String::as_str)
    }

    fn channel(&self, uuid: &str) -> Option<usize> {
        self.0.iter().position(|(u, _)| u == uuid)
    }

    /// Where the deck is: in its channel, else in whichever channel holds
    /// it now (moved to another).
    fn deck(&self, channel: &str, deck: &str) -> Option<(usize, usize)> {
        let in_channel = |ch: usize| {
            let at = self.0[ch].1.iter().position(|d| d == deck)?;
            Some((ch, at))
        };
        self.channel(channel)
            .and_then(in_channel)
            .or_else(|| (0..self.0.len()).find_map(in_channel))
    }
}

/// What the selection is on, by UUID (see [`UILayoutState::anchor`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionAnchor {
    channel: Option<String>,
    deck: Option<(String, String)>,
}

/// Where a channel moved before another (or to the end) goes: its position
/// now and after, in `order`. `None` when it is not there.
pub fn move_target(order: &[String], uuid: &str, before: Option<&str>) -> Option<(usize, usize)> {
    let from = order.iter().position(|u| u == uuid)?;
    let rest: Vec<&String> = order.iter().filter(|u| *u != uuid).collect();
    let to = before
        .filter(|b| *b != uuid)
        .and_then(|b| rest.iter().position(|u| *u == b))
        .unwrap_or(if before == Some(uuid) {
            from
        } else {
            rest.len()
        });
    Some((from, to))
}

/// Camera detection mode state machine.
///
/// Off → Live (camera feed) → Preview (frozen frame with contour selection) → Off
#[derive(Debug, Clone, Default)]
pub enum CameraDetectMode {
    #[default]
    Off,
    Live {
        camera_id: CameraId,
        params: DetectionParams,
    },
    Preview {
        camera_id: CameraId,
        contours: Vec<DetectedContour>,
        selected: Vec<bool>,
    },
}

/// Actions emitted by the camera detection UI.
#[derive(Debug, Clone)]
pub enum CameraDetectAction {
    Enter { camera_id: CameraId },
    Exit,
    UpdateParams(DetectionParams),
    Capture,
    ToggleContour(usize),
    SelectAll(bool),
    Accept,
}

impl UILayoutState {
    /// Apply selection actions from `UIActions`.
    pub fn apply_selections(&mut self, ui_actions: &UIActions) {
        let session = &ui_actions.session;
        if let Some(sel) = session.select_deck {
            self.selected_deck = Some(sel);
            self.selected_channel = None;
            self.selected_master = false;
            self.selected_sequence = None;
            self.selected_sequence_step = None;
            self.selected_macro = None;
        }
        if let Some(ch) = session.select_channel {
            self.selected_channel = Some(ch);
            self.selected_deck = None;
            self.selected_master = false;
            self.selected_sequence = None;
            self.selected_sequence_step = None;
            self.selected_macro = None;
        }
        if session.select_master {
            self.selected_master = true;
            self.selected_deck = None;
            self.selected_channel = None;
            self.selected_sequence = None;
            self.selected_sequence_step = None;
            self.selected_macro = None;
        }
        if let Some(seq) = session.select_sequence {
            self.selected_sequence = Some(seq);
            self.selected_sequence_step = None;
            self.selected_deck = None;
            self.selected_channel = None;
            self.selected_master = false;
            self.selected_macro = None;
        }
        if let Some(step) = session.select_sequence_step {
            self.selected_sequence_step = Some(step);
            // Ensure sequence is also selected
            self.selected_sequence = Some(step.0);
        }
        if let Some(uuid) = &session.select_macro {
            self.selected_macro = Some(uuid.clone());
            self.selected_deck = None;
            self.selected_channel = None;
            self.selected_master = false;
            self.selected_sequence = None;
            self.selected_sequence_step = None;
        }
        if session.deselect_macro {
            self.selected_macro = None;
        }
        if session.toggle_stage_editor {
            self.stage_editor_open = !self.stage_editor_open;
        }
        if session.toggle_arrangement_mode {
            self.arrangement_mode_open = !self.arrangement_mode_open;
        }
        if let Some(pps) = session.set_arrangement_zoom {
            self.arrangement_pixels_per_second =
                pps.clamp(MIN_PIXELS_PER_SECOND, MAX_PIXELS_PER_SECOND);
        }
        if let Some(scroll) = session.set_arrangement_scroll {
            self.arrangement_scroll = scroll.max(0.0);
        }
        if let Some(scroll) = session.set_arrangement_scroll_y {
            // The panel clamps against its own height, which only it knows.
            self.arrangement_scroll_y = scroll.max(0.0);
        }
        if session.toggle_arrangement_snap {
            self.arrangement_snap = !self.arrangement_snap;
        }
        if let Some(range) = session.set_arrangement_focus {
            self.arrangement_focus = Some(range);
        }
        if session.clear_arrangement_focus {
            self.arrangement_focus = None;
        }
        if let Some(size) = session.set_grid_size {
            self.stage_editor_grid_size = size;
        }
        if session.toggle_snap {
            self.stage_editor_snap = !self.stage_editor_snap;
        }
        if session.toggle_library_panel {
            self.library_panel_open = !self.library_panel_open;
        }
        if session.toggle_right_panel {
            self.right_panel_open = !self.right_panel_open;
        }
        if let Some(uuid) = &session.toggle_collapsed_channel {
            if let Some(at) = self.collapsed_channels.iter().position(|u| u == uuid) {
                self.collapsed_channels.remove(at);
            } else {
                self.collapsed_channels.push(uuid.clone());
            }
        }
        if session.toggle_dome_preview {
            self.dome_preview_open = !self.dome_preview_open;
        }
        // Dome mode actions
        for action in &session.dome_actions {
            match action {
                DomeAction::SetMode(active) => {
                    self.dome_mode_active = *active;
                    // When entering dome mode, also open dome preview
                    if *active {
                        self.dome_preview_open = true;
                    }
                }
                DomeAction::SetPreset(preset) => self.dome_preset = *preset,
                DomeAction::SetRadius(r) => self.dome_geometry.radius = *r,
                DomeAction::SetTruncation(deg) => self.dome_geometry.truncation_degrees = *deg,
                DomeAction::SetTilt(deg) => self.dome_geometry.tilt_degrees = *deg,
                DomeAction::SetContentAzimuth(deg) => {
                    self.dome_geometry.content_azimuth_degrees = *deg;
                }
                DomeAction::SetContentElevation(deg) => {
                    self.dome_geometry.content_elevation_degrees = *deg;
                }
                DomeAction::SetContentRoll(deg) => self.dome_geometry.content_roll_degrees = *deg,
                DomeAction::RotateCamera { .. }
                | DomeAction::ZoomCamera { .. }
                | DomeAction::ResetCamera => {
                    // Camera actions are handled by the runner, not layout state
                }
            }
        }
    }

    /// Channels to force-render for preview, derived from the current selection.
    ///
    /// Selecting a deck or a channel cues that channel so its off-air preview
    /// updates live (see /spec/channel-preview.md). Master or no selection cues
    /// nothing. Returned as a set (0 or 1 today) to leave room for multi-cue.
    pub fn preview_channels(&self) -> Vec<usize> {
        if let Some((ch, _)) = self.selected_deck {
            vec![ch]
        } else if let Some(ch) = self.selected_channel {
            vec![ch]
        } else {
            Vec::new()
        }
    }

    /// Keep the selection on the same channels after one moves from `from`
    /// to `to`: selections are held by position.
    pub fn fixup_channel_move(&mut self, from: usize, to: usize) {
        let moved = |i: usize| {
            if i == from {
                to
            } else if from < to && (from + 1..=to).contains(&i) {
                i - 1
            } else if to < from && (to..from).contains(&i) {
                i + 1
            } else {
                i
            }
        };
        if let Some((ch, deck)) = self.selected_deck {
            self.selected_deck = Some((moved(ch), deck));
        }
        if let Some(ch) = self.selected_channel {
            self.selected_channel = Some(moved(ch));
        }
    }

    /// What the selection is on, by UUID: the channel, or the deck, at the
    /// selected positions in `order`.
    pub fn anchor(&self, order: &ChannelOrder) -> SelectionAnchor {
        SelectionAnchor {
            channel: self
                .selected_channel
                .and_then(|ch| order.channel_at(ch))
                .map(str::to_string),
            deck: self.selected_deck.and_then(|(ch, deck)| {
                Some((
                    order.channel_at(ch)?.to_string(),
                    order.deck_at(ch, deck)?.to_string(),
                ))
            }),
        }
    }

    /// Put the selection back on what `anchor` names, wherever that is in
    /// `order` now. Selections are held by position, so after an undo, a
    /// redo or the API reordering channels they would land on whichever
    /// channel took that place — and the next take would send that one.
    /// What is gone is no longer selected; a selection `anchor` does not
    /// name, or one cleared since, is left as it is.
    pub fn reanchor(&mut self, anchor: &SelectionAnchor, order: &ChannelOrder) {
        if let (Some(uuid), Some(_)) = (&anchor.channel, self.selected_channel) {
            self.selected_channel = order.channel(uuid);
        }
        if let (Some((channel, deck)), Some(_)) = (&anchor.deck, self.selected_deck) {
            self.selected_deck = order.deck(channel, deck);
        }
    }

    /// Fix up selection indices after a channel is removed.
    pub fn fixup_channel_removal(&mut self, removed_ch: usize) {
        if let Some((sel_ch, _)) = self.selected_deck {
            if sel_ch == removed_ch {
                self.selected_deck = None;
            } else if sel_ch > removed_ch {
                // sel_ch > removed_ch, so selected_deck must be Some (we matched it above)
                if let Some((_, deck_idx)) = self.selected_deck {
                    self.selected_deck = Some((sel_ch - 1, deck_idx));
                }
            }
        }
        if let Some(sel_ch) = self.selected_channel {
            if sel_ch == removed_ch {
                self.selected_channel = None;
            } else if sel_ch > removed_ch {
                self.selected_channel = Some(sel_ch - 1);
            }
        }
    }
}

#[cfg(test)]
mod channel_move_tests {
    use super::*;

    fn order() -> Vec<String> {
        ["a", "b", "c", "d"]
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    #[test]
    fn a_channel_moves_before_another_or_to_the_end() {
        assert_eq!(move_target(&order(), "d", Some("a")), Some((3, 0)));
        assert_eq!(move_target(&order(), "a", Some("c")), Some((0, 1)));
        assert_eq!(move_target(&order(), "b", None), Some((1, 3)));
        assert_eq!(move_target(&order(), "b", Some("b")), Some((1, 1)));
        assert_eq!(move_target(&order(), "x", None), None);
    }

    /// Selections are held by position, so a move carries them along.
    #[test]
    fn the_selection_follows_the_channels_it_was_on() {
        let mut layout = UILayoutState {
            selected_channel: Some(3),
            ..Default::default()
        };
        layout.fixup_channel_move(3, 0);
        assert_eq!(layout.selected_channel, Some(0), "the moved one");
        let mut layout = UILayoutState {
            selected_deck: Some((1, 2)),
            ..Default::default()
        };
        layout.fixup_channel_move(3, 0);
        assert_eq!(layout.selected_deck, Some((2, 2)), "pushed along");
        layout.fixup_channel_move(0, 3);
        assert_eq!(layout.selected_deck, Some((1, 2)), "and back");
    }

    fn channels(of: &[(&str, &[&str])]) -> ChannelOrder {
        ChannelOrder(
            of.iter()
                .map(|(ch, decks)| {
                    let decks = decks.iter().map(|d| (*d).to_string()).collect();
                    ((*ch).to_string(), decks)
                })
                .collect(),
        )
    }

    /// Undo, redo or the API move channels under a selection held by
    /// position: it is found again on the channel and deck it was on.
    #[test]
    fn the_selection_stays_on_what_it_was_on_wherever_that_goes() {
        let before = channels(&[("a", &["a1"]), ("b", &["b1", "b2"]), ("c", &[])]);
        let after = channels(&[("a", &["a1"]), ("c", &[]), ("b", &["b2", "b1"])]);
        let mut layout = UILayoutState {
            selected_channel: Some(1),
            ..Default::default()
        };
        let anchor = layout.anchor(&before);
        layout.reanchor(&anchor, &after);
        assert_eq!(layout.selected_channel, Some(2), "still b");

        let mut layout = UILayoutState {
            selected_deck: Some((1, 0)),
            ..Default::default()
        };
        let anchor = layout.anchor(&before);
        layout.reanchor(&anchor, &after);
        assert_eq!(layout.selected_deck, Some((2, 1)), "still b1");

        let moved = channels(&[("a", &["a1", "b1"]), ("b", &["b2"]), ("c", &[])]);
        let mut layout = UILayoutState {
            selected_deck: Some((1, 0)),
            ..Default::default()
        };
        let anchor = layout.anchor(&before);
        layout.reanchor(&anchor, &moved);
        assert_eq!(
            layout.selected_deck,
            Some((0, 1)),
            "b1, moved into a, still"
        );
    }

    /// A channel gone takes its selection with it; one never named, or
    /// cleared since, is left alone.
    #[test]
    fn a_selection_gone_is_cleared_and_one_cleared_stays_so() {
        let before = channels(&[("a", &[]), ("b", &["b1"])]);
        let gone = channels(&[("a", &[])]);
        let mut layout = UILayoutState {
            selected_channel: Some(1),
            selected_deck: Some((1, 0)),
            ..Default::default()
        };
        let anchor = layout.anchor(&before);
        layout.reanchor(&anchor, &gone);
        assert_eq!(
            (layout.selected_channel, layout.selected_deck),
            (None, None)
        );

        let mut layout = UILayoutState {
            selected_channel: Some(5),
            ..Default::default()
        };
        let anchor = layout.anchor(&before);
        layout.reanchor(&anchor, &gone);
        assert_eq!(layout.selected_channel, Some(5), "not named: left");

        let mut layout = UILayoutState {
            selected_channel: Some(1),
            ..Default::default()
        };
        let anchor = layout.anchor(&before);
        layout.selected_channel = None;
        layout.reanchor(&anchor, &before);
        assert_eq!(layout.selected_channel, None, "cleared since: stays so");
    }

    #[test]
    fn a_fold_toggles() {
        let mut layout = UILayoutState::default();
        let mut actions = crate::usecases::ui::UIActions::new();
        actions.session.toggle_collapsed_channel = Some("ch1".into());
        layout.apply_selections(&actions);
        assert_eq!(layout.collapsed_channels, vec!["ch1".to_string()]);
        layout.apply_selections(&actions);
        assert!(layout.collapsed_channels.is_empty());
    }
}

#[cfg(test)]
mod preview_channel_tests {
    use super::*;

    #[test]
    fn selected_deck_cues_its_channel() {
        let layout = UILayoutState {
            selected_deck: Some((1, 3)),
            ..Default::default()
        };
        assert_eq!(layout.preview_channels(), vec![1]);
    }

    #[test]
    fn selected_channel_cues_itself() {
        let layout = UILayoutState {
            selected_channel: Some(2),
            ..Default::default()
        };
        assert_eq!(layout.preview_channels(), vec![2]);
    }

    #[test]
    fn selected_master_cues_nothing() {
        let layout = UILayoutState {
            selected_master: true,
            ..Default::default()
        };
        assert!(layout.preview_channels().is_empty());
    }

    #[test]
    fn no_selection_cues_nothing() {
        let layout = UILayoutState::default();
        assert!(layout.preview_channels().is_empty());
    }

    #[test]
    fn deck_takes_precedence_over_channel() {
        // apply_selections keeps these mutually exclusive, but the derivation
        // must be deterministic even if both happen to be set.
        let layout = UILayoutState {
            selected_deck: Some((0, 0)),
            selected_channel: Some(1),
            ..Default::default()
        };
        assert_eq!(layout.preview_channels(), vec![0]);
    }
}
