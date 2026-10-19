//! LED Screens: an output's feed carrying several LED screens at their own
//! pixels (see [`crate::engine::value::led`]), as Resolume's Advanced Output.
//!
//! The window lists the feed's screens and shows two maps of them: the
//! **Feed** — where each screen sits in the one signal the LED processor
//! takes — and the **Picture** — which part of Main or a bus a screen shows
//! when it takes a part rather than the whole. Screens are dragged on either
//! map, snapping to the frame and to each other, or typed in pixels.

use super::super::{OutputUI, UIActions, UIData};
use crate::engine::value::led::{LedFit, LedLayout, LedScreen};
use crate::engine::EngineCommand;
use crate::renderer::context::OutputSource;

/// Within this many points of an edge a dragged screen snaps to it.
const SNAP_POINTS: f32 = 6.0;
/// Width of the list and settings column.
const COLUMN: f32 = 270.0;

#[derive(Clone, Default)]
struct Editor {
    open: bool,
    /// The output whose feed is edited.
    output: Option<String>,
    /// The screen selected, by id.
    selected: Option<String>,
    /// Showing the Picture map rather than the Feed.
    picture_view: bool,
    /// Whose picture the Picture map shows.
    picture: Option<OutputSource>,
    drag: Option<Drag>,
}

#[derive(Clone)]
struct Drag {
    screen: String,
    /// The rectangle being moved or sized, as it was when the drag began, in
    /// the map's pixels.
    start: [i32; 4],
    resize: bool,
}

fn state_id() -> egui::Id {
    egui::Id::new("led_editor")
}

fn load(ctx: &egui::Context) -> Editor {
    ctx.data(|d| d.get_temp(state_id())).unwrap_or_default()
}

fn store(ctx: &egui::Context, editor: Editor) {
    ctx.data_mut(|d| d.insert_temp(state_id(), editor));
}

/// Open the LED Screens window.
pub(super) fn open(ctx: &egui::Context) {
    let mut editor = load(ctx);
    editor.open = true;
    store(ctx, editor);
}

/// Whether the window is open, for tests.
#[cfg(test)]
pub(super) fn is_open(ctx: &egui::Context) -> bool {
    load(ctx).open
}

pub(super) fn render_led_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let mut editor = load(ctx);
    if !editor.open {
        return;
    }
    let mut open = true;
    egui::Window::new("LED Screens")
        .id(egui::Id::new("led_window"))
        .open(&mut open)
        .default_size([980.0, 620.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| body(ui, data, actions, &mut editor));
    editor.open &= open;
    store(ctx, editor);
}

fn body(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions, editor: &mut Editor) {
    let is_feed = |uuid: &String| {
        data.outputs
            .iter()
            .any(|o| &o.uuid == uuid && o.led.is_some())
    };
    if !editor.output.as_ref().is_some_and(is_feed) {
        editor.output = data
            .outputs
            .iter()
            .find(|o| o.led.is_some())
            .map(|o| o.uuid.clone());
    }
    ui.horizontal(|ui| {
        feed_picker(ui, data, editor);
        new_feed_menu(ui, data, actions);
    });
    let Some((index, output)) = data
        .outputs
        .iter()
        .enumerate()
        .find(|(_, o)| Some(&o.uuid) == editor.output.as_ref())
    else {
        ui.add_space(12.0);
        ui.label(
            "No LED feed yet. A feed is the one signal the LED processor takes: open \
             one on the screen output the processor is connected to, then add the \
             screens it carries.",
        );
        return;
    };
    let Some(mut layout) = output.led.clone() else {
        return;
    };
    let before = layout.clone();
    if editor
        .selected
        .as_ref()
        .is_none_or(|s| !layout.screens.iter().any(|x| &x.uuid == s))
    {
        editor.selected = layout.screens.first().map(|s| s.uuid.clone());
    }

    feed_settings(ui, output, &mut layout, actions);
    ui.separator();
    let height = ui.available_height();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(COLUMN, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("led_column")
                    .show(ui, |ui| {
                        ui.set_width(COLUMN);
                        screen_list(ui, data, &mut layout, editor);
                        ui.separator();
                        screen_settings(ui, data, output, &mut layout, editor, actions);
                    });
            },
        );
        ui.separator();
        ui.vertical(|ui| maps(ui, data, index, &mut layout, editor));
    });
    if layout != before {
        push(actions, &output.uuid, Some(layout));
    }
}

fn push(actions: &mut UIActions, output: &str, layout: Option<LedLayout>) {
    actions.commands.push(EngineCommand::SetOutputLed {
        output_uuid: output.to_string(),
        layout,
    });
}

/// Which LED feed is edited, when there is more than one.
fn feed_picker(ui: &mut egui::Ui, data: &UIData, editor: &mut Editor) {
    let feeds: Vec<&OutputUI> = data.outputs.iter().filter(|o| o.led.is_some()).collect();
    if feeds.is_empty() {
        return;
    }
    ui.label("Feed:");
    let current = feeds
        .iter()
        .find(|o| Some(&o.uuid) == editor.output.as_ref())
        .map_or("", |o| o.name.as_str());
    egui::ComboBox::from_id_salt("led_feed")
        .selected_text(current)
        .show_ui(ui, |ui| {
            for feed in feeds {
                let chosen = Some(&feed.uuid) == editor.output.as_ref();
                if ui
                    .selectable_label(chosen, format!("{} — {}", feed.name, feed.target_label))
                    .clicked()
                {
                    editor.output = Some(feed.uuid.clone());
                    editor.selected = None;
                }
            }
        });
}

/// Open a new feed on a screen output or in a window, or make an existing
/// output one.
fn new_feed_menu(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    ui.menu_button("+ New feed", |ui| {
        let mut monitors: Vec<_> = data.available_monitors.iter().collect();
        monitors.sort_by_key(|m| m.is_control);
        for monitor in monitors {
            let mut label = format!("On {}", monitor.label);
            if monitor.is_control {
                label.push_str(" — Nida's own screen");
            }
            if ui.button(label).clicked() {
                actions.commands.push(EngineCommand::CreateLedOutput {
                    monitor_name: Some(monitor.name.clone()),
                    layout: LedLayout::single([monitor.width.max(1), monitor.height.max(1)]),
                });
                ui.close();
            }
        }
        if ui.button("In a window (1920 × 1080)").clicked() {
            actions.commands.push(EngineCommand::CreateLedOutput {
                monitor_name: None,
                layout: LedLayout::single([1920, 1080]),
            });
            ui.close();
        }
        let others: Vec<&OutputUI> = data.outputs.iter().filter(|o| o.led.is_none()).collect();
        if !others.is_empty() {
            ui.separator();
            for output in others {
                if ui
                    .button(format!("Use {} — {}", output.name, output.target_label))
                    .on_hover_text("It stops showing what it shows now and carries the screens")
                    .clicked()
                {
                    let size = [output.preview_width.max(1), output.preview_height.max(1)];
                    push(actions, &output.uuid, Some(LedLayout::single(size)));
                    ui.close();
                }
            }
        }
    });
}

/// The feed's size, the test card, and giving the output back.
fn feed_settings(
    ui: &mut egui::Ui,
    output: &OutputUI,
    layout: &mut LedLayout,
    actions: &mut UIActions,
) {
    ui.horizontal(|ui| {
        ui.label("Feed size");
        size_fields(ui, &mut layout.feed);
        ui.menu_button("⏷", |ui| {
            let mut sizes = vec![[1920, 1080], [3840, 2160]];
            let own = [output.preview_width, output.preview_height];
            if own[0] > 0 && !sizes.contains(&own) {
                sizes.push(own);
            }
            for size in sizes {
                if ui.button(format!("{} × {}", size[0], size[1])).clicked() {
                    layout.feed = size;
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text("What the LED processor takes in");
        ui.add_space(12.0);
        ui.checkbox(&mut layout.test_card, "Test card")
            .on_hover_text(
                "Every screen shows its name, size, place and a white edge instead of \
                 its picture, to line the feed up with the processor",
            );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Not an LED feed")
                .on_hover_text(format!("{} shows what it did before", output.name))
                .clicked()
            {
                push(actions, &output.uuid, None);
            }
        });
    });
}

fn size_fields(ui: &mut egui::Ui, size: &mut [u32; 2]) {
    let max = crate::engine::value::led::MAX_LED_PIXELS;
    ui.add(
        egui::DragValue::new(&mut size[0])
            .range(1..=max)
            .suffix(" px"),
    );
    ui.label("×");
    ui.add(
        egui::DragValue::new(&mut size[1])
            .range(1..=max)
            .suffix(" px"),
    );
}

fn colour(index: usize) -> egui::Color32 {
    let [r, g, b] = crate::renderer::led_card::screen_colour(index);
    egui::Color32::from_rgb(r, g, b)
}

fn source_name(data: &UIData, source: &OutputSource) -> String {
    match source {
        OutputSource::BusProgram(uuid) => data
            .buses
            .iter()
            .find(|b| &b.bus.uuid == uuid)
            .map_or_else(|| "(no bus)".to_string(), |b| b.bus.name.clone()),
        _ => "Main".to_string(),
    }
}

/// The size of `source`'s picture: the master's, or a bus's region of it.
fn picture_size(data: &UIData, source: &OutputSource) -> [u32; 2] {
    let master = [data.render_width.max(1), data.render_height.max(1)];
    match source {
        OutputSource::BusProgram(uuid) => data
            .buses
            .iter()
            .find(|b| &b.bus.uuid == uuid)
            .map_or(master, |b| [b.bus.canvas.width, b.bus.canvas.height]),
        _ => master,
    }
}

/// The colours of a list row's name and of the line under it. A row's text
/// is coloured here, so egui does not give the chosen one its selected
/// colour: chosen, both lines take it — the dim second line was faint on the
/// selection's blue — and the smaller type keeps them apart.
fn row_colours(visuals: &egui::Visuals, chosen: bool) -> (egui::Color32, egui::Color32) {
    if chosen {
        let selected = visuals.selection.stroke.color;
        (selected, selected)
    } else {
        (visuals.text_color(), visuals.weak_text_color())
    }
}

fn screen_list(ui: &mut egui::Ui, data: &UIData, layout: &mut LedLayout, editor: &mut Editor) {
    ui.label(egui::RichText::new("Screens").strong());
    for (i, screen) in layout.screens.iter().enumerate() {
        ui.horizontal(|ui| {
            let (swatch, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(swatch, 2.0, colour(i));
            let chosen = editor.selected.as_ref() == Some(&screen.uuid);
            // Where it is in the feed too: listed in the order made, screens
            // named for the wall's pieces read out of order with them — an
            // operator faded to "Bar 3" and the middle bar changed. On a line
            // of its own, under the name, and both wrapped: in a row egui
            // lets text run on, and a long name widened the column and
            // squeezed the maps.
            let (name_colour, where_colour) = row_colours(ui.visuals(), chosen);
            let mut label = egui::text::LayoutJob::default();
            label.append(
                &screen.name,
                0.0,
                egui::TextFormat {
                    font_id: egui::TextStyle::Body.resolve(ui.style()),
                    color: name_colour,
                    ..Default::default()
                },
            );
            label.append(
                &format!(
                    "\nat {},{}  {}×{}  {}",
                    screen.at[0],
                    screen.at[1],
                    screen.size[0],
                    screen.size[1],
                    source_name(data, &screen.source)
                ),
                0.0,
                egui::TextFormat {
                    font_id: egui::TextStyle::Small.resolve(ui.style()),
                    color: where_colour,
                    ..Default::default()
                },
            );
            if ui.add(egui::Button::selectable(chosen, label).wrap()).clicked() {
                editor.selected = Some(screen.uuid.clone());
            }
        });
    }
    ui.horizontal(|ui| {
        if ui.button("+ Add screen").clicked() {
            let screen = new_screen(layout);
            editor.selected = Some(screen.uuid.clone());
            layout.screens.push(screen);
        }
        ui.menu_button("Arrange", |ui| {
            if ui
                .button("Pack into the feed")
                .on_hover_text("Side by side from the top left, in list order")
                .clicked()
            {
                layout.pack_feed();
                ui.close();
            }
            let source = editor.picture.clone().unwrap_or(OutputSource::Master);
            if ui
                .button(format!("Parts of {} in a row", source_name(data, &source)))
                .on_hover_text(
                    "The screens taking parts of this picture, as they stand: in list \
                     order from the left, centred",
                )
                .clicked()
            {
                layout.arrange_parts(&source, picture_size(data, &source));
                ui.close();
            }
        });
    });
}

/// A new screen: a 300 px square to the right of the others, or under them
/// when that runs off the feed. A new id is made up here so the screen can
/// be selected before the engine has it.
fn new_screen(layout: &LedLayout) -> LedScreen {
    let n = layout.screens.len() + 1;
    let size = if layout.screens.is_empty() {
        layout.feed
    } else {
        [300, 300]
    };
    let right = layout
        .screens
        .iter()
        .map(|s| s.at[0] + s.size[0].cast_signed())
        .max()
        .unwrap_or(0);
    let below = layout
        .screens
        .iter()
        .map(|s| s.at[1] + s.size[1].cast_signed())
        .max()
        .unwrap_or(0);
    let at = if right + size[0].cast_signed() <= layout.feed[0].cast_signed() {
        [right, 0]
    } else {
        [0, below]
    };
    let mut screen = LedScreen::new(format!("Screen {n}"), size, at, OutputSource::Master);
    screen.uuid = crate::deck::generate_short_uuid();
    // A square screen is filled — a logo or a loop made for it covers it; a
    // wide one is fitted, so slides keep their edges.
    if size[0] < size[1] * 6 / 5 {
        screen.fit = LedFit::Fill;
    }
    screen
}

fn screen_settings(
    ui: &mut egui::Ui,
    data: &UIData,
    output: &OutputUI,
    layout: &mut LedLayout,
    editor: &mut Editor,
    actions: &mut UIActions,
) {
    let Some(index) = layout
        .screens
        .iter()
        .position(|s| Some(&s.uuid) == editor.selected.as_ref())
    else {
        return;
    };
    let mut remove = false;
    let mut duplicate = false;
    let screen = &mut layout.screens[index];
    egui::Grid::new("led_screen_grid")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Name");
            ui.add(egui::TextEdit::singleline(&mut screen.name).desired_width(150.0));
            ui.end_row();
            ui.label("Size");
            ui.horizontal(|ui| size_fields(ui, &mut screen.size));
            ui.end_row();
            ui.label("At");
            ui.horizontal(|ui| {
                let max = crate::engine::value::led::MAX_LED_PIXELS.cast_signed();
                ui.add(
                    egui::DragValue::new(&mut screen.at[0])
                        .range(-max..=max)
                        .prefix("x "),
                );
                ui.add(
                    egui::DragValue::new(&mut screen.at[1])
                        .range(-max..=max)
                        .prefix("y "),
                );
            });
            ui.end_row();
            ui.label("Shows");
            egui::ComboBox::from_id_salt("led_source")
                .selected_text(source_name(data, &screen.source))
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut screen.source, OutputSource::Master, "Main");
                    for bus in &data.buses {
                        let source = OutputSource::BusProgram(bus.bus.uuid.clone());
                        ui.selectable_value(&mut screen.source, source, &bus.bus.name);
                    }
                    ui.separator();
                    if ui
                        .button("New bus for this screen")
                        .on_hover_text(
                            "A bus named after the screen, with its own Preview and \
                             Program in the switcher: cut anything to it",
                        )
                        .clicked()
                    {
                        actions.commands.push(EngineCommand::CreateLedScreenBus {
                            output_uuid: output.uuid.clone(),
                            screen_uuid: screen.uuid.clone(),
                        });
                        ui.close();
                    }
                });
            ui.end_row();
            ui.label("How");
            let part = LedFit::Part {
                x: screen.at[0],
                y: screen.at[1],
                width: screen.size[0],
                height: screen.size[1],
            };
            let current = if screen.fit.is_part() {
                part
            } else {
                screen.fit
            };
            egui::ComboBox::from_id_salt("led_fit")
                .selected_text(screen.fit.label())
                .show_ui(ui, |ui| {
                    for fit in [LedFit::Fit, LedFit::Fill, LedFit::Stretch, part] {
                        let chosen =
                            std::mem::discriminant(&current) == std::mem::discriminant(&fit);
                        let hover = match fit {
                            LedFit::Fit => "The whole picture, as large as fits",
                            LedFit::Fill => "The screen covered; the picture's overflow cut off",
                            LedFit::Stretch => "The whole picture pulled to the screen's shape",
                            LedFit::Part { .. } => {
                                "Its part of the picture: screens taking parts of one \
                                 picture continue it across them"
                            }
                        };
                        if ui
                            .selectable_label(chosen, fit.label())
                            .on_hover_text(hover)
                            .clicked()
                            && !chosen
                        {
                            screen.fit = fit;
                        }
                    }
                });
            ui.end_row();
            if let LedFit::Part {
                x,
                y,
                width,
                height,
            } = &mut screen.fit
            {
                ui.label("Part");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(x).prefix("x "));
                        ui.add(egui::DragValue::new(y).prefix("y "));
                    });
                    ui.horizontal(|ui| {
                        let max = crate::engine::value::led::MAX_LED_PIXELS;
                        ui.add(egui::DragValue::new(width).range(1..=max).prefix("w "));
                        ui.add(egui::DragValue::new(height).range(1..=max).prefix("h "));
                        if ui
                            .small_button("1:1")
                            .on_hover_text("One pixel of picture per LED pixel")
                            .clicked()
                        {
                            *width = screen.size[0];
                            *height = screen.size[1];
                        }
                    });
                });
                ui.end_row();
            }
        });
    ui.horizontal(|ui| {
        duplicate = ui.button("Duplicate").clicked();
        remove = ui.button("🗑 Remove").clicked();
    });
    if duplicate {
        let mut copy = layout.screens[index].clone();
        copy.uuid = crate::deck::generate_short_uuid();
        copy.name = format!("{} copy", copy.name);
        copy.at[0] += copy.size[0].cast_signed();
        editor.selected = Some(copy.uuid.clone());
        layout.screens.insert(index + 1, copy);
    }
    if remove {
        layout.screens.remove(index);
        editor.selected = None;
    }
}

/// The Feed and Picture maps.
fn maps(
    ui: &mut egui::Ui,
    data: &UIData,
    output_index: usize,
    layout: &mut LedLayout,
    editor: &mut Editor,
) {
    let part_sources: Vec<OutputSource> = layout
        .screens
        .iter()
        .filter(|s| s.fit.is_part())
        .map(|s| s.source.clone())
        .fold(Vec::new(), |mut all, s| {
            if !all.contains(&s) {
                all.push(s);
            }
            all
        });
    if editor
        .picture
        .as_ref()
        .is_none_or(|p| !part_sources.contains(p))
    {
        let selected = layout
            .screens
            .iter()
            .find(|s| Some(&s.uuid) == editor.selected.as_ref() && s.fit.is_part());
        editor.picture = selected
            .map(|s| s.source.clone())
            .or_else(|| part_sources.first().cloned());
    }
    ui.horizontal(|ui| {
        ui.selectable_value(&mut editor.picture_view, false, "Feed")
            .on_hover_text("Where each screen sits in the signal to the processor");
        ui.selectable_value(&mut editor.picture_view, true, "Picture")
            .on_hover_text("The part of the picture each screen taking a part shows");
        if editor.picture_view && part_sources.len() > 1 {
            let current = editor.picture.clone().unwrap_or(OutputSource::Master);
            egui::ComboBox::from_id_salt("led_picture")
                .selected_text(source_name(data, &current))
                .show_ui(ui, |ui| {
                    for source in &part_sources {
                        let name = source_name(data, source);
                        ui.selectable_value(&mut editor.picture, Some(source.clone()), name);
                    }
                });
        }
    });
    if editor.picture_view {
        let Some(source) = editor.picture.clone() else {
            ui.add_space(20.0);
            ui.label(
                "No screen takes a part of a picture. Set a screen's How to \"Part of the \
                 picture\" to continue one image across several screens.",
            );
            return;
        };
        let frame = picture_size(data, &source);
        let texture = matches!(source, OutputSource::Master)
            .then_some(data.main_output_texture)
            .flatten();
        let rects: Vec<(usize, [i32; 4])> = layout
            .screens
            .iter()
            .enumerate()
            .filter(|(_, s)| s.source == source)
            .filter_map(|(i, s)| match s.fit {
                LedFit::Part {
                    x,
                    y,
                    width,
                    height,
                } => Some((i, [x, y, width.cast_signed(), height.cast_signed()])),
                _ => None,
            })
            .collect();
        if let Some((i, rect)) = map(
            ui,
            "led_picture_map",
            frame,
            texture,
            &rects,
            layout,
            editor,
            true,
        ) {
            if let LedFit::Part {
                x,
                y,
                width,
                height,
            } = &mut layout.screens[i].fit
            {
                *x = rect[0];
                *y = rect[1];
                *width = rect[2].max(1) as u32;
                *height = rect[3].max(1) as u32;
            }
        }
    } else {
        let texture = data.output_preview_textures.get(&output_index).copied();
        let rects: Vec<(usize, [i32; 4])> = layout
            .screens
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.feed_rect()))
            .collect();
        let frame = layout.feed;
        if let Some((i, rect)) = map(
            ui,
            "led_feed_map",
            frame,
            texture,
            &rects,
            layout,
            editor,
            false,
        ) {
            layout.screens[i].at = [rect[0], rect[1]];
        }
    }
}

/// One map: the `frame` (pixels) drawn to fit, `texture` in it when there is
/// a live picture, and a box per `rects` entry (screen index, rectangle in
/// pixels). Returns the rectangle a drag or a nudge moved a box to. On the
/// Picture map (`sizable`) the corner handle sizes a box, keeping the
/// screen's shape.
#[allow(clippy::too_many_arguments)]
fn map(
    ui: &mut egui::Ui,
    salt: &str,
    frame: [u32; 2],
    texture: Option<egui::TextureId>,
    rects: &[(usize, [i32; 4])],
    layout: &LedLayout,
    editor: &mut Editor,
    sizable: bool,
) -> Option<(usize, [i32; 4])> {
    ui.label(
        egui::RichText::new(if sizable {
            "Drag a screen to move its part, its corner to size it · arrow keys: 1 px, with Shift 10"
        } else {
            "Drag a screen to move it — it snaps to the edges and to the others · arrow keys: 1 px, with Shift 10"
        })
        .small()
        .weak(),
    );
    let space = ui.available_size().max(egui::vec2(240.0, 160.0));
    let (area, _) = ui.allocate_exact_size(space, egui::Sense::hover());
    let [fw, fh] = frame.map(|v| v.max(1) as f32);
    let scale = (area.width() / fw).min(area.height() / fh);
    let frame_rect = egui::Rect::from_center_size(area.center(), egui::vec2(fw, fh) * scale);
    let painter = ui.painter_at(area);
    painter.rect_filled(frame_rect, 0.0, egui::Color32::from_gray(18));
    if let Some(texture) = texture {
        painter.image(
            texture,
            frame_rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }
    painter.rect_stroke(
        frame_rect,
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(90)),
        egui::StrokeKind::Outside,
    );
    let to_screen = |r: [i32; 4]| {
        egui::Rect::from_min_size(
            frame_rect.min + egui::vec2(r[0] as f32, r[1] as f32) * scale,
            egui::vec2(r[2] as f32, r[3] as f32) * scale,
        )
    };
    let mut moved = None;
    for &(i, rect) in rects {
        let screen = &layout.screens[i];
        let shown = to_screen(rect);
        let chosen = editor.selected.as_ref() == Some(&screen.uuid);
        let c = colour(i);
        painter.rect_filled(
            shown,
            0.0,
            c.gamma_multiply(if texture.is_some() { 0.12 } else { 0.35 }),
        );
        painter.rect_stroke(
            shown,
            0.0,
            egui::Stroke::new(if chosen { 2.5 } else { 1.0 }, c),
            egui::StrokeKind::Inside,
        );
        painter.with_clip_rect(shown.intersect(area)).text(
            shown.left_top() + egui::vec2(4.0, 3.0),
            egui::Align2::LEFT_TOP,
            format!("{}\n{}×{}", screen.name, screen.size[0], screen.size[1]),
            egui::FontId::proportional(11.0),
            egui::Color32::WHITE,
        );
        let id = ui.id().with(salt).with(&screen.uuid);
        let body = ui
            .interact(shown, id, egui::Sense::click_and_drag())
            .on_hover_cursor(egui::CursorIcon::Grab);
        body.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                true,
                format!("{}\n{}×{}", screen.name, screen.size[0], screen.size[1]),
            )
        });
        let handle_rect =
            egui::Rect::from_center_size(shown.right_bottom(), egui::vec2(10.0, 10.0));
        let handle = sizable.then(|| {
            painter.rect_filled(handle_rect, 1.0, c);
            ui.interact(handle_rect, id.with("size"), egui::Sense::drag())
                .on_hover_cursor(egui::CursorIcon::ResizeNwSe)
        });
        let resizing = handle.as_ref().is_some_and(egui::Response::drag_started);
        if body.drag_started() || resizing {
            editor.drag = Some(Drag {
                screen: screen.uuid.clone(),
                start: rect,
                resize: resizing,
            });
            editor.selected = Some(screen.uuid.clone());
        }
        if body.clicked() {
            editor.selected = Some(screen.uuid.clone());
        }
        let dragging = body.dragged() || handle.as_ref().is_some_and(egui::Response::dragged);
        if dragging {
            if let Some(drag) = editor.drag.clone().filter(|d| d.screen == screen.uuid) {
                let total = ui
                    .input(|i| i.pointer.press_origin().zip(i.pointer.latest_pos()))
                    .map_or(egui::Vec2::ZERO, |(a, b)| b - a);
                let [dx, dy] = [total.x, total.y].map(|v| (v / scale).round() as i32);
                let mut r = drag.start;
                if drag.resize {
                    // Keep the screen's shape: the width leads.
                    let w = (drag.start[2] + dx).max(1);
                    let h = (w as f32 * screen.size[1] as f32 / screen.size[0].max(1) as f32)
                        .round()
                        .max(1.0) as i32;
                    r[2] = w;
                    r[3] = h;
                } else {
                    r[0] += dx;
                    r[1] += dy;
                    let others: Vec<[i32; 4]> = rects
                        .iter()
                        .filter(|(j, _)| *j != i)
                        .map(|(_, o)| *o)
                        .collect();
                    let reach = (SNAP_POINTS / scale).max(1.0) as i32;
                    snap(&mut r, frame, &others, reach);
                }
                moved = Some((i, r));
            }
        }
        if body.drag_stopped() || handle.as_ref().is_some_and(egui::Response::drag_stopped) {
            editor.drag = None;
        }
    }
    // Arrow keys move the selected screen a pixel (ten with Shift) while the
    // pointer is over the map.
    if moved.is_none() && ui.rect_contains_pointer(area) && !ui.ctx().egui_wants_keyboard_input() {
        if let Some(&(i, r)) = rects
            .iter()
            .find(|(i, _)| editor.selected.as_ref() == Some(&layout.screens[*i].uuid))
        {
            let (step, dx, dy) = ui.input(|input| {
                let step = if input.modifiers.shift { 10 } else { 1 };
                let key = |k| i32::from(input.key_pressed(k));
                (
                    step,
                    key(egui::Key::ArrowRight) - key(egui::Key::ArrowLeft),
                    key(egui::Key::ArrowDown) - key(egui::Key::ArrowUp),
                )
            });
            if dx != 0 || dy != 0 {
                moved = Some((i, [r[0] + dx * step, r[1] + dy * step, r[2], r[3]]));
            }
        }
    }
    moved
}

/// Pull `r` onto the frame's edges and the `others`' edges within `reach`
/// pixels, each axis on its own.
fn snap(r: &mut [i32; 4], frame: [u32; 2], others: &[[i32; 4]], reach: i32) {
    for (axis, extent) in frame.into_iter().enumerate() {
        let (pos, len) = (axis, axis + 2);
        let size = r[len];
        let mut targets = vec![0, extent.cast_signed() - size];
        for o in others {
            targets.extend([
                o[pos],
                o[pos] + o[len],
                o[pos] - size,
                o[pos] + o[len] - size,
            ]);
        }
        if let Some(best) = targets
            .into_iter()
            .filter(|t| (t - r[pos]).abs() <= reach)
            .min_by_key(|t| (t - r[pos]).abs())
        {
            r[pos] = best;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn feed_output(led: Option<LedLayout>) -> OutputUI {
        OutputUI {
            uuid: "ledout01".into(),
            name: "LED".into(),
            target: crate::renderer::context::OutputTarget::Windowed,
            target_label: "Windowed".into(),
            recording_file: None,
            is_windowed: true,
            is_active: true,
            active_duration: std::time::Duration::ZERO,
            surface_assignments: vec![],
            calibration_mode: crate::renderer::context::CalibrationMode::Off,
            edge_blend_mode: crate::renderer::edge_blend::EdgeBlendMode::default(),
            edge_blend: crate::renderer::edge_blend::EdgeBlendConfig::default(),
            rotation: crate::renderer::context::OutputRotation::default(),
            presentation_request: crate::engine::value::render::PresentationRequest::default(),
            resolved_presentation: crate::engine::value::render::ResolvedPresentation::default(),
            audio_passthrough: None,
            delivery: None,
            preview_width: 1920,
            preview_height: 1080,
            source: None,
            audio_bus: None,
            led,
        }
    }

    fn wall_and_cube() -> LedLayout {
        let mut wall = LedScreen::new("Wall", [1200, 720], [0, 0], OutputSource::Master);
        wall.uuid = "wall0001".into();
        let mut cube = LedScreen::new("Cube", [300, 300], [1500, 0], OutputSource::Master);
        cube.uuid = "cube0001".into();
        cube.fit = LedFit::Fill;
        LedLayout {
            feed: [1920, 1080],
            screens: vec![wall, cube],
            test_card: false,
        }
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        let data = std::rc::Rc::new(data);
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1100.0, 700.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open(ui.ctx());
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_led_window(ui.ctx(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        harness
    }

    fn last_layout(commands: &[EngineCommand]) -> Option<LedLayout> {
        commands.iter().rev().find_map(|c| match c {
            EngineCommand::SetOutputLed { layout, .. } => layout.clone(),
            _ => None,
        })
    }

    fn data_with(led: Option<LedLayout>) -> UIData {
        let mut data = UIData::test_fixture();
        data.outputs = vec![feed_output(led)];
        data
    }

    #[test]
    fn with_no_feed_a_new_one_opens_in_a_window() {
        let mut harness = harness(data_with(None));
        assert!(is_open(&harness.ctx));
        harness.get_by_label("+ New feed").click();
        harness.run();
        harness.get_by_label("In a window (1920 × 1080)").click();
        harness.run();
        assert!(
            harness.state().iter().any(|c| matches!(
                c,
                EngineCommand::CreateLedOutput { monitor_name: None, layout } if layout.feed == [1920, 1080]
            )),
            "{:?}",
            harness.state()
        );
    }

    #[test]
    fn adding_a_screen_puts_a_square_beside_the_others_filled() {
        let mut harness = harness(data_with(Some(wall_and_cube())));
        harness.get_by_label("+ Add screen").click();
        harness.run();
        let layout = last_layout(harness.state()).expect("a new layout");
        assert_eq!(layout.screens.len(), 3);
        let added = &layout.screens[2];
        assert_eq!(
            (added.size, added.at),
            ([300, 300], [0, 720]),
            "under: no room on the right"
        );
        assert_eq!(added.fit, LedFit::Fill);
        assert_eq!(added.uuid.len(), 8);
    }

    #[test]
    fn a_screen_dragged_on_the_feed_moves_and_snaps_to_its_neighbour() {
        let mut harness = harness(data_with(Some(wall_and_cube())));
        // The cube is at 1500; the wall ends at 1200. Drag it most of the way
        // left: it lands exactly against the wall.
        let cube = harness.get_by_label_contains("Cube\n300×300").rect();
        let start = cube.center();
        let frame_scale = {
            let wall = harness.get_by_label_contains("Wall\n1200×720").rect();
            wall.width() / 1200.0
        };
        let end = start - egui::vec2((300.0 - 3.0) * frame_scale, 0.0);
        harness.hover_at(start);
        harness.drag_at(start);
        harness.run();
        for step in 1..=5 {
            let t = step as f32 / 5.0;
            harness.hover_at(start + (end - start) * t);
            harness.run();
        }
        let layout = last_layout(harness.state()).expect("moved");
        assert_eq!(layout.screens[1].at, [1200, 0], "snapped against the wall");
    }

    /// Each screen in the list says where it sits in the feed, so screens
    /// named in a different order from the wall's pieces show it.
    #[test]
    fn the_list_says_where_each_screen_sits() {
        let harness = harness(data_with(Some(wall_and_cube())));
        harness.get_by_label_contains("Cube\nat 1500,0  300×300");
        harness.get_by_label_contains("Wall\nat 0,0  1200×720");
    }

    /// A chosen row's two lines take the selected colour, the dim one too;
    /// the others keep the plain and the dim.
    #[test]
    fn a_chosen_rows_lines_both_take_the_selected_colour() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let selected = visuals.selection.stroke.color;
            assert_eq!(row_colours(&visuals, true), (selected, selected));
            assert_eq!(
                row_colours(&visuals, false),
                (visuals.text_color(), visuals.weak_text_color())
            );
        }
    }

    /// A long name wraps within the list's column rather than widen it and
    /// squeeze the maps.
    #[test]
    fn a_long_screen_name_wraps_within_the_column() {
        let mut layout = wall_and_cube();
        layout.screens[1].name = "Main stage backdrop, left wing, upper half, behind the lectern".into();
        let harness = harness(data_with(Some(layout)));
        let row = harness.get_by_label_contains("the lectern\nat 1500,0").rect();
        assert!(row.width() <= COLUMN, "{row:?}");
    }

    #[test]
    fn a_screen_can_get_a_bus_of_its_own() {
        let mut harness = harness(data_with(Some(wall_and_cube())));
        harness.get_by_label_contains("Cube\nat 1500,0").click();
        harness.run();
        harness.get_by_value("Main").click();
        harness.run();
        harness.get_by_label("New bus for this screen").click();
        harness.run();
        assert!(
            harness.state().iter().any(|c| matches!(
                c,
                EngineCommand::CreateLedScreenBus { output_uuid, screen_uuid }
                    if output_uuid == "ledout01" && screen_uuid == "cube0001"
            )),
            "{:?}",
            harness.state()
        );
    }

    #[test]
    fn snapping_reaches_the_frame_and_each_neighbouring_edge() {
        let mut r = [1195, 3, 300, 300];
        snap(&mut r, [1920, 1080], &[[0, 0, 1200, 720]], 6);
        assert_eq!(r, [1200, 0, 300, 300]);
        let mut far = [1150, 40, 300, 300];
        snap(&mut far, [1920, 1080], &[[0, 0, 1200, 720]], 6);
        assert_eq!(far, [1150, 40, 300, 300], "out of reach stays put");
        let mut edge = [1618, 778, 300, 300];
        snap(&mut edge, [1920, 1080], &[], 6);
        assert_eq!(edge, [1620, 780, 300, 300], "the frame's far corner");
    }
}
