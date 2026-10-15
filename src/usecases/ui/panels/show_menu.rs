//! The Show menu, beside Save in the top bar: the show open, by name, and
//! moving between shows — a new empty one, opening another, Save As — as
//! vMix's preset menu. What each does is the runner's and the engine's (see
//! [`crate::app::shows`]); this asks, and for a new show asks to be sure.

use super::super::{ShowAction, UIActions, UIData};

fn confirm_id() -> egui::Id {
    egui::Id::new("show_menu_confirm_new")
}

/// The menu button, named after the show open.
pub(super) fn render_show_menu(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let refused = data.show_switch_refused.as_deref();
    let response = ui
        .menu_button(format!("🎬 {} ⏷", data.show_name), |ui| {
            let new = ui
                .add_enabled(refused.is_none(), egui::Button::new("New show"))
                .on_hover_text(
                    "A fresh show in a folder of its own, as one never set up. This one \
                     is saved first; MIDI and keys come along",
                )
                .on_disabled_hover_text(refused.unwrap_or_default());
            if new.clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(confirm_id(), true));
                ui.close();
            }
            let open = ui
                .add_enabled(refused.is_none(), egui::Button::new("Open…"))
                .on_hover_text("Open another show. This one is saved first")
                .on_disabled_hover_text(refused.unwrap_or_default());
            if open.clicked() {
                actions.session.show_action = Some(ShowAction::Open);
                ui.close();
            }
            let recent: Vec<&std::path::PathBuf> = data
                .recent_shows
                .iter()
                .filter(|p| **p != data.show_folder)
                .collect();
            ui.add_enabled_ui(refused.is_none() && !recent.is_empty(), |ui| {
                ui.menu_button("Open recent", |ui| {
                    for show in recent {
                        let name = show.file_name().map_or_else(
                            || show.display().to_string(),
                            |n| n.to_string_lossy().into_owned(),
                        );
                        if ui
                            .button(name)
                            .on_hover_text(show.display().to_string())
                            .clicked()
                        {
                            actions.session.show_action =
                                Some(ShowAction::OpenRecent(show.clone()));
                            ui.close();
                        }
                    }
                });
            });
            ui.separator();
            if ui.button("Save  (⌘S)").clicked() {
                actions.session.save_requested = true;
                ui.close();
            }
            if ui
                .button("Save As…")
                .on_hover_text("Save this show under a new name, and carry on in it")
                .clicked()
            {
                actions.session.show_action = Some(ShowAction::SaveAs);
                ui.close();
            }
            ui.separator();
            if ui.button("Show in Finder").clicked() {
                actions.session.show_action = Some(ShowAction::Reveal);
                ui.close();
            }
        })
        .response;
    response.on_hover_text(format!(
        "The show open — saved in {}",
        data.show_folder.display()
    ));
    render_confirm_new(ui.ctx(), data, actions);
}

/// "Start a new, fresh show?" — what goes to the screens changes.
fn render_confirm_new(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    if !ctx.data(|d| d.get_temp::<bool>(confirm_id()).unwrap_or(false)) {
        return;
    }
    let mut open = true;
    let mut close = false;
    egui::Window::new("New show")
        .id(egui::Id::new("show_menu_confirm_window"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .open(&mut open)
        .show(ctx, |ui| {
            ui.label(egui::RichText::new("Start a new, fresh show?").strong());
            ui.label(format!(
                "“{}” is saved first. The new show starts as one never set up: no inputs or \
                 outputs — the output windows close — the sound mixer with Master alone, the \
                 stream and recording settings as new. MIDI and keys come along.",
                data.show_name
            ));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui
                    .add(egui::Button::new(egui::RichText::new("New show").strong()))
                    .clicked()
                {
                    actions.session.show_action = Some(ShowAction::New);
                    close = true;
                }
                if ui.button("Cancel").clicked() {
                    close = true;
                }
            });
        });
    if close || !open {
        ctx.data_mut(|d| d.remove::<bool>(confirm_id()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    fn harness(data: UIData) -> egui_kittest::Harness<'static, Option<ShowAction>> {
        let data = std::rc::Rc::new(data);
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(700.0, 500.0))
            .build_ui_state(
                move |ui, seen: &mut Option<ShowAction>| {
                    let mut actions = UIActions::new();
                    render_show_menu(ui, &data, &mut actions);
                    if actions.session.show_action.is_some() {
                        *seen = actions.session.show_action;
                    }
                },
                None,
            )
    }

    fn open_menu(harness: &mut egui_kittest::Harness<'static, Option<ShowAction>>) {
        harness.run();
        harness.get_by_label_contains("Conference").click();
        harness.run();
    }

    #[test]
    fn the_menu_is_named_after_the_show() {
        let mut harness = harness(UIData::test_fixture());
        harness.run();
        harness.get_by_label("🎬 Conference ⏷");
    }

    #[test]
    fn a_new_show_is_asked_for_twice() {
        let mut harness = harness(UIData::test_fixture());
        open_menu(&mut harness);
        harness.get_by_label("New show").click();
        harness.run();
        assert_eq!(*harness.state(), None, "not yet: it asks first");
        harness
            .get_by_role_and_label(egui::accesskit::Role::Button, "New show")
            .click();
        harness.run();
        assert_eq!(*harness.state(), Some(ShowAction::New));
    }

    #[test]
    fn save_as_is_asked_of_the_runner() {
        let mut harness = harness(UIData::test_fixture());
        open_menu(&mut harness);
        harness.get_by_label("Save As…").click();
        harness.run();
        assert_eq!(*harness.state(), Some(ShowAction::SaveAs));
    }

    #[test]
    fn while_streaming_another_show_cannot_be_opened() {
        let mut data = UIData::test_fixture();
        data.show_switch_refused = Some("Stop the stream before opening another show".into());
        let mut harness = harness(data);
        open_menu(&mut harness);
        assert!(harness.get_by_label("Open…").accesskit_node().is_disabled());
        assert!(harness
            .get_by_label("New show")
            .accesskit_node()
            .is_disabled());
        assert!(!harness
            .get_by_label("Save As…")
            .accesskit_node()
            .is_disabled());
    }
}
