//! Layers, as Resolume shows them: a row per channel, top layer on top, its
//! controls at the left and its decks across like clips.
//!
//! The rows run the other way from the Channels panel's columns because a
//! later channel is drawn over an earlier one, as a higher layer is in
//! Resolume. The deck cards are the same cards the Channels panel uses —
//! click, double-click, drag and right-click do the same.

use super::super::{ChannelUIInfo, DeckDrag, LibraryDrag, UIActions, UIData};
use super::main_desk::{self, NEXT_GREEN, ON_MAIN_RED};
use super::utils::channel_color;
use crate::engine::EngineCommand;
use crate::mixer::MixMode;
use crate::BlendMode;

/// Width of the controls at the left of each row.
const HEADER_WIDTH: f32 = 190.0;
const ROW_GAP: f32 = 6.0;

pub(super) fn render_layers(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let main_mode = main_desk::in_main_mode(data);
    if main_mode {
        main_desk::render_toolbar(ui, data, actions);
    } else {
        render_layered_toolbar(ui, actions);
    }
    ui.add_space(4.0);
    egui::ScrollArea::both()
        .id_salt("layers_rows")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = ROW_GAP;
            for ch in data.channels.iter().rev() {
                render_row(ui, ch, data, actions);
            }
            render_new_row(ui, data, actions);
        });
}

fn render_layered_toolbar(ui: &mut egui::Ui, actions: &mut UIActions) {
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(egui::RichText::new("+ Layer").size(15.0).strong())
                    .min_size(egui::vec2(0.0, 28.0)),
            )
            .on_hover_text("Add a channel as a new top layer")
            .clicked()
        {
            actions.commands.push(EngineCommand::AddChannel);
        }
        ui.label(
            egui::RichText::new("Every layer mixes to the output at its opacity")
                .small()
                .weak(),
        );
        if ui
            .button("Main mode")
            .on_hover_text("One channel on Main at a time, taken with Cut or Fade")
            .clicked()
        {
            actions.commands.push(EngineCommand::SetMixMode {
                mode: MixMode::Program,
            });
        }
    });
}

/// Width given to each deck card; the card sizes its picture from it.
fn cell_width(data: &UIData) -> f32 {
    if main_desk::in_main_mode(data) {
        180.0
    } else {
        140.0
    }
}

fn render_row(ui: &mut egui::Ui, ch: &ChannelUIInfo, data: &UIData, actions: &mut UIActions) {
    let accent = channel_color(ch.ch_idx);
    let main_mode = main_desk::in_main_mode(data);
    let on_main = main_mode && main_desk::channel_on_main(data, ch);
    let selected = data.selected_channel == Some(ch.ch_idx);
    let source_drag = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx())
        .is_some_and(|p| !matches!(&*p, LibraryDrag::Effect(_)))
        || egui::DragAndDrop::has_payload_of_type::<DeckDrag>(ui.ctx());

    ui.push_id(("layer_row", ch.ch_idx), |ui| {
        let hover = source_drag && ui.rect_contains_pointer(ui.max_rect());
        // Where each card is drawn, front first: a drop between two goes
        // there, as in a Classic column.
        let mut cards: super::insertion::Cards = Vec::with_capacity(ch.decks.len());
        let stroke = if hover {
            egui::Stroke::new(2.0, accent)
        } else if on_main {
            egui::Stroke::new(3.0, ON_MAIN_RED)
        } else if selected {
            egui::Stroke::new(2.0, accent.linear_multiply(0.6))
        } else {
            egui::Stroke::new(1.0, egui::Color32::from_rgb(50, 50, 60))
        };
        // Sensed before the row's contents are, so its cards and buttons stay
        // on top and a click on the row's bare background selects the channel.
        let row = ui
            .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                egui::Frame::default()
                    .fill(if selected {
                        accent.linear_multiply(0.06)
                    } else {
                        egui::Color32::TRANSPARENT
                    })
                    .stroke(stroke)
                    .corner_radius(4.0)
                    .inner_margin(4.0)
                    .show(ui, |ui| {
                        // Rows run the panel's width, as Resolume's layers do.
                        ui.set_min_width(ui.available_width());
                        ui.horizontal_top(|ui| {
                            ui.allocate_ui_with_layout(
                                egui::vec2(HEADER_WIDTH, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.set_width(HEADER_WIDTH);
                                    render_header(ui, ch, on_main, data, actions);
                                },
                            );
                            // Not `ui.separator()`: in a row it stretches to the
                            // height left in the scroll area, making each row as
                            // tall as the panel.
                            ui.add_space(4.0);
                            let cell = cell_width(data);
                            // Front first, as the other lists of layers.
                            for deck in ch.decks.iter().rev() {
                                let card = ui.allocate_ui_with_layout(
                                    egui::vec2(cell, 0.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        super::mixer::render_deck_thumbnail(
                                            ui, ch.ch_idx, deck, accent, data, actions,
                                        );
                                    },
                                );
                                cards.push((deck.uuid.clone(), card.response.rect));
                            }
                            render_empty_cell(ui, ch, hover, accent, data, actions);
                        });
                    })
            })
            .response;
        let rect = row.rect;
        if row.clicked() {
            actions.session.select_channel = Some(ch.ch_idx);
        }
        row.context_menu(|ui| super::mixer::channel_context_menu(ui, data, actions, ch));
        super::publish_drop_rect(ui, ch.ch_idx, rect);
        // The cards' part of the row, from the first card to the row's end.
        if let Some((_, first)) = cards.first() {
            let area = egui::Rect::from_min_max(
                egui::pos2(first.left() - 4.0, rect.top()),
                rect.max,
            );
            super::insertion::publish_cards_across(ui, ch.ch_idx, cards, area);
        }
        super::dnd::publish_channel_surface_fx(ui.ctx(), &ch.uuid, ch.ch_idx, rect);
        super::mixer::handle_deck_drop(ui, ch, rect, accent, actions);
    });
}

/// A layer's controls: name, what it is doing, the takes, and its mix.
fn render_header(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    on_main: bool,
    data: &UIData,
    actions: &mut UIActions,
) {
    let accent = channel_color(ch.ch_idx);
    let main_mode = main_desk::in_main_mode(data);
    let name = ui.add(
        egui::Label::new(
            egui::RichText::new(format!("▌ {}", ch.name))
                .strong()
                .size(17.0)
                .color(accent),
        )
        .sense(egui::Sense::click()),
    );
    if name.clicked() {
        actions.session.select_channel = Some(ch.ch_idx);
    }
    if name.double_clicked() {
        super::channel_settings::open_channel(ui.ctx(), ch);
    }
    name.context_menu(|ui| super::mixer::channel_context_menu(ui, data, actions, ch));
    if let Some((deck, list)) = super::playlist::of_channel(ch) {
        ui.horizontal(|ui| {
            let size = egui::vec2(28.0, 22.0);
            super::playlist::play_button(ui, &deck.uuid, list, size, actions);
            super::playlist::list_button(ui, &deck.uuid, size);
        });
    }
    if let Some((deck, timer)) = super::timer::of_channel(ch) {
        ui.horizontal(|ui| {
            let size = egui::vec2(28.0, 22.0);
            super::timer::start_button(ui, &deck.uuid, timer, size, actions);
            super::timer::settings_button(ui, &deck.uuid, size);
        });
    }
    if let Some((deck, _)) = super::presentation::of_channel(ch) {
        ui.horizontal(|ui| {
            super::presentation::step_buttons(ui, &deck.uuid, egui::vec2(28.0, 22.0), actions);
        });
    }

    if main_mode {
        let next = main_desk::next_channel(data).is_some_and(|n| n.uuid == ch.uuid);
        ui.horizontal(|ui| {
            let badge = |ui: &mut egui::Ui, text: &str, color: egui::Color32| {
                ui.label(
                    egui::RichText::new(format!(" {text} "))
                        .strong()
                        .color(egui::Color32::WHITE)
                        .background_color(color),
                );
            };
            if on_main {
                badge(ui, "ON MAIN", ON_MAIN_RED);
            }
            if next {
                badge(ui, "NEXT", NEXT_GREEN);
            }
            let mut overlay = ch.overlay;
            if ui
                .checkbox(&mut overlay, "Overlay")
                .on_hover_text("Switched on and off over Main instead of taken to it")
                .changed()
            {
                actions.commands.push(EngineCommand::SetChannelOverlay {
                    channel_uuid: ch.uuid.clone(),
                    overlay,
                });
            }
        });
        ui.horizontal(|ui| {
            let button = |text: &str| {
                egui::Button::new(egui::RichText::new(text).size(14.0).strong())
                    .min_size(egui::vec2(56.0, 24.0))
            };
            if ch.overlay {
                let on = main_desk::overlay_on(data, ch);
                let (text, fill) = if on {
                    ("ON", ON_MAIN_RED)
                } else {
                    ("OFF", egui::Color32::from_rgb(60, 60, 70))
                };
                if ui.add(button(text).fill(fill)).clicked() {
                    main_desk::take_channel(ch, data, actions, false);
                }
                if ui.add(button("Fade")).clicked() {
                    main_desk::take_channel(ch, data, actions, true);
                }
            } else {
                if ui
                    .add_enabled(!on_main, button("Cut"))
                    .on_hover_text("Put this layer on Main now")
                    .clicked()
                {
                    main_desk::take_channel(ch, data, actions, false);
                }
                if ui
                    .add_enabled(!on_main && data.main.fade.is_none(), button("Fade"))
                    .on_hover_text("Fade this layer onto Main")
                    .clicked()
                {
                    main_desk::take_channel(ch, data, actions, true);
                }
            }
        });
    }

    // The mix: opacity, and in the layered mixer how it blends. In Main mode
    // only an overlay's opacity counts.
    if !main_mode || ch.overlay {
        let mut opacity = ch.opacity;
        let slider = ui.add(
            egui::Slider::new(&mut opacity, 0.0..=1.0)
                .show_value(false)
                .text("Opacity"),
        );
        if slider.dragged() {
            actions.session.gesture_active = true;
        }
        if slider.changed() {
            actions.commands.push(EngineCommand::SetChannelOpacity {
                channel_uuid: ch.uuid.clone(),
                opacity,
            });
        }
    }
    if !main_mode {
        let modes = BlendMode::all();
        let mut chosen = ch.blend_mode;
        egui::ComboBox::from_id_salt(("layer_blend", ch.ch_idx))
            .selected_text(chosen.short_name())
            .width(80.0)
            .show_ui(ui, |ui| {
                for mode in modes {
                    ui.selectable_value(&mut chosen, *mode, mode.short_name());
                }
            });
        if chosen != ch.blend_mode {
            actions.commands.push(EngineCommand::SetChannelBlendMode {
                channel_uuid: ch.uuid.clone(),
                mode: chosen,
            });
        }
    }
}

/// The space after a layer's decks: where to drop the next one.
fn render_empty_cell(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    hover: bool,
    accent: egui::Color32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let height = if main_desk::in_main_mode(data) {
        110.0
    } else {
        90.0
    };
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(cell_width(data), height), egui::Sense::click());
    let color = if hover {
        accent
    } else {
        egui::Color32::from_rgb(70, 70, 85)
    };
    ui.painter().rect_stroke(
        rect.shrink(2.0),
        4.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if hover {
            "➕ Drop here"
        } else if ch.decks.is_empty() {
            "Drop files or\nLibrary items here"
        } else {
            "+ drop"
        },
        egui::FontId::proportional(12.0),
        color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{} drop cell", ch.name),
        )
    });
    if response.clicked() {
        actions.session.select_channel = Some(ch.ch_idx);
    }
}

/// Below the bottom layer: drop here for a new layer.
fn render_new_row(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let dragging = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx())
        .is_some_and(|p| !matches!(&*p, LibraryDrag::Effect(_)));
    let width = ui.available_width().max(HEADER_WIDTH + cell_width(data));
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 44.0), egui::Sense::click());
    let hover = dragging && ui.rect_contains_pointer(rect);
    let color = if hover {
        egui::Color32::from_rgb(150, 150, 190)
    } else {
        egui::Color32::from_rgb(60, 60, 75)
    };
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        4.0,
        egui::Stroke::new(1.0, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if hover {
            "➕ Drop for a new layer"
        } else {
            "+ Layer — or drop something here"
        },
        egui::FontId::proportional(13.0),
        color,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New layer"));
    if response.clicked() {
        actions.commands.push(EngineCommand::AddChannel);
    }
    ui.ctx().memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new("new_ch_drop_rect").with(1_usize), rect);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mixer::MainSource;
    use egui_kittest::kittest::Queryable;

    struct Seen {
        commands: Vec<EngineCommand>,
        select_channel: Option<usize>,
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, Seen> {
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_ui_state(
                move |ui, seen: &mut Seen| {
                    let mut actions = UIActions::new();
                    render_layers(ui, &data, &mut actions);
                    seen.commands.extend(actions.commands);
                    seen.select_channel = seen.select_channel.or(actions.session.select_channel);
                },
                Seen {
                    commands: Vec::new(),
                    select_channel: None,
                },
            );
        harness.run();
        harness
    }

    /// A layer's cards run across its row, front first: a drop between two
    /// goes there — and a card dragged between two moves there — as in a
    /// Classic column; past the last, behind them all.
    #[test]
    fn a_drop_between_a_layers_cards_goes_between_them() {
        let data = UIData::test_fixture();
        assert!(data.channels[0].decks.len() >= 2, "precondition: two decks");
        let front = data.channels[0].decks.last().unwrap().uuid.clone();
        let back = data.channels[0].decks[0].uuid.clone();
        let harness = harness(data);
        let stacks = harness
            .ctx
            .memory(|m| {
                m.data
                    .get_temp::<Vec<super::super::insertion::Stack>>(super::super::insertion::stacks_id(0))
            })
            .expect("the row publishes its cards");
        let stack = stacks.first().expect("one view");
        assert!(stack.across, "across the row");
        let ids: Vec<&str> = stack.cards.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids.first().copied(), Some(front.as_str()), "front first");
        let (first, second) = (stack.cards[0].1, stack.cards[1].1);
        let between = egui::pos2(second.left() + 4.0, second.center().y);
        assert_eq!(
            super::super::insertion::insertion_at(&harness.ctx, 0, between).before,
            Some(front.clone())
        );
        let past = egui::pos2(stack.cards.last().unwrap().1.right() + 20.0, first.center().y);
        assert_eq!(
            super::super::insertion::insertion_at(&harness.ctx, 0, past).before,
            Some(back),
            "behind them all"
        );
    }

    fn main_mode_fixture() -> UIData {
        let mut data = UIData::test_fixture();
        data.main.mode = MixMode::Program;
        data.main.program = MainSource::Channel(data.channels[0].uuid.clone());
        data.selected_deck = None;
        data.selected_channel = None;
        data
    }

    /// The top layer — the last channel — is the top row.
    #[test]
    fn the_top_layer_is_the_top_row() {
        let data = UIData::test_fixture();
        let first = data.channels.first().unwrap().name.clone();
        let last = data.channels.last().unwrap().name.clone();
        let harness = harness(data);
        let y = |name: &str| {
            harness
                .get_by_label(&format!("{name} drop cell"))
                .rect()
                .top()
        };
        assert!(y(&last) < y(&first), "{last} above {first}");
    }

    /// A row is as tall as its cards, not as the panel.
    #[test]
    fn rows_are_as_tall_as_their_cards() {
        let data = UIData::test_fixture();
        let first = data.channels.first().unwrap().name.clone();
        let last = data.channels.last().unwrap().name.clone();
        let harness = harness(data);
        let y = |name: &str| {
            harness
                .get_by_label(&format!("{name} drop cell"))
                .rect()
                .top()
        };
        let pitch = (y(&first) - y(&last)) / (UIData::test_fixture().channels.len() - 1) as f32;
        assert!(pitch < 250.0, "rows {pitch} px apart");
    }

    /// In a row, as in a channel's other lists: the layer in front first.
    #[test]
    fn a_rows_cards_put_the_layer_in_front_first() {
        let data = UIData::test_fixture();
        let back = data.channels[0].decks[0].name.clone();
        let front = data.channels[0].decks[1].name.clone();
        let harness = harness(data);
        let x = |name: &str| {
            harness
                .get_by_label(&format!("{name} deck card"))
                .rect()
                .left()
        };
        assert!(x(&front) < x(&back), "{front} before {back}");
    }

    #[test]
    fn every_deck_has_its_card() {
        let data = UIData::test_fixture();
        let names: Vec<String> = data
            .channels
            .iter()
            .flat_map(|c| c.decks.iter().map(|d| d.name.clone()))
            .collect();
        let harness = harness(data);
        for name in names {
            assert!(
                harness
                    .query_by_label(&format!("{name} deck card"))
                    .is_some(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_layers_cut_takes_it_to_main() {
        let data = main_mode_fixture();
        let top = data.channels.last().unwrap().uuid.clone();
        let mut harness = harness(data);
        // The top row comes first, and it is not on Main.
        harness.get_all_by_label("Cut").next().unwrap().click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::TakeToMain { source: MainSource::Channel(u), fade: false } if *u == top
            )),
            "{:?}",
            harness.state().commands
        );
    }

    #[test]
    fn the_layered_mixer_shows_each_layers_opacity() {
        let harness = harness(UIData::test_fixture());
        assert!(harness.get_all_by_label("Opacity").count() >= 2);
    }

    #[test]
    fn a_rows_drop_cell_selects_its_channel() {
        let data = UIData::test_fixture();
        let top = data.channels.len() - 1;
        let name = data.channels[top].name.clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("{name} drop cell")).click();
        harness.run();
        assert_eq!(harness.state().select_channel, Some(top));
    }
}
