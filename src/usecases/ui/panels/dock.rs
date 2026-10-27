//! The panels below the top bar, as a dock: tabs that can be dragged into
//! other groups or to any edge, dividers that resize, and the **Layout** menu
//! that picks Classic, Broadcast, VJ or a saved layout, saves the
//! arrangement, and opens or closes panels. See [`crate::usecases::ui::screen_layout`].
//!
//! A panel can also be in a window of its own, to put on another screen: its
//! tab dragged out of the dock (or **Open in a window** from its tab's menu,
//! or **Window** in the Layout menu). Closing that window, or **Put back** in it,
//! returns the panel to the dock. Each is an egui viewport, drawn in this
//! window's frame — a window of the runner's own where it has them (see
//! `runner::panel_windows`), else a window inside this one (tests).

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
            Pane::Presentations => super::slides::render_slides_panel(ui, data, actions),
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

    /// Dragged out of the dock, or **Open in a window**: the dock floats it
    /// for a moment, and [`render_dock`] takes it into a window of its own.
    fn allowed_in_windows(&self, tab: &mut Pane) -> bool {
        !self.locked && tab.can_have_window()
    }

    /// Panels that lay themselves out against their size, or scroll inside,
    /// get no scroll area around them.
    fn scroll_bars(&self, tab: &Pane) -> [bool; 2] {
        match tab {
            Pane::Switcher
            | Pane::Rundown
            | Pane::Presentations
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

/// Draw the dock in the space left under the top bar, and the panels in
/// windows of their own. Returns where windows that belong to the channels
/// (the deck adjuster) should open beside.
pub(super) fn render_dock(
    ui: &mut egui::Ui,
    screen: &mut ScreenLayout,
    data: &UIData,
    actions: &mut UIActions,
) -> egui::Rect {
    let area = ui.max_rect();
    let locked = screen.locked;
    let mut viewer = Viewer {
        data,
        actions,
        locked,
        modal_drawn: false,
        desk_rect: None,
    };
    if screen.dock.iter_all_tabs().next().is_none() {
        let text = if screen.windows.is_empty() {
            "No panels open — choose a layout, or open panels, from Layout in the top bar"
        } else {
            "Every panel open is in a window of its own — Layout in the top bar puts them back"
        };
        ui.centered_and_justified(|ui| {
            ui.label(egui::RichText::new(text).weak());
        });
    } else {
        show_dock(ui, screen, &mut viewer, area);
    }
    render_windows(ui.ctx(), screen, &mut viewer);
    viewer.desk_rect.unwrap_or(area)
}

/// The tab menu's item that floats a panel out of the dock — into a window
/// of its own, here.
const OPEN_IN_A_WINDOW: &str = "Open in a window";

fn show_dock(ui: &mut egui::Ui, screen: &mut ScreenLayout, viewer: &mut Viewer<'_>, area: egui::Rect) {
    let locked = screen.locked;
    let menu = &mut screen.dock.translations.tab_context_menu;
    if menu.eject_button != OPEN_IN_A_WINDOW {
        OPEN_IN_A_WINDOW.clone_into(&mut menu.eject_button);
    }
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
        .show_inside(ui, viewer);
    // Floated out of the dock — its tab dragged out, or Open in a window —
    // or a layout from before with a floating window: each such panel in a
    // window of its own (one that cannot have one back in the dock).
    for pane in take_floating(&mut screen.dock) {
        if pane.can_have_window() {
            screen.open_in_window(pane);
        } else {
            screen_layout::add_pane(&mut screen.dock, pane);
        }
    }
}

/// The panels in the dock's floating windows, taken out of it with them.
fn take_floating(dock: &mut egui_dock::DockState<Pane>) -> Vec<Pane> {
    let floating: Vec<egui_dock::SurfaceIndex> = dock
        .iter_surfaces_indexed()
        .filter(|(_, surface)| matches!(surface, egui_dock::Surface::Window(..)))
        .map(|(index, _)| index)
        .collect();
    let mut panes = Vec::new();
    for index in floating.into_iter().rev() {
        if let Some(surface) = dock.remove_surface(index) {
            panes.extend(surface.iter_all_tabs().map(|(_, pane)| *pane));
        }
    }
    panes.reverse();
    panes
}

/// The viewport of a panel's window of its own.
pub fn window_viewport(pane: Pane) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("nida_panel_window", pane))
}

/// How big a panel's window first opens.
fn window_size(pane: Pane) -> [f32; 2] {
    match pane {
        Pane::Audio => [760.0, 460.0],
        Pane::Switcher | Pane::Monitors => [960.0, 540.0],
        Pane::Output => [800.0, 480.0],
        Pane::Timer(_) => [420.0, 320.0],
        _ => [520.0, 640.0],
    }
}

/// Each panel in a window of its own, drawn there; put back in the dock when
/// its window is closed or **Put back** is pressed in it, and where its
/// window now is kept.
fn render_windows(ctx: &egui::Context, screen: &mut ScreenLayout, viewer: &mut Viewer<'_>) {
    let windows = screen.windows.clone();
    let locked = screen.locked;
    let mut back = Vec::new();
    for window in windows {
        let pane = window.pane;
        let title = pane_title(pane, viewer.data);
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("{title} — Nida"))
            .with_min_inner_size([240.0, 160.0])
            .with_close_button(!locked);
        builder = match window.rect {
            Some([x, y, w, h]) => builder.with_position([x, y]).with_inner_size([w, h]),
            None => builder.with_inner_size(window_size(pane)),
        };
        let (put_back, rect) = ctx.show_viewport_immediate(
            window_viewport(pane),
            builder,
            |ui, class| render_window(ui, class, pane, &title, locked, viewer),
        );
        if put_back {
            back.push(pane);
        // Where it is kept follows the window — unless a layout chosen has
        // just asked for it elsewhere, and it is still to be moved there.
        } else if let Some(rect) = rect.filter(|r| !window.place && moved(window.rect, *r)) {
            if let Some(kept) = screen.windows.iter_mut().find(|w| w.pane == pane) {
                kept.rect = Some(rect);
            }
        }
    }
    for pane in back {
        screen.put_back(pane);
    }
}

/// A panel's window: a bar with **Put back** over the panel. Returns whether
/// it is to be put back, and where the window is — a window of its own
/// only (`class`): one inside this window has no place of its own to keep.
fn render_window(
    ui: &mut egui::Ui,
    class: egui::ViewportClass,
    pane: Pane,
    title: &str,
    locked: bool,
    viewer: &mut Viewer<'_>,
) -> (bool, Option<[f32; 4]>) {
    let own = class == egui::ViewportClass::Immediate;
    // The show's shortcuts work from a window of its own as from the main
    // window: on its own keys, as its own text field allows (one inside
    // the main window has the main window's).
    let was_typing = own && super::shortcuts_first(ui, viewer.data, viewer.actions);
    if own {
        // What its panel publishes for drops is this window's alone, made
        // again each pass (the main window's are cleared in its own).
        super::dnd::begin_fx_surface_frame(ui.ctx());
    }
    let mut put_back = false;
    egui::Panel::top(egui::Id::new(("panel_window_bar", pane)))
        .exact_size(26.0)
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                // A window of its own has the name on its title bar.
                if !own {
                    ui.label(egui::RichText::new(title).strong());
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !locked {
                        put_back = ui
                            .button("Put back")
                            .on_hover_text("Back among the panels of the main window")
                            .clicked();
                    }
                });
            });
        });
    egui::CentralPanel::default().show(ui, |ui| {
        let mut tab = pane;
        let [across, down] = egui_dock::TabViewer::scroll_bars(viewer, &tab);
        if across || down {
            egui::ScrollArea::new([across, down])
                .auto_shrink(false)
                .show(ui, |ui| egui_dock::TabViewer::ui(viewer, ui, &mut tab));
        } else {
            egui_dock::TabViewer::ui(viewer, ui, &mut tab);
        }
    });
    // The Rundown's step windows open beside it, in its window; files from
    // Finder land on the Presentations panel in its.
    if own && pane == Pane::Rundown {
        super::rundown::render_windows(ui.ctx(), viewer.data, viewer.actions);
    }
    if own && pane == Pane::Presentations {
        super::file_drop::handle_window_file_drop(ui, viewer.actions);
    }
    if own {
        super::keymap_shortcuts(ui, viewer.data, viewer.actions, was_typing);
    }
    if own && !locked && ui.input(|i| i.viewport().close_requested()) {
        put_back = true;
    }
    let rect = own
        .then(|| {
            ui.input(|i| {
                let info = i.viewport();
                let (outer, inner) = (info.outer_rect?, info.inner_rect?);
                Some([outer.min.x, outer.min.y, inner.width(), inner.height()])
            })
        })
        .flatten();
    (put_back, rect)
}

/// Whether a window is somewhere else now than `kept` says: moved or sized
/// by half a point or more.
fn moved(kept: Option<[f32; 4]>, now: [f32; 4]) -> bool {
    kept.is_none_or(|kept| kept.iter().zip(now).any(|(a, b)| (a - b).abs() >= 0.5))
}

/// A panel's row in the Layout menu: open or not, and — for one that can
/// have it — a window of its own (**Window**), or **Put back** from one.
fn panel_row(ui: &mut egui::Ui, screen: &mut ScreenLayout, pane: Pane, title: &str) {
    ui.horizontal(|ui| {
        let mut open = screen.has(pane);
        if ui
            .checkbox(&mut open, title)
            .on_hover_text(pane.describe())
            .changed()
        {
            screen.toggle(pane);
        }
        if !pane.can_have_window() || screen.locked {
            return;
        }
        // Named for the panel, for screen readers (and tests): each row has
        // its own.
        let named = |response: egui::Response, name: String| {
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Button, true, name.clone())
            });
            response
        };
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if screen.in_window(pane) {
                let button = ui
                    .small_button("Put back")
                    .on_hover_text(format!("{title}: back from its window into the panels here"));
                if named(button, format!("Put {title} back")).clicked() {
                    screen.put_back(pane);
                }
            } else {
                let button = ui.small_button("Window").on_hover_text(format!(
                    "{title} in a window of its own — to put on another screen. Closing the \
                     window puts it back"
                ));
                if named(button, format!("{title} in a window")).clicked() {
                    screen.open_in_window(pane);
                }
            }
        });
    });
}

/// Whether the arrangement is no longer the layout it is named after.
fn is_modified(screen: &ScreenLayout, data: &UIData) -> bool {
    match Builtin::named(&screen.name) {
        Some(builtin) => !screen.arranged_as(&builtin.dock(), &[]),
        None => data
            .saved_layouts
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&screen.name))
            .is_none_or(|s| !screen.arranged_as(&s.dock, &s.windows)),
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
            screen.arrange(builtin.dock(), &[]);
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
                    screen.arrange(saved.dock.clone(), &saved.windows);
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
        let reference = Builtin::named(&screen.name)
            .map(|builtin| (builtin.dock(), Vec::new()))
            .or_else(|| {
                data.saved_layouts
                    .iter()
                    .find(|s| s.name.eq_ignore_ascii_case(&screen.name))
                    .map(|s| (s.dock.clone(), s.windows.clone()))
            });
        if let Some((dock, windows)) = reference {
            if ui
                .button(format!("Reset to \"{}\"", screen.name))
                .on_hover_text(
                    "Put the panels back as the layout has them — those in windows of their \
                     own too",
                )
                .clicked()
            {
                screen.arrange(dock, &windows);
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
        panel_row(ui, screen, pane, pane.title());
    }
    // A control panel for each timer in the show, and any the layout still
    // has of timers gone.
    let mut timers: Vec<Pane> = super::timer::panels(data)
        .into_iter()
        .map(Pane::Timer)
        .collect();
    let laid_out = screen
        .dock
        .iter_all_tabs()
        .map(|(_, pane)| *pane)
        .chain(screen.windows.iter().map(|w| w.pane));
    for pane in laid_out.collect::<Vec<_>>() {
        if matches!(pane, Pane::Timer(_)) && !timers.contains(&pane) {
            timers.push(pane);
        }
    }
    if !timers.is_empty() {
        ui.label(egui::RichText::new("Timers").strong());
        for pane in timers {
            panel_row(ui, screen, pane, &pane_title(pane, data));
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
        /// The commands asked, in order (the panels' windows' tests).
        commands: Vec<crate::engine::EngineCommand>,
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

    /// A layout changed — a panel closed, another in a window of its own on
    /// the second screen — is marked; Reset puts both back as the layout has
    /// them, the window's panel in the dock again.
    #[test]
    fn a_changed_layout_is_marked_and_can_be_reset() {
        let mut data = UIData::test_fixture();
        screen_layout::remove_pane(&mut data.screen.dock, Pane::Setup);
        data.screen.open_in_window(Pane::Audio);
        // Its window one of its own, as on a Mac — not floating over the
        // layout's button.
        let (mut harness, data) = harness_with_windows(data);
        assert!(harness.query_by_label("Layout: Classic *").is_some());
        open_layout_menu(&mut harness);
        harness.get_by_label("Reset to \"Classic\"").click();
        harness.run();
        let screen = &data.borrow().screen;
        assert!(screen.has(Pane::Setup));
        assert!(screen.windows.is_empty(), "the window's panel back in the dock");
        assert!(screen_layout::has_pane(&screen.dock, Pane::Audio));
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
            windows: Vec::new(),
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

    /// A panel opens in a window of its own from the Layout menu — a window
    /// inside this one here, the tests having no other — out of the dock;
    /// Put back in that window returns it to the dock.
    #[test]
    fn a_panel_opens_in_a_window_of_its_own_and_is_put_back() {
        let mut data = UIData::test_fixture();
        data.screen = ScreenLayout::builtin(Builtin::Classic);
        let (mut harness, data) = harness(data);
        open_layout_menu(&mut harness);
        assert!(
            harness.query_by_label("Library in a window").is_none(),
            "dragged from: no window for it"
        );
        harness.get_by_label("Audio Mixer in a window").click();
        harness.run();
        assert!(data.borrow().screen.in_window(Pane::Audio));
        assert!(!has_pane(&data.borrow().screen.dock, Pane::Audio));
        assert!(
            harness.query_by_label("Put Audio Mixer back").is_some(),
            "the menu offers it back"
        );
        open_layout_menu(&mut harness);
        harness.run();
        assert!(
            harness.query_by_label_contains("Audio Mixer — Nida").is_some(),
            "its window"
        );
        harness.get_by_label("Put back").click();
        harness.run();
        assert!(!data.borrow().screen.in_window(Pane::Audio));
        assert!(has_pane(&data.borrow().screen.dock, Pane::Audio));
        assert!(harness.query_by_label_contains("Audio Mixer — Nida").is_none());
    }

    /// A tab floated out of the dock — dragged out, or a layout from before
    /// with a floating window — opens in a window of its own; one things are
    /// dragged into goes back in the dock. No floating window is left.
    #[test]
    fn a_tab_floated_out_of_the_dock_opens_in_a_window_of_its_own() {
        let mut data = UIData::test_fixture();
        let mut screen = ScreenLayout::builtin(Builtin::Classic);
        screen_layout::remove_pane(&mut screen.dock, Pane::Switcher);
        screen_layout::remove_pane(&mut screen.dock, Pane::Library);
        screen.dock.add_window(vec![Pane::Switcher]);
        screen.dock.add_window(vec![Pane::Library]);
        data.screen = screen;
        let (_harness, data) = harness(data);
        let screen = data.borrow().screen.clone();
        assert!(screen.in_window(Pane::Switcher));
        assert!(!screen.in_window(Pane::Library));
        assert!(has_pane(&screen.dock, Pane::Library), "back in the dock");
        assert!(
            screen
                .dock
                .iter_surfaces()
                .all(|s| !matches!(s, egui_dock::Surface::Window(..))),
            "no floating window left"
        );
    }

    thread_local! {
        /// What the tests' panel windows are told in their next pass: where
        /// they are, their close asked (told once), the keys pressed.
        static WINDOW: RefCell<(egui::ViewportInfo, Vec<egui::Event>)> =
            RefCell::new((egui::ViewportInfo::default(), Vec::new()));
        /// The keys a window's last pass left pressed in its input.
        static KEYS_LEFT: RefCell<Vec<egui::Key>> = const { RefCell::new(Vec::new()) };
    }

    /// The runner's way with panels' windows, for tests: each its own pass,
    /// 800×600, told what [`WINDOW`] holds.
    fn own_windows(ctx: &egui::Context) {
        ctx.set_embed_viewports(false);
        egui::Context::set_immediate_viewport_renderer(|ctx, viewport| {
            let id = viewport.ids.this;
            let (info, events) = WINDOW.with(|w| {
                let mut w = w.borrow_mut();
                let told = (w.0.clone(), std::mem::take(&mut w.1));
                w.0.events.clear();
                told
            });
            let input = egui::RawInput {
                viewport_id: id,
                viewports: std::iter::once((id, info)).collect(),
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                ..Default::default()
            };
            let mut show = viewport.viewport_ui_cb;
            let mut out = ctx.run_ui(input, |ui| show(ui));
            out.textures_delta.clear();
            let left = ctx.input_for(id, |i| {
                [egui::Key::Space, egui::Key::Escape]
                    .into_iter()
                    .filter(|k| i.key_pressed(*k))
                    .collect()
            });
            KEYS_LEFT.with(|k| *k.borrow_mut() = left);
        });
    }

    /// [`harness`], the panels' windows passes of their own.
    fn harness_with_windows(
        data: UIData,
    ) -> (egui_kittest::Harness<'static, Runner>, Rc<RefCell<UIData>>) {
        WINDOW.with(|w| *w.borrow_mut() = (egui::ViewportInfo::default(), Vec::new()));
        let data = Rc::new(RefCell::new(data));
        let shared = data.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                move |ui, runner: &mut Runner| {
                    let mut actions = super::super::render_ui(ui, &shared.borrow());
                    if let Some(screen) = actions.session.screen_layout.take() {
                        shared.borrow_mut().screen = screen;
                    }
                    runner.commands.append(&mut actions.commands);
                },
                Runner::default(),
            );
        own_windows(&harness.ctx);
        harness.run();
        (harness, data)
    }

    /// A panel's window of its own: where it is kept; closed, the panel is
    /// back in the dock — unless the panels are locked.
    #[test]
    fn a_panels_window_is_kept_where_it_is_and_closed_puts_it_back() {
        let mut data = UIData::test_fixture();
        data.screen = ScreenLayout::builtin(Builtin::Classic);
        data.screen.open_in_window(Pane::Audio);
        let (mut harness, data) = harness_with_windows(data);
        WINDOW.with(|w| {
            let info = &mut w.borrow_mut().0;
            info.outer_rect = Some(egui::Rect::from_min_size(
                egui::pos2(100.0, 200.0),
                egui::vec2(760.0, 488.0),
            ));
            info.inner_rect = Some(egui::Rect::from_min_size(
                egui::pos2(100.0, 228.0),
                egui::vec2(760.0, 460.0),
            ));
        });
        harness.run();
        assert_eq!(
            data.borrow().screen.windows[0].rect,
            Some([100.0, 200.0, 760.0, 460.0]),
            "its frame's corner, its inside's size"
        );

        data.borrow_mut().screen.locked = true;
        WINDOW.with(|w| w.borrow_mut().0.events.push(egui::ViewportEvent::Close));
        harness.run();
        assert!(data.borrow().screen.in_window(Pane::Audio), "locked: it stays");

        data.borrow_mut().screen.locked = false;
        WINDOW.with(|w| w.borrow_mut().0.events.push(egui::ViewportEvent::Close));
        harness.run();
        assert!(!data.borrow().screen.in_window(Pane::Audio));
        assert!(has_pane(&data.borrow().screen.dock, Pane::Audio), "back in the dock");
    }

    /// A layout chosen asks a window already open to go where the layout
    /// keeps it: that place is not overwritten by where the window still is,
    /// till the runner has moved it.
    #[test]
    fn a_window_a_layout_puts_elsewhere_keeps_that_place_till_moved() {
        let mut data = UIData::test_fixture();
        data.screen = ScreenLayout::builtin(Builtin::Classic);
        data.screen.open_in_window(Pane::Audio);
        data.screen.windows[0].rect = Some([2000.0, 100.0, 600.0, 400.0]);
        data.screen.windows[0].place = true;
        let (mut harness, data) = harness_with_windows(data);
        WINDOW.with(|w| {
            let info = &mut w.borrow_mut().0;
            info.outer_rect = Some(egui::Rect::from_min_size(
                egui::pos2(100.0, 200.0),
                egui::vec2(760.0, 488.0),
            ));
            info.inner_rect = Some(egui::Rect::from_min_size(
                egui::pos2(100.0, 228.0),
                egui::vec2(760.0, 460.0),
            ));
        });
        harness.run();
        assert_eq!(
            data.borrow().screen.windows[0].rect,
            Some([2000.0, 100.0, 600.0, 400.0]),
            "the layout's place, not where it still is"
        );
    }

    /// Preview / Program in a window of its own keeps only this pass's drop
    /// targets: none pile up frame after frame.
    #[test]
    fn a_panels_window_keeps_no_drop_targets_of_passes_before() {
        let mut data = UIData::test_fixture();
        data.screen = ScreenLayout::builtin(Builtin::Broadcast);
        data.screen.open_in_window(Pane::Monitors);
        let (mut harness, _data) = harness_with_windows(data);
        let window = window_viewport(Pane::Monitors);
        let first = super::super::dnd::fx_surface_count(&harness.ctx, window);
        for _ in 0..5 {
            harness.run();
        }
        let after = super::super::dnd::fx_surface_count(&harness.ctx, window);
        assert!(first > 0, "the Program picture takes effects");
        assert_eq!(after, first, "this pass's only");
    }

    /// The show's shortcuts work from a panel's window, on its own keys: GO
    /// pressed there is one GO — held, as it repeats, still one — and the key
    /// is taken from the panel, so a button focused there is not pressed by
    /// it too. A key bound to something else (Escape) is left to the panel's
    /// popups.
    #[test]
    fn the_shows_shortcuts_work_from_a_panels_window() {
        use crate::keymap::{ActionId, KeyCombo, KeyTarget};
        let key = |key: egui::Key| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        let combo = |name: &str| KeyCombo {
            key: name.into(),
            command: false,
            shift: false,
            alt: false,
        };
        let mut data = UIData::test_fixture();
        data.keymap_bindings
            .retain(|c, _| c.key != "Space" && c.key != "Escape");
        data.keymap_bindings
            .insert(combo("Space"), KeyTarget::Action(ActionId::RundownGo));
        data.keymap_bindings
            .insert(combo("Escape"), KeyTarget::Action(ActionId::ClearDrawing));
        data.screen.open_in_window(Pane::Switcher);
        let (mut harness, _data) = harness_with_windows(data);
        let gos = |harness: &egui_kittest::Harness<'_, Runner>| {
            harness
                .state()
                .commands
                .iter()
                .filter(|c| matches!(c, crate::engine::EngineCommand::RundownGo { .. }))
                .count()
        };
        WINDOW.with(|w| w.borrow_mut().1.extend([key(egui::Key::Space), key(egui::Key::Escape)]));
        harness.step();
        assert_eq!(gos(&harness), 1, "GO from the window");
        assert_eq!(KEYS_LEFT.with(|k| k.borrow().clone()), vec![egui::Key::Escape]);
        // Held: the key comes again, a repeat.
        WINDOW.with(|w| w.borrow_mut().1.push(key(egui::Key::Space)));
        harness.step();
        assert_eq!(gos(&harness), 1, "one GO a press");
        assert!(KEYS_LEFT.with(|k| k.borrow().is_empty()), "the repeat taken too");
    }

    /// Open in a window, from a tab's menu: the panel in a window of its
    /// own.
    #[test]
    fn open_in_a_window_from_a_tabs_menu() {
        let mut data = UIData::test_fixture();
        data.screen = ScreenLayout::builtin(Builtin::Classic);
        let (mut harness, data) = harness(data);
        harness.get_all_by_label("Rundown").next().unwrap().click_secondary();
        harness.run();
        harness.get_by_label(OPEN_IN_A_WINDOW).click();
        harness.run();
        harness.run();
        let screen = data.borrow().screen.clone();
        assert!(screen.in_window(Pane::Rundown), "{:?}", screen.windows);
        assert!(!has_pane(&screen.dock, Pane::Rundown));
    }
}
