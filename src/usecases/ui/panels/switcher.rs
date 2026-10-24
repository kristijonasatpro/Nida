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
/// A source button: big enough to hit without looking twice.
const SOURCE_BUTTON: egui::Vec2 = egui::vec2(84.0, 40.0);
/// `CUT` and `AUTO`.
const TAKE_BUTTON: egui::Vec2 = egui::vec2(76.0, 34.0);

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

        ui.horizontal(|ui| {
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                render_screen_picker(ui, bus_ui, data, actions);
                render_master_menu(ui, bus_ui, data, actions);
            });
        });

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

        ui.horizontal_wrapped(|ui| {
            for (input, label) in sources(data) {
                let fill = if input == bus.program {
                    Some(PROGRAM_RED)
                } else if input == bus.preview {
                    Some(PREVIEW_GREEN)
                } else {
                    None
                };
                let mut text = egui::RichText::new(&label).strong();
                if fill.is_some() {
                    text = text.color(egui::Color32::WHITE);
                }
                let mut button = egui::Button::new(text).min_size(SOURCE_BUTTON);
                if let Some(fill) = fill {
                    button = button.fill(fill);
                }
                if ui
                    .add(button)
                    .on_hover_text(format!("Put {label} on {}'s Preview", bus.name))
                    .clicked()
                {
                    actions.commands.push(EngineCommand::SetBusPreview {
                        uuid: uuid.clone(),
                        input,
                    });
                }
            }
        });

        ui.horizontal(|ui| {
            let transitioning = bus.transition.is_some();
            if ui
                .add_enabled(
                    !transitioning,
                    egui::Button::new(egui::RichText::new("CUT").strong()).min_size(TAKE_BUTTON),
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
                    egui::Button::new(egui::RichText::new("AUTO").strong()).min_size(TAKE_BUTTON),
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
                .width(56.0)
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
                });
            if let Some(t) = &bus.transition {
                ui.add(egui::ProgressBar::new(t.progress).desired_width(60.0));
                ui.ctx().request_repaint();
            }
        });
    });
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
    egui::ComboBox::from_id_salt(format!("bus_screen_{uuid}"))
        .selected_text(egui::RichText::new(current).small())
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
    ui.label(egui::RichText::new("Screen:").small());
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
            .map(|ch| (BusInputRef::Channel(ch.uuid.clone()), ch.name.clone())),
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
        for label in ["Black", "Master", "Ch A", "Ch B"] {
            harness.get_by_role_and_label(egui::accesskit::Role::Button, label);
        }
        for deck in ["test_generator_a", "test_generator_d"] {
            assert!(harness.query_by_label(deck).is_none(), "{deck} listed");
        }
    }
}
