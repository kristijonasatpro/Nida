//! Production switcher: one card per bus with Preview/Program, the sources it
//! can take, `CUT` / `AUTO`, and the screen it is shown on.
//!
//! Clicking a source only stages it on Preview; nothing reaches air until
//! `CUT` or `AUTO`. Program is red and Preview green, as on a hardware switcher.

use super::super::{BusUI, MonitorInfo, UIActions, UIData};
use crate::bus::BusInputRef;
use crate::engine::EngineCommand;

pub(super) const PROGRAM_RED: egui::Color32 = egui::Color32::from_rgb(190, 40, 40);
pub(super) const PREVIEW_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 150, 60);
/// `AUTO` durations offered, in seconds. A fixed menu rather than a drag
/// value: one pick is one command, not a stream of them while dragging.
pub(super) const AUTO_DURATIONS: [f32; 6] = [0.5, 1.0, 1.5, 2.0, 3.0, 5.0];
/// A source button: big enough to hit without looking twice, and every one
/// the same size — a long name takes two lines, then ends in "…".
const SOURCE_BUTTON: egui::Vec2 = egui::vec2(112.0, 44.0);
/// `CUT` and `AUTO`: bigger than a source, apart from them on the right.
const TAKE_BUTTON: egui::Vec2 = egui::vec2(120.0, 46.0);
/// The column the take buttons stand in.
const TAKE_COLUMN: f32 = 128.0;
/// The coloured band along the top of a channel's button.
const COLOUR_BAND: f32 = 5.0;
/// The room the line between the sources and the takes stands in.
const DIVIDER: f32 = 10.0;
/// The most of a screen's name the picker shows; the rest is "…".
const SCREEN_NAME_CHARS: usize = 20;

/// `text` cut to `chars` characters, ending in "…" when cut.
fn cut_short(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        text.to_string()
    } else {
        let kept: String = text.chars().take(chars.saturating_sub(1)).collect();
        format!("{}…", kept.trim_end())
    }
}

/// The width a card's menus take beside its name: Buttons, Master and the
/// Screen picker, at their longest.
const MENUS_ROOM: f32 = 400.0;

pub(super) fn render_switcher_section(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    for bus in &data.buses {
        render_bus(ui, bus, data, actions);
        ui.add_space(6.0);
    }
    if ui
        .small_button("+ Bus")
        .on_hover_text("Add a production bus at the master resolution")
        .clicked()
    {
        actions.commands.push(EngineCommand::AddBus {
            name: format!("Bus {}", data.buses.len() + 1),
            width: data.render_width,
            height: data.render_height,
        });
    }
}

fn render_bus(ui: &mut egui::Ui, bus_ui: &BusUI, data: &UIData, actions: &mut UIActions) {
    let bus = &bus_ui.bus;
    let uuid = &bus.uuid;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());

        // Its name, then its menus — at the right where there is room, else
        // in lines under it rather than widening the card past the panel.
        let name_width = ui
            .painter()
            .layout_no_wrap(
                bus.name.clone(),
                egui::TextStyle::Body.resolve(ui.style()),
                egui::Color32::WHITE,
            )
            .size()
            .x;
        let roomy = ui.available_width() >= name_width + MENUS_ROOM;
        let header = |ui: &mut egui::Ui, actions: &mut UIActions| {
            let name = ui
                .add(egui::Label::new(egui::RichText::new(&bus.name).strong()).sense(egui::Sense::click()))
                .on_hover_text("Double-click to rename; right-click for more");
            if name.double_clicked() {
                super::rename::open_bus(ui.ctx(), uuid, &bus.name);
            }
            name.context_menu(|ui| {
                    if ui.button("Rename bus…").clicked() {
                        super::rename::open_bus(ui.ctx(), uuid, &bus.name);
                        ui.close();
                    }
                    if ui.button("Remove bus").clicked() {
                        actions
                            .commands
                            .push(EngineCommand::RemoveBus { uuid: uuid.clone() });
                        ui.close();
                    }
                });
            if roomy {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    render_screen_picker(ui, bus_ui, data, actions);
                    ui.label(egui::RichText::new("Screen:").small());
                    render_master_menu(ui, bus_ui, data, actions);
                    render_buttons_menu(ui, bus, data, actions);
                });
            }
        };
        ui.horizontal(|ui| header(ui, actions));
        if !roomy {
            ui.horizontal(|ui| {
                render_buttons_menu(ui, bus, data, actions);
                render_master_menu(ui, bus_ui, data, actions);
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Screen:").small());
                render_screen_picker(ui, bus_ui, data, actions);
            });
        }

        ui.horizontal(|ui| {
            tally(
                ui,
                "PGM",
                PROGRAM_RED,
                &input_label(&bus.program, bus.program_resolved, bus, data),
            );
            ui.add_space(8.0);
            tally(
                ui,
                "PVW",
                PREVIEW_GREEN,
                &input_label(&bus.preview, bus.preview_resolved, bus, data),
            );
        });

        // The sources on the left, the takes on the right: a take is never
        // mistaken for one more source. Too narrow for both side by side,
        // the takes go under the sources.
        let spacing = ui.spacing().item_spacing.x;
        let room = ui.available_width() - TAKE_COLUMN - DIVIDER - 2.0 * spacing;
        if room < SOURCE_BUTTON.x {
            ui.horizontal_wrapped(|ui| render_sources(ui, bus, data, actions));
            ui.add_space(4.0);
            ui.allocate_ui_with_layout(
                egui::vec2(TAKE_COLUMN, SOURCE_BUTTON.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    super::record::with_record_warning(ui, data, Some(18.0), |ui| {
                        render_takes(ui, bus, actions);
                    });
                },
            );
            return;
        }
        let row = ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(room, SOURCE_BUTTON.y),
                egui::Layout::left_to_right(egui::Align::Min).with_main_wrap(true),
                |ui| {
                    ui.set_min_width(room);
                    render_sources(ui, bus, data, actions);
                },
            );
            // A line between them as tall as the row, drawn once the row's
            // height is known: a separator would run to the panel's foot.
            let line = ui.cursor().left() + DIVIDER / 2.0;
            ui.add_space(DIVIDER);
            ui.allocate_ui_with_layout(
                egui::vec2(TAKE_COLUMN, SOURCE_BUTTON.y),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    super::record::with_record_warning(ui, data, Some(18.0), |ui| {
                        render_takes(ui, bus, actions);
                    });
                },
            );
            line
        });
        ui.painter().vline(
            row.inner,
            row.response.rect.y_range(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
    });
}

/// The bus's source buttons — those it has not had taken off its row — Program
/// red and Preview green; a click stages one on Preview.
fn render_sources(
    ui: &mut egui::Ui,
    bus: &crate::engine::BusSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let uuid = &bus.uuid;
    let mut shown = 0;
    for (input, label) in sources(data) {
        if bus.hidden_sources.contains(&input) {
            continue;
        }
        shown += 1;
        let fill = if input == bus.program {
            Some(PROGRAM_RED)
        } else if input == bus.preview {
            Some(PREVIEW_GREEN)
        } else {
            None
        };
        let channel = match &input {
            BusInputRef::Channel(ch) => data.channels.iter().find(|c| &c.uuid == ch),
            _ => None,
        };
        let response = source_button(ui, &label, fill, channel)
            .on_hover_text(format!("{label}\nPut it on {}'s Preview", bus.name));
        if response.clicked() {
            actions.commands.push(EngineCommand::SetBusPreview {
                uuid: uuid.clone(),
                input: input.clone(),
            });
        }
        response.context_menu(|ui| {
            if ui
                .button(format!("Remove from {}", bus.name))
                .on_hover_text(format!(
                    "Take this button off {}'s row — Buttons ▾ puts it back. \
                     What is on the bus is not changed.",
                    bus.name
                ))
                .clicked()
            {
                actions.commands.push(EngineCommand::SetBusSourceHidden {
                    uuid: uuid.clone(),
                    input: input.clone(),
                    hidden: true,
                });
                ui.close();
            }
            if let Some(ch) = channel {
                ui.separator();
                super::colours::palette(ui, ch, actions);
            }
        });
    }
    if shown == 0 {
        ui.label(
            egui::RichText::new(format!("No buttons on {} — choose some in Buttons ▾", bus.name))
                .small()
                .weak(),
        );
    }
}

/// A source button, every one the same size: its name over two lines at
/// most, then "…"; a channel's colour along its top, and a colour the
/// operator gave it all over it. Program and Preview fill it red and green.
fn source_button(
    ui: &mut egui::Ui,
    label: &str,
    fill: Option<egui::Color32>,
    channel: Option<&super::super::ChannelUIInfo>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(SOURCE_BUTTON, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let given = channel.and_then(|ch| ch.color);
        let background = fill.unwrap_or_else(|| match given {
            Some(colour) => {
                let dark = colour.gamma_multiply(0.45);
                egui::Color32::from_rgb(dark.r(), dark.g(), dark.b())
            }
            None => visuals.weak_bg_fill,
        });
        let painter = ui.painter();
        painter.rect(
            rect,
            visuals.corner_radius,
            background,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        if let Some(ch) = channel {
            let band = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), COLOUR_BAND));
            painter.rect_filled(
                band,
                egui::CornerRadius {
                    nw: 3,
                    ne: 3,
                    sw: 0,
                    se: 0,
                },
                ch.accent(),
            );
        }
        let colour = if fill.is_some() || given.is_some() {
            egui::Color32::WHITE
        } else {
            visuals.text_color()
        };
        let mut job = egui::text::LayoutJob::simple(
            label.to_string(),
            egui::FontId::proportional(13.5),
            colour,
            rect.width() - 10.0,
        );
        job.wrap = egui::text::TextWrapping {
            max_width: rect.width() - 10.0,
            max_rows: 2,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        job.halign = egui::Align::Center;
        let galley = painter.layout_job(job);
        let top = rect.center().y + COLOUR_BAND / 2.0 - galley.size().y / 2.0;
        painter.galley(egui::pos2(rect.center().x, top), galley, colour);
    }
    let label = label.to_string();
    response.widget_info(move || {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &label)
    });
    response
}

/// `CUT` and `AUTO`, big, one over the other, with the AUTO time and the
/// take's progress under them.
fn render_takes(ui: &mut egui::Ui, bus: &crate::engine::BusSnapshot, actions: &mut UIActions) {
    let uuid = &bus.uuid;
    let transitioning = bus.transition.is_some();
    let big = |text: &str| egui::RichText::new(text).size(18.0).strong();
    if ui
        .add_enabled(
            !transitioning,
            egui::Button::new(big("CUT")).min_size(TAKE_BUTTON),
        )
        .on_hover_text("Swap Preview and Program now")
        .clicked()
    {
        actions
            .commands
            .push(EngineCommand::BusCut { uuid: uuid.clone() });
    }
    if ui
        .add_enabled(
            !transitioning,
            egui::Button::new(big("AUTO")).min_size(TAKE_BUTTON),
        )
        .on_hover_text(format!(
            "Fade Preview to Program over {}",
            seconds(bus.auto_duration)
        ))
        .clicked()
    {
        actions.commands.push(EngineCommand::BusAuto {
            uuid: uuid.clone(),
            duration_secs: None,
        });
    }
    egui::ComboBox::from_id_salt(format!("bus_auto_{uuid}"))
        .selected_text(seconds(bus.auto_duration))
        .width(TAKE_BUTTON.x - 8.0)
        .show_ui(ui, |ui| {
            for d in AUTO_DURATIONS {
                let current = (bus.auto_duration - d).abs() < 1e-3;
                if ui.selectable_label(current, seconds(d)).clicked() && !current {
                    actions.commands.push(EngineCommand::SetBusAutoDuration {
                        uuid: uuid.clone(),
                        duration_secs: d,
                    });
                }
            }
        })
        .response
        .on_hover_text("How long AUTO fades");
    if let Some(t) = &bus.transition {
        ui.add(egui::ProgressBar::new(t.progress).desired_width(TAKE_BUTTON.x));
        ui.ctx().request_repaint();
    }
}

/// Which sources the bus's row has a button for: an LED wall that only ever
/// shows three inputs offers those three. Taking one off only takes the
/// button away; the right-click menu does it too.
fn render_buttons_menu(
    ui: &mut egui::Ui,
    bus: &crate::engine::BusSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let all = sources(data);
    let shown = all
        .iter()
        .filter(|(input, _)| !bus.hidden_sources.contains(input))
        .count();
    let label = if shown == all.len() {
        "Buttons: all".to_string()
    } else {
        format!("Buttons: {shown} of {}", all.len())
    };
    ui.menu_button(egui::RichText::new(label).small(), |ui| {
        ui.label(egui::RichText::new(format!("Buttons on {}", bus.name)).strong());
        ui.label(
            egui::RichText::new(
                "The sources this bus's row has a button for. \
                 What is on the bus is not changed.",
            )
            .small()
            .weak(),
        );
        for (input, name) in &all {
            let mut on = !bus.hidden_sources.contains(input);
            if ui.checkbox(&mut on, name).changed() {
                actions.commands.push(EngineCommand::SetBusSourceHidden {
                    uuid: bus.uuid.clone(),
                    input: input.clone(),
                    hidden: !on,
                });
            }
        }
        if ui
            .add_enabled(shown < all.len(), egui::Button::new("Show all"))
            .clicked()
        {
            for (input, _) in &all {
                if bus.hidden_sources.contains(input) {
                    actions.commands.push(EngineCommand::SetBusSourceHidden {
                        uuid: bus.uuid.clone(),
                        input: input.clone(),
                        hidden: false,
                    });
                }
            }
        }
    })
    .response
    .on_hover_text(format!(
        "Choose which inputs {}'s row has buttons for — e.g. three on an LED wall",
        bus.name
    ));
}

/// Which channels the bus's Master is made of — vMix's per-output overlay
/// checkboxes. The stream can keep the lower third while the TV, on
/// another bus, leaves it off. Only Master: a bus showing one channel
/// shows that channel whatever is ticked here.
fn render_master_menu(ui: &mut egui::Ui, bus_ui: &BusUI, data: &UIData, actions: &mut UIActions) {
    let bus = &bus_ui.bus;
    let hidden = hidden_count(bus, data);
    let label = if hidden == 0 {
        "Master: all".to_string()
    } else {
        format!("Master: {hidden} left out")
    };
    ui.menu_button(egui::RichText::new(label).small(), |ui| {
        ui.label(egui::RichText::new(format!("Master on {}", bus.name)).strong());
        ui.label(
            egui::RichText::new(
                "The channels this bus shows when it takes Master. \
                 A channel button shows its channel whatever is ticked here.",
            )
            .small()
            .weak(),
        );
        for ch in &data.channels {
            let mut shown = !bus.hidden_layers.contains(&ch.uuid);
            if ui.checkbox(&mut shown, &ch.name).changed() {
                actions.commands.push(EngineCommand::SetBusLayerHidden {
                    uuid: bus.uuid.clone(),
                    channel_uuid: ch.uuid.clone(),
                    hidden: !shown,
                });
            }
        }
    })
    .response
    .on_hover_text(format!(
        "Leave channels out of Master on {} only — e.g. the lower third on the \
         stream but not on the TV",
        bus.name
    ));
}

/// How many of the scene's layers a bus leaves out of Master. A hidden
/// channel that has since been removed is not counted.
fn hidden_count(bus: &crate::engine::BusSnapshot, data: &UIData) -> usize {
    data.channels
        .iter()
        .filter(|ch| bus.hidden_layers.contains(&ch.uuid))
        .count()
}

fn render_screen_picker(ui: &mut egui::Ui, bus_ui: &BusUI, data: &UIData, actions: &mut UIActions) {
    let uuid = &bus_ui.bus.uuid;
    // Shown as the screen is called, not by what outputs address it by.
    let current = bus_ui.screen.as_deref().map_or_else(
        || "Off".to_string(),
        |key| {
            data.available_monitors
                .iter()
                .find(|m| m.name == key)
                .map_or_else(|| key.to_string(), |m| m.label.clone())
        },
    );
    // Cut short: a long screen name must not widen the menus past the room
    // the card's header leaves them. The list shows it whole.
    egui::ComboBox::from_id_salt(format!("bus_screen_{uuid}"))
        .selected_text(egui::RichText::new(cut_short(&current, SCREEN_NAME_CHARS)).small())
        .width(120.0)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(bus_ui.screen.is_none(), "Off")
                .clicked()
                && bus_ui.screen.is_some()
            {
                actions.commands.push(EngineCommand::ShowBusOnScreen {
                    uuid: uuid.clone(),
                    monitor_name: None,
                });
            }
            for monitor in &data.available_monitors {
                let on_it = bus_ui.screen.as_deref() == Some(monitor.name.as_str());
                let label = screen_label(monitor);
                let item = ui.selectable_label(on_it, label);
                let item = if monitor.is_control {
                    item.on_hover_text(
                        "Nida's own window is on this screen — an output here covers it. \
                         Press Esc on the output to take it off.",
                    )
                } else {
                    item
                };
                if item.clicked() && !on_it {
                    actions.commands.push(EngineCommand::ShowBusOnScreen {
                        uuid: uuid.clone(),
                        monitor_name: Some(monitor.name.clone()),
                    });
                }
            }
            // A Blackmagic device's HDMI and SDI, as one more screen.
            for device in &data.decklink_devices {
                let label = crate::decklink::screen_label(&device.name);
                let on_it = bus_ui.screen.as_deref() == Some(label.as_str());
                if ui
                    .selectable_label(on_it, &label)
                    .on_hover_text(
                        "A Blackmagic device: its HDMI and SDI out, in 1080p at the show's rate",
                    )
                    .clicked()
                    && !on_it
                {
                    actions.commands.push(EngineCommand::ShowBusOnBlackmagic {
                        uuid: uuid.clone(),
                        device: Some(device.name.clone()),
                    });
                }
            }
        });
}

/// "Cut to" / "Fade to" every bus, for an input's right-click menu: the quick
/// way to send one deck or channel to a screen without going through the
/// switcher's Preview. Cut stages it on the bus's Preview and takes it at once;
/// Fade takes it with the bus's AUTO time. A bus already showing it has both
/// greyed out, and Fade waits while the bus is mid-transition.
pub(super) fn take_menu_items(
    ui: &mut egui::Ui,
    input: &BusInputRef,
    data: &UIData,
    actions: &mut UIActions,
) {
    if data.buses.is_empty() {
        return;
    }
    for cut in [true, false] {
        for bus_ui in &data.buses {
            let bus = &bus_ui.bus;
            let on_air = &bus.program == input;
            let enabled = !on_air && (cut || bus.transition.is_none());
            let verb = if cut { "Cut to" } else { "Fade to" };
            let label = if on_air {
                format!("{verb} {} (on air)", bus.name)
            } else {
                format!("{verb} {}", bus.name)
            };
            let hover = if cut {
                format!("Put it on {} now", bus.name)
            } else {
                format!(
                    "Fade {} to it over {}",
                    bus.name,
                    seconds(bus.auto_duration)
                )
            };
            if ui
                .add_enabled(enabled, egui::Button::new(label))
                .on_hover_text(hover)
                .clicked()
            {
                take(actions, &bus.uuid, input, cut);
                ui.close();
            }
        }
        if cut {
            ui.separator();
        }
    }
}

/// Stage `input` on a bus's Preview and take it: at once for a cut, else with
/// the bus's AUTO fade.
fn take(actions: &mut UIActions, bus_uuid: &str, input: &BusInputRef, cut: bool) {
    actions.commands.push(EngineCommand::SetBusPreview {
        uuid: bus_uuid.to_string(),
        input: input.clone(),
    });
    actions.commands.push(if cut {
        EngineCommand::BusCut {
            uuid: bus_uuid.to_string(),
        }
    } else {
        EngineCommand::BusAuto {
            uuid: bus_uuid.to_string(),
            duration_secs: None,
        }
    });
}

/// Every bus with a Cut and a Fade button for one input, laid out as a
/// column to sit beside that input's preview — the right-click take menu,
/// always open. A bus name is red where the input is on air and green where
/// it waits on Preview; as in the menu, a bus already showing it has both
/// buttons greyed out, and Fade waits while the bus is mid-transition.
pub(super) fn render_take_column(
    ui: &mut egui::Ui,
    input: &BusInputRef,
    data: &UIData,
    actions: &mut UIActions,
) {
    if data.buses.is_empty() {
        ui.label(egui::RichText::new("No buses yet").small().weak());
        return;
    }
    egui::Grid::new(("take_column", input.uuid().unwrap_or("master")))
        .num_columns(3)
        .spacing([4.0, 6.0])
        .show(ui, |ui| {
            for bus_ui in &data.buses {
                let bus = &bus_ui.bus;
                let on_air = &bus.program == input;
                let cued = !on_air && &bus.preview == input;
                let mut name = egui::RichText::new(format!(" {} ", bus.name))
                    .small()
                    .strong();
                if on_air {
                    name = name
                        .color(egui::Color32::WHITE)
                        .background_color(PROGRAM_RED);
                } else if cued {
                    name = name
                        .color(egui::Color32::WHITE)
                        .background_color(PREVIEW_GREEN);
                }
                let state = if on_air {
                    "on air"
                } else if cued {
                    "on Preview"
                } else {
                    "not on it"
                };
                ui.label(name)
                    .on_hover_text(format!("{}: {state}", bus.name));
                let button = |text: &str| {
                    egui::Button::new(egui::RichText::new(text).small())
                        .min_size(egui::vec2(40.0, 20.0))
                };
                if ui
                    .add_enabled(!on_air, button("Cut"))
                    .on_hover_text(format!("Put it on {} now", bus.name))
                    .clicked()
                {
                    take(actions, &bus.uuid, input, true);
                }
                if ui
                    .add_enabled(!on_air && bus.transition.is_none(), button("Fade"))
                    .on_hover_text(format!(
                        "Fade {} to it over {}",
                        bus.name,
                        seconds(bus.auto_duration)
                    ))
                    .clicked()
                {
                    take(actions, &bus.uuid, input, false);
                }
                ui.end_row();
            }
        });
    if data.buses.iter().any(|b| b.bus.transition.is_some()) {
        ui.ctx().request_repaint();
    }
}

/// Room [`render_take_column`] takes across, for sizing what sits beside it.
pub(super) const TAKE_COLUMN_WIDTH: f32 = 190.0;

/// A screen as the picker lists it, marking the one Varda's own window is on:
/// an output sent there covers the controls.
pub(super) fn screen_label(monitor: &MonitorInfo) -> String {
    let base = monitor.label.clone();
    if monitor.is_control {
        format!("{base} — control screen")
    } else {
        base
    }
}

/// A tally: the row's name on its colour, then what it holds.
fn tally(ui: &mut egui::Ui, which: &str, color: egui::Color32, what: &str) {
    ui.label(
        egui::RichText::new(format!(" {which} "))
            .small()
            .strong()
            .color(egui::Color32::WHITE)
            .background_color(color),
    );
    ui.label(egui::RichText::new(what).small());
}

/// The buttons a bus row offers: black, the master mix and every channel.
/// Not every input in them — a show of many made a wall of buttons; one
/// input still goes to a bus alone from its right-click menu.
fn sources(data: &UIData) -> Vec<(BusInputRef, String)> {
    let mut out = vec![
        (BusInputRef::Black, "Black".to_string()),
        (BusInputRef::Master, "Master".to_string()),
    ];
    out.extend(
        data.channels
            .iter()
            .map(|ch| (BusInputRef::Channel(ch.uuid.clone()), ch.numbered())),
    );
    out
}

/// What an input is called on a bus, flagged when it no longer resolves.
/// Master names the layers the bus leaves out of it.
fn input_label(
    input: &BusInputRef,
    resolved: bool,
    bus: &crate::engine::BusSnapshot,
    data: &UIData,
) -> String {
    let name = match input {
        BusInputRef::Black => return "Black".to_string(),
        BusInputRef::Master => {
            let hidden: Vec<&str> = data
                .channels
                .iter()
                .filter(|ch| bus.hidden_layers.contains(&ch.uuid))
                .map(|ch| ch.name.as_str())
                .collect();
            Some(if hidden.is_empty() {
                "Master".to_string()
            } else {
                format!("Master without {}", hidden.join(", "))
            })
        }
        BusInputRef::Channel(uuid) => data
            .channels
            .iter()
            .find(|ch| &ch.uuid == uuid)
            .map(|ch| ch.name.clone()),
        BusInputRef::Deck(uuid) => data
            .channels
            .iter()
            .flat_map(|ch| &ch.decks)
            .find(|d| &d.uuid == uuid)
            .map(|d| d.name.clone()),
    };
    match (name, resolved) {
        (Some(name), true) => name,
        _ => "⚠ missing — black".to_string(),
    }
}

pub(super) fn seconds(secs: f32) -> String {
    format!("{secs} s")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    fn run(data: &UIData, click: &str) -> UIActions {
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 480.0))
                .build_ui(|ui| {
                    render_switcher_section(ui, data, &mut actions);
                });
            harness.get_by_label(click).click();
            // A fixed number of frames: mid-transition the panel keeps
            // repainting for its progress bar, so it never settles.
            harness.run_steps(4);
        }
        actions
    }

    /// A bus's name offers Rename… on a right-click, next to Remove: it asks
    /// for the new name.
    #[test]
    fn a_bus_can_be_renamed_from_its_right_click_menu() {
        let data = UIData::test_fixture();
        let bus = data.buses[0].bus.clone();
        let mut actions = UIActions::new();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 480.0))
            .build_ui(|ui| {
                render_switcher_section(ui, &data, &mut actions);
            });
        harness.get_by_label(&bus.name).click_secondary();
        harness.run_steps(4);
        harness.get_by_label("Rename bus…").click();
        harness.run_steps(4);
        assert_eq!(super::super::rename::renaming(&harness.ctx), Some(bus.uuid));
    }

    /// A connected Blackmagic device is one more screen in a bus's Screen:
    /// list, and choosing it sends the bus there.
    #[test]
    fn a_bus_can_go_to_a_blackmagic_device_from_its_screen_list() {
        let mut data = UIData::test_fixture();
        data.buses.truncate(1);
        data.buses[0].screen = None;
        data.decklink_devices = vec![crate::decklink::DeckLinkDevice {
            name: "UltraStudio Monitor 3G".into(),
            arrival: 0,
            modes: vec![],
        }];
        let bus = data.buses[0].bus.uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 480.0))
                .build_ui(|ui| {
                    render_switcher_section(ui, &data, &mut actions);
                });
            harness.get_by_value("Off").click();
            harness.run_steps(4);
            harness.get_by_label("UltraStudio Monitor 3G (Blackmagic)").click();
            harness.run_steps(4);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::ShowBusOnBlackmagic { uuid, device: Some(device) }
                    if *uuid == bus && device == "UltraStudio Monitor 3G"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn clicking_a_source_stages_it_on_preview_only() {
        let data = UIData::test_fixture();
        let actions = run(&data, "Black");
        assert!(actions.commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetBusPreview { uuid, input: BusInputRef::Black } if uuid == "bus00001"
        )));
        assert!(
            !actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::BusCut { .. } | EngineCommand::BusAuto { .. }
            )),
            "staging must not take anything to air"
        );
    }

    #[test]
    fn cut_and_auto_take_the_bus_they_sit_on() {
        let data = UIData::test_fixture();
        let cut = run(&data, "CUT");
        assert!(cut
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::BusCut { uuid } if uuid == "bus00001")));
        let auto = run(&data, "AUTO");
        assert!(auto.commands.iter().any(|c| matches!(
            c,
            EngineCommand::BusAuto { uuid, duration_secs: None } if uuid == "bus00001"
        )));
    }

    #[test]
    fn cut_and_auto_are_disabled_mid_transition() {
        let mut data = UIData::test_fixture();
        data.buses[0].bus.transition = Some(crate::engine::BusTransitionSnapshot {
            from: BusInputRef::Channel("ca000001".into()),
            to: BusInputRef::Channel("cb000001".into()),
            duration: 1.0,
            progress: 0.4,
            mix: 0.4,
            from_resolved: true,
            to_resolved: true,
        });
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 480.0))
                .build_ui(|ui| {
                    render_switcher_section(ui, &data, &mut actions);
                });
            harness.run_steps(2);
            for take in ["CUT", "AUTO"] {
                assert!(
                    harness.get_by_label(take).accesskit_node().is_disabled(),
                    "{take} must be disabled while a transition runs"
                );
            }
            harness.get_by_label("CUT").click();
            harness.run_steps(2);
        }
        assert!(
            !actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::BusCut { .. } | EngineCommand::BusAuto { .. }
            )),
            "a second take while one runs would be refused by the engine anyway"
        );
    }

    #[test]
    fn an_unresolved_program_says_so() {
        let mut data = UIData::test_fixture();
        data.buses[0].bus.program_resolved = false;
        let mut actions = UIActions::new();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 480.0))
            .build_ui(|ui| {
                render_switcher_section(ui, &data, &mut actions);
            });
        harness.get_by_label("⚠ missing — black");
    }

    #[test]
    fn the_control_screen_is_marked_in_the_picker() {
        let screen = |name: &str, is_control| MonitorInfo {
            name: name.into(),
            index: 0,
            width: 1920,
            height: 1080,
            is_control,
            label: format!("{name} (1920×1080)"),
            model: None,
            given_name: None,
            identity: name.into(),
            main: is_control,
        };
        assert_eq!(
            screen_label(&screen("Main", true)),
            "Main (1920×1080) — control screen"
        );
        assert_eq!(screen_label(&screen("LCD", false)), "LCD (1920×1080)");
    }

    fn take_menu(data: &UIData, input: &BusInputRef, click: &str) -> UIActions {
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(300.0, 300.0))
                .build_ui(|ui| take_menu_items(ui, input, data, &mut actions));
            harness.get_by_label(click).click();
            harness.run_steps(2);
        }
        actions
    }

    #[test]
    fn cut_to_a_bus_stages_the_input_then_takes_it() {
        let data = UIData::test_fixture();
        let input = BusInputRef::Deck("b0000002".into());
        let actions = take_menu(&data, &input, "Cut to TV");
        assert!(
            matches!(
                actions.commands.as_slice(),
                [
                    EngineCommand::SetBusPreview { uuid, input: staged },
                    EngineCommand::BusCut { uuid: taken },
                ] if uuid == "bus00001" && taken == "bus00001" && *staged == input
            ),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn fade_to_a_bus_stages_the_input_then_autos() {
        let data = UIData::test_fixture();
        let input = BusInputRef::Channel("cb000001".into());
        let actions = take_menu(&data, &input, "Fade to TV");
        assert!(
            matches!(
                actions.commands.as_slice(),
                [
                    EngineCommand::SetBusPreview { uuid, .. },
                    EngineCommand::BusAuto { uuid: taken, duration_secs: None },
                ] if uuid == "bus00001" && taken == "bus00001"
            ),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn an_input_already_on_a_bus_cannot_be_taken_to_it_again() {
        let data = UIData::test_fixture();
        let on_air = data.buses[0].bus.program.clone();
        let mut actions = UIActions::new();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(300.0, 300.0))
            .build_ui(|ui| take_menu_items(ui, &on_air, &data, &mut actions));
        for label in ["Cut to TV (on air)", "Fade to TV (on air)"] {
            assert!(
                harness.get_by_label(label).accesskit_node().is_disabled(),
                "{label}"
            );
        }
    }

    #[test]
    fn master_is_a_source_every_bus_can_take() {
        let data = UIData::test_fixture();
        let actions = run(&data, "Master");
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetBusPreview { uuid, input: BusInputRef::Master }
                    if uuid == "bus00001"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_layer_can_be_left_off_one_bus() {
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 480.0))
                .build_ui(|ui| render_switcher_section(ui, &data, &mut actions));
            harness.get_by_label("Master: all").click();
            harness.run_steps(2);
            // The checkbox, not the source button of the same name.
            harness
                .get_by_role_and_label(egui::accesskit::Role::CheckBox, "Ch B")
                .click();
            harness.run_steps(2);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetBusLayerHidden { uuid, channel_uuid, hidden: true }
                    if uuid == "bus00001" && channel_uuid == "cb000001"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_bus_on_master_names_the_layers_it_leaves_out() {
        let mut data = UIData::test_fixture();
        data.buses[0].bus.program = BusInputRef::Master;
        data.buses[0].bus.hidden_layers = vec!["cb000001".into(), "gone0001".into()];
        let mut actions = UIActions::new();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 480.0))
            .build_ui(|ui| render_switcher_section(ui, &data, &mut actions));
        harness.get_by_label("Master without Ch B");
        // A hidden channel that no longer exists is not counted.
        harness.get_by_label("Master: 1 left out");
    }

    /// A bus row offers channels, not the inputs in them.
    #[test]
    fn a_bus_row_offers_black_master_and_channels_only() {
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 480.0))
            .build_ui(|ui| render_switcher_section(ui, &data, &mut actions));
        for label in ["Black", "Master", "1  Ch A", "2  Ch B"] {
            harness.get_by_role_and_label(egui::accesskit::Role::Button, label);
        }
        for deck in ["test_generator_a", "test_generator_d"] {
            assert!(harness.query_by_label(deck).is_none(), "{deck} listed");
        }
    }

    fn harness_of(data: UIData, size: egui::Vec2) -> egui_kittest::Harness<'static, UIActions> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_ui_state(
                move |ui, actions: &mut UIActions| render_switcher_section(ui, &data, actions),
                UIActions::new(),
            )
    }

    /// Every source button is the same size, however long its name; the
    /// name is cut to two lines rather than widening it.
    #[test]
    fn source_buttons_are_one_size_whatever_the_name() {
        let mut data = UIData::test_fixture();
        data.channels[1].name = "1_Forward-looking statements about the year ahead".into();
        let long = data.channels[1].numbered();
        let mut harness = harness_of(data, egui::vec2(700.0, 480.0));
        harness.run_steps(2);
        let short = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "1  Ch A")
            .rect();
        let long = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, &long)
            .rect();
        let black = harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "Black")
            .rect();
        assert_eq!(long.size(), short.size(), "a long name must not widen its button");
        assert_eq!(black.size(), short.size());
        assert_eq!(short.size(), SOURCE_BUTTON);
    }

    /// CUT and AUTO stand apart, right of every source and bigger than one.
    #[test]
    fn cut_and_auto_stand_right_of_the_sources_and_bigger() {
        let data = UIData::test_fixture();
        let mut harness = harness_of(data, egui::vec2(700.0, 480.0));
        harness.run_steps(2);
        let sources_right = ["Black", "Master", "1  Ch A", "2  Ch B"]
            .iter()
            .map(|l| {
                harness
                    .get_by_role_and_label(egui::accesskit::Role::Button, l)
                    .rect()
                    .right()
            })
            .fold(f32::MIN, f32::max);
        for take in ["CUT", "AUTO"] {
            let rect = harness.get_by_label(take).rect();
            assert!(rect.left() > sources_right, "{take} at {rect:?}, sources end {sources_right}");
            assert!(rect.width() > SOURCE_BUTTON.x, "{take} is not bigger: {rect:?}");
        }
    }

    /// A source taken off a bus's row has no button there; the Buttons
    /// menu says how many are left and puts it back.
    #[test]
    fn a_bus_row_shows_only_the_buttons_chosen_for_it() {
        let mut data = UIData::test_fixture();
        data.buses[0].bus.hidden_sources = vec![
            BusInputRef::Black,
            BusInputRef::Channel("ca000001".into()),
        ];
        let mut harness = harness_of(data, egui::vec2(700.0, 480.0));
        harness.run_steps(2);
        assert!(harness
            .query_by_role_and_label(egui::accesskit::Role::Button, "Black")
            .is_none());
        assert!(harness
            .query_by_role_and_label(egui::accesskit::Role::Button, "1  Ch A")
            .is_none());
        harness.get_by_role_and_label(egui::accesskit::Role::Button, "2  Ch B");
        harness.get_by_label("Buttons: 2 of 4").click();
        harness.run_steps(2);
        harness
            .get_by_role_and_label(egui::accesskit::Role::CheckBox, "1  Ch A")
            .click();
        harness.run_steps(2);
        assert!(
            harness.state().commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetBusSourceHidden { uuid, input: BusInputRef::Channel(ch), hidden: false }
                    if uuid == "bus00001" && ch == "ca000001"
            )),
            "{:?}",
            harness.state().commands
        );
    }

    /// A source's right-click takes its button off the row, and gives a
    /// channel its colour.
    #[test]
    fn a_sources_right_click_removes_it_or_colours_it() {
        let data = UIData::test_fixture();
        let mut harness = harness_of(data, egui::vec2(700.0, 600.0));
        harness.run_steps(2);
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "2  Ch B")
            .click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Teal").click();
        harness.run_steps(2);
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "2  Ch B")
            .click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Remove from TV").click();
        harness.run_steps(2);
        let commands = &harness.state().commands;
        assert!(
            commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetChannelColor { channel_uuid, color: Some([35, 160, 155]) }
                    if channel_uuid == "cb000001"
            )),
            "{commands:?}"
        );
        assert!(
            commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetBusSourceHidden { uuid, input: BusInputRef::Channel(ch), hidden: true }
                    if uuid == "bus00001" && ch == "cb000001"
            )),
            "{commands:?}"
        );
        assert!(
            !commands.iter().any(|c| matches!(c, EngineCommand::SetBusPreview { .. })),
            "a right-click stages nothing: {commands:?}"
        );
    }

    /// With the reminder on, NOT RECORDING stands over the takes while not
    /// recording, and goes once recording — or with the reminder off.
    #[test]
    fn not_recording_shows_over_the_takes_only_when_asked_and_not_recording() {
        let shows = |warn: bool, recording: bool| {
            let mut data = UIData::test_fixture();
            data.record.options.warn_when_off = warn;
            data.record.status.recording = recording;
            let mut harness = harness_of(data, egui::vec2(700.0, 480.0));
            harness.run_steps(2);
            harness.query_by_label("NOT RECORDING").is_some()
        };
        assert!(shows(true, false));
        assert!(!shows(true, true));
        assert!(!shows(false, false));
    }

    /// In a narrow panel the takes go under the sources, inside the panel;
    /// wide, beside them, and "+ Bus" stays just under the card.
    #[test]
    fn the_takes_stay_inside_a_narrow_panel() {
        for width in [240.0, 700.0] {
            let data = UIData::test_fixture();
            let mut harness = harness_of(data, egui::vec2(width, 600.0));
            harness.run_steps(2);
            let black = harness
                .get_by_role_and_label(egui::accesskit::Role::Button, "Black")
                .rect();
            for take in ["CUT", "AUTO"] {
                let rect = harness.get_by_label(take).rect();
                assert!(rect.right() <= width, "{take} at {rect:?} in {width}");
                if width < 300.0 {
                    assert!(rect.top() > black.bottom(), "{take} under the sources: {rect:?}");
                } else {
                    assert!(rect.left() > black.right(), "{take} beside the sources");
                }
            }
            let screen = harness.get_by_value("Off").rect();
            assert!(screen.right() <= width, "the Screen picker at {screen:?} in {width}");
            let add = harness.get_by_label("+ Bus").rect();
            assert!(add.top() < 520.0, "+ Bus at {add:?} in {width}");
        }
    }

    /// A long bus name in a middling panel: its menus go under it, not over
    /// it.
    #[test]
    fn a_long_bus_name_keeps_its_menus_off_it() {
        let mut data = UIData::test_fixture();
        data.buses[0].bus.name = "Conference Livestream Program".into();
        let width = 470.0;
        let mut harness = harness_of(data, egui::vec2(width, 600.0));
        harness.run_steps(2);
        let name = harness.get_by_label("Conference Livestream Program").rect();
        for menu in ["Buttons: all", "Master: all"] {
            let rect = harness.get_by_label(menu).rect();
            assert!(
                rect.left() >= name.right() || rect.top() >= name.bottom(),
                "{menu} at {rect:?} over the name at {name:?}"
            );
            assert!(rect.left() >= 0.0 && rect.right() <= width, "{menu} at {rect:?}");
        }
        let screen = harness.get_by_value("Off").rect();
        assert!(screen.left() >= name.right() || screen.top() >= name.bottom());
    }

    /// A long screen name is cut to the picker's width, not widening it.
    #[test]
    fn a_long_screen_name_is_cut_to_the_picker() {
        let mut data = UIData::test_fixture();
        let name = "LG UltraFine 5K Display (5120×2880) on the left of the desk";
        data.available_monitors = vec![MonitorInfo {
            name: "lg".into(),
            index: 1,
            width: 5120,
            height: 2880,
            is_control: false,
            label: name.into(),
            model: None,
            given_name: None,
            identity: "lg".into(),
            main: false,
        }];
        data.buses[0].screen = Some("lg".into());
        let mut harness = harness_of(data, egui::vec2(700.0, 600.0));
        harness.run_steps(2);
        let picker = harness
            .get_by_value(&cut_short(name, SCREEN_NAME_CHARS))
            .rect();
        assert!(picker.width() <= 140.0, "the picker at {picker:?}");
        assert_eq!(cut_short("Off", SCREEN_NAME_CHARS), "Off");
    }
}
