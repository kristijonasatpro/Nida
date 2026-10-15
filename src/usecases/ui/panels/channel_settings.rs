//! Input settings, as vMix opens them on a double-click: one window for a
//! channel — an input — with its layers listed down the left, and everything
//! that can be adjusted on the channel or on one layer.
//!
//! The left column keeps the channel's result in view, then the channel
//! itself and its layers, numbered as vMix numbers them — Layer 1 at the
//! back, each higher one drawn over it: select one to edit it, hide it, move
//! it back or forward, delete it, or add another. The right side edits what
//! is selected — a preview to drag (Shift-drag to resize) and zoom a layer
//! on, then tabs: Position (opacity, blend, crop, transform, copy and paste),
//! Colour, Key, Effects, and for a video layer Playback; for the channel,
//! General (Main, overlay, buses), Layout (full screen, side by side,
//! picture in picture, grid), Colour, Key and Effects.
//!
//! Opened by double-clicking a channel (an Inputs tile, a Layers row's name,
//! the Preview picture) or a deck card, or from Settings… / Adjust… in their
//! menus. It follows the selection like the Properties panel: click another
//! channel or card and it moves there.

use super::super::{ChannelUIInfo, DeckUIInfo, EffectInfo, UIActions, UIData};
use super::main_desk::{self, NEXT_GREEN, ON_MAIN_RED};
use super::utils::channel_color;
use crate::engine::{EffectTarget, EngineCommand};
use crate::mixer::MainSource;
use crate::usecases::ui::window_choices::WindowChoices;
use crate::BlendMode;

/// What the window is editing.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Item {
    /// The channel as a whole: its effects, and how it goes on air.
    Channel,
    /// One layer (deck) of it, by UUID.
    Layer(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Source,
    Position,
    Playback,
    General,
    Layout,
    Colour,
    Key,
    Effects,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Self::Source => "Source",
            Self::Position => "Position",
            Self::Playback => "Playback",
            Self::General => "General",
            Self::Layout => "Layout",
            Self::Colour => "Colour",
            Self::Key => "Key",
            Self::Effects => "Effects",
        }
    }
}

#[derive(Clone, Debug)]
struct Open {
    channel: String,
    item: Item,
    tab: Tab,
}

fn open_id() -> egui::Id {
    egui::Id::new("channel_settings_open")
}

/// The selection as of the last frame: (channel UUID, deck UUID).
fn seen_id() -> egui::Id {
    egui::Id::new("channel_settings_seen")
}

fn current(ctx: &egui::Context) -> Option<Open> {
    ctx.memory(|m| m.data.get_temp::<Open>(open_id()))
}

fn store(ctx: &egui::Context, open: Open) {
    ctx.memory_mut(|m| m.data.insert_temp(open_id(), open));
}

/// Open the window on a channel. A channel of one picture opens on that
/// picture's position, as vMix opens an input; one of several on the
/// channel, with its layers listed.
pub(super) fn open_channel(ctx: &egui::Context, ch: &ChannelUIInfo) {
    let open = match ch.decks.as_slice() {
        // A clip opens on how it plays on air, as vMix opens a video input
        // on its General tab.
        [deck] => Open {
            channel: ch.uuid.clone(),
            item: Item::Layer(deck.uuid.clone()),
            tab: if deck.video_playback.is_some() {
                Tab::Playback
            } else {
                Tab::Position
            },
        },
        _ => Open {
            channel: ch.uuid.clone(),
            item: Item::Channel,
            tab: Tab::General,
        },
    };
    store(ctx, open);
}

/// Open the window on one layer. The channel is looked up when the window is
/// drawn, so a deck of any channel can be named.
pub(super) fn open_layer(ctx: &egui::Context, deck_uuid: &str) {
    store(
        ctx,
        Open {
            channel: String::new(),
            item: Item::Layer(deck_uuid.to_string()),
            tab: Tab::Position,
        },
    );
}

/// Open the window on a clip's Playback tab: what it does going on and off
/// air, and its sound.
pub(super) fn open_layer_playback(ctx: &egui::Context, deck_uuid: &str) {
    store(
        ctx,
        Open {
            channel: String::new(),
            item: Item::Layer(deck_uuid.to_string()),
            tab: Tab::Playback,
        },
    );
}

/// The channel the window is open on, for tests elsewhere in the panels.
#[cfg(test)]
pub(super) fn open_on_channel(ctx: &egui::Context) -> Option<String> {
    current(ctx).map(|o| o.channel)
}

/// The selection as channel and deck UUIDs.
fn selection(data: &UIData) -> (Option<String>, Option<String>) {
    if let Some((ch_idx, deck_idx)) = data.selected_deck {
        let ch = data.channels.get(ch_idx);
        let deck = ch.and_then(|c| c.decks.iter().find(|d| d.deck_idx == deck_idx));
        return (ch.map(|c| c.uuid.clone()), deck.map(|d| d.uuid.clone()));
    }
    let ch = data.selected_channel.and_then(|i| data.channels.get(i));
    (ch.map(|c| c.uuid.clone()), None)
}

/// Move an open window to what was just selected. Only a change of selection
/// moves it: the frame a double-click opens the window can still carry the
/// previous selection, and that must not pull it straight back.
fn follow_selection(ctx: &egui::Context, data: &UIData) {
    let now = selection(data);
    let seen = ctx.memory(|m| {
        m.data
            .get_temp::<(Option<String>, Option<String>)>(seen_id())
    });
    ctx.memory_mut(|m| m.data.insert_temp(seen_id(), now.clone()));
    let Some(mut open) = current(ctx) else {
        return;
    };
    // Nothing seen before this frame is no change either.
    if seen.is_none_or(|seen| seen == now) {
        return;
    }
    match now {
        (_, Some(deck)) => {
            if open.item != Item::Layer(deck.clone()) {
                open.item = Item::Layer(deck);
                open.channel = String::new();
                if !matches!(open.tab, Tab::Position | Tab::Playback) {
                    open.tab = Tab::Position;
                }
                store(ctx, open);
            }
        }
        (Some(channel), None) => {
            if open.channel != channel {
                if let Some(ch) = data.channels.iter().find(|c| c.uuid == channel) {
                    open_channel(ctx, ch);
                }
            }
        }
        (None, None) => {}
    }
}

/// The selection is about to become `channel_uuid` for a reason of its own
/// (Preview swapping with Program after a take): an open window stays where
/// it is rather than following it.
pub(super) fn stay_on_selection_change(ctx: &egui::Context, channel_uuid: &str) {
    let next: (Option<String>, Option<String>) = (Some(channel_uuid.to_string()), None);
    ctx.memory_mut(|m| m.data.insert_temp(seen_id(), next));
}

/// What a clip does by itself going on and off air (vMix's Auto Play, Auto
/// Pause, Auto Restart) and whether its sound follows (Audio Auto).
fn render_on_air(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    vp: &super::super::VideoPlaybackUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("On air").strong());
    let auto = vp.auto;
    let mut set = |auto: crate::engine::value::video::AutoPlayback| {
        actions.commands.push(EngineCommand::VideoSetAutoPlayback {
            deck_uuid: deck.uuid.clone(),
            auto,
        });
    };
    ui.horizontal(|ui| {
        ui.label("Starts and stops on:");
        let mut main_only = auto.main_only;
        let any = ui
            .radio_value(&mut main_only, false, "Main or any bus")
            .on_hover_text("On Main, or on any bus's Program — the LED, the stream…");
        let main = ui
            .radio_value(&mut main_only, true, "Main only")
            .on_hover_text("Only Main: on a bus it neither starts nor stops by itself");
        if (any.changed() || main.changed()) && main_only != auto.main_only {
            set(crate::engine::value::video::AutoPlayback { main_only, ..auto });
        }
    });
    let mut play = auto.play;
    if ui
        .checkbox(&mut play, "Play when it goes on air (Auto Play)")
        .changed()
    {
        set(crate::engine::value::video::AutoPlayback { play, ..auto });
    }
    let mut pause = auto.pause;
    if ui
        .checkbox(&mut pause, "Pause when it comes off air (Auto Pause)")
        .changed()
    {
        set(crate::engine::value::video::AutoPlayback { pause, ..auto });
    }
    let mut restart = auto.restart;
    if ui
        .checkbox(
            &mut restart,
            "Back to the start when it comes off air (Auto Restart)",
        )
        .changed()
    {
        set(crate::engine::value::video::AutoPlayback { restart, ..auto });
    }
    if let Some(source) = data.audio_mix.sources.iter().find(|s| {
        matches!(&s.kind, crate::audio_mix::AudioSourceKind::Clip { deck_uuid } if *deck_uuid == deck.uuid)
    }) {
        super::audio_mixer::audio_auto_settings(ui, source, data, actions);
    } else {
        ui.label(egui::RichText::new("No sound in this clip.").small().weak());
    }
}

/// The channel and the layer being edited, found afresh each frame.
fn resolve<'a>(
    data: &'a UIData,
    open: &Open,
) -> Option<(&'a ChannelUIInfo, Option<&'a DeckUIInfo>)> {
    match &open.item {
        Item::Layer(uuid) => data.channels.iter().find_map(|c| {
            c.decks
                .iter()
                .find(|d| &d.uuid == uuid)
                .map(|d| (c, Some(d)))
        }),
        Item::Channel => data
            .channels
            .iter()
            .find(|c| c.uuid == open.channel)
            .map(|c| (c, None)),
    }
}

/// Draw the window, if it is open. Closes itself when its channel goes; a
/// deleted layer falls back to the channel. `desk` is where the channels are
/// shown: the window first opens at its lower right.
pub(super) fn render_channel_settings(
    ctx: &egui::Context,
    desk: egui::Rect,
    data: &UIData,
    actions: &mut UIActions,
) {
    follow_selection(ctx, data);
    let Some(mut open) = current(ctx) else {
        return;
    };
    let Some((ch, layer)) = resolve(data, &open).or_else(|| {
        // The layer is gone: back to its channel, if that is still here.
        let ch = data.channels.iter().find(|c| c.uuid == open.channel)?;
        Some((ch, None))
    }) else {
        ctx.memory_mut(|m| m.data.remove::<Open>(open_id()));
        return;
    };
    // Keep what is stored in step with what is found.
    open.channel.clone_from(&ch.uuid);
    if layer.is_none() {
        open.item = Item::Channel;
    }
    let tabs = tabs_for(layer);
    if !tabs.contains(&open.tab) {
        open.tab = tabs[0];
    }
    store(ctx, open.clone());

    let mut still_open = true;
    let window = egui::Window::new(format!("{} — Input Settings", ch.name))
        .id(egui::Id::new("channel_settings_window"))
        .open(&mut still_open)
        .collapsible(false)
        .resizable(true)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .default_pos(egui::pos2(
            desk.right() - 8.0,
            ctx.content_rect().bottom() - 8.0,
        ))
        // Wide enough for the controls beside the picture, their sliders
        // long.
        .default_size(egui::vec2(1180.0, 680.0))
        .show(ctx, |ui| {
            render_body(ui, ch, layer, &open, data, actions);
        });
    if let Some(inner) = &window {
        let rect = inner.response.rect;
        // Files and Library items dropped on the window go into this channel.
        ctx.memory_mut(|m| {
            m.data.insert_temp(drop_window_id(), (ch.ch_idx, rect));
        });
        // Effects dropped on it go on what it is editing.
        if let Some(layer) = layer {
            super::dnd::publish_deck_surface_fx(ctx, &layer.uuid, ch.ch_idx, layer.deck_idx, rect);
        } else {
            super::dnd::publish_channel_surface_fx(ctx, &ch.uuid, ch.ch_idx, rect);
        }
    }
    let escaped = ctx.input(|i| i.key_pressed(egui::Key::Escape))
        && window
            .as_ref()
            .is_some_and(|w| ctx.rect_contains_pointer(w.response.layer_id, w.response.rect));
    if !still_open || escaped {
        ctx.memory_mut(|m| m.data.remove::<Open>(open_id()));
    }
}

/// Where the open window takes drops, for the drop handlers: which channel,
/// and the window's rect. It is checked before the channel views, since the
/// window is drawn over them.
pub(super) fn drop_window_id() -> egui::Id {
    egui::Id::new("ch_drop_window")
}

fn tabs_for(layer: Option<&DeckUIInfo>) -> Vec<Tab> {
    match layer {
        // A sound file has nothing to see: only how it plays.
        Some(deck) if deck.video_playback.as_ref().is_some_and(|vp| vp.sound_only) => {
            vec![Tab::Playback]
        }
        Some(deck) => {
            let mut tabs = vec![Tab::Source, Tab::Position];
            if deck.video_playback.is_some() {
                tabs.push(Tab::Playback);
            }
            tabs.extend([Tab::Colour, Tab::Key, Tab::Effects]);
            tabs
        }
        None => vec![
            Tab::General,
            Tab::Layout,
            Tab::Colour,
            Tab::Key,
            Tab::Effects,
        ],
    }
}

const LIST_WIDTH: f32 = 250.0;

fn render_body(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    layer: Option<&DeckUIInfo>,
    open: &Open,
    data: &UIData,
    actions: &mut UIActions,
) {
    let size = ui.available_size();
    let list_width = ui
        .ctx()
        .data(|d| d.get_temp::<f32>(list_width_id()))
        .unwrap_or(LIST_WIDTH)
        .clamp(MIN_LIST_WIDTH, (size.x * 0.5).max(MIN_LIST_WIDTH));
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.allocate_ui_with_layout(
            egui::vec2(list_width, size.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_width(list_width);
                ui.spacing_mut().item_spacing.x = 8.0;
                render_layer_list(ui, ch, open, data, actions);
            },
        );
        let moved = splitter(ui, true, size.y, "Resize the layer list");
        if moved != 0.0 {
            let width = list_width + moved;
            ui.ctx().data_mut(|d| d.insert_temp(list_width_id(), width));
        }
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), size.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                render_editor(ui, ch, layer, open, data, actions);
            },
        );
    });
}

const MIN_LIST_WIDTH: f32 = 170.0;

/// The layer list's width, once dragged.
fn list_width_id() -> egui::Id {
    egui::Id::new("channel_settings_list_width")
}

/// The share of the editor the picture takes, once dragged.
fn picture_share_id() -> egui::Id {
    egui::Id::new("channel_settings_picture_share")
}

/// Whether the tabs sit beside the picture (`Some(true)`) or under it, once
/// chosen; until then, beside when the window is wide.
fn beside_id() -> egui::Id {
    egui::Id::new("channel_settings_beside")
}

/// A divider to drag. `across` is a vertical line dragged sideways; otherwise
/// a horizontal one dragged up and down. Returns how far it moved.
fn splitter(ui: &mut egui::Ui, across: bool, length: f32, what: &str) -> f32 {
    let size = if across {
        egui::vec2(8.0, length)
    } else {
        egui::vec2(length, 8.0)
    };
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::drag());
    let response = response.on_hover_cursor(if across {
        egui::CursorIcon::ResizeHorizontal
    } else {
        egui::CursorIcon::ResizeVertical
    });
    let active = response.hovered() || response.dragged();
    let stroke = if active {
        egui::Stroke::new(2.0, ui.visuals().selection.bg_fill)
    } else {
        ui.visuals().widgets.noninteractive.bg_stroke
    };
    let line = if across {
        [rect.center_top(), rect.center_bottom()]
    } else {
        [rect.left_center(), rect.right_center()]
    };
    ui.painter().line_segment(line, stroke);
    let what = what.to_string();
    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &what));
    if across {
        response.drag_delta().x
    } else {
        response.drag_delta().y
    }
}

/// The channel's result, the channel, its layers frontmost first, and adding
/// one.
fn render_layer_list(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    open: &Open,
    data: &UIData,
    actions: &mut UIActions,
) {
    let accent = channel_color(ch.ch_idx);
    // The result stays in view whatever is being edited.
    let result = super::utils::preview_size(
        egui::vec2(LIST_WIDTH, LIST_WIDTH),
        data.render_width,
        data.render_height,
    );
    if let Some(&tex) = data.channel_preview_textures.get(&ch.ch_idx) {
        super::deck_detail::channel_result_preview(ui, tex, result, &ch.name, accent);
    }
    ui.add_space(4.0);

    // The channel itself.
    let selected = open.item == Item::Channel;
    let mut tally = String::new();
    if main_desk::in_main_mode(data) {
        if ch.overlay {
            tally = if main_desk::overlay_on(data, ch) {
                "  · overlay on".into()
            } else {
                "  · overlay".into()
            };
        } else if main_desk::channel_on_main(data, ch) {
            tally = "  · ON MAIN".into();
        }
    }
    let row = ui
        .selectable_label(
            selected,
            egui::RichText::new(format!("▌ {}{tally}", ch.name))
                .strong()
                .color(accent),
        )
        .on_hover_text("The whole channel: its effects, and how it goes on air");
    if row.clicked() {
        store(
            ui.ctx(),
            Open {
                channel: ch.uuid.clone(),
                item: Item::Channel,
                tab: Tab::General,
            },
        );
        actions.session.select_channel = Some(ch.ch_idx);
    }

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!(
            "Layers ({}) — 1 is at the back, higher ones over it",
            ch.decks.len()
        ))
        .small()
        .weak(),
    );
    egui::ScrollArea::vertical()
        .id_salt("channel_settings_layers")
        .max_height((ui.available_height() - 40.0).max(80.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            // Later decks are drawn over earlier ones.
            for (pos, deck) in ch.decks.iter().enumerate() {
                render_layer_row(ui, ch, pos, deck, open, data, actions);
            }
            if ch.decks.is_empty() {
                ui.label(
                    egui::RichText::new("No layers yet — add one, or drop files on this window")
                        .small()
                        .weak(),
                );
            }
        });
    ui.add_space(4.0);
    render_add_layer(ui, ch);
}

/// One layer: its picture and name (click to edit it), shown or not, move
/// forward / back, delete.
fn render_layer_row(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    pos: usize,
    deck: &DeckUIInfo,
    open: &Open,
    data: &UIData,
    actions: &mut UIActions,
) {
    let accent = channel_color(ch.ch_idx);
    let selected = open.item == Item::Layer(deck.uuid.clone());
    ui.push_id(("layer_row", &deck.uuid), |ui| {
        let frame = egui::Frame::default()
            .inner_margin(3.0)
            .corner_radius(3.0)
            .fill(if selected {
                accent.linear_multiply(0.22)
            } else {
                egui::Color32::TRANSPARENT
            });
        let row = ui
            .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let thumb = super::utils::preview_size(
                            egui::vec2(56.0, 32.0),
                            data.render_width,
                            data.render_height,
                        );
                        let (rect, _) = ui.allocate_exact_size(thumb, egui::Sense::hover());
                        if let Some(&tex) = data.deck_preview_textures.get(&deck.uuid) {
                            ui.painter().image(
                                tex,
                                rect,
                                egui::Rect::from_min_max(
                                    egui::pos2(0.0, 0.0),
                                    egui::pos2(1.0, 1.0),
                                ),
                                if deck.mute {
                                    egui::Color32::from_gray(90)
                                } else {
                                    egui::Color32::WHITE
                                },
                            );
                        } else {
                            ui.painter().rect_filled(
                                rect,
                                2.0,
                                egui::Color32::from_rgb(30, 30, 40),
                            );
                        }
                        ui.vertical(|ui| {
                            let name = format!(
                                "{}  {}",
                                pos + 1,
                                super::utils::truncate_chars(&deck.name, 18)
                            );
                            let text = if deck.mute {
                                egui::RichText::new(name).weak().strikethrough()
                            } else {
                                egui::RichText::new(name)
                            };
                            ui.label(text).on_hover_text(&deck.name);
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 2.0;
                                let mut shown = !deck.mute;
                                if ui
                                    .checkbox(&mut shown, "")
                                    .on_hover_text("Shown — untick to hide this layer")
                                    .changed()
                                {
                                    actions.commands.push(EngineCommand::SetDeckMute {
                                        deck_uuid: deck.uuid.clone(),
                                        mute: !shown,
                                    });
                                }
                                if ui
                                    .add_enabled(pos > 0, egui::Button::new("⏶").small())
                                    .on_hover_text("Back — swap with the layer before it")
                                    .clicked()
                                {
                                    actions.commands.push(EngineCommand::MoveDeck {
                                        deck_uuid: deck.uuid.clone(),
                                        dst_channel_uuid: ch.uuid.clone(),
                                        before_deck_uuid: Some(ch.decks[pos - 1].uuid.clone()),
                                    });
                                }
                                let front = pos + 1 >= ch.decks.len();
                                if ui
                                    .add_enabled(!front, egui::Button::new("⏷").small())
                                    .on_hover_text("Forward — swap with the layer after it")
                                    .clicked()
                                {
                                    actions.commands.push(EngineCommand::MoveDeck {
                                        deck_uuid: deck.uuid.clone(),
                                        dst_channel_uuid: ch.uuid.clone(),
                                        before_deck_uuid: ch
                                            .decks
                                            .get(pos + 2)
                                            .map(|d| d.uuid.clone()),
                                    });
                                }
                                if ui
                                    .add(egui::Button::new("🗑").small())
                                    .on_hover_text("Delete this layer")
                                    .clicked()
                                {
                                    actions.commands.push(EngineCommand::RemoveDeck {
                                        deck_uuid: deck.uuid.clone(),
                                    });
                                }
                            });
                        });
                    });
                });
            })
            .response;
        row.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("{} layer", deck.name),
            )
        });
        if row.clicked() {
            store(
                ui.ctx(),
                Open {
                    channel: ch.uuid.clone(),
                    item: Item::Layer(deck.uuid.clone()),
                    tab: if selected { open.tab } else { Tab::Position },
                },
            );
            actions.session.select_deck = Some((ch.ch_idx, deck.deck_idx));
        }
        row.context_menu(|ui| {
            if main_desk::in_main_mode(data) {
                ui.label(egui::RichText::new("This layer alone").small().weak());
                main_desk::main_menu_items(
                    ui,
                    &MainSource::Deck(deck.uuid.clone()),
                    None,
                    data,
                    actions,
                );
                ui.separator();
            }
            if !data.buses.is_empty() {
                super::switcher::take_menu_items(
                    ui,
                    &crate::bus::BusInputRef::Deck(deck.uuid.clone()),
                    data,
                    actions,
                );
                ui.separator();
            }
            super::deck_detail::framing_menu_items(ui, deck, actions);
            ui.separator();
            let subject = super::clipboard_menu::Subject::deck(&deck.uuid, &deck.name);
            super::clipboard_menu::items(ui, data, actions, &subject);
        });
    });
}

/// "+ Add layer": the Add Input window, adding to this channel — the same
/// kinds as a new input.
fn render_add_layer(ui: &mut egui::Ui, ch: &ChannelUIInfo) {
    if ui
        .button("+ Add layer")
        .on_hover_text("Add a layer on top of this channel — or drop files, cameras and streams from the Library on this window")
        .clicked()
    {
        super::add_input::open_for_layer(ui.ctx(), &ch.uuid);
    }
}

/// The preview and the tabs for what is selected.
fn render_editor(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    layer: Option<&DeckUIInfo>,
    open: &Open,
    data: &UIData,
    actions: &mut UIActions,
) {
    readable(ui);
    let heading = match layer {
        Some(deck) => format!("Layer {} — {}", deck.deck_idx + 1, deck.name),
        None => format!("{} — the whole channel", ch.name),
    };
    // Beside unless the operator put them under it, which is kept for every
    // show (see `window_choices`).
    let chosen: Option<bool> = ui.ctx().data(|d| d.get_temp(beside_id()));
    let beside = chosen.unwrap_or(!data.window_choices.settings_controls_below);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(heading).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (value, label, hover) in [
                (true, "Beside", "Tabs beside the picture"),
                (false, "Below", "Tabs under the picture"),
            ] {
                if ui
                    .selectable_label(beside == value, egui::RichText::new(label).small())
                    .on_hover_text(hover)
                    .clicked()
                {
                    ui.ctx().data_mut(|d| d.insert_temp(beside_id(), value));
                    actions.session.window_choices = Some(WindowChoices {
                        settings_controls_below: !value,
                    });
                }
            }
            ui.label(egui::RichText::new("Controls:").small().weak());
        });
    });
    let area = ui.available_size();
    let share: f32 = ui
        .ctx()
        .data(|d| d.get_temp(picture_share_id()))
        .unwrap_or(0.5);
    if beside {
        let width = (area.x * share).clamp(200.0, (area.x - 260.0).max(200.0));
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.allocate_ui_with_layout(
                egui::vec2(width, area.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_width(width);
                    super::deck_adjust::render_preview(
                        ui,
                        ch.ch_idx,
                        &ch.name,
                        layer,
                        area.y - 60.0,
                        data,
                        actions,
                    );
                },
            );
            let moved = splitter(ui, true, area.y, "Resize the picture");
            if moved != 0.0 && area.x > 0.0 {
                let share = (width + moved) / area.x;
                ui.ctx()
                    .data_mut(|d| d.insert_temp(picture_share_id(), share));
            }
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), area.y),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    ui.add_space(2.0);
                    render_tabs(ui, ch, layer, open, data, actions);
                },
            );
        });
    } else {
        let height = (area.y * share).clamp(120.0, (area.y - 160.0).max(120.0));
        ui.allocate_ui_with_layout(
            egui::vec2(area.x, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_min_height(height);
                super::deck_adjust::render_preview(
                    ui,
                    ch.ch_idx,
                    &ch.name,
                    layer,
                    height - 50.0,
                    data,
                    actions,
                );
            },
        );
        let moved = splitter(ui, false, area.x, "Resize the picture");
        if moved != 0.0 && area.y > 0.0 {
            let share = (height + moved) / area.y;
            ui.ctx()
                .data_mut(|d| d.insert_temp(picture_share_id(), share));
        }
        render_tabs(ui, ch, layer, open, data, actions);
    }
}

/// The editor's words sized and lit to be read at a glance — an operator
/// found its settings' names tiny and dark on the dark panel: each named as
/// large as a section was, the sections' names larger still, and every
/// line nearer the strong colour.
fn readable(ui: &mut egui::Ui) {
    use egui::{FontId, TextStyle};
    let style = ui.style_mut();
    let body = style
        .text_styles
        .get(&TextStyle::Body)
        .cloned()
        .unwrap_or_else(|| FontId::proportional(12.5));
    style.text_styles.insert(TextStyle::Small, body.clone());
    style.text_styles.insert(
        super::deck_detail::section_style(),
        FontId::new(body.size * 1.3, body.family),
    );
    let visuals = &mut style.visuals;
    let strong = visuals.widgets.active.fg_stroke.color;
    let text = &mut visuals.widgets.noninteractive.fg_stroke.color;
    *text = text.lerp_to_gamma(strong, 0.5);
    visuals.weak_text_color = None;
    visuals.weak_text_alpha = 0.8;
}

/// What a slider's row holds besides the slider: the setting's name, its
/// number and the modulation menu.
const SLIDER_ROOM: f32 = 200.0;

/// The tabs for what is selected, and the open one's controls.
fn render_tabs(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    layer: Option<&DeckUIInfo>,
    open: &Open,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.horizontal(|ui| {
        for tab in tabs_for(layer) {
            if ui
                .selectable_label(open.tab == tab, egui::RichText::new(tab.title()).strong())
                .clicked()
            {
                let mut next = open.clone();
                next.tab = tab;
                store(ui.ctx(), next);
            }
        }
    });
    ui.separator();

    // Sliders as long as the room allows, for fine moves.
    ui.spacing_mut().slider_width = (ui.available_width() - SLIDER_ROOM).clamp(100.0, 640.0);
    egui::ScrollArea::vertical()
        .id_salt("channel_settings_tab")
        .auto_shrink([false, false])
        .show(ui, |ui| match (layer, open.tab) {
            (Some(deck), Tab::Source) => render_source(ui, deck, data, actions),
            (Some(deck), Tab::Position) => render_position(ui, ch, deck, data, actions),
            (Some(deck), Tab::Playback) => {
                if let Some(vp) = &deck.video_playback {
                    render_on_air(ui, deck, vp, data, actions);
                    ui.add_space(8.0);
                    super::deck_detail::render_playback_controls(ui, deck, vp, data, actions);
                }
            }
            (None, Tab::General) => render_general(ui, ch, data, actions),
            (None, Tab::Layout) => render_layout(ui, ch, actions),
            (_, Tab::Colour) => {
                let chain = chain(ch, layer);
                for (shader, title, what) in COLOUR_EFFECTS {
                    render_single_effect(ui, &chain, shader, title, what, data, actions);
                    ui.add_space(8.0);
                }
            }
            (_, Tab::Key) => {
                ui.label(
                    egui::RichText::new(
                        "A logo or title on black? Luma key cuts the black out — or blend the \
                         channel with Screen over Main (Position tab).",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
                render_single_effect(
                    ui,
                    &chain(ch, layer),
                    "chroma_key",
                    "Chroma key",
                    "Make one colour — a green or blue screen — transparent",
                    data,
                    actions,
                );
                ui.add_space(8.0);
                render_single_effect(
                    ui,
                    &chain(ch, layer),
                    "luma_key",
                    "Luma key",
                    "Make dark (or, inverted, bright) parts transparent",
                    data,
                    actions,
                );
            }
            (_, Tab::Effects) => render_effects(ui, &chain(ch, layer), data, actions),
            _ => {}
        });
}

/// An effect chain being edited: whose it is and what is in it.
struct Chain<'a> {
    target: EffectTarget,
    /// The chain's MIDI address, `deck/<uuid>` or `ch/<uuid>`.
    owner: String,
    effects: &'a [EffectInfo],
}

fn chain<'a>(ch: &'a ChannelUIInfo, layer: Option<&'a DeckUIInfo>) -> Chain<'a> {
    match layer {
        Some(deck) => Chain {
            target: EffectTarget::Deck(deck.uuid.clone()),
            owner: format!("deck/{}", deck.uuid),
            effects: &deck.effects,
        },
        None => Chain {
            target: EffectTarget::Channel(ch.uuid.clone()),
            owner: format!("ch/{}", ch.uuid),
            effects: &ch.effects,
        },
    }
}

/// The Colour tab: vMix's Colour Correction (lift / gamma / gain is Colour
/// balance here) and Colour Adjust.
const COLOUR_EFFECTS: [(&str, &str, &str); 3] = [
    (
        "color_correction",
        "Colour correction",
        "Brightness, contrast, saturation, hue and gamma",
    ),
    (
        "color_balance",
        "Colour balance",
        "Red, green and blue of the shadows, midtones and highlights — lift, gamma, gain",
    ),
    (
        "levels",
        "Levels",
        "Black and white points in and out, with a gamma curve",
    ),
];

/// Effects with a tab of their own, left out of the Effects list.
const OWN_TABS: [&str; 7] = [
    "crop",
    "transform",
    "color_correction",
    "color_balance",
    "levels",
    "chroma_key",
    "luma_key",
];

/// The layer's source: how its picture is scaled into the frame — Fit keeps
/// all of it with transparent bars, Fill fills the frame and cuts the edges
/// — how often and how large it is drawn, and a generator's own parameters.
fn render_source(ui: &mut egui::Ui, deck: &DeckUIInfo, data: &UIData, actions: &mut UIActions) {
    ui.label(egui::RichText::new(&deck.name).strong());
    if deck.scaling_mode.is_some() {
        ui.label(
            egui::RichText::new(
                "Scale — Fit: the whole picture, the rest transparent · Fill: fills the frame, \
                 cutting the edges · Stretch: fills it out of shape · Center: at its own size",
            )
            .small()
            .weak(),
        );
    }
    super::deck_detail::render_source_settings(ui, deck, data, actions);
    super::deck_detail::render_generator_params(
        ui,
        deck,
        &format!("settings_{}", deck.uuid),
        data,
        actions,
    );
}

fn render_position(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    deck: &DeckUIInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    if ui
        .button("Reset all")
        .on_hover_text("Crop, border, transform, opacity and blend back to how a new layer starts")
        .clicked()
    {
        for (uuid, ..) in deck
            .effects
            .iter()
            .filter(|(_, name, ..)| name == "crop" || name == "transform")
        {
            actions.commands.push(EngineCommand::ResetEffectParams {
                effect_uuid: uuid.clone(),
                names: Vec::new(),
            });
        }
        actions.commands.push(EngineCommand::SetDeckOpacity {
            deck_uuid: deck.uuid.clone(),
            opacity: 1.0,
        });
        actions.commands.push(EngineCommand::SetDeckBlendMode {
            deck_uuid: deck.uuid.clone(),
            mode: BlendMode::Normal,
        });
    }
    let mut opacity = deck.opacity;
    let slider = ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0).text("Opacity"));
    if slider.dragged() {
        actions.session.gesture_active = true;
    }
    if slider.changed() {
        actions.commands.push(EngineCommand::SetDeckOpacity {
            deck_uuid: deck.uuid.clone(),
            opacity,
        });
    }
    // A layer blends with the layers below it in its channel; Layer 1 has
    // none, so a blend there does nothing — the channel's own blend, over
    // Main or the channels below, is what it needs.
    let bottom = ch.decks.first().is_some_and(|d| d.uuid == deck.uuid);
    ui.horizontal(|ui| {
        ui.label("Blend with layers below");
        ui.add_enabled_ui(!bottom, |ui| {
            let mut chosen = deck.blend_mode;
            egui::ComboBox::from_id_salt("layer_blend")
                .selected_text(chosen.label())
                .show_ui(ui, |ui| {
                    for mode in BlendMode::all() {
                        ui.selectable_value(&mut chosen, *mode, mode.label());
                    }
                });
            if chosen != deck.blend_mode {
                actions.commands.push(EngineCommand::SetDeckBlendMode {
                    deck_uuid: deck.uuid.clone(),
                    mode: chosen,
                });
            }
        })
        .response
        .on_disabled_hover_text(
            "Layer 1 has nothing below it in this channel — blend the channel instead",
        );
    });
    render_channel_blend(ui, ch, data, actions);
    render_copy_paste(ui, deck, actions);
    super::deck_detail::render_framing(ui, deck, data, actions);
}

/// Copy a layer's crop, border and transform together, paste them onto
/// another — of this channel or any other. Each part also has its own Copy
/// and Paste.
fn render_copy_paste(ui: &mut egui::Ui, deck: &DeckUIInfo, actions: &mut UIActions) {
    use super::deck_detail::{clip_holds, copy_framing, paste_framing, FramingPart};
    ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new("Copy position").small())
            .on_hover_text("Copy this layer's crop, border and transform")
            .clicked()
        {
            copy_framing(ui.ctx(), deck, &FramingPart::ALL);
        }
        let held = FramingPart::ALL.iter().any(|p| clip_holds(ui.ctx(), *p));
        if ui
            .add_enabled(held, egui::Button::new("Paste position").small())
            .on_hover_text("Give this layer the crop, border and transform copied")
            .clicked()
        {
            paste_framing(ui.ctx(), deck, &FramingPart::ALL, actions);
        }
    });
}

/// A layout: where each layer goes, in layer order — centre (in frame
/// units, +y down) and size. Layers past the end are left where they are.
struct Preset {
    name: &'static str,
    places: &'static [(f32, f32, f32)],
}

const PRESETS: [Preset; 6] = [
    Preset {
        name: "Full screen",
        places: &[(0.0, 0.0, 1.0); 10],
    },
    Preset {
        name: "Side by side",
        places: &[(-0.25, 0.0, 0.5), (0.25, 0.0, 0.5)],
    },
    Preset {
        name: "Top and bottom",
        places: &[(0.0, -0.25, 0.5), (0.0, 0.25, 0.5)],
    },
    Preset {
        name: "PiP top right",
        places: &[(0.0, 0.0, 1.0), (0.3, -0.3, 0.33)],
    },
    Preset {
        name: "PiP bottom right",
        places: &[(0.0, 0.0, 1.0), (0.3, 0.3, 0.33)],
    },
    Preset {
        name: "2 × 2 grid",
        places: &[
            (-0.25, -0.25, 0.5),
            (0.25, -0.25, 0.5),
            (-0.25, 0.25, 0.5),
            (0.25, 0.25, 0.5),
        ],
    },
];

/// The channel's Layout tab: vMix's layer templates. Each places the layers
/// by their transform; a layer without one is left alone.
fn render_layout(ui: &mut egui::Ui, ch: &ChannelUIInfo, actions: &mut UIActions) {
    ui.label(
        egui::RichText::new(
            "Put the layers in place at once — Layer 1 goes first. Then drag them to fine-tune.",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        for preset in &PRESETS {
            if preset_button(ui, preset).clicked() {
                for (deck, &(x, y, scale)) in ch.decks.iter().zip(preset.places) {
                    let Some((uuid, ..)) = deck.effects.iter().find(|(_, n, ..)| n == "transform")
                    else {
                        continue;
                    };
                    for (name, value) in [
                        ("pos_x", x),
                        ("pos_y", y),
                        ("scale", scale),
                        ("rotation", 0.0),
                    ] {
                        actions.commands.push(EngineCommand::SetEffectParam {
                            effect_uuid: uuid.clone(),
                            name: name.to_string(),
                            value: crate::params::ParamValue::Float(value),
                        });
                    }
                }
            }
        }
    });
    let without: Vec<&str> = ch
        .decks
        .iter()
        .filter(|d| !d.effects.iter().any(|(_, n, ..)| n == "transform"))
        .map(|d| d.name.as_str())
        .collect();
    if !without.is_empty() {
        ui.label(
            egui::RichText::new(format!(
                "Left alone, having no Transform: {} — switch it on under the layer's Position",
                without.join(", ")
            ))
            .small()
            .weak(),
        );
    }
}

/// A layout as a small picture of where its layers go.
fn preset_button(ui: &mut egui::Ui, preset: &Preset) -> egui::Response {
    let size = egui::vec2(96.0, 54.0);
    let (rect, response) =
        ui.allocate_exact_size(size + egui::vec2(0.0, 16.0), egui::Sense::click());
    let frame = egui::Rect::from_min_size(rect.min, size);
    let hovered = response.hovered();
    let painter = ui.painter();
    painter.rect_filled(frame, 3.0, egui::Color32::from_rgb(22, 22, 30));
    painter.rect_stroke(
        frame,
        3.0,
        egui::Stroke::new(
            if hovered { 2.0 } else { 1.0 },
            egui::Color32::from_rgb(110, 110, 140),
        ),
        egui::StrokeKind::Inside,
    );
    // Only as many as the layout places, and the full-screen one as one.
    let places = if preset.places.iter().all(|p| *p == (0.0, 0.0, 1.0)) {
        &preset.places[..1]
    } else {
        preset.places
    };
    for (i, &(x, y, scale)) in places.iter().enumerate() {
        let centre = frame.center() + egui::vec2(x * frame.width(), y * frame.height());
        let layer = egui::Rect::from_center_size(centre, frame.size() * scale).shrink(1.5);
        painter.rect_filled(layer, 2.0, channel_color(i).linear_multiply(0.7));
    }
    painter.text(
        egui::pos2(rect.center().x, frame.bottom() + 2.0),
        egui::Align2::CENTER_TOP,
        preset.name,
        egui::FontId::proportional(11.0),
        ui.visuals().text_color(),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, preset.name));
    response
}

/// The channel's blend where it counts: an overlay's over Main, or in the
/// layered mixer over the channels below. A logo on black shows only its
/// light parts over Main with Screen or Add.
fn render_channel_blend(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    let label = if !main_desk::in_main_mode(data) {
        "Blend over the channels below"
    } else if ch.overlay {
        "Blend over Main"
    } else {
        return;
    };
    ui.horizontal(|ui| {
        ui.label(label);
        let mut chosen = ch.blend_mode;
        egui::ComboBox::from_id_salt("channel_blend_over")
            .selected_text(chosen.label())
            .show_ui(ui, |ui| {
                for mode in BlendMode::all() {
                    ui.selectable_value(&mut chosen, *mode, mode.label());
                }
            })
            .response
            .on_hover_text("For the whole channel — Screen or Add drop a black background");
        if chosen != ch.blend_mode {
            actions.commands.push(EngineCommand::SetChannelBlendMode {
                channel_uuid: ch.uuid.clone(),
                mode: chosen,
            });
        }
    });
}

fn render_general(ui: &mut egui::Ui, ch: &ChannelUIInfo, data: &UIData, actions: &mut UIActions) {
    ui.horizontal(|ui| {
        ui.label("Name");
        ui.label(egui::RichText::new(&ch.name).strong());
        if ui.small_button("Rename…").clicked() {
            super::rename::open(ui.ctx(), ch);
        }
    });
    ui.add_space(4.0);
    if main_desk::in_main_mode(data) {
        ui.horizontal(|ui| {
            let button = |text: &str| {
                egui::Button::new(egui::RichText::new(text).size(15.0).strong())
                    .min_size(egui::vec2(96.0, 30.0))
            };
            if ch.overlay {
                let on = main_desk::overlay_on(data, ch);
                let (text, fill) = if on {
                    ("Overlay ON", ON_MAIN_RED)
                } else {
                    ("Overlay OFF", egui::Color32::from_rgb(60, 60, 70))
                };
                if ui.add(button(text).fill(fill)).clicked() {
                    main_desk::take_channel(ch, data, actions, false);
                }
                if ui.add(button("Fade")).clicked() {
                    main_desk::take_channel(ch, data, actions, true);
                }
            } else {
                let on_main = main_desk::channel_on_main(data, ch);
                if ui.add_enabled(!on_main, button("Cut to Main")).clicked() {
                    main_desk::take_channel(ch, data, actions, false);
                }
                if ui
                    .add_enabled(!on_main && data.main.fade.is_none(), button("Fade to Main"))
                    .clicked()
                {
                    main_desk::take_channel(ch, data, actions, true);
                }
                if on_main {
                    ui.label(
                        egui::RichText::new(" ON MAIN ")
                            .strong()
                            .color(egui::Color32::WHITE)
                            .background_color(ON_MAIN_RED),
                    );
                } else if main_desk::next_channel(data).is_some_and(|n| n.uuid == ch.uuid) {
                    ui.label(
                        egui::RichText::new(" NEXT ")
                            .strong()
                            .color(egui::Color32::WHITE)
                            .background_color(NEXT_GREEN),
                    );
                }
            }
        });
        let mut overlay = ch.overlay;
        if ui
            .checkbox(
                &mut overlay,
                "Overlay — switched on and off over Main (logo, lower third)",
            )
            .changed()
        {
            actions.commands.push(EngineCommand::SetChannelOverlay {
                channel_uuid: ch.uuid.clone(),
                overlay,
            });
        }
    }
    // Opacity counts in the layered mixer, and for an overlay in Main mode.
    if !main_desk::in_main_mode(data) || ch.overlay {
        let mut opacity = ch.opacity;
        let slider = ui.add(egui::Slider::new(&mut opacity, 0.0..=1.0).text("Opacity"));
        if slider.dragged() {
            actions.session.gesture_active = true;
        }
        if slider.changed() {
            actions.commands.push(EngineCommand::SetChannelOpacity {
                channel_uuid: ch.uuid.clone(),
                opacity,
            });
        }
        ui.horizontal(|ui| {
            ui.label("Blend");
            let mut chosen = ch.blend_mode;
            egui::ComboBox::from_id_salt("channel_blend")
                .selected_text(chosen.label())
                .show_ui(ui, |ui| {
                    for mode in BlendMode::all() {
                        ui.selectable_value(&mut chosen, *mode, mode.label());
                    }
                });
            if chosen != ch.blend_mode {
                actions.commands.push(EngineCommand::SetChannelBlendMode {
                    channel_uuid: ch.uuid.clone(),
                    mode: chosen,
                });
            }
        });
    }
    if !data.buses.is_empty() {
        ui.add_space(8.0);
        super::deck_detail::render_take_frame(ui, &ch.uuid, data, actions);
    }
}

/// A tab holding one effect: its controls if the chain has it, a button to
/// add it if not.
fn render_single_effect(
    ui: &mut egui::Ui,
    chain: &Chain<'_>,
    shader: &str,
    title: &str,
    what: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    let Some(effect) = chain.effects.iter().find(|(_, name, ..)| name == shader) else {
        ui.label(egui::RichText::new(what).small().weak());
        if ui.button(format!("+ {title}")).clicked() {
            actions.commands.push(EngineCommand::AddEffect {
                target: chain.target.clone(),
                shader_name: shader.to_string(),
            });
        }
        return;
    };
    ui.horizontal(|ui| {
        let mut on = effect.2;
        if ui
            .checkbox(&mut on, super::deck_detail::section_title(ui, title))
            .on_hover_text("On / bypassed")
            .changed()
        {
            actions.commands.push(EngineCommand::ToggleEffect {
                effect_uuid: effect.0.clone(),
            });
        }
        super::deck_detail::reset_button(
            ui,
            &effect.0,
            Vec::new(),
            &format!("{title} back as it was"),
            actions,
        );
        if ui
            .small_button("Remove")
            .on_hover_text(format!("Take {title} off"))
            .clicked()
        {
            actions.commands.push(EngineCommand::RemoveEffect {
                effect_uuid: effect.0.clone(),
            });
        }
    });
    super::deck_detail::render_effect_controls(ui, &chain.owner, effect, data, actions);
}

/// Every other effect on the chain, in the order they run, each with its
/// controls; and adding one.
fn render_effects(ui: &mut egui::Ui, chain: &Chain<'_>, data: &UIData, actions: &mut UIActions) {
    render_add_effect(ui, chain, data, actions);
    ui.add_space(4.0);
    let listed: Vec<usize> = (0..chain.effects.len())
        .filter(|&i| !OWN_TABS.contains(&chain.effects[i].1.as_str()))
        .collect();
    if listed.is_empty() {
        ui.label(
            egui::RichText::new(
                "No effects yet — add one, or drag one from the Library onto this window",
            )
            .small()
            .weak(),
        );
    }
    for (k, &i) in listed.iter().enumerate() {
        let effect = &chain.effects[i];
        ui.push_id(&effect.0, |ui| {
            egui::Frame::default()
                .inner_margin(6.0)
                .corner_radius(4.0)
                .fill(ui.visuals().faint_bg_color)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let mut on = effect.2;
                        if ui
                            .checkbox(&mut on, egui::RichText::new(pretty(&effect.1)).strong())
                            .on_hover_text("On / bypassed")
                            .changed()
                        {
                            actions.commands.push(EngineCommand::ToggleEffect {
                                effect_uuid: effect.0.clone(),
                            });
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .small_button("Remove")
                                .on_hover_text("Take this effect off")
                                .clicked()
                            {
                                actions.commands.push(EngineCommand::RemoveEffect {
                                    effect_uuid: effect.0.clone(),
                                });
                            }
                            super::deck_detail::reset_button(
                                ui,
                                &effect.0,
                                Vec::new(),
                                "This effect's settings back as they were",
                                actions,
                            );
                            // The effects run top to bottom; move one past its
                            // neighbour in the list.
                            let later = listed.get(k + 1).copied();
                            if ui
                                .add_enabled(later.is_some(), egui::Button::new("⏷").small())
                                .on_hover_text("Run later")
                                .clicked()
                            {
                                if let Some(to) = later {
                                    actions.commands.push(EngineCommand::MoveEffect {
                                        target: chain.target.clone(),
                                        from_idx: i,
                                        to_idx: to,
                                    });
                                }
                            }
                            let earlier = k.checked_sub(1).map(|j| listed[j]);
                            if ui
                                .add_enabled(earlier.is_some(), egui::Button::new("⏶").small())
                                .on_hover_text("Run earlier")
                                .clicked()
                            {
                                if let Some(to) = earlier {
                                    actions.commands.push(EngineCommand::MoveEffect {
                                        target: chain.target.clone(),
                                        from_idx: i,
                                        to_idx: to,
                                    });
                                }
                            }
                        });
                    });
                    super::deck_detail::render_effect_controls(
                        ui,
                        &chain.owner,
                        effect,
                        data,
                        actions,
                    );
                });
        });
        ui.add_space(4.0);
    }
}

fn effect_search_id() -> egui::Id {
    egui::Id::new("channel_settings_effect_search")
}

/// The "+ Add effect" menu: every effect in the Library, found by typing
/// part of its name.
fn render_add_effect(ui: &mut egui::Ui, chain: &Chain<'_>, data: &UIData, actions: &mut UIActions) {
    ui.menu_button("+ Add effect", |ui| {
        let mut search: String = ui
            .ctx()
            .data(|d| d.get_temp(effect_search_id()))
            .unwrap_or_default();
        ui.add(
            egui::TextEdit::singleline(&mut search)
                .hint_text("Find an effect")
                .desired_width(200.0),
        );
        let needle = search.to_lowercase().replace(' ', "_");
        egui::ScrollArea::vertical()
            .max_height(320.0)
            .show(ui, |ui| {
                let mut names: Vec<&str> = data
                    .filters
                    .iter()
                    .map(|(name, _)| name.as_str())
                    .filter(|name| needle.is_empty() || name.contains(&needle))
                    .collect();
                names.sort_unstable();
                for name in names {
                    if ui.button(pretty(name)).clicked() {
                        actions.commands.push(EngineCommand::AddEffect {
                            target: chain.target.clone(),
                            shader_name: name.to_string(),
                        });
                        ui.close();
                    }
                }
            });
        ui.ctx()
            .data_mut(|d| d.insert_temp(effect_search_id(), search));
    });
}

/// A shader's file name as a title: `color_correction` → "Color correction".
pub(super) fn pretty(name: &str) -> String {
    let spaced = name.replace('_', " ");
    let mut chars = spaced.chars();
    chars
        .next()
        .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// One frame of the window alone, on a bare context.
    fn frame(ctx: &egui::Context, data: &UIData) -> UIActions {
        let mut actions = UIActions::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            render_channel_settings(ui.ctx(), ui.max_rect(), data, &mut actions);
        });
        output.textures_delta.clear();
        actions
    }

    fn open_on(ctx: &egui::Context) -> Option<(String, Item)> {
        current(ctx).map(|o| (o.channel, o.item))
    }

    #[test]
    fn a_channel_of_several_layers_opens_on_the_channel() {
        let data = UIData::test_fixture();
        let ctx = egui::Context::default();
        open_channel(&ctx, &data.channels[0]);
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx),
            Some((data.channels[0].uuid.clone(), Item::Channel))
        );
    }

    #[test]
    fn a_channel_of_one_picture_opens_on_that_picture() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks.truncate(1);
        let ctx = egui::Context::default();
        open_channel(&ctx, &data.channels[0]);
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx).map(|o| o.1),
            Some(Item::Layer(data.channels[0].decks[0].uuid.clone()))
        );
    }

    #[test]
    fn the_window_closes_when_its_channel_is_gone() {
        let data = UIData::test_fixture();
        let ctx = egui::Context::default();
        frame(&ctx, &data);
        open_layer(&ctx, "nosuchdk");
        frame(&ctx, &data);
        assert!(open_on(&ctx).is_none());
    }

    #[test]
    fn a_deleted_layer_falls_back_to_its_channel() {
        let mut data = UIData::test_fixture();
        let ctx = egui::Context::default();
        let gone = data.channels[0].decks[1].uuid.clone();
        open_layer(&ctx, &gone);
        frame(&ctx, &data);
        data.channels[0].decks.remove(1);
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx),
            Some((data.channels[0].uuid.clone(), Item::Channel))
        );
    }

    #[test]
    fn the_window_follows_the_selection() {
        let mut data = UIData::test_fixture();
        data.selected_deck = Some((0, 0));
        let ctx = egui::Context::default();
        frame(&ctx, &data);
        open_layer(&ctx, "a0000001");
        frame(&ctx, &data);
        data.selected_deck = Some((0, 1));
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx).map(|o| o.1),
            Some(Item::Layer("a0000002".into())),
            "another card of the channel"
        );
        data.selected_deck = Some((1, 0));
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx).map(|o| o.1),
            Some(Item::Layer("b0000001".into())),
            "a card of another channel"
        );
        data.selected_deck = None;
        data.selected_channel = Some(0);
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx),
            Some((data.channels[0].uuid.clone(), Item::Channel)),
            "a channel"
        );
    }

    #[test]
    fn opening_is_not_undone_by_the_old_selection() {
        let mut data = UIData::test_fixture();
        data.selected_deck = Some((0, 0));
        let ctx = egui::Context::default();
        frame(&ctx, &data);
        open_layer(&ctx, "a0000002");
        frame(&ctx, &data);
        frame(&ctx, &data);
        assert_eq!(
            open_on(&ctx).map(|o| o.1),
            Some(Item::Layer("a0000002".into()))
        );
    }

    #[test]
    fn selecting_does_not_open_a_closed_window() {
        let mut data = UIData::test_fixture();
        data.selected_deck = Some((0, 0));
        let ctx = egui::Context::default();
        frame(&ctx, &data);
        data.selected_deck = Some((0, 1));
        frame(&ctx, &data);
        assert!(open_on(&ctx).is_none());
    }

    struct Seen {
        commands: Vec<EngineCommand>,
        select_deck: Option<(usize, usize)>,
        window_choices: Option<WindowChoices>,
    }

    /// The window open on `open`, in a harness that keeps what it asks for.
    fn harness(
        data: UIData,
        open: impl Fn(&egui::Context) + 'static,
    ) -> egui_kittest::Harness<'static, Seen> {
        harness_of_size(data, open, egui::vec2(1400.0, 900.0))
    }

    /// As [`harness`], on a screen of `size`.
    fn harness_of_size(
        data: UIData,
        open: impl Fn(&egui::Context) + 'static,
        size: egui::Vec2,
    ) -> egui_kittest::Harness<'static, Seen> {
        let data = std::rc::Rc::new(data);
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(size)
            .build_ui_state(
                move |ui, seen: &mut Seen| {
                    if !opened {
                        open(ui.ctx());
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                    seen.commands.extend(actions.commands);
                    seen.select_deck = seen.select_deck.or(actions.session.select_deck);
                    seen.window_choices = actions.session.window_choices.or(seen.window_choices);
                },
                Seen {
                    commands: Vec::new(),
                    select_deck: None,
                    window_choices: None,
                },
            );
        harness.run();
        harness
    }

    fn on_channel_a(data: &UIData) -> impl Fn(&egui::Context) + 'static {
        let ch = data.channels[0].clone();
        move |ctx: &egui::Context| open_channel(ctx, &ch)
    }

    /// A mask on an effect: picked under the effect, then drawn by dragging
    /// on the picture.
    #[test]
    fn a_mask_is_picked_under_an_effect_and_drawn_on_the_picture() {
        use crate::engine::value::mask::MaskShape;
        let data = UIData::test_fixture();
        let deck = data.channels[0].decks[0].clone();
        let effect = deck.effects[0].0.clone();
        let mut harness = {
            let uuid = deck.uuid.clone();
            harness(data, move |ctx: &egui::Context| open_layer(ctx, &uuid))
        };
        harness.get_by_label("Effects").click();
        harness.run();
        harness.get_by_label("Rectangle").click();
        harness.run();
        let masks = |harness: &egui_kittest::Harness<'static, Seen>| -> Vec<MaskShape> {
            harness
                .state()
                .commands
                .iter()
                .filter_map(|c| match c {
                    EngineCommand::SetEffectMask {
                        effect_uuid,
                        mask: Some(mask),
                    } if *effect_uuid == effect => Some(mask.shape.clone()),
                    _ => None,
                })
                .collect()
        };
        assert!(
            matches!(masks(&harness).as_slice(), [MaskShape::Rectangle { .. }]),
            "a rectangle in the middle to start with: {:?}",
            masks(&harness)
        );

        // Drag from a quarter of the way in to three quarters across and
        // half way down.
        let picture = harness.get_by_label("Adjust preview").rect();
        let at = |x: f32, y: f32| {
            picture.left_top() + egui::vec2(x * picture.width(), y * picture.height())
        };
        harness.hover_at(at(0.25, 0.25));
        harness.run();
        harness.drag_at(at(0.25, 0.25));
        harness.run();
        for step in 1..=5 {
            let t = step as f32 / 5.0;
            harness.hover_at(at(0.25 + 0.5 * t, 0.25 + 0.25 * t));
            harness.run();
        }
        harness.event(egui::Event::PointerButton {
            pos: at(0.75, 0.5),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        match masks(&harness).last() {
            Some(MaskShape::Rectangle {
                x,
                y,
                width,
                height,
            }) => {
                let near = |a: f32, b: f32| (a - b).abs() < 0.03;
                assert!(
                    near(*x, 0.25) && near(*y, 0.25) && near(*width, 0.5) && near(*height, 0.25),
                    "drawn where dragged: {x} {y} {width} {height}"
                );
            }
            other => panic!("the drawn rectangle: {other:?}"),
        }
        assert!(
            !harness
                .state()
                .commands
                .iter()
                .any(|c| matches!(c, EngineCommand::SetEffectParam { name, .. } if name.starts_with("pos_"))),
            "the drag drew the mask, and did not move the layer"
        );
    }

    /// As vMix numbers them: Layer 1, at the back, first.
    #[test]
    fn the_layers_are_listed_back_first() {
        let data = UIData::test_fixture();
        let names: Vec<String> = data.channels[0]
            .decks
            .iter()
            .map(|d| d.name.clone())
            .collect();
        let harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        let y = |name: &str| harness.get_by_label(&format!("{name} layer")).rect().top();
        assert!(y(&names[0]) < y(&names[1]));
    }

    #[test]
    fn clicking_a_layer_edits_it() {
        let data = UIData::test_fixture();
        let deck = data.channels[0].decks[1].clone();
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        harness
            .get_by_label(&format!("{} layer", deck.name))
            .click();
        harness.run();
        assert_eq!(harness.state().select_deck, Some((0, deck.deck_idx)));
        assert!(
            harness.query_by_label("Position").is_some(),
            "the layer's tabs"
        );
        assert!(harness
            .query_by_label_contains(&format!("Layer {}", deck.deck_idx + 1))
            .is_some());
    }

    #[test]
    fn a_layer_moves_forward_and_back() {
        let data = UIData::test_fixture();
        let ch = data.channels[0].clone();
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        // Layer 1 (listed first) can only go forward.
        harness.get_all_by_label("⏷").next().unwrap().click();
        harness.run();
        let moved = harness.state().commands.iter().find_map(|c| match c {
            EngineCommand::MoveDeck {
                deck_uuid,
                before_deck_uuid,
                ..
            } => Some((deck_uuid.clone(), before_deck_uuid.clone())),
            _ => None,
        });
        assert_eq!(
            moved,
            Some((
                ch.decks[0].uuid.clone(),
                ch.decks.get(2).map(|d| d.uuid.clone())
            )),
            "Layer 1 goes over Layer 2"
        );
    }

    #[test]
    fn a_layer_is_hidden_and_deleted_from_its_row() {
        let data = UIData::test_fixture();
        let front = data.channels[0].decks[0].uuid.clone();
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        harness
            .get_all_by_role(egui::accesskit::Role::CheckBox)
            .next()
            .unwrap()
            .click();
        harness.run();
        harness.get_all_by_label("🗑").next().unwrap().click();
        harness.run();
        let commands = &harness.state().commands;
        assert!(commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetDeckMute { deck_uuid, mute: true } if *deck_uuid == front
        )));
        assert!(commands.iter().any(|c| matches!(
            c,
            EngineCommand::RemoveDeck { deck_uuid } if *deck_uuid == front
        )));
    }

    #[test]
    fn the_colour_tab_adds_colour_correction_to_the_layer() {
        let data = UIData::test_fixture();
        let deck = data.channels[0].decks[0].uuid.clone();
        let open = deck.clone();
        let mut harness = harness(data, move |ctx| open_layer(ctx, &open));
        harness.get_by_label("Colour").click();
        harness.run();
        harness.get_by_label("+ Colour correction").click();
        harness.run();
        assert!(harness.state().commands.iter().any(|c| matches!(
            c,
            EngineCommand::AddEffect {
                target: EffectTarget::Deck(d),
                shader_name,
            } if *d == deck && shader_name == "color_correction"
        )));
    }

    #[test]
    fn an_effect_is_added_to_the_channel_from_the_list() {
        let data = UIData::test_fixture();
        let ch = data.channels[0].uuid.clone();
        let filter = data.filters[0].0.clone();
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        harness.get_by_label("Effects").click();
        harness.run();
        harness.get_by_label("+ Add effect").click();
        harness.run();
        // The menu is drawn last, over an effect of that name in the list.
        harness
            .get_all_by_label(&pretty(&filter))
            .last()
            .unwrap()
            .click();
        harness.run();
        assert!(harness.state().commands.iter().any(|c| matches!(
            c,
            EngineCommand::AddEffect {
                target: EffectTarget::Channel(u),
                shader_name,
            } if *u == ch && *shader_name == filter
        )));
    }

    #[test]
    fn a_layout_places_the_layers_by_their_transforms() {
        let mut data = UIData::test_fixture();
        for (i, deck) in data.channels[0].decks.iter_mut().enumerate() {
            deck.effects.push(transform(&format!("fxt{i}")));
        }
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        harness.get_by_label("Layout").click();
        harness.run();
        harness.get_by_label("Side by side").click();
        harness.run();
        let set = |uuid: &str, name: &str| {
            harness.state().commands.iter().rev().find_map(|c| match c {
                EngineCommand::SetEffectParam {
                    effect_uuid,
                    name: n,
                    value: crate::params::ParamValue::Float(v),
                } if effect_uuid == uuid && n == name => Some(*v),
                _ => None,
            })
        };
        assert_eq!(set("fxt0", "pos_x"), Some(-0.25), "Layer 1 on the left");
        assert_eq!(set("fxt1", "pos_x"), Some(0.25), "Layer 2 on the right");
        assert_eq!(set("fxt1", "scale"), Some(0.5));
    }

    #[test]
    fn a_position_is_copied_from_one_layer_and_pasted_on_another() {
        let mut data = UIData::test_fixture();
        let mut from = transform("fxfrom");
        from.3.params[0].value = crate::params::ParamValue::Float(0.4);
        data.channels[0].decks[0].effects.push(from);
        data.channels[1].decks[0].effects.push(transform("fxto"));
        let first = data.channels[0].decks[0].uuid.clone();
        let second = data.channels[1].decks[0].uuid.clone();
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 900.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        open_layer(&harness.ctx, &first);
        harness.run();
        harness.get_by_label("Copy position").click();
        harness.run();
        open_layer(&harness.ctx, &second);
        harness.run();
        harness.get_by_label("Paste position").click();
        harness.run();
        assert!(harness.state().iter().any(|c| matches!(
            c,
            EngineCommand::SetEffectParam {
                effect_uuid,
                name,
                value: crate::params::ParamValue::Float(v),
            } if effect_uuid == "fxto" && name == "pos_x" && (*v - 0.4).abs() < 1e-6
        )));
    }

    fn transform(uuid: &str) -> EffectInfo {
        use crate::usecases::ui::{ParamUIInfo, ShaderParamsUI};
        let float = |name: &str, v: f32| ParamUIInfo {
            name: name.into(),
            label: None,
            value: crate::params::ParamValue::Float(v),
            min: None,
            max: None,
        };
        (
            uuid.into(),
            "transform".into(),
            true,
            ShaderParamsUI {
                shader_name: "transform".into(),
                params: vec![
                    float("pos_x", 0.0),
                    float("pos_y", 0.0),
                    float("scale", 1.0),
                    float("rotation", 0.0),
                ],
            },
        )
    }

    /// A crop as new decks have it, border settings and all.
    fn crop(uuid: &str, border: bool) -> EffectInfo {
        use crate::params::ParamValue;
        use crate::usecases::ui::{ParamUIInfo, ShaderParamsUI};
        let param = |name: &str, value: ParamValue| ParamUIInfo {
            name: name.into(),
            label: (name == "border_width").then(|| "Thickness".to_string()),
            value,
            min: Some(0.0),
            max: Some(1.0),
        };
        (
            uuid.into(),
            "crop".into(),
            true,
            ShaderParamsUI {
                shader_name: "crop".into(),
                params: vec![
                    param("crop_left", ParamValue::Float(0.0)),
                    param("crop_right", ParamValue::Float(1.0)),
                    param("border_mode", ParamValue::Bool(border)),
                    param("border_color", ParamValue::Color([1.0, 1.0, 1.0, 1.0])),
                    param("border_width", ParamValue::Float(6.0)),
                ],
            },
        )
    }

    fn layer_harness(data: UIData) -> egui_kittest::Harness<'static, Seen> {
        let deck = data.channels[0].decks[0].uuid.clone();
        harness(data, move |ctx| open_layer(ctx, &deck))
    }

    #[test]
    fn a_layer_gets_a_border_from_its_position_tab() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0]
            .effects
            .push(crop("fxcrop", false));
        let mut harness = layer_harness(data);
        harness
            .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Border")
            .click();
        harness.run();
        assert!(harness.state().commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetEffectParam {
                effect_uuid,
                name,
                value: crate::params::ParamValue::Bool(true),
            } if effect_uuid == "fxcrop" && name == "border_mode"
        )));
    }

    #[test]
    fn a_border_shows_its_settings_only_when_on() {
        for on in [false, true] {
            let mut data = UIData::test_fixture();
            data.channels[0].decks[0].effects.push(crop("fxcrop", on));
            let harness = layer_harness(data);
            assert_eq!(
                harness.query_by_label_contains("Thickness").is_some(),
                on,
                "border {on}"
            );
        }
    }

    #[test]
    fn crop_and_border_reset_apart_and_reset_all_resets_everything() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0].effects.push(crop("fxcrop", true));
        data.channels[0].decks[0].effects.push(transform("fxtrans"));
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = layer_harness(data);
        let resets: Vec<_> = harness.get_all_by_label("Reset").collect();
        assert_eq!(resets.len(), 3, "crop, border, transform");
        resets[0].click();
        harness.run();
        resets_of(&harness, "fxcrop", |names| {
            assert!(names.contains(&"crop_left".to_string()));
            assert!(
                !names.iter().any(|n| n.starts_with("border_")),
                "not the border"
            );
        });
        harness.get_all_by_label("Reset").nth(1).unwrap().click();
        harness.run();
        resets_of(&harness, "fxcrop", |names| {
            assert!(names.iter().all(|n| n.starts_with("border_")), "{names:?}");
        });
        harness.get_by_label("Reset all").click();
        harness.run();
        let commands = &harness.state().commands;
        for fx in ["fxcrop", "fxtrans"] {
            assert!(commands.iter().any(|c| matches!(
                c,
                EngineCommand::ResetEffectParams { effect_uuid, names } if effect_uuid == fx && names.is_empty()
            )));
        }
        assert!(commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetDeckOpacity { deck_uuid, opacity } if *deck_uuid == deck && (*opacity - 1.0).abs() < 1e-6
        )));
    }

    /// The last reset asked for on `effect`, checked by `check`.
    fn resets_of(
        harness: &egui_kittest::Harness<'_, Seen>,
        effect: &str,
        check: impl Fn(&[String]),
    ) {
        let names = harness
            .state()
            .commands
            .iter()
            .rev()
            .find_map(|c| match c {
                EngineCommand::ResetEffectParams { effect_uuid, names }
                    if effect_uuid == effect =>
                {
                    Some(names.clone())
                }
                _ => None,
            })
            .expect("a reset");
        check(&names);
    }

    #[test]
    fn the_dividers_resize_the_list_and_the_picture() {
        let data = UIData::test_fixture();
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        for what in ["Resize the layer list", "Resize the picture"] {
            let at = harness.get_by_label(what).rect().center();
            harness.hover_at(at);
            harness.drag_at(at);
            harness.run();
            harness.hover_at(at + egui::vec2(60.0, 60.0));
            harness.run();
            harness.drop_at(at + egui::vec2(60.0, 60.0));
            harness.run();
        }
        let list: Option<f32> = harness.ctx.data(|d| d.get_temp(list_width_id()));
        assert!(
            list.is_some_and(|w| (w - (LIST_WIDTH + 60.0)).abs() < 1.0),
            "{list:?}"
        );
        let share: Option<f32> = harness.ctx.data(|d| d.get_temp(picture_share_id()));
        assert!(share.is_some(), "the picture's share was dragged");
    }

    /// The window opens with its controls beside the picture, whatever its
    /// shape — an operator found them under it and moved them each time —
    /// unless they were put under it, which is kept for every show.
    #[test]
    fn the_controls_open_beside_the_picture_unless_put_under_it() {
        let beside = |harness: &egui_kittest::Harness<'static, Seen>| {
            let picture = harness.get_by_label("Adjust preview").rect();
            harness.get_by_label("General").rect().left() > picture.right()
        };
        let data = UIData::test_fixture();
        let mut first = {
            let open = on_channel_a(&data);
            harness_of_size(data, open, egui::vec2(900.0, 900.0))
        };
        assert!(beside(&first), "beside it, in a window as tall as wide");
        first.get_by_label("Below").click();
        first.run();
        assert_eq!(
            first.state().window_choices,
            Some(WindowChoices {
                settings_controls_below: true
            }),
            "kept for every show"
        );

        let mut data = UIData::test_fixture();
        data.window_choices.settings_controls_below = true;
        let again = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        assert!(!beside(&again), "under it, as chosen before");
    }

    /// Each setting is named as large as a section was, the section's name
    /// larger still, and the sliders take the room there is.
    #[test]
    fn the_settings_read_large_with_long_sliders() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0]
            .effects
            .push(crop("fxcrop", false));
        let mut harness = layer_harness(data);
        harness.get_by_label("Position").click();
        harness.run();
        let name = harness.get_by_label("crop_left").rect();
        let section = harness.get_by_label("Mask").rect();
        assert!(
            name.height() >= 14.0,
            "a setting's name at the body size: {name:?}"
        );
        assert!(
            section.height() >= name.height() + 3.0,
            "a section's larger: {section:?}"
        );
        let slider = harness
            .get_all_by_role(egui::accesskit::Role::Slider)
            .next()
            .expect("a slider")
            .rect();
        assert!(slider.width() >= 240.0, "{slider:?}");
    }

    /// A setting with a long name keeps its row in the column: its slider
    /// is shortened to the room left, rather than the row pushed past the
    /// window's edge.
    #[test]
    fn a_long_setting_name_keeps_its_row_in_the_column() {
        use crate::params::ParamValue;
        use crate::usecases::ui::{ParamUIInfo, ShaderParamsUI};
        use egui::accesskit::Role;
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0].effects.push((
            "fxtransform".into(),
            "transform".into(),
            true,
            ShaderParamsUI {
                shader_name: "transform".into(),
                params: vec![ParamUIInfo {
                    name: "wrap".into(),
                    label: Some("Wrap (0=Clamp 1=Repeat 2=Mirror)".into()),
                    value: ParamValue::Float(0.0),
                    min: Some(0.0),
                    max: Some(2.0),
                }],
            },
        ));
        let mut harness = layer_harness(data);
        harness.get_by_label("Position").click();
        harness.run();
        let edge = harness.get_by_label("Beside").rect().right();
        let rightmost = harness
            .get_all_by_role(Role::Slider)
            .chain(harness.get_all_by_role(Role::SpinButton))
            .map(|node| node.rect().right())
            .fold(f32::MIN, f32::max);
        assert!(
            rightmost <= edge + 1.0,
            "a control ends at {rightmost}, the column at {edge}"
        );
    }

    /// A generator's setting with a long name keeps its row in the column
    /// too, on the Source tab.
    #[test]
    fn a_long_generator_setting_keeps_its_row_in_the_column() {
        use crate::params::ParamValue;
        use crate::usecases::ui::ParamUIInfo;
        use egui::accesskit::Role;
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0]
            .generator
            .params
            .push(ParamUIInfo {
                name: "shape".into(),
                label: Some("Shape (0=Circle 1=Square 2=Triangle 3=Star)".into()),
                value: ParamValue::Float(0.0),
                min: Some(0.0),
                max: Some(3.0),
            });
        let mut harness = layer_harness(data);
        harness.get_by_label("Source").click();
        harness.run();
        let edge = harness.get_by_label("Beside").rect().right();
        let rightmost = harness
            .get_all_by_role(Role::Slider)
            .chain(harness.get_all_by_role(Role::SpinButton))
            .map(|node| node.rect().right())
            .fold(f32::MIN, f32::max);
        assert!(
            rightmost <= edge + 1.0,
            "a control ends at {rightmost}, the column at {edge}"
        );
    }

    #[test]
    fn the_tabs_go_below_or_beside_the_picture() {
        let data = UIData::test_fixture();
        let mut harness = {
            let open = on_channel_a(&data);
            harness(data, open)
        };
        harness.get_by_label("Below").click();
        harness.run();
        let picture = harness.get_by_label("Adjust preview").rect();
        let tab = harness.get_by_label("General").rect();
        assert!(tab.top() > picture.bottom(), "under it");
        harness.get_by_label("Beside").click();
        harness.run();
        let picture = harness.get_by_label("Adjust preview").rect();
        let tab = harness.get_by_label("General").rect();
        assert!(tab.left() > picture.right(), "beside it");
    }

    /// Copy one layer's transform and paste it alone on a layer of another
    /// channel; its crop and border stay as they were.
    #[test]
    fn a_transform_is_copied_and_pasted_on_its_own() {
        let mut data = UIData::test_fixture();
        let mut from = transform("fxfrom");
        from.3.params[0].value = crate::params::ParamValue::Float(0.4);
        data.channels[0].decks[0].effects.push(from);
        data.channels[0].decks[0]
            .effects
            .push(crop("cropfrom", true));
        data.channels[1].decks[0].effects.push(transform("fxto"));
        data.channels[1].decks[0]
            .effects
            .push(crop("cropto", false));
        let first = data.channels[0].decks[0].uuid.clone();
        let second = data.channels[1].decks[0].uuid.clone();
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 1000.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        open_layer(&harness.ctx, &first);
        harness.run();
        // Crop, Border, Transform: the third Copy is the transform's.
        harness
            .get_all_by_label("Copy")
            .nth(2)
            .unwrap()
            .scroll_to_me();
        harness.run();
        harness.get_all_by_label("Copy").nth(2).unwrap().click();
        harness.run();
        open_layer(&harness.ctx, &second);
        harness.run();
        assert_eq!(harness.get_all_by_label("Paste").count(), 3);
        harness
            .get_all_by_label("Paste")
            .nth(2)
            .unwrap()
            .scroll_to_me();
        harness.run();
        harness.get_all_by_label("Paste").nth(2).unwrap().click();
        harness.run();
        let commands = harness.state();
        assert!(commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetEffectParam {
                effect_uuid,
                name,
                value: crate::params::ParamValue::Float(v),
            } if effect_uuid == "fxto" && name == "pos_x" && (*v - 0.4).abs() < 1e-6
        )));
        assert!(
            !commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetEffectParam { effect_uuid, .. } if effect_uuid == "cropto"
            )),
            "the crop and border were not copied"
        );
    }

    /// After Copy position (all three parts), a part's own Paste pastes just
    /// that part.
    #[test]
    fn a_parts_paste_takes_only_its_part_of_a_whole_copy() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0].effects.push(transform("fxfrom"));
        data.channels[0].decks[0]
            .effects
            .push(crop("cropfrom", true));
        data.channels[1].decks[0].effects.push(transform("fxto"));
        data.channels[1].decks[0]
            .effects
            .push(crop("cropto", false));
        let first = data.channels[0].decks[0].uuid.clone();
        let second = data.channels[1].decks[0].uuid.clone();
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 1000.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        open_layer(&harness.ctx, &first);
        harness.run();
        harness.get_by_label("Copy position").click();
        harness.run();
        open_layer(&harness.ctx, &second);
        harness.run();
        // Transform's Paste, the third.
        harness
            .get_all_by_label("Paste")
            .nth(2)
            .unwrap()
            .scroll_to_me();
        harness.run();
        harness.get_all_by_label("Paste").nth(2).unwrap().click();
        harness.run();
        let touched: Vec<&str> = harness
            .state()
            .iter()
            .filter_map(|c| match c {
                EngineCommand::SetEffectParam { effect_uuid, .. } => Some(effect_uuid.as_str()),
                _ => None,
            })
            .collect();
        assert!(touched.contains(&"fxto"), "{touched:?}");
        assert!(
            !touched.contains(&"cropto"),
            "only the transform: {touched:?}"
        );
    }

    #[test]
    fn a_border_is_pasted_from_one_layer_to_another() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0]
            .effects
            .push(crop("cropfrom", true));
        data.channels[0].decks[1]
            .effects
            .push(crop("cropto", false));
        let first = data.channels[0].decks[0].uuid.clone();
        let second = data.channels[0].decks[1].uuid.clone();
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 1000.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        open_layer(&harness.ctx, &first);
        harness.run();
        // Crop's Copy, then Border's.
        harness.get_all_by_label("Copy").nth(1).unwrap().click();
        harness.run();
        open_layer(&harness.ctx, &second);
        harness.run();
        harness.get_all_by_label("Paste").nth(1).unwrap().click();
        harness.run();
        let pasted: Vec<&str> = harness
            .state()
            .iter()
            .filter_map(|c| match c {
                EngineCommand::SetEffectParam {
                    effect_uuid, name, ..
                } if effect_uuid == "cropto" => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert!(pasted.contains(&"border_mode"), "{pasted:?}");
        assert!(
            pasted.iter().all(|n| n.starts_with("border_")),
            "{pasted:?}"
        );
    }

    #[test]
    fn the_source_tab_changes_how_the_picture_is_scaled() {
        let data = UIData::test_fixture();
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = layer_harness(data);
        harness.get_by_label("Source").click();
        harness.run();
        harness.get_by_value("Fit").click();
        harness.run();
        harness.get_by_label("Fill").click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetDeckScalingMode { deck_uuid, mode: crate::ScalingMode::Fill }
                    if *deck_uuid == deck
            )),
            "{:?}",
            harness.state().commands
        );
    }

    /// Another input as a layer: its Source tab says whether it keeps that
    /// input's transparency, and unticking it flattens the layer over black.
    #[test]
    fn a_taps_source_tab_flattens_it_over_black() {
        let mut data = UIData::test_fixture();
        let source = data.channels[1].uuid.clone();
        let layer = &mut data.channels[0].decks[0];
        layer.tap = Some(crate::usecases::ui::TapUI {
            kind: "channel".into(),
            channel_uuid: Some(source),
            label: "Channel B".into(),
            bound: true,
        });
        layer.transparent = true;
        let deck = layer.uuid.clone();
        let mut harness = layer_harness(data);
        harness.get_by_label("Source").click();
        harness.run();
        harness.get_by_label("Transparent").click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetDeckTransparent { deck_uuid, transparent: false }
                    if *deck_uuid == deck
            )),
            "{:?}",
            harness.state().commands
        );
    }

    /// Layer 1 has nothing below it in its channel to blend with.
    #[test]
    fn layer_one_blends_the_channel_not_itself() {
        use egui_kittest::kittest::NodeT;
        for (layer, disabled) in [(0, true), (1, false)] {
            let data = UIData::test_fixture();
            let deck = data.channels[0].decks[layer].uuid.clone();
            let mut harness = harness(data, move |ctx| open_layer(ctx, &deck));
            harness.get_by_label("Position").click();
            harness.run();
            let blends: Vec<_> = harness.get_all_by_value("Normal").collect();
            assert!(blends.len() >= 2, "the layer's and the channel's");
            assert_eq!(
                blends[0].accesskit_node().is_disabled(),
                disabled,
                "layer {layer}"
            );
            assert!(!blends[1].accesskit_node().is_disabled(), "the channel's");
        }
    }

    #[test]
    fn an_overlay_layer_offers_the_blend_over_main() {
        let mut data = UIData::test_fixture();
        data.main.mode = crate::mixer::MixMode::Program;
        data.channels[1].overlay = true;
        let ch = data.channels[1].uuid.clone();
        let deck = data.channels[1].decks[0].uuid.clone();
        let mut harness = harness(data, move |ctx| open_layer(ctx, &deck));
        harness.get_by_label("Position").click();
        harness.run();
        assert!(harness.query_by_label("Blend over Main").is_some());
        harness.get_all_by_value("Normal").last().unwrap().click();
        harness.run();
        harness.get_by_label("Screen").click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetChannelBlendMode { channel_uuid, mode: BlendMode::Screen }
                    if *channel_uuid == ch
            )),
            "{:?}",
            harness.state().commands
        );
    }

    #[test]
    fn names_read_as_titles() {
        assert_eq!(pretty("color_correction"), "Color correction");
        assert_eq!(pretty("blur"), "Blur");
        assert_eq!(pretty(""), "");
    }

    fn clip_playback() -> crate::usecases::ui::VideoPlaybackUI {
        crate::usecases::ui::VideoPlaybackUI {
            playing: false,
            position: 0.0,
            duration: 30.0,
            speed: 1.0,
            effective_speed: 1.0,
            position_offset: 0.0,
            loop_mode: crate::video::LoopMode::OneShot,
            in_point: 0.0,
            out_point: 0.0,
            frame_rate: 25.0,
            transport_sync: crate::video::DeckTransportSync::default(),
            hardware_decode: true,
            auto: crate::engine::value::video::AutoPlayback::default(),
            sound_only: false,
        }
    }

    #[test]
    fn a_clip_opens_on_what_it_does_on_air() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks.truncate(1);
        data.channels[0].decks[0].video_playback = Some(clip_playback());
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1400.0, 900.0))
                .build_ui(|ui| {
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                });
            open_channel(&h.ctx, &data.channels[0]);
            h.run();
            assert_eq!(current(&h.ctx).map(|o| o.tab), Some(Tab::Playback));
            h.get_by_label("Pause when it comes off air (Auto Pause)")
                .click();
            h.run();
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::VideoSetAutoPlayback { deck_uuid, auto }
                    if *deck_uuid == deck && auto.play && !auto.pause && auto.restart
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_clip_can_be_set_to_start_and_stop_on_main_only() {
        let mut data = UIData::test_fixture();
        data.channels[0].decks.truncate(1);
        data.channels[0].decks[0].video_playback = Some(clip_playback());
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1400.0, 900.0))
                .build_ui(|ui| {
                    render_channel_settings(ui.ctx(), ui.max_rect(), &data, &mut actions);
                });
            open_channel(&h.ctx, &data.channels[0]);
            h.run();
            h.get_by_label("Main only").click();
            h.run();
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::VideoSetAutoPlayback { deck_uuid, auto }
                    if *deck_uuid == deck && auto.main_only && auto.play && auto.pause
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_sound_file_has_only_its_playback() {
        let mut data = UIData::test_fixture();
        let mut vp = clip_playback();
        vp.sound_only = true;
        data.channels[0].decks[0].video_playback = Some(vp);
        assert_eq!(tabs_for(Some(&data.channels[0].decks[0])), [Tab::Playback]);
        assert!(tabs_for(Some(&data.channels[0].decks[1])).contains(&Tab::Position));
    }
}
