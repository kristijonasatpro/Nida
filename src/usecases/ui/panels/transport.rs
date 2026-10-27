//! A clip's transport, as vMix shows one under Preview: the playhead on a bar
//! with the in and out points marked, play/pause, back and forward, and In
//! and Out set where the playhead is. Works for sound files as for video.
//!
//! Click or drag the bar to move the playhead. |◀ goes to the in-point;
//! ◀◀ and ▶▶ jump [`JUMP_SECS`] back and forward.

use super::super::{ChannelUIInfo, DeckUIInfo, UIActions, UIData, VideoPlaybackUI};
use crate::engine::EngineCommand;
use crate::video::LoopMode;

/// Room the transport takes under a picture.
pub(super) const HEIGHT: f32 = 50.0;
/// How far ◀◀ and ▶▶ jump.
const JUMP_SECS: f64 = 5.0;
const BAR_HEIGHT: f32 = 16.0;
const BUTTON: egui::Vec2 = egui::vec2(30.0, 24.0);
const PLAYED: egui::Color32 = egui::Color32::from_rgb(70, 130, 200);
const KEPT: egui::Color32 = egui::Color32::from_rgb(52, 56, 70);
const TRACK: egui::Color32 = egui::Color32::from_rgb(28, 30, 38);
const MARK: egui::Color32 = egui::Color32::from_rgb(240, 200, 80);

/// A clip playing, on its tile and chips.
pub(super) const PLAYING_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 150, 60);

/// "1:23": a time on a tile, to the second.
fn short_clock(secs: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = secs.max(0.0).round() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// The input's top clip that shows — not hidden, not see-through, not
/// left out by a solo, not an auto-transition's waiting its turn or done —
/// or, with none showing, its top clip: the one its picture is of.
pub(super) fn top_clip(ch: &ChannelUIInfo) -> Option<(&DeckUIInfo, &VideoPlaybackUI)> {
    use crate::channel::DeckTransitionPhase;
    let any_solo = ch.decks.iter().any(|d| d.solo);
    let waiting = |d: &DeckUIInfo| {
        d.auto_transition.as_ref().is_some_and(|at| {
            at.enabled
                && matches!(
                    at.phase,
                    DeckTransitionPhase::Inactive | DeckTransitionPhase::Done
                )
        })
    };
    let clips = || {
        ch.decks
            .iter()
            .rev()
            .filter_map(|d| d.video_playback.as_ref().map(|vp| (d, vp)))
    };
    clips()
        .find(|(d, _)| !d.mute && d.opacity > 0.0 && (!any_solo || d.solo) && !waiting(d))
        .or_else(|| clips().next())
}

/// A clip's own controls on its channel's tile, as a playlist has: back to
/// the start, play or pause, and the time gone and left, with the playhead
/// along the foot of the picture — a clip started, stopped and watched
/// without putting it on Preview. The clip is the one the tile shows (see
/// [`top_clip`]), named when the input holds others; `rows_below` are a
/// timer's or a presentation's controls under its row. A sound, which has
/// no picture, says what it is above them.
pub(super) fn overlay_on_picture(
    ui: &mut egui::Ui,
    picture: egui::Rect,
    ch: &ChannelUIInfo,
    rows_below: usize,
    actions: &mut UIActions,
) {
    // A playlist's own controls are there instead.
    if ch.decks.iter().any(|d| d.playlist.is_some()) {
        return;
    }
    let Some((deck, vp)) = top_clip(ch) else {
        return;
    };
    let named = ch
        .decks
        .iter()
        .filter(|d| d.video_playback.is_some())
        .count()
        > 1;
    let start = vp.in_point;
    let end = if vp.out_point > 0.0 {
        vp.out_point
    } else {
        vp.duration
    };
    if end > start {
        #[allow(clippy::cast_possible_truncation)]
        let played = ((vp.position - start) / (end - start)).clamp(0.0, 1.0) as f32;
        let bar = egui::Rect::from_min_max(
            egui::pos2(picture.left(), picture.bottom() - 3.0),
            picture.right_bottom(),
        );
        ui.painter()
            .rect_filled(bar, 0.0, egui::Color32::from_black_alpha(150));
        let mut done = bar;
        done.set_right(bar.left() + bar.width() * played);
        let color = if vp.playing {
            PLAYING_GREEN
        } else {
            egui::Color32::from_gray(200)
        };
        ui.painter().rect_filled(done, 0.0, color);
    }
    // Its row over the rows below it, if the picture has room for it.
    #[allow(clippy::cast_precision_loss)]
    let raised = rows_below as f32 * (BUTTON.y + 4.0);
    let has_row = picture.height() >= raised + BUTTON.y + 7.0 + 20.0;
    let row_top = if has_row {
        picture.bottom() - BUTTON.y - 7.0 - raised
    } else {
        picture.bottom() - 3.0 - raised
    };
    if vp.sound_only {
        sound_name(ui, picture, row_top, &deck.name);
    }
    if !has_row {
        return;
    }
    let row = egui::Rect::from_min_size(
        egui::pos2(picture.left() + 4.0, row_top),
        egui::vec2(picture.width() - 8.0, BUTTON.y),
    );
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row_ui.set_clip_rect(picture.intersect(ui.clip_rect()));
    row_ui.spacing_mut().item_spacing.x = 3.0;
    let button = |ui: &mut egui::Ui, glyph: &str, hover: &str, lit: bool| {
        ui.add(
            egui::Button::new(
                egui::RichText::new(glyph)
                    .size(BUTTON.y * 0.6)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .min_size(BUTTON)
            .fill(if lit {
                PLAYING_GREEN
            } else {
                egui::Color32::from_black_alpha(170)
            }),
        )
        .on_hover_text(hover)
    };
    if button(&mut row_ui, "⏮", "Back to the start", false).clicked() {
        actions.commands.push(EngineCommand::VideoSeek {
            deck_uuid: deck.uuid.clone(),
            position_secs: start,
        });
    }
    let (glyph, hover) = if vp.playing {
        ("⏸", "Pause")
    } else {
        ("▶", "Play")
    };
    if button(&mut row_ui, glyph, hover, vp.playing).clicked() {
        actions.commands.push(EngineCommand::VideoTogglePlay {
            deck_uuid: deck.uuid.clone(),
        });
    }
    let name = if named {
        format!("{} · ", deck.name)
    } else {
        String::new()
    };
    row_ui.add(
        egui::Label::new(
            egui::RichText::new(format!(
                " {name}{} · -{} ",
                short_clock(vp.position - start),
                short_clock(end - vp.position)
            ))
            .small()
            .color(egui::Color32::WHITE)
            .background_color(egui::Color32::from_black_alpha(170)),
        )
        .selectable(false)
        .truncate(),
    );
}

/// A sound's ♪ and name, in the picture above `bottom` — the controls under
/// them — as much as there is room for.
fn sound_name(ui: &mut egui::Ui, picture: egui::Rect, bottom: f32, name: &str) {
    let free = egui::Rect::from_min_max(picture.min, egui::pos2(picture.right(), bottom - 2.0));
    if free.height() < 18.0 {
        return;
    }
    let mut name_at = free.center();
    if free.height() >= 40.0 {
        let size = (free.height() * 0.4).min(48.0);
        ui.painter_at(picture).text(
            free.center() - egui::vec2(0.0, 9.0),
            egui::Align2::CENTER_BOTTOM,
            "♪",
            egui::FontId::proportional(size),
            egui::Color32::from_gray(170),
        );
        name_at.y += 2.0;
    }
    // Not selectable: a click on it is the tile's.
    ui.put(
        egui::Rect::from_center_size(name_at, egui::vec2(picture.width() - 16.0, 18.0)),
        egui::Label::new(
            egui::RichText::new(format!("♪ {name}")).color(egui::Color32::from_gray(210)),
        )
        .selectable(false)
        .truncate(),
    );
}

/// The clip a channel's transport drives: the selected layer when it is a
/// clip in this channel, else the topmost clip.
pub(super) fn clip_of<'a>(
    data: &UIData,
    ch: &'a ChannelUIInfo,
) -> Option<(&'a DeckUIInfo, &'a VideoPlaybackUI)> {
    let selected = data
        .selected_deck
        .filter(|(c, _)| *c == ch.ch_idx)
        .and_then(|(_, d)| ch.decks.get(d));
    selected
        .into_iter()
        .chain(ch.decks.iter().rev())
        .find_map(|d| d.video_playback.as_ref().map(|vp| (d, vp)))
}

/// "01:23.4", or "1:02:03.4" past an hour.
fn clock(secs: f64) -> String {
    let secs = secs.max(0.0);
    let tenths = (secs * 10.0).floor() as u64;
    let (h, m, s, t) = (
        tenths / 36_000,
        tenths / 600 % 60,
        tenths / 10 % 60,
        tenths % 10,
    );
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{t}")
    } else {
        format!("{m:02}:{s:02}.{t}")
    }
}

/// The transport for `clip`, `width` wide; a word of explanation when the
/// input has none.
pub(super) fn render_transport(
    ui: &mut egui::Ui,
    clip: Option<(&DeckUIInfo, &VideoPlaybackUI)>,
    several_layers: bool,
    width: f32,
    actions: &mut UIActions,
) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, HEIGHT),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_width(width);
            let Some((deck, vp)) = clip else {
                ui.add_space(8.0);
                ui.label(
                    egui::RichText::new("No video or sound in this input to play")
                        .small()
                        .weak(),
                );
                return;
            };
            let seek = |actions: &mut UIActions, secs: f64| {
                actions.commands.push(EngineCommand::VideoSeek {
                    deck_uuid: deck.uuid.clone(),
                    position_secs: secs.clamp(0.0, vp.duration.max(0.0)),
                });
            };
            if let Some(secs) = bar(ui, vp, width) {
                seek(actions, secs);
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                if button(ui, "⏮", "To the in-point", "Back to the in-point") {
                    seek(actions, vp.in_point);
                }
                if button(ui, "⏪", "Back 5 seconds", "Back 5 seconds") {
                    seek(actions, vp.position - JUMP_SECS);
                }
                let (glyph, name) = if vp.playing {
                    ("⏸", "Pause")
                } else {
                    ("▶", "Play")
                };
                if button(ui, glyph, name, name) {
                    actions.commands.push(EngineCommand::VideoTogglePlay {
                        deck_uuid: deck.uuid.clone(),
                    });
                }
                if button(ui, "⏩", "Forward 5 seconds", "Forward 5 seconds") {
                    seek(actions, vp.position + JUMP_SECS);
                }
                ui.add_space(6.0);
                let out = if vp.out_point > 0.0 {
                    vp.out_point
                } else {
                    vp.duration
                };
                ui.label(egui::RichText::new(clock(vp.position)).monospace())
                    .on_hover_text("Where the playhead is");
                ui.label(
                    egui::RichText::new(format!("-{}", clock(out - vp.position)))
                        .monospace()
                        .weak(),
                )
                .on_hover_text("Left to play, to the out-point");
                ui.add_space(6.0);
                if ui
                    .small_button("[ In")
                    .on_hover_text("Start here: set the in-point at the playhead")
                    .clicked()
                {
                    actions.commands.push(EngineCommand::VideoSetInPoint {
                        deck_uuid: deck.uuid.clone(),
                        secs: vp.position,
                    });
                }
                if ui
                    .small_button("Out ]")
                    .on_hover_text("End here: set the out-point at the playhead")
                    .clicked()
                {
                    actions.commands.push(EngineCommand::VideoSetOutPoint {
                        deck_uuid: deck.uuid.clone(),
                        secs: vp.position,
                    });
                }
                let trimmed = vp.in_point > 0.0 || vp.out_point > 0.0;
                if ui
                    .add_enabled(trimmed, egui::Button::new("×").small())
                    .on_hover_text("Clear the in and out points: play all of it")
                    .clicked()
                {
                    actions.commands.push(EngineCommand::VideoClearInOutPoints {
                        deck_uuid: deck.uuid.clone(),
                    });
                }
                let mut looping = vp.loop_mode == LoopMode::Loop;
                if ui
                    .checkbox(&mut looping, "Loop")
                    .on_hover_text("Play it round and round; off, it stops at the end")
                    .changed()
                {
                    actions.commands.push(EngineCommand::VideoSetLoopMode {
                        deck_uuid: deck.uuid.clone(),
                        mode: if looping {
                            LoopMode::Loop
                        } else {
                            LoopMode::HoldLast
                        },
                    });
                }
                if several_layers {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&deck.name).small().weak()).truncate(),
                    );
                }
            });
        },
    );
}

/// A transport button: a glyph, named for screen readers and tests.
fn button(ui: &mut egui::Ui, glyph: &str, name: &str, hover: &str) -> bool {
    let (rect, response) = ui.allocate_exact_size(BUTTON, egui::Sense::click());
    let visuals = ui.style().interact(&response);
    ui.painter().rect_filled(rect, 3.0, visuals.bg_fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(15.0),
        visuals.text_color(),
    );
    let name = name.to_string();
    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &name));
    response.on_hover_text(hover).clicked()
}

/// The bar: the whole clip, the part between in and out lighter, what has
/// played of it filled, the in and out points marked, the playhead a white
/// line. Returns where it was clicked or dragged to, in seconds.
fn bar(ui: &mut egui::Ui, vp: &VideoPlaybackUI, width: f32) -> Option<f64> {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, BAR_HEIGHT), egui::Sense::click_and_drag());
    let duration = vp.duration.max(1e-6);
    let x_of = |secs: f64| {
        let f = if secs.is_finite() {
            (secs / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        rect.left() + f * rect.width()
    };
    // In and out the wrong way round — put so by a learned control or the
    // API, or kept so by an older show — nothing is kept between them.
    let start = if vp.in_point.is_finite() {
        vp.in_point
    } else {
        0.0
    };
    let out = if vp.out_point > 0.0 {
        vp.out_point
    } else {
        vp.duration
    }
    .max(start);
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, TRACK);
    let kept = egui::Rect::from_x_y_ranges(x_of(start)..=x_of(out), rect.y_range());
    painter.rect_filled(kept, 3.0, KEPT);
    let played = egui::Rect::from_x_y_ranges(
        x_of(start)..=x_of(vp.position.clamp(start, out)),
        rect.shrink2(egui::vec2(0.0, 4.0)).y_range(),
    );
    painter.rect_filled(played, 2.0, PLAYED);
    for (at, inward) in [(vp.in_point, 1.0), (out, -1.0)] {
        if at > 0.0 && at < vp.duration {
            let x = x_of(at);
            let stroke = egui::Stroke::new(2.0, MARK);
            painter.line_segment(
                [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
                stroke,
            );
            for y in [rect.top(), rect.bottom()] {
                painter.line_segment([egui::pos2(x, y), egui::pos2(x + 4.0 * inward, y)], stroke);
            }
        }
    }
    let x = x_of(vp.position);
    painter.line_segment(
        [
            egui::pos2(x, rect.top() - 2.0),
            egui::pos2(x, rect.bottom() + 2.0),
        ],
        egui::Stroke::new(2.0, egui::Color32::WHITE),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, "Playhead"));
    let response = response.on_hover_text("Click or drag to move the playhead");
    if response.clicked() || response.dragged() {
        let at = response.interact_pointer_pos()?;
        let f = f64::from(((at.x - rect.left()) / rect.width()).clamp(0.0, 1.0));
        return Some(f * vp.duration);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_read_as_minutes_seconds_and_tenths() {
        assert_eq!(clock(0.0), "00:00.0");
        assert_eq!(clock(83.46), "01:23.4");
        assert_eq!(clock(3723.4), "1:02:03.4");
        assert_eq!(clock(-2.0), "00:00.0");
    }
}

#[cfg(test)]
mod tile_tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn clip(sound_only: bool) -> VideoPlaybackUI {
        VideoPlaybackUI {
            playing: false,
            position: 20.0,
            duration: 60.0,
            speed: 1.0,
            effective_speed: 1.0,
            position_offset: 0.0,
            loop_mode: LoopMode::Loop,
            in_point: 0.0,
            out_point: 0.0,
            frame_rate: 25.0,
            transport_sync: crate::video::DeckTransportSync::default(),
            hardware_decode: false,
            auto: crate::engine::value::video::AutoPlayback::default(),
            sound_only,
        }
    }

    /// The clip a tile shows is the top one not hidden, see-through or left
    /// out by a solo — the top one when none shows.
    #[test]
    fn the_top_clip_is_the_one_that_shows() {
        let mut ch = UIData::test_fixture().channels[1].clone();
        ch.decks[0].video_playback = Some(clip(false));
        ch.decks[1].video_playback = Some(clip(false));
        let (lower, upper) = (ch.decks[0].uuid.clone(), ch.decks[1].uuid.clone());
        let top = |ch: &ChannelUIInfo| top_clip(ch).map(|(d, _)| d.uuid.clone());
        assert_eq!(top(&ch), Some(upper.clone()));
        ch.decks[1].mute = true;
        assert_eq!(top(&ch), Some(lower.clone()), "the upper one hidden");
        ch.decks[1].mute = false;
        ch.decks[1].opacity = 0.0;
        assert_eq!(top(&ch), Some(lower.clone()), "the upper one see-through");
        ch.decks[1].opacity = 1.0;
        ch.decks[0].solo = true;
        assert_eq!(top(&ch), Some(lower.clone()), "the lower one solo");
        ch.decks[0].mute = true;
        ch.decks[1].mute = true;
        assert_eq!(top(&ch), Some(upper.clone()), "none shows: the top one");
        ch.decks[0].solo = false;
        ch.decks[0].mute = false;
        ch.decks[1].mute = false;
        ch.decks[1].auto_transition = Some(super::super::super::AutoTransitionUI {
            enabled: true,
            trigger_is_clip_end: true,
            play_duration_value: 5.0,
            play_duration_is_beats: false,
            transition_duration_value: 1.0,
            transition_duration_is_beats: false,
            transition_shader_name: None,
            phase: crate::channel::DeckTransitionPhase::Inactive,
        });
        assert_eq!(top(&ch), Some(lower), "the upper one waits its turn");
    }

    /// In and out the wrong way round — an In put past the Out by a learned
    /// control or the API, or one from an older show — the bar is drawn all
    /// the same, the playhead where it is: it took Nida down.
    #[test]
    fn the_bar_draws_with_in_and_out_the_wrong_way_round() {
        let deck = UIData::test_fixture().channels[1].decks[0].clone();
        for (in_point, out_point, position) in [
            (40.0, 30.0, 35.0),
            (40.0, 30.0, 50.0),
            (30.0, 30.0, 10.0),
            (f64::NAN, 30.0, 10.0),
        ] {
            let mut vp = clip(false);
            vp.in_point = in_point;
            vp.out_point = out_point;
            vp.position = position;
            let deck = deck.clone();
            let mut harness = egui_kittest::Harness::new_ui(move |ui| {
                render_transport(ui, Some((&deck, &vp)), false, 400.0, &mut UIActions::new());
            });
            harness.run();
            harness.get_by_label("Playhead");
        }
    }

    /// A sound's name stays above the controls, raised over another row.
    #[test]
    fn a_sounds_name_stays_above_its_controls() {
        let mut ch = UIData::test_fixture().channels[1].clone();
        ch.decks[0].video_playback = Some(clip(true));
        let name = format!("♪ {}", ch.decks[0].name);
        let harness = egui_kittest::Harness::new_ui(move |ui| {
            let picture = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 120.0));
            overlay_on_picture(ui, picture, &ch, 1, &mut UIActions::new());
        });
        let label = harness.get_by_label(&name).rect();
        let play = harness.get_by_label("▶").rect();
        assert!(label.bottom() <= play.top(), "{label:?} over {play:?}");
    }

    /// Over a timer's or a presentation's row at the foot of a tile, the
    /// clip's row sits a row higher — not on it, where their buttons would
    /// take its clicks.
    #[test]
    fn a_clips_row_sits_over_the_rows_below_it() {
        let mut ch = UIData::test_fixture().channels[1].clone();
        ch.decks[0].video_playback = Some(clip(false));
        let play_top = |rows_below: usize| {
            let ch = ch.clone();
            let harness = egui_kittest::Harness::new_ui(move |ui| {
                let picture =
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 180.0));
                overlay_on_picture(ui, picture, &ch, rows_below, &mut UIActions::new());
            });
            harness.get_by_label("▶").rect().top()
        };
        let (alone, raised) = (play_top(0), play_top(1));
        assert!(
            raised <= alone - BUTTON.y,
            "a row higher: {raised} against {alone}"
        );
    }
}
