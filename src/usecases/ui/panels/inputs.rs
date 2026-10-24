//! Inputs, as vMix shows them: a tile per channel with its picture, its
//! number and name, and Cut and Fade on the tile itself.
//!
//! Clicking a tile puts it in Preview (it becomes what Space and Enter take);
//! double-clicking opens its input settings; the tile's own buttons take it
//! to Main at once. The strip of small pictures
//! along a tile's foot are the decks layered in it: click one for its
//! settings, double-click to adjust it, right-click to take it to Main alone.
//! Files and Library items dropped on a tile go into that channel.
//!
//! A tile's name bar is its handle: drag it to move the channel along the
//! row, double-click it to rename the channel, right-click it to fold the
//! tile to a narrow strip (right-click the strip to open it out), and its ×
//! deletes the channel.

use super::super::{ChannelUIInfo, DeckDrag, DeckUIInfo, LibraryDrag, UIActions, UIData};
use super::main_desk::{self, NEXT_GREEN, ON_MAIN_RED};
use super::utils::channel_color;
use crate::engine::EngineCommand;
use crate::mixer::{MainSource, MixMode};

const MIN_TILE_WIDTH: f32 = 220.0;
const MAX_TILE_WIDTH: f32 = 340.0;
const GAP: f32 = 8.0;
const PAD: f32 = 4.0;
const TITLE_HEIGHT: f32 = 22.0;
const BUTTONS_HEIGHT: f32 = 30.0;
const CHIP_HEIGHT: f32 = 30.0;
/// A folded tile: its name down a narrow strip.
const STRIP_WIDTH: f32 = 44.0;

/// A channel being dragged by its name bar to a new place in the row.
#[derive(Clone, Debug)]
struct ChannelDrag {
    uuid: String,
    name: String,
}

/// Whether an input's × asks before deleting it: while it is on air — on
/// Main or a bus's Program, itself or inside another input there, fading
/// or not, as the engine reckons it (`ChannelUIInfo::on_air`). Off air it
/// goes at once — **Undo** brings it back — so a row of inputs is cleared a
/// click each.
fn asks_before_deleting(ch: &ChannelUIInfo) -> bool {
    ch.on_air.is_some()
}

/// The channel whose × asks "delete it and its layers?".
fn confirm_delete_id() -> egui::Id {
    egui::Id::new("inputs_confirm_delete")
}

pub(super) fn render_inputs(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    render_toolbar(ui, data, actions);
    ui.add_space(4.0);
    let width = ui.available_width();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let columns = (((width + GAP) / (MIN_TILE_WIDTH + GAP)).floor() as usize).max(1);
    #[allow(clippy::cast_precision_loss)]
    let tile_width =
        ((width - GAP * (columns as f32 - 1.0)) / columns as f32).clamp(120.0, MAX_TILE_WIDTH);
    let height = tile_height(tile_width, data);
    egui::ScrollArea::vertical()
        .id_salt("inputs_tiles")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
            // Tiles flow and wrap, so a folded one gives its room to the rest.
            // Placed by hand, so every row's tops line up.
            let origin = ui.cursor().min;
            let room = ui.available_width();
            let (mut x, mut y) = (0.0_f32, 0.0_f32);
            let mut place = |width: f32| -> egui::Rect {
                if x > 0.0 && x + width > room {
                    x = 0.0;
                    y += height + GAP;
                }
                let rect =
                    egui::Rect::from_min_size(origin + egui::vec2(x, y), egui::vec2(width, height));
                x += width + GAP;
                rect
            };
            for ch in &data.channels {
                let folded = data.collapsed_channels.contains(&ch.uuid);
                let rect = place(if folded { STRIP_WIDTH } else { tile_width });
                ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                    if folded {
                        render_strip(ui, ch, height, data, actions);
                    } else {
                        render_tile(ui, ch, tile_width, data, actions);
                    }
                });
            }
            // The last cell is a place to make a new channel.
            let rect = place(tile_width);
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                render_new_tile(ui, tile_width, data, actions);
            });
            // What the tiles cover, for the scroll area.
            let used = egui::Rect::from_min_size(origin, egui::vec2(room, y + height));
            ui.allocate_rect(used, egui::Sense::hover());
        });
    paint_channel_drag(ui.ctx());
}

/// A channel dragged by its name bar: its name follows the pointer.
fn paint_channel_drag(ctx: &egui::Context) {
    let Some(drag) = egui::DragAndDrop::payload::<ChannelDrag>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("channel_drag_ghost"),
    ));
    let galley = painter.layout_no_wrap(
        drag.name.clone(),
        egui::FontId::proportional(15.0),
        egui::Color32::WHITE,
    );
    let rect = egui::Rect::from_min_size(
        pos + egui::vec2(12.0, 8.0),
        galley.size() + egui::vec2(12.0, 6.0),
    );
    painter.rect_filled(
        rect,
        3.0,
        egui::Color32::from_rgba_unmultiplied(80, 120, 200, 200),
    );
    painter.galley(
        rect.min + egui::vec2(6.0, 3.0),
        galley,
        egui::Color32::WHITE,
    );
}

/// A channel dragged over this tile or strip: a line marks where it would
/// go — before it (left half) or after it (right half) — and letting go
/// moves it there.
fn reorder_target(
    ui: &egui::Ui,
    ch: &ChannelUIInfo,
    rect: egui::Rect,
    data: &UIData,
    actions: &mut UIActions,
) {
    let ctx = ui.ctx();
    let Some(drag) = egui::DragAndDrop::payload::<ChannelDrag>(ctx) else {
        return;
    };
    let Some(pos) = ctx.pointer_latest_pos().filter(|p| rect.contains(*p)) else {
        return;
    };
    let before_this = pos.x < rect.center().x;
    let x = if before_this {
        rect.left() - GAP / 2.0
    } else {
        rect.right() + GAP / 2.0
    };
    ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("channel_insertion_marker"),
    ))
    .line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(3.0, egui::Color32::from_rgb(90, 170, 255)),
    );
    if !ctx.input(|i| i.pointer.any_released()) {
        return;
    }
    egui::DragAndDrop::clear_payload(ctx);
    let before = if before_this {
        Some(ch.uuid.clone())
    } else {
        data.channels.get(ch.ch_idx + 1).map(|c| c.uuid.clone())
    };
    let from = data.channels.iter().position(|c| c.uuid == drag.uuid);
    // Dropped back where it already is: nothing to do, and no undo step.
    let unmoved = drag.uuid == ch.uuid
        || before.as_deref() == Some(drag.uuid.as_str())
        || from.is_some_and(|i| {
            before.as_deref() == data.channels.get(i + 1).map(|c| c.uuid.as_str())
        });
    if !unmoved {
        actions.commands.push(EngineCommand::MoveChannel {
            channel_uuid: drag.uuid.clone(),
            before_channel_uuid: before,
        });
    }
}

/// A folded tile: the number and name down a narrow strip, framed red when
/// on Main and green in Preview. Click it for Preview, double-click for its
/// settings, right-click to open it out, drag it to move the channel.
fn render_strip(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    height: f32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let accent = channel_color(ch.ch_idx);
    let main_mode = main_desk::in_main_mode(data);
    let on_main = main_mode
        && (main_desk::channel_on_main(data, ch)
            || (ch.overlay && main_desk::overlay_on(data, ch)));
    let next = main_desk::next_channel(data).is_some_and(|n| n.uuid == ch.uuid);
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(STRIP_WIDTH, height),
        egui::Sense::click_and_drag(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, accent.linear_multiply(0.25));
    let (width, color) = if on_main {
        (3.0, ON_MAIN_RED)
    } else if next {
        (3.0, NEXT_GREEN)
    } else {
        (1.0, egui::Color32::from_rgb(55, 55, 68))
    };
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(width, color),
        egui::StrokeKind::Inside,
    );
    // Read from the bottom up, as a folded name on a spine.
    let galley = painter.layout_no_wrap(
        format!("{}  {}", ch.ch_idx + 1, ch.name),
        egui::FontId::proportional(15.0),
        egui::Color32::WHITE,
    );
    let at = egui::pos2(rect.center().x - galley.size().y / 2.0, rect.bottom() - 8.0);
    painter.with_clip_rect(rect.shrink(2.0)).add(
        egui::epaint::TextShape::new(at, galley, egui::Color32::WHITE)
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{} strip", ch.name))
    });
    let response = response.on_hover_text(format!(
        "{} — right-click to open it out · drag to move it",
        ch.name
    ));
    if response.clicked() {
        actions.session.select_channel = Some(ch.ch_idx);
    }
    if response.double_clicked() {
        super::channel_settings::open_channel(ui.ctx(), ch);
    }
    if response.secondary_clicked() {
        actions.session.toggle_collapsed_channel = Some(ch.uuid.clone());
    }
    if response.drag_started() {
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            ChannelDrag {
                uuid: ch.uuid.clone(),
                name: ch.name.clone(),
            },
        );
    }
    ui.ctx().memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new("ch_drop_rect").with(ch.ch_idx), rect);
    });
    super::dnd::publish_channel_surface_fx(ui.ctx(), &ch.uuid, ch.ch_idx, rect);
    super::mixer::handle_deck_drop(ui, ch, rect, accent, actions);
    reorder_target(ui, ch, rect, data, actions);
}

fn render_toolbar(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    ui.horizontal(|ui| {
        if ui
            .add(
                egui::Button::new(egui::RichText::new("Add Input").size(15.0).strong())
                    .min_size(egui::vec2(0.0, 28.0)),
            )
            .on_hover_text(
                "A clip, picture, camera, capture card, NDI source, screen, stream or colour",
            )
            .clicked()
        {
            super::add_input::open(ui.ctx());
        }
        if ui
            .add(egui::Button::new("+ Channel").min_size(egui::vec2(0.0, 28.0)))
            .on_hover_text("An empty channel, to drop files or Library items on")
            .clicked()
        {
            actions.commands.push(EngineCommand::AddChannel);
        }
        if main_desk::in_main_mode(data) {
            ui.label(
                egui::RichText::new("Click a tile for Preview · Space cuts · Enter fades")
                    .small()
                    .weak(),
            );
        } else {
            ui.label(
                egui::RichText::new("The layered mixer is on — every channel mixes to the output")
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
        }
    });
}

/// Height of a tile `width` wide.
fn tile_height(width: f32, data: &UIData) -> f32 {
    let picture = picture_size(width, data);
    PAD + TITLE_HEIGHT + picture.y + PAD + BUTTONS_HEIGHT + PAD + CHIP_HEIGHT + PAD
}

fn picture_size(width: f32, data: &UIData) -> egui::Vec2 {
    super::utils::preview_size(
        egui::vec2(width - 2.0 * PAD, width),
        data.render_width,
        data.render_height,
    )
}

fn render_tile(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    width: f32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let accent = channel_color(ch.ch_idx);
    let main_mode = main_desk::in_main_mode(data);
    let on_main = main_mode && main_desk::channel_on_main(data, ch);
    let next = main_desk::next_channel(data).is_some_and(|n| n.uuid == ch.uuid);
    let overlay_on = main_desk::overlay_on(data, ch);
    let size = egui::vec2(width, tile_height(width, data));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());

    let source_drag = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx())
        .is_some_and(|p| !matches!(&*p, LibraryDrag::Effect(_)))
        || egui::DragAndDrop::has_payload_of_type::<DeckDrag>(ui.ctx());
    let drop_hover = source_drag && ui.rect_contains_pointer(rect);

    let painter = ui.painter();
    painter.rect_filled(rect, 4.0, egui::Color32::from_rgb(24, 24, 32));
    let (stroke, color) = if drop_hover {
        (2.0, accent)
    } else if on_main || (ch.overlay && overlay_on) {
        (3.0, ON_MAIN_RED)
    } else if next {
        (3.0, NEXT_GREEN)
    } else {
        (1.0, egui::Color32::from_rgb(55, 55, 68))
    };
    painter.rect_stroke(
        rect,
        4.0,
        egui::Stroke::new(stroke, color),
        egui::StrokeKind::Inside,
    );

    // Title: number, name, and what it is doing.
    let title = egui::Rect::from_min_size(
        rect.min + egui::vec2(PAD, PAD),
        egui::vec2(width - 2.0 * PAD, TITLE_HEIGHT),
    );
    painter.rect_filled(title, 2.0, accent.linear_multiply(0.25));
    let name = format!("{}  {}", ch.ch_idx + 1, ch.name);
    painter.with_clip_rect(title).text(
        title.left_center() + egui::vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &name,
        egui::FontId::proportional(15.0),
        egui::Color32::WHITE,
    );
    let tally = if ch.overlay {
        Some(if overlay_on {
            ("OVERLAY ON", ON_MAIN_RED)
        } else {
            ("OVERLAY", egui::Color32::from_rgb(70, 70, 85))
        })
    } else if on_main {
        Some(("ON MAIN", ON_MAIN_RED))
    } else if next {
        Some(("PREVIEW", NEXT_GREEN))
    } else {
        None
    };
    if let Some((text, fill)) = tally.filter(|_| main_mode) {
        let galley = painter.layout_no_wrap(
            text.to_string(),
            egui::FontId::proportional(12.0),
            egui::Color32::WHITE,
        );
        let badge = egui::Rect::from_min_size(
            egui::pos2(
                title.right() - galley.size().x - 12.0 - CLOSE_WIDTH,
                title.center().y - 9.0,
            ),
            egui::vec2(galley.size().x + 8.0, 18.0),
        );
        painter.rect_filled(badge, 3.0, fill);
        painter.galley(
            badge.min + egui::vec2(4.0, 2.0),
            galley,
            egui::Color32::WHITE,
        );
    }

    // Picture.
    let picture = picture_size(width, data);
    let picture_rect = egui::Rect::from_min_size(
        egui::pos2(
            rect.center().x - picture.x / 2.0,
            title.bottom() + PAD / 2.0,
        ),
        picture,
    );
    match data.channel_preview_textures.get(&ch.ch_idx) {
        Some(&id) if !ch.decks.is_empty() => {
            painter.image(
                id,
                picture_rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        _ => {
            painter.rect_filled(picture_rect, 2.0, egui::Color32::from_rgb(16, 16, 22));
            painter.text(
                picture_rect.center(),
                egui::Align2::CENTER_CENTER,
                if drop_hover {
                    "➕ Drop to add"
                } else if ch.decks.is_empty() {
                    "Empty — drop files here"
                } else {
                    "No picture yet"
                },
                egui::FontId::proportional(13.0),
                egui::Color32::GRAY,
            );
        }
    }

    // Buttons: Cut / Fade (or an overlay's On / Off), and the overlay switch.
    let buttons = egui::Rect::from_min_size(
        egui::pos2(rect.left() + PAD, picture_rect.bottom() + PAD),
        egui::vec2(width - 2.0 * PAD, BUTTONS_HEIGHT),
    );
    let mut buttons_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(buttons)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    render_tile_buttons(&mut buttons_ui, ch, on_main, overlay_on, data, actions);

    // Decks layered in the channel.
    let chips = egui::Rect::from_min_size(
        egui::pos2(rect.left() + PAD, buttons.bottom() + PAD),
        egui::vec2(width - 2.0 * PAD, CHIP_HEIGHT),
    );
    let mut chips_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(chips)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    render_deck_chips(&mut chips_ui, ch, accent, data, actions);

    // A playlist starts and stops from its tile.
    let confirming = ui
        .ctx()
        .data(|d| d.get_temp::<String>(confirm_delete_id()))
        .is_some_and(|asking| asking == ch.uuid);
    if !confirming {
        super::playlist::overlay_on_picture(ui, picture_rect, ch, actions);
        // Rows at the foot of the picture, from the bottom: a timer's, a
        // presentation's, a clip's.
        let timer = usize::from(super::timer::of_channel(ch).is_some());
        let slides = usize::from(super::presentation::of_channel(ch).is_some());
        super::transport::overlay_on_picture(ui, picture_rect, ch, timer + slides, actions);
        super::timer::overlay_on_picture(ui, picture_rect, ch, actions);
        super::presentation::overlay_on_picture(ui, picture_rect, ch, timer, actions);
    }

    render_title_bar(ui, ch, title, data, actions);
    render_delete_confirm(ui, ch, picture_rect, actions);

    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("{} tile", ch.name))
    });
    if response.clicked() {
        actions.session.select_channel = Some(ch.ch_idx);
    }
    // Its input settings, as vMix opens them.
    if response.double_clicked() {
        actions.session.select_channel = Some(ch.ch_idx);
        super::channel_settings::open_channel(ui.ctx(), ch);
    }
    response.context_menu(|ui| super::mixer::channel_context_menu(ui, data, actions, ch));

    ui.ctx().memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new("ch_drop_rect").with(ch.ch_idx), rect);
    });
    super::dnd::publish_channel_surface_fx(ui.ctx(), &ch.uuid, ch.ch_idx, rect);
    super::mixer::handle_deck_drop(ui, ch, rect, accent, actions);
    reorder_target(ui, ch, rect, data, actions);
}

/// Room at the name bar's right end for its ×.
const CLOSE_WIDTH: f32 = 22.0;

/// The name bar as a handle: click for Preview, double-click to rename,
/// right-click to fold the tile, drag to move the channel; and its × to
/// delete it.
fn render_title_bar(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    title: egui::Rect,
    data: &UIData,
    actions: &mut UIActions,
) {
    let handle = ui
        .interact(
            title,
            ui.id().with(("tile_title", &ch.uuid)),
            egui::Sense::click_and_drag(),
        )
        .on_hover_cursor(egui::CursorIcon::Grab);
    handle.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{} name bar", ch.name),
        )
    });
    let handle =
        handle.on_hover_text("Drag to move · double-click to rename · right-click to fold");
    if handle.clicked() {
        actions.session.select_channel = Some(ch.ch_idx);
    }
    if handle.double_clicked() {
        super::rename::open(ui.ctx(), ch);
    }
    if handle.secondary_clicked() {
        actions.session.toggle_collapsed_channel = Some(ch.uuid.clone());
    }
    if handle.drag_started() {
        egui::DragAndDrop::set_payload(
            ui.ctx(),
            ChannelDrag {
                uuid: ch.uuid.clone(),
                name: ch.name.clone(),
            },
        );
    }

    let close = egui::Rect::from_min_size(
        egui::pos2(title.right() - CLOSE_WIDTH, title.top()),
        egui::vec2(CLOSE_WIDTH, title.height()),
    );
    // The mixer keeps two channels: the crossfader needs both ends.
    let can_delete = data.channels.len() > 2;
    let x = ui
        .put(
            close,
            egui::Button::new(
                egui::RichText::new("×")
                    .size(15.0)
                    .color(egui::Color32::WHITE),
            )
            .frame(false),
        )
        .on_hover_text(if can_delete {
            "Delete this channel — Undo brings it back"
        } else {
            "A show keeps at least two channels"
        });
    x.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            can_delete,
            format!("Delete {}", ch.name),
        )
    });
    if x.clicked() && can_delete {
        if asks_before_deleting(ch) {
            ui.ctx()
                .data_mut(|d| d.insert_temp(confirm_delete_id(), ch.uuid.clone()));
        } else {
            actions.session.remove_channel = Some(ch.ch_idx);
        }
    }
}

/// "Ch 24 is on Main. Delete it and its layer?" over the picture, once its ×
/// is clicked on a channel that asks first (see `asks_before_deleting`).
fn render_delete_confirm(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    picture: egui::Rect,
    actions: &mut UIActions,
) {
    let asking: Option<String> = ui.ctx().data(|d| d.get_temp(confirm_delete_id()));
    if asking.as_deref() != Some(ch.uuid.as_str()) {
        return;
    }
    ui.painter()
        .rect_filled(picture, 2.0, egui::Color32::from_black_alpha(220));
    let mut box_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(picture.shrink(8.0))
            .layout(egui::Layout::top_down(egui::Align::Center)),
    );
    box_ui.add_space((picture.height() / 2.0 - 34.0).max(0.0));
    let layers = if ch.decks.len() == 1 {
        "its layer".to_string()
    } else {
        format!("its {} layers", ch.decks.len())
    };
    let question = match &ch.on_air {
        Some(at) => format!("{} is on {at}.\nDelete it and {layers}?", ch.name),
        None => format!("Delete {} and {layers}?", ch.name),
    };
    box_ui.label(
        egui::RichText::new(question)
            .strong()
            .color(egui::Color32::WHITE),
    );
    box_ui.horizontal(|ui| {
        if ui
            .add(egui::Button::new("Delete").fill(ON_MAIN_RED))
            .on_hover_text("Undo brings it back")
            .clicked()
        {
            actions.session.remove_channel = Some(ch.ch_idx);
            ui.ctx()
                .data_mut(|d| d.remove::<String>(confirm_delete_id()));
        }
        if ui.button("Keep").clicked() {
            ui.ctx()
                .data_mut(|d| d.remove::<String>(confirm_delete_id()));
        }
    });
}

fn render_tile_buttons(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    on_main: bool,
    overlay_on: bool,
    data: &UIData,
    actions: &mut UIActions,
) {
    if !main_desk::in_main_mode(data) {
        return;
    }
    let button = |text: &str| {
        egui::Button::new(egui::RichText::new(text).size(14.0).strong())
            .min_size(egui::vec2(58.0, BUTTONS_HEIGHT - 4.0))
    };
    if ch.overlay {
        let (text, fill) = if overlay_on {
            ("ON", ON_MAIN_RED)
        } else {
            ("OFF", egui::Color32::from_rgb(60, 60, 70))
        };
        if ui
            .add(button(text).fill(fill))
            .on_hover_text("Switch this overlay on or off over Main")
            .clicked()
        {
            main_desk::take_channel(ch, data, actions, false);
        }
        if ui
            .add(button("Fade"))
            .on_hover_text("Fade this overlay on or off")
            .clicked()
        {
            main_desk::take_channel(ch, data, actions, true);
        }
    } else {
        if ui
            .add_enabled(!on_main, button("Cut"))
            .on_hover_text("Put this channel on Main now")
            .clicked()
        {
            main_desk::take_channel(ch, data, actions, false);
        }
        if ui
            .add_enabled(!on_main && data.main.fade.is_none(), button("Fade"))
            .on_hover_text("Fade this channel onto Main")
            .clicked()
        {
            main_desk::take_channel(ch, data, actions, true);
        }
    }
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        let mut overlay = ch.overlay;
        if ui
            .checkbox(&mut overlay, "Overlay")
            .on_hover_text(
                "An overlay (logo, lower third) is switched on and off over whatever is on \
                 Main, instead of being taken to Main",
            )
            .changed()
        {
            actions.commands.push(EngineCommand::SetChannelOverlay {
                channel_uuid: ch.uuid.clone(),
                overlay,
            });
        }
    });
}

/// The decks layered in a channel, as small pictures.
fn render_deck_chips(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    accent: egui::Color32,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.spacing_mut().item_spacing.x = 4.0;
    let chip = super::utils::preview_size(
        egui::vec2(CHIP_HEIGHT * 3.0, CHIP_HEIGHT - 2.0),
        data.render_width,
        data.render_height,
    );
    let fits = ((ui.available_width() + 4.0) / (chip.x + 4.0))
        .floor()
        .max(1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let shown = (fits as usize).min(ch.decks.len());
    let shown = if shown < ch.decks.len() {
        shown.saturating_sub(1)
    } else {
        shown
    };
    // Front first: the layer seen is the first chip.
    for deck in ch.decks.iter().rev().take(shown) {
        render_chip(ui, ch, deck, chip, accent, data, actions);
    }
    if shown < ch.decks.len() {
        ui.label(
            egui::RichText::new(format!("+{}", ch.decks.len() - shown))
                .small()
                .weak(),
        )
        .on_hover_text("More decks — the Channels or Layers panel shows them all");
    }
}

fn render_chip(
    ui: &mut egui::Ui,
    ch: &ChannelUIInfo,
    deck: &DeckUIInfo,
    size: egui::Vec2,
    accent: egui::Color32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    match data.deck_preview_textures.get(&deck.uuid) {
        Some(&id) => {
            ui.painter().image(
                id,
                rect,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
        None => {
            ui.painter()
                .rect_filled(rect, 2.0, egui::Color32::from_rgb(30, 30, 40));
        }
    }
    let selected = data.selected_deck == Some((ch.ch_idx, deck.deck_idx));
    let on_main = main_desk::deck_on_main(data, &deck.uuid);
    let (width, color) = if on_main {
        (2.0, ON_MAIN_RED)
    } else if selected {
        (2.0, accent)
    } else {
        (1.0, accent.linear_multiply(0.35))
    };
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(width, color),
        egui::StrokeKind::Outside,
    );
    if deck.offline.is_some() {
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "⚠",
            egui::FontId::proportional(rect.height().min(rect.width()) * 0.5),
            ui.visuals().warn_fg_color,
        );
    }
    // A clip is marked, a picture not: ▶ a video, ♪ a sound (which has no
    // picture of its own), green while it plays.
    let kind = deck.video_playback.as_ref().map(|vp| {
        if vp.sound_only {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "♪",
                egui::FontId::proportional(rect.height() * 0.5),
                egui::Color32::from_gray(150),
            );
        }
        let badge = egui::Rect::from_min_size(
            rect.left_bottom() - egui::vec2(0.0, 13.0),
            egui::vec2(15.0, 13.0),
        );
        let fill = if vp.playing {
            super::transport::PLAYING_GREEN
        } else {
            egui::Color32::from_black_alpha(180)
        };
        ui.painter().rect_filled(badge, 2.0, fill);
        let (glyph, kind) = if vp.sound_only {
            ("♪", "sound ")
        } else {
            ("▶", "video ")
        };
        ui.painter().text(
            badge.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(9.5),
            egui::Color32::WHITE,
        );
        kind
    });
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{} {}deck", deck.name, kind.unwrap_or_default()),
        )
    });
    let response = match &deck.offline {
        Some(why) => response.on_hover_text(format!("{} — offline: {why}", deck.name)),
        None => response.on_hover_text(&deck.name),
    };
    if response.clicked() {
        actions.session.select_deck = Some((ch.ch_idx, deck.deck_idx));
    }
    if response.double_clicked() {
        actions.session.select_deck = Some((ch.ch_idx, deck.deck_idx));
        super::deck_adjust::open(ui.ctx(), &deck.uuid);
    }
    response.context_menu(|ui| {
        if main_desk::in_main_mode(data) {
            ui.label(egui::RichText::new("This input alone").small().weak());
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
        if ui.button("Adjust…").clicked() {
            actions.session.select_deck = Some((ch.ch_idx, deck.deck_idx));
            super::deck_adjust::open(ui.ctx(), &deck.uuid);
            ui.close();
        }
        super::deck_detail::framing_menu_items(ui, deck, actions);
        if ui.button("Remove deck").clicked() {
            actions.commands.push(EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            });
            ui.close();
        }
    });
}

/// The empty cell after the tiles: click for a new channel, or drop a Library
/// item or file on it to make one holding it.
fn render_new_tile(ui: &mut egui::Ui, width: f32, data: &UIData, actions: &mut UIActions) {
    let size = egui::vec2(width, tile_height(width, data));
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let dragging = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx())
        .is_some_and(|p| !matches!(&*p, LibraryDrag::Effect(_)));
    let hover = dragging && ui.rect_contains_pointer(rect);
    let color = if hover {
        egui::Color32::from_rgb(150, 150, 190)
    } else {
        egui::Color32::from_rgb(60, 60, 75)
    };
    ui.painter().rect_stroke(
        rect.shrink(1.0),
        4.0,
        egui::Stroke::new(if hover { 2.0 } else { 1.0 }, color),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        if hover {
            "➕ Drop for a new channel"
        } else {
            "+ Channel\nor drop here"
        },
        egui::FontId::proportional(15.0),
        color,
    );
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "New channel tile")
    });
    if response.clicked() {
        actions.commands.push(EngineCommand::AddChannel);
    }
    ui.ctx().memory_mut(|m| {
        m.data
            .insert_temp(egui::Id::new("new_ch_drop_rect").with(0_usize), rect);
    });
    // A channel dropped here goes to the end of the row.
    if let Some(drag) = egui::DragAndDrop::payload::<ChannelDrag>(ui.ctx()) {
        if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_released()) {
            egui::DragAndDrop::clear_payload(ui.ctx());
            if data.channels.last().is_some_and(|c| c.uuid != drag.uuid) {
                actions.commands.push(EngineCommand::MoveChannel {
                    channel_uuid: drag.uuid.clone(),
                    before_channel_uuid: None,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn main_mode_fixture() -> UIData {
        let mut data = UIData::test_fixture();
        data.main.mode = MixMode::Program;
        data.main.program = MainSource::Channel(data.channels[0].uuid.clone());
        // What the engine says of it, on Main.
        data.channels[0].on_air = Some("Main".to_string());
        data.selected_deck = None;
        data.selected_channel = None;
        data
    }

    struct Seen {
        commands: Vec<EngineCommand>,
        select_channel: Option<usize>,
        select_deck: Option<(usize, usize)>,
        remove_channel: Option<usize>,
        toggle_collapsed: Option<String>,
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, Seen> {
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1200.0, 900.0))
            // Short frames, so two clicks fall within egui's double-click time.
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                move |ui, seen: &mut Seen| {
                    let mut actions = UIActions::new();
                    render_inputs(ui, &data, &mut actions);
                    seen.commands.extend(actions.commands);
                    seen.select_channel = seen.select_channel.or(actions.session.select_channel);
                    seen.select_deck = seen.select_deck.or(actions.session.select_deck);
                    seen.remove_channel = seen.remove_channel.or(actions.session.remove_channel);
                    if actions.session.toggle_collapsed_channel.is_some() {
                        seen.toggle_collapsed = actions.session.toggle_collapsed_channel;
                    }
                },
                Seen {
                    commands: Vec::new(),
                    select_channel: None,
                    select_deck: None,
                    remove_channel: None,
                    toggle_collapsed: None,
                },
            );
        harness.run();
        harness
    }

    #[test]
    fn every_channel_has_a_tile() {
        let data = main_mode_fixture();
        let names: Vec<String> = data.channels.iter().map(|c| c.name.clone()).collect();
        let harness = harness(data);
        for name in names {
            assert!(
                harness.query_by_label(&format!("{name} tile")).is_some(),
                "{name}"
            );
        }
        assert!(harness.query_by_label("New channel tile").is_some());
    }

    #[test]
    fn clicking_a_tile_puts_it_in_preview() {
        let data = main_mode_fixture();
        let name = data.channels[1].name.clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("{name} tile")).click();
        harness.run();
        assert_eq!(harness.state().select_channel, Some(1));
        assert!(harness.state().commands.is_empty(), "a click only previews");
    }

    #[test]
    fn a_tiles_cut_takes_it_to_main_at_once() {
        let data = main_mode_fixture();
        let uuid = data.channels[1].uuid.clone();
        let mut harness = harness(data);
        // Channel 0 is on Main, so its Cut is disabled; the first enabled one
        // belongs to channel 1.
        let cuts: Vec<_> = harness.get_all_by_label("Cut").collect();
        assert!(cuts.len() >= 2);
        cuts[1].click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::TakeToMain { source: MainSource::Channel(u), fade: false } if *u == uuid
            )),
            "{:?}",
            harness.state().commands
        );
    }

    /// As vMix opens an input's settings.
    #[test]
    fn double_clicking_a_tile_opens_its_input_settings() {
        let data = main_mode_fixture();
        let ch = data.channels[1].clone();
        let mut harness = harness(data);
        let at = harness
            .get_by_label(&format!("{} tile", ch.name))
            .rect()
            .center();
        harness.hover_at(at);
        harness.run();
        for _ in 0..2 {
            harness.drag_at(at);
            harness.run();
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run();
        }
        assert_eq!(
            super::super::channel_settings::open_on_channel(&harness.ctx),
            Some(ch.uuid)
        );
    }

    #[test]
    fn a_deck_chip_selects_its_deck() {
        let data = main_mode_fixture();
        let deck = data.channels[0].decks[0].clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("{} deck", deck.name)).click();
        harness.run();
        assert_eq!(harness.state().select_deck, Some((0, deck.deck_idx)));
    }

    #[test]
    fn the_empty_tile_adds_a_channel() {
        let mut harness = harness(main_mode_fixture());
        harness.get_by_label("New channel tile").click();
        harness.run();
        assert!(harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::AddChannel)));
    }

    #[test]
    fn a_tile_catches_drops_for_its_channel() {
        let data = main_mode_fixture();
        let harness = harness(data);
        let rect: Option<egui::Rect> = harness
            .ctx
            .memory(|m| m.data.get_temp(egui::Id::new("ch_drop_rect").with(1_usize)));
        assert!(rect.is_some_and(|r| r.width() >= MIN_TILE_WIDTH));
    }

    /// The fixture with a third channel, so one can be deleted.
    fn three_channels() -> UIData {
        let mut data = main_mode_fixture();
        let mut extra = data.channels[1].clone();
        extra.ch_idx = 2;
        extra.uuid = "cc000003".into();
        extra.name = "Ch C".into();
        extra.decks.clear();
        data.channels.push(extra);
        data.channel_count = 3;
        data
    }

    fn double_click(harness: &mut egui_kittest::Harness<'static, Seen>, at: egui::Pos2) {
        harness.hover_at(at);
        harness.run();
        for _ in 0..2 {
            harness.drag_at(at);
            harness.run();
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run();
        }
    }

    #[test]
    fn right_clicking_a_name_bar_folds_the_tile() {
        let data = main_mode_fixture();
        let ch = data.channels[1].clone();
        let mut harness = harness(data);
        harness
            .get_by_label(&format!("{} name bar", ch.name))
            .click_secondary();
        harness.run();
        assert_eq!(harness.state().toggle_collapsed, Some(ch.uuid));
    }

    #[test]
    fn a_folded_channel_is_a_strip_that_right_click_opens_out() {
        let mut data = main_mode_fixture();
        let ch = data.channels[1].clone();
        data.collapsed_channels = vec![ch.uuid.clone()];
        let mut harness = harness(data);
        assert!(harness
            .query_by_label(&format!("{} tile", ch.name))
            .is_none());
        let strip = harness.get_by_label(&format!("{} strip", ch.name)).rect();
        assert!(strip.width() < 60.0, "narrow: {strip:?}");
        harness
            .get_by_label(&format!("{} strip", ch.name))
            .click_secondary();
        harness.run();
        assert_eq!(harness.state().toggle_collapsed, Some(ch.uuid));
    }

    #[test]
    fn double_clicking_a_name_bar_renames_the_channel() {
        let data = main_mode_fixture();
        let ch = data.channels[1].clone();
        let mut harness = harness(data);
        let at = harness
            .get_by_label(&format!("{} name bar", ch.name))
            .rect()
            .left_center()
            + egui::vec2(30.0, 0.0);
        double_click(&mut harness, at);
        assert_eq!(super::super::rename::renaming(&harness.ctx), Some(ch.uuid));
    }

    #[test]
    fn an_empty_channel_is_deleted_by_its_cross() {
        let mut harness = harness(three_channels());
        harness.get_by_label("Delete Ch C").click();
        harness.run();
        assert_eq!(harness.state().remove_channel, Some(2));
    }

    #[test]
    fn an_input_off_air_goes_at_once_layers_and_all() {
        let data = three_channels();
        let ch = data.channels[1].clone();
        assert!(!ch.decks.is_empty(), "precondition: it has layers");
        assert!(ch.on_air.is_none(), "precondition: off air");
        let mut harness = harness(data);
        harness.get_by_label(&format!("Delete {}", ch.name)).click();
        harness.run();
        assert_eq!(harness.state().remove_channel, Some(1));
    }

    #[test]
    fn an_input_on_a_bus_asks_and_says_which() {
        let mut data = three_channels();
        let bus = data.buses[0].bus.name.clone();
        data.channels[1].on_air = Some(bus.clone());
        let ch = data.channels[1].clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("Delete {}", ch.name)).click();
        harness.run();
        assert_eq!(harness.state().remove_channel, None, "not yet");
        assert!(harness
            .query_by_label_contains(&format!("{} is on {bus}.", ch.name))
            .is_some());
    }

    #[test]
    fn an_input_on_main_asks_before_it_goes() {
        let data = three_channels();
        let name = data.channels[0].name.clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("Delete {name}")).click();
        harness.run();
        assert_eq!(harness.state().remove_channel, None, "not yet");
        assert!(harness
            .query_by_label_contains(&format!("{name} is on Main."))
            .is_some());
        harness.get_by_label("Keep").click();
        harness.run();
        assert_eq!(harness.state().remove_channel, None, "kept");
        harness.get_by_label(&format!("Delete {name}")).click();
        harness.run();
        harness.get_by_label("Delete").click();
        harness.run();
        assert_eq!(harness.state().remove_channel, Some(0));
    }

    #[test]
    fn the_last_two_channels_cannot_be_deleted() {
        let data = main_mode_fixture();
        assert_eq!(data.channels.len(), 2);
        let name = data.channels[1].name.clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("Delete {name}")).click();
        harness.run();
        assert_eq!(harness.state().remove_channel, None);
    }

    #[test]
    fn dragging_a_name_bar_moves_the_channel() {
        let data = main_mode_fixture();
        let (first, second) = (data.channels[0].clone(), data.channels[1].clone());
        let mut harness = harness(data);
        let from = harness
            .get_by_label(&format!("{} name bar", second.name))
            .rect()
            .left_center()
            + egui::vec2(20.0, 0.0);
        let onto = harness.get_by_label(&format!("{} tile", first.name)).rect();
        // The left half of the first tile: before it.
        let to = egui::pos2(onto.left() + 20.0, onto.center().y);
        harness.hover_at(from);
        harness.drag_at(from);
        harness.run_steps(3);
        for t in [0.2_f32, 0.6, 1.0] {
            harness.hover_at(from + (to - from) * t);
            harness.run_steps(3);
        }
        harness.drop_at(to);
        harness.run_steps(3);
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::MoveChannel { channel_uuid, before_channel_uuid: Some(before) }
                    if *channel_uuid == second.uuid && *before == first.uuid
            )),
            "{:?}",
            harness.state().commands
        );
    }

    /// Front first, as in Premiere: the layer seen is the first chip.
    #[test]
    fn a_tiles_chips_put_the_layer_in_front_first() {
        let data = UIData::test_fixture();
        let back = data.channels[0].decks[0].name.clone();
        let front = data.channels[0].decks[1].name.clone();
        let harness = harness(data);
        let x = |name: &str| harness.get_by_label(&format!("{name} deck")).rect().left();
        assert!(x(&front) < x(&back), "{front} before {back}");
    }

    /// A playlist starts from its tile, without opening anything.
    #[test]
    fn a_tile_holding_a_playlist_plays_it() {
        let mut data = main_mode_fixture();
        let deck = data.channels[1].decks[0].uuid.clone();
        data.channels[1].decks[0].playlist =
            Some(crate::engine::value::playlist::PlaylistSnapshot {
                items: vec![crate::engine::value::playlist::PlaylistItem::new("/a.png")],
                settings: crate::engine::value::playlist::PlaylistSettings::default(),
                playing: false,
                shown: Some(0),
                coming: None,
                next_ready: None,
                failed: Vec::new(),
                remaining: None,
            });
        let mut harness = harness(data);
        let plays: Vec<_> = harness.get_all_by_label("▶").collect();
        assert_eq!(plays.len(), 1, "only the tile holding it has one");
        plays[0].click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetPlaylistPlaying { deck_uuid, playing: true } if *deck_uuid == deck
            )),
            "{:?}",
            harness.state().commands
        );
    }

    fn clip(sound_only: bool) -> crate::usecases::ui::VideoPlaybackUI {
        crate::usecases::ui::VideoPlaybackUI {
            playing: false,
            position: 20.0,
            duration: 60.0,
            speed: 1.0,
            effective_speed: 1.0,
            position_offset: 0.0,
            loop_mode: crate::video::LoopMode::Loop,
            in_point: 0.0,
            out_point: 0.0,
            frame_rate: 25.0,
            transport_sync: crate::video::DeckTransportSync::default(),
            hardware_decode: false,
            auto: crate::engine::value::video::AutoPlayback::default(),
            sound_only,
        }
    }

    /// A clip plays, pauses and goes back to its start from its tile, which
    /// shows the time gone and left — without putting it on Preview.
    #[test]
    fn a_tile_holding_a_clip_plays_and_restarts_it() {
        let mut data = main_mode_fixture();
        let deck = data.channels[1].decks[0].uuid.clone();
        data.channels[1].decks[0].video_playback = Some(clip(false));
        let mut harness = harness(data);
        harness.get_by_label(" 0:20 · -0:40 ");
        let plays: Vec<_> = harness.get_all_by_label("▶").collect();
        assert_eq!(plays.len(), 1, "only the tile holding it has one");
        plays[0].click();
        harness.run();
        harness.get_by_label("⏮").click();
        harness.run();
        let commands = &harness.state().commands;
        assert!(
            commands.iter().any(
                |c| matches!(c, EngineCommand::VideoTogglePlay { deck_uuid } if *deck_uuid == deck)
            ),
            "{commands:?}"
        );
        assert!(
            commands.iter().any(|c| matches!(
                c,
                EngineCommand::VideoSeek { deck_uuid, position_secs } if *deck_uuid == deck && *position_secs == 0.0
            )),
            "{commands:?}"
        );
    }

    /// A video and a picture are told apart on the chips; a sound, with no
    /// picture, says what it is on its tile.
    #[test]
    fn clips_are_marked_and_a_sound_says_what_it_is() {
        let mut data = main_mode_fixture();
        data.channels[0].decks[0].video_playback = Some(clip(false));
        data.channels[1].decks[0].video_playback = Some(clip(true));
        let video = data.channels[0].decks[0].name.clone();
        let picture = data.channels[0].decks[1].name.clone();
        let sound = data.channels[1].decks[0].name.clone();
        let harness = harness(data);
        harness.get_by_label(&format!("{video} video deck"));
        harness.get_by_label(&format!("{picture} deck"));
        harness.get_by_label(&format!("{sound} sound deck"));
        harness.get_by_label(&format!("♪ {sound}"));
    }

    /// A sound's name on its tile is not a widget of its own: a click on it
    /// is the tile's.
    #[test]
    fn clicking_a_sounds_name_selects_its_tile() {
        let mut data = main_mode_fixture();
        data.selected_channel = None;
        data.channels[1].decks[0].video_playback = Some(clip(true));
        let sound = data.channels[1].decks[0].name.clone();
        let mut harness = harness(data);
        harness.get_by_label(&format!("♪ {sound}")).click();
        harness.run();
        assert_eq!(harness.state().select_channel, Some(1));
    }

    /// The tile's clip controls are the top clip's, which the tile shows —
    /// named when the input holds others — whatever layer is selected.
    #[test]
    fn a_tiles_clip_controls_are_its_top_clips_and_say_so() {
        let mut data = main_mode_fixture();
        data.channels[1].decks[0].video_playback = Some(clip(false));
        data.channels[1].decks[1].video_playback = Some(clip(false));
        let top = data.channels[1].decks[1].uuid.clone();
        let name = data.channels[1].decks[1].name.clone();
        data.selected_deck = Some((1, 0));
        let mut harness = harness(data);
        harness.get_by_label(&format!(" {name} · 0:20 · -0:40 "));
        harness.get_by_label("▶").click();
        harness.run();
        assert!(
            harness.state().commands.iter().any(
                |c| matches!(c, EngineCommand::VideoTogglePlay { deck_uuid } if *deck_uuid == top)
            ),
            "{:?}",
            harness.state().commands
        );
    }
}
