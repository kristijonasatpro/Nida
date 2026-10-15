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
        let f = (secs / duration).clamp(0.0, 1.0) as f32;
        rect.left() + f * rect.width()
    };
    let out = if vp.out_point > 0.0 {
        vp.out_point
    } else {
        vp.duration
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, TRACK);
    let kept = egui::Rect::from_x_y_ranges(x_of(vp.in_point)..=x_of(out), rect.y_range());
    painter.rect_filled(kept, 3.0, KEPT);
    let played = egui::Rect::from_x_y_ranges(
        x_of(vp.in_point)..=x_of(vp.position.clamp(vp.in_point, out)),
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
