//! The Screens window: each screen connected — what the computer calls it,
//! its size and rate — and a name of the operator's own for it ("Stage LED",
//! "ATEM in 2"), kept on this computer and shown wherever screens are listed.
//! **Identify** puts each screen's name big on it for a few seconds, so two
//! identical screens can be told apart (see `app::screens`).

use crate::engine::EngineCommand;
use crate::usecases::ui::{UIActions, UIData};

fn open_id() -> egui::Id {
    egui::Id::new("screens_window_open")
}

/// A name being typed, per screen, until it is kept.
fn draft_id(monitor: &str) -> egui::Id {
    egui::Id::new(("screen_name_draft", monitor))
}

/// Open the Screens window.
pub(super) fn open(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(open_id(), true));
}

pub(super) fn render_screens_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let mut open = ctx.data(|d| d.get_temp::<bool>(open_id())).unwrap_or(false);
    if !open {
        return;
    }
    egui::Window::new("Screens")
        .id(egui::Id::new("screens_window"))
        .open(&mut open)
        .default_width(560.0)
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| body(ui, data, actions));
    ctx.data_mut(|d| d.insert_temp(open_id(), open));
}

fn body(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    ui.horizontal(|ui| {
        if ui
            .button("Identify")
            .on_hover_text("Put each screen's name big on it for a few seconds")
            .clicked()
        {
            actions.commands.push(EngineCommand::IdentifyScreens);
        }
        ui.label(
            egui::RichText::new("Names are kept on this computer, for any show.")
                .small()
                .weak(),
        );
    });
    ui.add_space(4.0);
    if data.available_monitors.is_empty() {
        ui.label(egui::RichText::new("No screens").small().weak());
        return;
    }
    egui::Grid::new("screens_grid")
        .num_columns(3)
        .striped(true)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label(egui::RichText::new("Name").small().strong());
            ui.label(egui::RichText::new("Screen").small().strong());
            ui.label(egui::RichText::new("Outputs know it as").small().strong());
            ui.end_row();
            for monitor in &data.available_monitors {
                name_field(ui, monitor, actions);
                let mut what = format!(
                    "{} · {}×{}",
                    monitor
                        .model
                        .as_deref()
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or(&monitor.name),
                    monitor.width,
                    monitor.height
                );
                if monitor.is_control {
                    what.push_str(" · Nida's own screen");
                }
                ui.label(egui::RichText::new(what).small());
                ui.label(egui::RichText::new(&monitor.name).small().weak());
                ui.end_row();
            }
        });
}

/// The screen's own name, typed in place: kept when Enter is pressed or
/// the field is left; emptied, the name is taken away.
fn name_field(ui: &mut egui::Ui, monitor: &crate::usecases::ui::MonitorInfo, actions: &mut UIActions) {
    let id = draft_id(&monitor.name);
    let given = monitor.given_name.clone().unwrap_or_default();
    let mut text = ui.data(|d| d.get_temp::<String>(id)).unwrap_or_else(|| given.clone());
    let response = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text("Name it…")
            .desired_width(160.0),
    );
    if response.changed() {
        ui.data_mut(|d| d.insert_temp(id, text.clone()));
    }
    if response.lost_focus() {
        ui.data_mut(|d| d.remove::<String>(id));
        if text.trim() != given {
            actions.commands.push(EngineCommand::RenameScreen {
                monitor_name: monitor.name.clone(),
                name: Some(text.trim().to_string()).filter(|t| !t.is_empty()),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn screens() -> UIData {
        let mut data = UIData::test_fixture();
        let screen = |name: &str, model: &str, given: Option<&str>| crate::usecases::ui::MonitorInfo {
            name: name.into(),
            index: 0,
            width: 1920,
            height: 1080,
            is_control: false,
            label: format!("{} (1920×1080)", given.unwrap_or(model)),
            model: Some(model.into()),
            given_name: given.map(str::to_string),
            identity: name.into(),
            main: false,
        };
        data.available_monitors = vec![
            screen("Monitor #0 @ 1728,0", "BMD HDMI", None),
            screen("Monitor #0 @ -1920,0", "BMD HDMI", Some("ATEM in 2")),
        ];
        data
    }

    /// A screen is named in the window: typed and Enter pressed, the name is
    /// kept for it — the screen addressed as outputs know it.
    #[test]
    fn a_screen_is_named_in_the_screens_window() {
        let data = screens();
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(640.0, 400.0))
                .build_ui(|ui| body(ui, &data, &mut actions));
            harness
                .get_all_by_role(egui::accesskit::Role::TextInput)
                .next()
                .expect("a name field")
                .focus();
            harness.run();
            harness
                .get_all_by_role(egui::accesskit::Role::TextInput)
                .next()
                .expect("a name field")
                .type_text("Stage TV");
            harness.key_press(egui::Key::Enter);
            harness.run();
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::RenameScreen { monitor_name, name: Some(name) }
                    if monitor_name == "Monitor #0 @ 1728,0" && name == "Stage TV"
            )),
            "{:?}",
            actions.commands
        );
    }

    /// Identify asks for each screen's name on it.
    #[test]
    fn identify_puts_each_screens_name_on_it() {
        let data = screens();
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(640.0, 400.0))
                .build_ui(|ui| body(ui, &data, &mut actions));
            harness.get_by_label("Identify").click();
            harness.run();
        }
        assert!(actions
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::IdentifyScreens)));
    }
}
