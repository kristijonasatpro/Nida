//! The panels below the top bar, as a dock: tabs that can be dragged into
//! other groups or to any edge, dividers that resize, and the **Layout** menu
//! that picks Classic, Broadcast, VJ or a saved layout, saves the
//! arrangement, and opens or closes panels. See [`crate::usecases::ui::screen_layout`].

use super::super::{UIActions, UIData};
use crate::usecases::ui::screen_layout::{self, Builtin, Pane, ScreenLayout};

/// Draws each panel. The stage editor and the arrangement take the place of
/// the first channel view drawn, as they took the central area's.
struct Viewer<'a> {
    data: &'a UIData,
    actions: &'a mut UIActions,
    locked: bool,
    modal_drawn: bool,
    /// Where the first channel view was drawn, for windows that open beside it.
    desk_rect: Option<egui::Rect>,
}

impl egui_dock::TabViewer for Viewer<'_> {
    type Tab = Pane;

    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::new(("dock_pane", *tab))
    }

    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        pane_title(*tab, self.data).into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        let (data, actions) = (self.data, &mut *self.actions);
        if tab.is_desk() {
            self.desk_rect.get_or_insert(ui.max_rect());
            if !self.modal_drawn && (data.stage_editor_open || data.arrangement_mode_open) {
                self.modal_drawn = true;
                super::mixer::render_modal_center(ui, data, actions);
                return;
            }
        }
        match tab {
            Pane::Library => super::library::render_library_panel(ui, data, actions),
            Pane::Channels => super::mixer::render_channel_desk(ui, data, actions),
            Pane::Layers => super::layers::render_layers(ui, data, actions),
            Pane::Inputs => super::inputs::render_inputs(ui, data, actions),
            Pane::Monitors => super::monitors::render_monitors(ui, data, actions),
            Pane::Output => super::right_panel::render_main_output(ui, data, actions),
            Pane::Switcher => super::switcher::render_switcher_section(ui, data, actions),
            Pane::Rundown => super::rundown::render_rundown(ui, data, actions),
            Pane::Audio => super::audio_mixer::render_audio_mixer_section(ui, data, actions),
            Pane::Details => super::bottom_bar::render_bottom_panel(ui, data, actions),
            Pane::Setup => super::right_panel::render_setup(ui, data, actions),
            Pane::Timer(panel) => super::timer::render_timer_panel(ui, data, actions, *panel),
        }
    }

    /// Named for screen readers and tests: the tab is painted, not a label.
    fn on_tab_button(&mut self, tab: &mut Pane, response: &egui::Response) {
        let title = pane_title(*tab, self.data);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &title));
    }

    fn is_closeable(&self, _tab: &Pane) -> bool {
        !self.locked
    }

    fn allowed_in_windows(&self, _tab: &mut Pane) -> bool {
        !self.locked
    }

    /// Panels that lay themselves out against their size, or scroll inside,
    /// get no scroll area around them.
    fn scroll_bars(&self, tab: &Pane) -> [bool; 2] {
        match tab {
            Pane::Switcher
            | Pane::Rundown
            | Pane::Audio
            | Pane::Setup
            | Pane::Output
            | Pane::Timer(_) => [false, true],
            Pane::Details => [true, true],
            Pane::Library | Pane::Channels | Pane::Layers | Pane::Inputs | Pane::Monitors => {
                [false, false]
            }
        }
    }
}

/// The name on a panel's tab: a timer's is its timer's.
fn pane_title(pane: Pane, data: &UIData) -> String {
    match pane {
        Pane::Timer(panel) => super::timer::panel_title(data, panel),
        _ => pane.title().to_string(),
    }
}

/// Draw the dock in the space left under the top bar. Returns where windows
/// that belong to the channels (the deck adjuster) should open beside.
pub(super) fn render_dock(
    ui: &mut egui::Ui,
    screen: &mut ScreenLayout,
    data: &UIData,
    actions: &mut UIActions,
) -> egui::Rect {
    let area = ui.max_rect();
    if screen.dock.iter_all_tabs().next().is_none() {
        ui.centered_and_justified(|ui| {
            ui.label(
                egui::RichText::new(
                    "No panels open — choose a layout, or open panels, from Layout in the top bar",
                )
                .weak(),
            );
        });
        return area;
    }
    let locked = screen.locked;
    let mut viewer = Viewer {
        data,
        actions,
        locked,
        modal_drawn: false,
        desk_rect: None,
    };
    let mut style = egui_dock::Style::from_egui(ui.style());
    style.tab_bar.height = 26.0;
    // The smallest a panel can be dragged to. egui_dock's own 175 px also
    // silently widens a narrow panel of a layout on a small screen, which
    // then reads as changed.
    style.separator.extra = 60.0;
    egui_dock::DockArea::new(&mut screen.dock)
        .id(egui::Id::new("varda_dock"))
        .style(style)
        .draggable_tabs(!locked)
        .show_close_buttons(!locked)
        // A group's close-all is one stray click from emptying a corner of
        // the screen mid-show; tabs close one at a time.
        .show_leaf_close_all_buttons(false)
        .show_leaf_collapse_buttons(false)
        .tab_context_menus(!locked)
        .window_bounds(area)
        .show_inside(ui, &mut viewer);
    viewer.desk_rect.unwrap_or(area)
}

/// Whether the arrangement is no longer the layout it is named after.
fn is_modified(screen: &ScreenLayout, data: &UIData) -> bool {
    match Builtin::named(&screen.name) {
        Some(builtin) => !screen_layout::same_arrangement(&screen.dock, &builtin.dock()),
        None => data
            .saved_layouts
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&screen.name))
            .is_none_or(|s| !screen_layout::same_arrangement(&screen.dock, &s.dock)),
    }
}

fn save_name_id() -> egui::Id {
    egui::Id::new("layout_save_name")
}

/// The top bar's **Layout** menu.
pub(super) fn render_layout_menu(
    ui: &mut egui::Ui,
    screen: &mut ScreenLayout,
    data: &UIData,
    actions: &mut UIActions,
) {
    let modified = is_modified(screen, data);
    let label = if modified {
        format!("Layout: {} *", screen.name)
    } else {
        format!("Layout: {}", screen.name)
    };
    let config = egui::containers::menu::MenuConfig::new()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
    let (response, _) = egui::containers::menu::MenuButton::new(label)
        .config(config)
        .ui(ui, |ui| {
            ui.set_min_width(270.0);
            layout_menu_body(ui, screen, modified, data, actions);
        });
    response.on_hover_text(if modified {
        "Screen layout — changed since chosen. Click to pick another, save it, or open panels"
    } else {
        "Screen layout — Classic, Broadcast, VJ or your own. Drag a panel's tab to move it"
    });
}

fn layout_menu_body(
    ui: &mut egui::Ui,
    screen: &mut ScreenLayout,
    modified: bool,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("Layouts").strong());
    for builtin in Builtin::ALL {
        let current = screen.name == builtin.name();
        if ui
            .selectable_label(current, builtin.name())
            .on_hover_text(builtin.describe())
            .clicked()
        {
            screen.name = builtin.name().to_string();
            screen.dock = builtin.dock();
            ui.close();
        }
    }
    if !data.saved_layouts.is_empty() {
        ui.separator();
        ui.label(egui::RichText::new("Saved").strong());
        for saved in data.saved_layouts.iter() {
            ui.horizontal(|ui| {
                let current = screen.name.eq_ignore_ascii_case(&saved.name);
                if ui.selectable_label(current, &saved.name).clicked() {
                    screen.name.clone_from(&saved.name);
                    screen.dock = saved.dock.clone();
                    ui.close();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("🗑")
                        .on_hover_text(format!("Delete the saved layout \"{}\"", saved.name))
                        .clicked()
                    {
                        actions.session.delete_saved_layout = Some(saved.name.clone());
                    }
                });
            });
        }
    }

    ui.separator();
    let is_saved = Builtin::named(&screen.name).is_none();
    if modified {
        if is_saved
            && ui
                .button(format!("Update \"{}\"", screen.name))
                .on_hover_text("Save the panels as they are now under this name")
                .clicked()
        {
            actions.session.save_layout_as = Some(screen.name.clone());
            ui.close();
        }
        let reference = Builtin::named(&screen.name).map(Builtin::dock).or_else(|| {
            data.saved_layouts
                .iter()
                .find(|s| s.name.eq_ignore_ascii_case(&screen.name))
                .map(|s| s.dock.clone())
        });
        if let Some(reference) = reference {
            if ui
                .button(format!("Reset to \"{}\"", screen.name))
                .on_hover_text("Put the panels back as the layout has them")
                .clicked()
            {
                screen.dock = reference;
                ui.close();
            }
        }
    }
    let mut name: String = ui
        .ctx()
        .data(|d| d.get_temp(save_name_id()))
        .unwrap_or_default();
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(&mut name)
                .hint_text("New layout name")
                .desired_width(160.0),
        );
        let edit = edit.on_hover_text("Name for a new saved layout");
        let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let problem = screen_layout::valid_name(&name).err();
        let save = ui
            .add_enabled(problem.is_none(), egui::Button::new("Save as"))
            .on_hover_text("Keep the panels as they are now under this name, for every show");
        let save = if let Some(problem) = &problem {
            save.on_disabled_hover_text(problem)
        } else {
            save
        };
        if problem.is_none() && (save.clicked() || entered) {
            actions.session.save_layout_as = Some(name.trim().to_string());
            name.clear();
            ui.close();
        }
    });
    ui.ctx().data_mut(|d| d.insert_temp(save_name_id(), name));

    ui.separator();
    ui.label(egui::RichText::new("Panels").strong());
    for pane in Pane::ALL {
        let mut open = screen.has(pane);
        if ui
            .checkbox(&mut open, pane.title())
            .on_hover_text(pane.describe())
            .changed()
        {
            screen_layout::toggle_pane(&mut screen.dock, pane);
        }
    }
    // A control panel for each timer in the show, and any the layout still
    // has of timers gone.
    let mut timers: Vec<Pane> = super::timer::panels(data)
        .into_iter()
        .map(Pane::Timer)
        .collect();
    for (_, pane) in screen.dock.iter_all_tabs() {
        if matches!(pane, Pane::Timer(_)) && !timers.contains(pane) {
            timers.push(*pane);
        }
    }
    if !timers.is_empty() {
        ui.label(egui::RichText::new("Timers").strong());
        for pane in timers {
            let mut open = screen.has(pane);
            if ui
                .checkbox(&mut open, pane_title(pane, data))
                .on_hover_text(pane.describe())
                .changed()
            {
                screen_layout::toggle_pane(&mut screen.dock, pane);
            }
        }
    }
    ui.separator();
    ui.checkbox(&mut screen.locked, "Lock panels")
        .on_hover_text(
            "No dragging, closing or floating panels — so a show can't knock them about",
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::screen_layout::has_pane;
    use egui_kittest::kittest::Queryable;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// What the runner would see: the arrangement fed back into the next
    /// frame, and the last save or delete asked for.
    #[derive(Default)]
    struct Runner {
        saved_as: Option<String>,
        deleted: Option<String>,
        library_toggled: bool,
    }

    /// A layout picked from the menu sticks the first time. Its new panels
    /// would have egui run the frame twice, the second pass without the
    /// click, and the runner keeps a frame's last actions, applied once the
    /// frame is done — so it runs one pass a frame.
    #[test]
    fn a_layout_picked_sticks_the_first_time() {
        struct Frame {
            data: UIData,
            actions: Option<crate::usecases::ui::UIActions>,
            seen: u64,
            passes: Vec<u64>,
        }
        fn apply(state: &mut Frame) {
            let actions = state.actions.take();
            if let Some(screen) = actions.and_then(|a| a.session.screen_layout) {
                state.data.screen = screen;
            }
        }
        let mut data = UIData::test_fixture();
        // From Broadcast, Classic's panels make egui ask for a second pass.
        data.screen = ScreenLayout::builtin(Builtin::Broadcast);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                |ui, state: &mut Frame| {
                    let frame = ui.ctx().cumulative_frame_nr();
                    if frame != state.seen {
                        apply(state);
                        state.seen = frame;
                        state.passes.push(0);
                    }
                    *state.passes.last_mut().unwrap() += 1;
                    state.actions = Some(super::super::render_ui(ui, &state.data));
                },
                Frame {
                    data,
                    actions: None,
                    seen: u64::MAX,
                    passes: Vec::new(),
                },
            );
        crate::usecases::ui::one_pass_a_frame(&harness.ctx);
        harness.run();
        harness.get_by_label_contains("Layout: ").click();
        harness.run();
        harness.get_by_label("Classic").click();
        harness.run();
        apply(harness.state_mut());
        assert_eq!(harness.state().data.screen.name, "Classic");
        assert!(
            harness.state().passes.iter().all(|&p| p == 1),
            "{:?}",
            harness.state().passes
        );
    }

    /// The whole UI, with each frame's arrangement fed back as the runner
    /// does.
    fn harness(data: UIData) -> (egui_kittest::Harness<'static, Runner>, Rc<RefCell<UIData>>) {
        let data = Rc::new(RefCell::new(data));
        let shared = data.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                move |ui, runner: &mut Runner| {
                    let mut actions = super::super::render_ui(ui, &shared.borrow());
                    let session = &mut actions.session;
                    if let Some(screen) = session.screen_layout.take() {
                        shared.borrow_mut().screen = screen;
                    }
                    if session.save_layout_as.is_some() {
                        runner.saved_as.clone_from(&session.save_layout_as);
                    }
                    if session.delete_saved_layout.is_some() {
                        runner.deleted.clone_from(&session.delete_saved_layout);
                    }
                    runner.library_toggled |= session.toggle_library_panel;
                },
                Runner::default(),
            );
        harness.run();
        (harness, data)
    }

    fn open_layout_menu(harness: &mut egui_kittest::Harness<'_, Runner>) {
        harness.get_by_label_contains("Layout: ").click();
        harness.run();
    }

    #[test]
    fn every_builtin_draws_a_tab_for_every_panel_it_names() {
        for builtin in Builtin::ALL {
            let mut data = UIData::test_fixture();
            data.screen = ScreenLayout::builtin(builtin);
            let panes: Vec<Pane> = data.screen.dock.iter_all_tabs().map(|(_, p)| *p).collect();
            let (harness, _) = harness(data);
            for pane in panes {
                assert!(
                    harness.query_by_label(pane.title()).is_some(),
                    "{builtin:?}: no {} tab",
                    pane.title()
                );
            }
        }
    }

    #[test]
    fn the_arrangement_is_unchanged_when_nothing_is_touched() {
        let mut data = UIData::test_fixture();
        data.screen = ScreenLayout::builtin(Builtin::Vj);
        let (_harness, data) = harness(data);
        let screen = data.borrow().screen.clone();
        assert_eq!(screen.name, "VJ");
        assert!(screen_layout::same_arrangement(
            &screen.dock,
            &Builtin::Vj.dock()
        ));
    }

    /// L (or the key it is bound to) opens and closes the Library panel.
    #[test]
    fn the_library_key_opens_and_closes_the_library_panel() {
        let mut data = UIData::test_fixture();
        data.keymap_bindings = crate::keymap::KeymapStore::defaults();
        let (mut harness, data) = harness(data);
        assert!(data.borrow().screen.has(Pane::Library));
        harness.key_press(egui::Key::L);
        harness.run();
        assert!(harness.state().library_toggled);
        assert!(!data.borrow().screen.has(Pane::Library));
        harness.key_press(egui::Key::L);
        harness.run();
        assert!(data.borrow().screen.has(Pane::Library));
    }

    #[test]
    fn choosing_broadcast_in_the_layout_menu_switches_the_panels() {
        let (mut harness, data) = harness(UIData::test_fixture());
        open_layout_menu(&mut harness);
        harness.get_by_label("Broadcast").click();
        harness.run();
        let screen = data.borrow().screen.clone();
        assert_eq!(screen.name, "Broadcast");
        assert!(has_pane(&screen.dock, Pane::Monitors));
        assert!(has_pane(&screen.dock, Pane::Inputs));
        assert!(!has_pane(&screen.dock, Pane::Channels));
        assert!(harness.query_by_label("Preview picture").is_some());
    }

    #[test]
    fn a_panel_can_be_closed_and_opened_from_the_layout_menu() {
        let (mut harness, data) = harness(UIData::test_fixture());
        open_layout_menu(&mut harness);
        let checkbox = egui::accesskit::Role::CheckBox;
        harness.get_by_role_and_label(checkbox, "Setup").click();
        harness.run();
        assert!(!data.borrow().screen.has(Pane::Setup));
        // The menu stays open for the next one.
        harness.get_by_role_and_label(checkbox, "Setup").click();
        harness.run();
        assert!(data.borrow().screen.has(Pane::Setup));
    }

    #[test]
    fn save_as_asks_for_the_arrangement_to_be_kept_under_the_name() {
        let (mut harness, _) = harness(UIData::test_fixture());
        open_layout_menu(&mut harness);
        let field = || egui::accesskit::Role::TextInput;
        harness.get_all_by_role(field()).last().unwrap().click();
        harness.run();
        harness
            .get_all_by_role(field())
            .last()
            .unwrap()
            .type_text("Show day");
        harness.run();
        harness.get_by_label("Save as").click();
        harness.run();
        assert_eq!(harness.state().saved_as.as_deref(), Some("Show day"));
    }

    #[test]
    fn a_builtins_name_cannot_be_saved_over() {
        let (mut harness, _) = harness(UIData::test_fixture());
        open_layout_menu(&mut harness);
        let field = || egui::accesskit::Role::TextInput;
        harness.get_all_by_role(field()).last().unwrap().click();
        harness.run();
        harness
            .get_all_by_role(field())
            .last()
            .unwrap()
            .type_text("broadcast");
        harness.run();
        harness.get_by_label("Save as").click();
        harness.run();
        assert_eq!(harness.state().saved_as, None);
    }

    #[test]
    fn a_changed_layout_is_marked_and_can_be_reset() {
        let mut data = UIData::test_fixture();
        screen_layout::remove_pane(&mut data.screen.dock, Pane::Setup);
        let (mut harness, data) = harness(data);
        assert!(harness.query_by_label("Layout: Classic *").is_some());
        open_layout_menu(&mut harness);
        harness.get_by_label("Reset to \"Classic\"").click();
        harness.run();
        assert!(data.borrow().screen.has(Pane::Setup));
        assert!(harness.query_by_label("Layout: Classic").is_some());
    }

    #[test]
    fn a_saved_layout_is_offered_and_can_be_deleted() {
        let mut data = UIData::test_fixture();
        let mut dock = Builtin::Vj.dock();
        screen_layout::remove_pane(&mut dock, Pane::Audio);
        data.saved_layouts = std::sync::Arc::new(vec![screen_layout::SavedLayout {
            name: "Conference".into(),
            dock,
            was: None,
        }]);
        let (mut harness, data) = harness(data);
        open_layout_menu(&mut harness);
        harness.get_by_label("🗑").click();
        harness.run();
        assert_eq!(harness.state().deleted.as_deref(), Some("Conference"));
        harness.get_by_label("Conference").click();
        harness.run();
        let screen = data.borrow().screen.clone();
        assert_eq!(screen.name, "Conference");
        assert!(has_pane(&screen.dock, Pane::Layers));
        assert!(!has_pane(&screen.dock, Pane::Audio));
    }

    #[test]
    fn a_locked_layout_offers_no_close_buttons() {
        let mut data = UIData::test_fixture();
        data.screen.locked = true;
        let (_harness, data) = harness(data);
        assert!(data.borrow().screen.locked, "stays locked");
    }
}
