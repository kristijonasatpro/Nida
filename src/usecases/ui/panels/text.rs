//! A text input's controls in Input Settings: its lines — edited live, each
//! change on air at once — shown or hidden with a fade, and how they look.

use super::super::{DeckUIInfo, UIActions};
use crate::engine::value::text::{TextAlign, TextSettings, TextSnapshot, MAX_SIZE, MIN_SIZE};
use crate::engine::EngineCommand;

const WARN: egui::Color32 = egui::Color32::from_rgb(230, 170, 60);

fn set_settings(actions: &mut UIActions, deck_uuid: &str, settings: TextSettings) {
    actions.commands.push(EngineCommand::SetTextSettings {
        deck_uuid: deck_uuid.to_string(),
        settings,
    });
}

/// Its settings as an edit already made this frame left them, else as they
/// are: each send is the whole settings.
fn shown_settings(actions: &UIActions, deck_uuid: &str, text: &TextSnapshot) -> TextSettings {
    actions
        .commands
        .iter()
        .rev()
        .find_map(|command| match command {
            EngineCommand::SetTextSettings {
                deck_uuid: uuid,
                settings,
            } if uuid == deck_uuid => Some(settings.clone()),
            _ => None,
        })
        .unwrap_or_else(|| text.settings.clone())
}

/// Its controls in Input Settings.
pub(super) fn render_controls(
    ui: &mut egui::Ui,
    data: &super::super::UIData,
    deck: &DeckUIInfo,
    text: &TextSnapshot,
    actions: &mut UIActions,
) {
    ui.separator();
    // "Updating…" comes and goes with each redraw while typing: on the
    // heading's row, so the fields below never jump.
    ui.horizontal(|ui| {
        ui.strong("Text");
        if text.error.is_none() && text.pending {
            ui.weak("Updating…");
        }
    });
    if let Some(error) = &text.error {
        ui.colored_label(WARN, error);
    }
    lines(ui, deck, text, actions);
    shown(ui, deck, text, actions);
    // Where it comes on: Main, and buses — over what each shows.
    let before = shown_settings(actions, &deck.uuid, text);
    let mut settings = before.clone();
    super::lower_third::show_on_row(ui, data, &deck.uuid, &mut settings.targets, actions);
    if settings != before {
        set_settings(actions, &deck.uuid, settings);
    }
    egui::CollapsingHeader::new("Look")
        .id_salt(("text_look", &deck.uuid))
        .default_open(true)
        .show(ui, |ui| look(ui, deck, text, actions));
}

/// A field typed into ahead of the engine: what is typed, what the engine
/// was last seen to hold, and what was sent it since — so its copy a frame
/// behind is told from a change made elsewhere (an undo, the API), which
/// the typing gives way to.
#[derive(Clone, Default)]
pub(super) struct Typing {
    pub text: String,
    acked: String,
    pending: Vec<String>,
}

impl Typing {
    fn fresh(engine: &str) -> Self {
        Self {
            text: engine.to_string(),
            acked: engine.to_string(),
            pending: Vec::new(),
        }
    }

    /// The typing kept for field `id`, against the engine's copy `engine`;
    /// and whether it gave way to a change made elsewhere.
    pub(super) fn for_field(ctx: &egui::Context, id: egui::Id, engine: &str) -> (Self, bool) {
        let Some(mut typing) = ctx.data(|d| d.get_temp::<Self>(id)) else {
            return (Self::fresh(engine), false);
        };
        if let Some(at) = typing.pending.iter().position(|p| p == engine) {
            typing.acked = engine.to_string();
            typing.pending.drain(..=at);
            (typing, false)
        } else if engine == typing.acked {
            (typing, false)
        } else {
            (Self::fresh(engine), true)
        }
    }

    /// What was typed, sent.
    pub(super) fn sent(&mut self) {
        self.pending.push(self.text.clone());
    }

    /// Kept while the field has focus, else let go.
    pub(super) fn keep(self, ctx: &egui::Context, id: egui::Id, focused: bool) {
        if focused {
            ctx.data_mut(|d| d.insert_temp(id, self));
        } else {
            ctx.data_mut(|d| d.remove::<Self>(id));
        }
    }
}

/// The lines, typed: what is typed goes on air at once; the field shows the
/// typing, not the engine's copy a frame behind — unless the text changed
/// elsewhere meanwhile (an undo), which it gives way to. A run of typing is
/// one undo step, from its first letter while the field has focus.
fn lines(ui: &mut egui::Ui, deck: &DeckUIInfo, text: &TextSnapshot, actions: &mut UIActions) {
    let id = egui::Id::new(("text_typing", &deck.uuid));
    let (mut typing, gave_way) = Typing::for_field(ui.ctx(), id, &text.settings.text);
    let response = ui.add(
        egui::TextEdit::multiline(&mut typing.text)
            .id(id.with("field"))
            .hint_text("Lines of text — Enter starts a new one")
            .desired_rows(3)
            .desired_width(ui.available_width().clamp(160.0, 420.0)),
    );
    let changed = response.changed();
    if changed {
        let mut settings = shown_settings(actions, &deck.uuid, text);
        settings.text.clone_from(&typing.text);
        set_settings(actions, &deck.uuid, settings);
        typing.sent();
    }
    let focused = response.has_focus();
    typing.keep(ui.ctx(), id, focused);
    let step_id = id.with("step");
    let was = !gave_way
        && ui
            .ctx()
            .data(|d| d.get_temp::<bool>(step_id))
            .unwrap_or(false);
    let stepping = focused && (changed || was);
    ui.ctx().data_mut(|d| d.insert_temp(step_id, stepping));
    if stepping {
        actions.session.gesture_active = true;
    }
}

fn shown(ui: &mut egui::Ui, deck: &DeckUIInfo, text: &TextSnapshot, actions: &mut UIActions) {
    let before = shown_settings(actions, &deck.uuid, text);
    let mut settings = before.clone();
    let held = ui.horizontal(|ui| {
        ui.checkbox(&mut settings.visible, "Shown")
            .on_hover_text("Off: the input is clear — faded out, and in again when on");
        ui.label("fade");
        let fade = ui
            .add(
                egui::DragValue::new(&mut settings.fade_secs)
                    .range(0.0..=5.0)
                    .speed(0.02)
                    .suffix(" s"),
            )
            .on_hover_text("How long it takes to fade in or out; 0 cuts");
        fade.dragged()
    });
    // A drag is one undo step: from its first change while held.
    let step_id = egui::Id::new(("text_fade_step", &deck.uuid));
    let was = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(step_id))
        .unwrap_or(false);
    let stepping =
        ui.input(|i| i.pointer.any_down()) && (was || (held.inner && settings != before));
    ui.ctx().data_mut(|d| d.insert_temp(step_id, stepping));
    if stepping {
        actions.session.gesture_active = true;
    }
    if settings != before {
        set_settings(actions, &deck.uuid, settings);
    }
}

fn look(ui: &mut egui::Ui, deck: &DeckUIInfo, text: &TextSnapshot, actions: &mut UIActions) {
    let before = shown_settings(actions, &deck.uuid, text);
    let mut settings = before.clone();
    let look = &mut settings.look;
    // One of its sliders or numbers being dragged; a colour changed.
    let mut held = false;
    let mut recoloured = false;
    let percent = |v: f64, _| format!("{:.0}%", v * 100.0);
    egui::Grid::new(("text_look_grid", &deck.uuid))
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Font");
            ui.horizontal(|ui| {
                super::timer::font_picker(ui, &mut look.font);
                ui.checkbox(&mut look.bold, "Bold");
                ui.checkbox(&mut look.italic, "Italic");
                if !super::lower_third::has_lithuanian(ui.ctx(), &look.font) {
                    ui.label(egui::RichText::new("⚠").color(WARN)).on_hover_text(
                        "This font lacks some Lithuanian letters (ąčęėįšųūž): they would show as boxes",
                    );
                }
            });
            ui.end_row();
            ui.label("Size");
            held |= ui
                .add(
                    egui::Slider::new(&mut look.size, MIN_SIZE..=MAX_SIZE)
                        .logarithmic(true)
                        .custom_formatter(|v, _| format!("{:.1}%", v * 100.0)),
                )
                .on_hover_text("The letters' size, as a share of the picture's height")
                .dragged();
            ui.end_row();
            ui.label("Colour");
            recoloured |= colour(ui, &mut look.colour, "The letters");
            ui.end_row();
            ui.label("Lines");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("text_align", &deck.uuid))
                    .selected_text(look.align.label())
                    .show_ui(ui, |ui| {
                        for align in TextAlign::ALL {
                            ui.selectable_value(&mut look.align, align, align.label());
                        }
                    });
                ui.label("spacing");
                held |= ui
                    .add(
                        egui::DragValue::new(&mut look.spacing)
                            .range(0.8..=3.0)
                            .speed(0.01),
                    )
                    .on_hover_text("Line to line, as a share of the size")
                    .dragged();
            });
            ui.end_row();
            ui.label("Place");
            ui.horizontal(|ui| {
                super::timer::anchor_picker(ui, &mut look.anchor);
                ui.label("in");
                held |= ui
                    .add(
                        egui::Slider::new(&mut look.margin, 0.0..=0.4)
                            .custom_formatter(percent),
                    )
                    .on_hover_text("Kept in from the edges, as a share of the height")
                    .dragged();
            });
            ui.end_row();
            ui.label("Box");
            ui.horizontal(|ui| {
                held |= ui
                    .add(
                        egui::Slider::new(&mut look.box_opacity, 0.0..=1.0)
                            .custom_formatter(percent),
                    )
                    .on_hover_text("How solid the box behind the text is: 0% is none")
                    .dragged();
                if look.box_opacity > 0.0 {
                    recoloured |= colour(ui, &mut look.box_colour, "The box");
                }
            });
            ui.end_row();
            if look.box_opacity > 0.0 {
                ui.label("");
                ui.horizontal(|ui| {
                    ui.label("corners");
                    held |= ui
                        .add(
                            egui::DragValue::new(&mut look.box_radius)
                                .range(0.0..=2.0)
                                .speed(0.01),
                        )
                        .dragged();
                    ui.label("room");
                    held |= ui
                        .add(
                            egui::DragValue::new(&mut look.box_padding)
                                .range(0.0..=3.0)
                                .speed(0.01),
                        )
                        .dragged();
                });
                ui.end_row();
            }
            ui.label("Outline");
            ui.horizontal(|ui| {
                held |= ui
                    .add(
                        egui::Slider::new(&mut look.outline, 0.0..=0.2)
                            .custom_formatter(percent),
                    )
                    .on_hover_text("An outline round the letters, as a share of the size")
                    .dragged();
                if look.outline > 0.0 {
                    recoloured |= colour(ui, &mut look.outline_colour, "The outline");
                }
            });
            ui.end_row();
            ui.label("Shadow");
            held |= ui
                .add(
                    egui::Slider::new(&mut look.shadow_opacity, 0.0..=1.0)
                        .custom_formatter(percent),
                )
                .on_hover_text("A soft shadow under the letters, for reading over a picture")
                .dragged();
            ui.end_row();
        });
    // A drag is one undo step, from its first change while held.
    let step_id = egui::Id::new(("text_look_step", &deck.uuid));
    let was = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(step_id))
        .unwrap_or(false);
    let stepping =
        ui.input(|i| i.pointer.any_down()) && (was || ((held || recoloured) && settings != before));
    ui.ctx().data_mut(|d| d.insert_temp(step_id, stepping));
    if stepping {
        actions.session.gesture_active = true;
    }
    if settings != before {
        set_settings(actions, &deck.uuid, settings);
    }
}

fn colour(ui: &mut egui::Ui, rgb: &mut [u8; 3], hover: &str) -> bool {
    egui::color_picker::color_edit_button_srgb(ui, rgb)
        .on_hover_text(hover)
        .changed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::UIData;
    use egui_kittest::kittest::Queryable;

    #[test]
    fn text_render_failure_is_visible_without_changing_settings() {
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.unwrap();
        let deck = data.channels[ch].decks[dk].clone();
        let text = TextSnapshot {
            settings: TextSettings::default(),
            pending: false,
            error: Some("Text could not be drawn; previous picture retained".into()),
        };
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 900.0))
            .build_ui_state(
                move |ui, sent: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_controls(ui, &data, &deck, &text, &mut actions);
                    sent.extend(actions.commands);
                },
                Vec::new(),
            );
        h.run_steps(3);
        assert!(h
            .query_by_label("Text could not be drawn; previous picture retained")
            .is_some());
        assert!(h.state().is_empty());
    }

    /// "Updating…" shows while each typed letter is drawn: the field being
    /// typed in stays where it is.
    #[test]
    fn updating_does_not_move_the_text_field() {
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.unwrap();
        let deck = data.channels[ch].decks[dk].clone();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 900.0))
            .build_ui_state(
                move |ui, text: &mut TextSnapshot| {
                    let mut actions = UIActions::new();
                    render_controls(ui, &data, &deck, text, &mut actions);
                },
                TextSnapshot {
                    settings: TextSettings::default(),
                    pending: false,
                    error: None,
                },
            );
        let field = |h: &egui_kittest::Harness<'_, TextSnapshot>| {
            h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
                .next()
                .expect("the text field")
                .rect()
        };
        h.run_steps(3);
        let idle = field(&h);
        h.state_mut().pending = true;
        h.run_steps(3);
        assert!(h.query_by_label("Updating…").is_some());
        assert_eq!(field(&h), idle);
    }

    /// Typing, then an undo (the engine's text changed elsewhere): the
    /// field gives way, and the next letter goes on the undone text — the
    /// undone typing is not sent back.
    #[test]
    fn typing_gives_way_to_an_undo() {
        struct State {
            text: TextSnapshot,
            sent: Vec<String>,
        }
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let ui_data = UIData::test_fixture();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                move |ui, state: &mut State| {
                    let mut actions = UIActions::new();
                    render_controls(ui, &ui_data, &deck, &state.text, &mut actions);
                    state
                        .sent
                        .extend(actions.commands.into_iter().filter_map(|c| match c {
                            EngineCommand::SetTextSettings { settings, .. } => Some(settings.text),
                            _ => None,
                        }));
                },
                State {
                    text: TextSnapshot {
                        pending: false,
                        error: None,
                        settings: TextSettings {
                            text: "Salė A".into(),
                            ..TextSettings::default()
                        },
                    },
                    sent: Vec::new(),
                },
            );
        h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .next()
            .expect("the text field")
            .click();
        h.run_steps(2);
        h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .next()
            .expect("the text field")
            .type_text("X");
        h.run_steps(2);
        assert!(h.state().sent.last().is_some_and(|t| t.contains('X')));
        // Undone elsewhere: the engine holds another text now.
        h.state_mut().text.settings.text = "Salė B".into();
        h.run_steps(2);
        h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .next()
            .expect("the text field")
            .type_text("Y");
        h.run_steps(2);
        let last = h.state().sent.last().cloned().unwrap_or_default();
        assert!(
            last.starts_with("Salė B") && !last.contains('X'),
            "sent {last:?}"
        );
    }

    /// Typing in the field sends the new text at once; Shown off sends it
    /// hidden.
    #[test]
    fn typing_sends_the_text_and_shown_hides_it() {
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let text = TextSnapshot {
            pending: false,
            error: None,
            settings: TextSettings::default(),
        };
        let ui_data = UIData::test_fixture();
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                move |ui, sent: &mut Vec<TextSettings>| {
                    let mut actions = UIActions::new();
                    render_controls(ui, &ui_data, &deck, &text, &mut actions);
                    sent.extend(actions.commands.into_iter().filter_map(|c| match c {
                        EngineCommand::SetTextSettings { settings, .. } => Some(settings),
                        _ => None,
                    }));
                },
                Vec::new(),
            );
        h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .next()
            .expect("the text field")
            .click();
        h.run_steps(2);
        h.query_all_by_role(egui::accesskit::Role::MultilineTextInput)
            .next()
            .expect("the text field")
            .type_text(" Ąžuolas");
        h.run_steps(2);
        assert!(
            h.state().iter().any(|s| s.text.ends_with("Ąžuolas")),
            "the text sent: {:?}",
            h.state().last().map(|s| s.text.clone())
        );
        h.get_by_label("Shown").click();
        h.run_steps(2);
        assert!(h.state().iter().any(|s| !s.visible), "hidden");
    }
}
