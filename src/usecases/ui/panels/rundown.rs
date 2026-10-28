//! The rundown: the show in order, each step taken with one press.
//!
//! `GO` takes the next step; every step has its own `Cut` and `Fade` too. A
//! step changes every bus it sets together, takes Main, switches overlays and
//! starts what it starts — all of it or none: one that cannot be taken says
//! why on its row before it is pressed.
//!
//! `+ Step` keeps what is on now; double-click a step for what it sets.

use super::super::{widgets, BusUI, UIActions, UIData};
use super::switcher::{seconds, AUTO_DURATIONS, PREVIEW_GREEN, PROGRAM_RED};
use crate::bus::BusInputRef;
use crate::engine::value::render::OutputSource;
use crate::engine::{EngineCommand, RundownStepSnapshot};
use crate::mixer::MainSource;
use crate::engine::value::timer::{
    parse_span, parse_time_of_day, span_text, target_text, TimeFormat, MAX_DURATION_SECS,
};
use crate::rundown::{PlayAction, PlaySet, Step, TimerSet, DAY_SECS, MAX_THEN_SECS};

const WARN: egui::Color32 = egui::Color32::from_rgb(230, 180, 60);
/// `GO`: the one button pressed without looking.
const GO_BUTTON: egui::Vec2 = egui::vec2(150.0, 40.0);
/// A step's `Cut` and `Fade`.
const TAKE_BUTTON: egui::Vec2 = egui::vec2(46.0, 24.0);
/// What a keyboard-learn target is called for `GO`.
const GO_ACTION: &str = "RundownGo";

// ── Names ──────────────────────────────────────────────────────────

fn bus_name<'a>(data: &'a UIData, uuid: &str) -> Option<&'a str> {
    data.buses
        .iter()
        .find(|b| b.bus.uuid == uuid)
        .map(|b| b.bus.name.as_str())
}

/// An input by its number and name, as its tile shows it.
fn channel_name(data: &UIData, uuid: &str) -> Option<String> {
    data.channels
        .iter()
        .find(|ch| ch.uuid == uuid)
        .map(super::super::ChannelUIInfo::numbered)
}

/// A layer by its input's number and its own name.
fn deck_name(data: &UIData, uuid: &str) -> Option<String> {
    data.channels.iter().find_map(|ch| {
        let deck = ch.decks.iter().find(|d| d.uuid == uuid)?;
        Some(format!("{}  {}", ch.number(), deck.name))
    })
}

const MISSING: &str = "⚠ missing";

/// What an input is called.
fn input_name(data: &UIData, input: &BusInputRef) -> String {
    match input {
        BusInputRef::Black => "Black".to_string(),
        BusInputRef::Master => "Master".to_string(),
        BusInputRef::Channel(uuid) => channel_name(data, uuid).unwrap_or_else(|| MISSING.into()),
        BusInputRef::Deck(uuid) => deck_name(data, uuid).unwrap_or_else(|| MISSING.into()),
    }
}

fn main_name(data: &UIData, source: &MainSource) -> String {
    match source {
        MainSource::Black => "Black".to_string(),
        MainSource::Channel(uuid) => channel_name(data, uuid).unwrap_or_else(|| MISSING.into()),
        MainSource::Deck(uuid) => deck_name(data, uuid).unwrap_or_else(|| MISSING.into()),
    }
}

fn action_name(action: PlayAction) -> &'static str {
    match action {
        PlayAction::Start => "Start from the beginning",
        PlayAction::Resume => "Carry on",
        PlayAction::Pause => "Pause",
        PlayAction::Stop => "Stop, back to the beginning",
    }
}

fn action_mark(action: PlayAction) -> &'static str {
    match action {
        PlayAction::Start | PlayAction::Resume => "▶",
        PlayAction::Pause => "⏸",
        PlayAction::Stop => "⏹",
    }
}

/// One line on what a step sets, under its name.
fn summary(data: &UIData, step: &Step) -> String {
    let mut parts: Vec<String> = step
        .screens
        .iter()
        .map(|s| {
            format!(
                "{}: {}",
                bus_name(data, &s.bus).unwrap_or("⚠ a bus that is gone"),
                input_name(data, &s.input)
            )
        })
        .collect();
    if let Some(source) = &step.main {
        parts.push(format!("Main: {}", main_name(data, source)));
    }
    parts.extend(step.overlays.iter().map(|o| {
        format!(
            "{} {}",
            channel_name(data, &o.channel).unwrap_or_else(|| MISSING.into()),
            if o.on { "on" } else { "off" }
        )
    }));
    parts.extend(step.plays.iter().map(|p| {
        let mut said = format!(
            "{} {}",
            action_mark(p.action),
            deck_name(data, &p.deck).unwrap_or_else(|| MISSING.into())
        );
        let playing = matches!(p.action, PlayAction::Start | PlayAction::Resume);
        if p.until_next && playing {
            said.push_str(" till the next step");
        }
        match p.timer.filter(|_| p.action == PlayAction::Start) {
            Some(TimerSet::Down { secs }) => {
                said.push_str(" from ");
                said.push_str(&span_text(u64::from(secs), TimeFormat::Auto));
            }
            Some(TimerSet::Up) => said.push_str(" counting up"),
            None => {}
        }
        said
    }));
    if let Some(deck) = step.then_ends.as_deref() {
        let name = deck_name(data, deck).unwrap_or_else(|| MISSING.into());
        parts.push(format!("then the next when {name} ends"));
    } else if let Some(secs) = step.then_secs {
        parts.push(format!("then the next after {}", seconds(secs)));
    }
    if let Some(at) = step.at_secs {
        let always = if step.at_always { ", even if not next" } else { "" };
        parts.insert(0, format!("at {}{always}", target_text(at)));
    }
    if parts.is_empty() {
        "Sets nothing yet — double-click to say what".to_string()
    } else {
        parts.join("  ·  ")
    }
}

fn step_called<'a>(data: &'a UIData, uuid: Option<&str>) -> Option<&'a RundownStepSnapshot> {
    let uuid = uuid?;
    data.rundown.steps.iter().find(|s| s.step.uuid == uuid)
}

/// Why a step cannot be taken, for its hover text.
fn why_not(step: &RundownStepSnapshot) -> String {
    format!(
        "Cannot be taken — not in the show: {}",
        step.missing.join("; ")
    )
}

fn why_not_faded(step: &RundownStepSnapshot) -> String {
    format!(
        "Still fading: {}. A cut goes at once.",
        step.fading.join(", ")
    )
}

/// The step GO takes next, taken by itself at `at` (seconds after
/// midnight): how long till then, with Hold — or held, or its time passed.
fn render_due_at(
    ui: &mut egui::Ui,
    data: &UIData,
    next: &RundownStepSnapshot,
    at: u32,
    actions: &mut UIActions,
) {
    let uuid = next.step.uuid.as_str();
    let held = data.rundown.at_held.as_deref() == Some(uuid)
        || data.rundown.held_always.iter().any(|h| h == uuid);
    let left = f64::from(at) - crate::deck::timer::time_of_day();
    let (name, when) = (&next.step.name, target_text(at));
    ui.horizontal(|ui| {
        if held {
            // GO takes it only when it is the step next.
            let then = if data.rundown.next.as_deref() == Some(uuid) {
                "held: GO takes it"
            } else {
                "held: not taken by its time"
            };
            ui.label(egui::RichText::new(format!("{name} at {when} — {then}")).weak());
            if ui
                .small_button("By its time again")
                .on_hover_text("Taken by itself at its time after all")
                .clicked()
            {
                // One taken even when not next keeps the rundown where it is;
                // its own time, set again, lets its hold go.
                actions.commands.push(if next.step.at_always {
                    EngineCommand::SetRundownAt {
                        uuid: next.step.uuid.clone(),
                        at_secs: Some(at),
                        always: None,
                    }
                } else {
                    EngineCommand::SetRundownNext {
                        uuid: next.step.uuid.clone(),
                    }
                });
            }
        } else if left > 0.0 {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let left_text = span_text(left.ceil() as u64, TimeFormat::Auto);
            let text = egui::RichText::new(format!("{name} at {when} — in {left_text}")).strong();
            ui.label(if left <= 60.0 { text.color(WARN) } else { text });
            if ui
                .small_button("Hold")
                .on_hover_text("Not by itself at its time: it waits for GO")
                .clicked()
            {
                actions.commands.push(EngineCommand::RundownHold);
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        } else {
            ui.label(
                egui::RichText::new(format!("{name} at {when} — that time has gone: GO takes it"))
                    .weak(),
            );
        }
    });
}

/// Whether the stream or the recording takes this bus.
fn on_air_bus(data: &UIData, bus: &BusUI) -> bool {
    [
        &data.stream_settings.options.source,
        &data.record.options.source,
    ]
    .iter()
    .any(|source| matches!(source, OutputSource::BusProgram(b) if *b == bus.bus.uuid))
}

// ── The panel ──────────────────────────────────────────────────────

/// Where the panel is, in the window it is drawn in (`ctx`'s viewport), for
/// a step's window to open beside it there.
fn panel_rect_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::new("rundown_panel_rect").with(ctx.viewport_id())
}

pub(super) fn render_rundown(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let panel = ui.max_rect();
    let rect_id = panel_rect_id(ui.ctx());
    ui.ctx().data_mut(|d| d.insert_temp(rect_id, panel));
    render_top(ui, data, actions);
    if let Some(wait) = &data.rundown.wait {
        ui.horizontal(|ui| {
            let to = step_called(data, Some(&wait.to)).map_or("the next step", |s| &s.step.name);
            let text = match wait.until.as_deref() {
                Some(deck) => {
                    let name = deck_name(data, deck).unwrap_or_else(|| MISSING.into());
                    let left = if wait.secs_left > 0.0 {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let left = span_text(wait.secs_left.ceil() as u64, TimeFormat::Auto);
                        format!(" — {left} left")
                    } else {
                        String::new()
                    };
                    format!("Then {to} when {name} ends{left}")
                }
                None => format!("Then {to} in {:.0} s", wait.secs_left.ceil()),
            };
            ui.label(egui::RichText::new(text).strong().color(WARN));
            if ui
                .small_button("Hold")
                .on_hover_text("Do not go on by itself: stay on this step until GO")
                .clicked()
            {
                actions.commands.push(EngineCommand::RundownHold);
            }
        });
    }
    // The timed step coming first — counted down while a wait runs too when
    // it is one taken even when not next: it may come before the wait's.
    if let Some(coming) = crate::rundown::coming_at(
        data.rundown.steps.iter().map(|s| &s.step),
        data.rundown.next.as_deref(),
        data.rundown.last.as_deref(),
        crate::deck::timer::time_of_day(),
    ) {
        let off_next = coming.at_always && data.rundown.next.as_deref() != Some(&coming.uuid);
        if data.rundown.wait.is_none() || off_next {
            if let (Some(at), Some(step)) = (coming.at_secs, step_called(data, Some(&coming.uuid))) {
                render_due_at(ui, data, step, at, actions);
            }
        }
    }
    ui.add_space(4.0);
    if data.rundown.steps.is_empty() {
        ui.label(
            egui::RichText::new(
                "The show in order. A step changes every screen together, and can take \
                 Main and start clips — taken with one press. Set the screens up as the \
                 first moment wants them, then + Step.",
            )
            .small()
            .weak(),
        );
    }
    for step in &data.rundown.steps {
        render_step(ui, step, data, actions);
        ui.add_space(4.0);
    }
    // A fade ending, or a wait counting down, changes what can be pressed.
    let moving = data.rundown.wait.is_some()
        || data.rundown.steps.iter().any(|s| !s.fading.is_empty())
        || data.buses.iter().any(|b| b.bus.transition.is_some());
    if moving {
        ui.ctx().request_repaint();
    }
}

/// `GO`, its cut, the fade time and `+ Step`.
fn render_top(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let rundown = &data.rundown;
    let next = step_called(data, rundown.next.as_deref());
    ui.horizontal(|ui| {
        let label = match next {
            Some(step) => format!("GO ▸ {}", step.step.name),
            None if rundown.steps.is_empty() => "GO".to_string(),
            None => "GO — the end".to_string(),
        };
        let can_cut = next.is_some_and(|s| s.missing.is_empty());
        let can_fade = can_cut && next.is_some_and(|s| s.fading.is_empty());
        let learning = data.keyboard_learn_active;
        // In the colour of what comes next, so it is found without reading.
        let go = ui.add_enabled(
            can_fade || learning,
            egui::Button::new(
                egui::RichText::new(label)
                    .strong()
                    .size(16.0)
                    .color(egui::Color32::WHITE),
            )
            .fill(PREVIEW_GREEN)
            .min_size(GO_BUTTON),
        );
        let go = match next {
            Some(step) if !step.missing.is_empty() => go.on_disabled_hover_text(why_not(step)),
            Some(step) if !step.fading.is_empty() => {
                go.on_disabled_hover_text(why_not_faded(step))
            }
            Some(_) => go.on_hover_text(format!(
                "Fade to the next step over {}",
                seconds(rundown.fade_secs)
            )),
            None => go.on_disabled_hover_text("No step comes next"),
        };
        let go_with = |fade: bool| EngineCommand::RundownGo {
            fade,
            expect: rundown.next.clone(),
        };
        if learning {
            // A key for GO: picked here, then pressed.
            if data.keyboard_learn_target.as_deref() == Some(GO_ACTION) {
                widgets::draw_keyboard_learn_selected(ui, go.rect);
            } else {
                widgets::draw_keyboard_learn_glow(ui, go.rect);
            }
            if go.clicked() {
                actions.session.keyboard_learn_select = Some(crate::keymap::KeyTarget::Action(
                    crate::keymap::ActionId::RundownGo,
                ));
            }
        } else if go.clicked() {
            actions.commands.push(go_with(true));
        }
        let cut = ui
            .add_enabled(
                can_cut,
                egui::Button::new("Cut").min_size(egui::vec2(TAKE_BUTTON.x, GO_BUTTON.y)),
            )
            .on_hover_text("Cut to the next step")
            .on_disabled_hover_text(next.map_or_else(|| "No step comes next".to_string(), why_not));
        if cut.clicked() {
            actions.commands.push(go_with(false));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("+ Step")
                .on_hover_text("A new step of what is on now")
                .clicked()
            {
                open_new_step(ui.ctx(), data);
            }
            egui::ComboBox::from_id_salt("rundown_fade")
                .selected_text(seconds(rundown.fade_secs))
                .width(56.0)
                .show_ui(ui, |ui| {
                    for d in AUTO_DURATIONS {
                        let current = (rundown.fade_secs - d).abs() < 1e-3;
                        if ui.selectable_label(current, seconds(d)).clicked() && !current {
                            actions
                                .commands
                                .push(EngineCommand::SetRundownFade { secs: d });
                        }
                    }
                })
                .response
                .on_hover_text("How long the screens take to fade to a step");
            ui.label(egui::RichText::new("Fade:").small());
        });
    });
}

fn tally(ui: &mut egui::Ui, text: &str, color: egui::Color32, hover: &str) {
    ui.label(
        egui::RichText::new(format!(" {text} "))
            .small()
            .strong()
            .color(egui::Color32::WHITE)
            .background_color(color),
    )
    .on_hover_text(hover);
}

fn render_step(
    ui: &mut egui::Ui,
    step: &RundownStepSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let uuid = &step.step.uuid;
    let rundown = &data.rundown;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            // Both, when the operator has made the step the show is on the
            // one GO takes next.
            if rundown.last.as_deref() == Some(uuid) {
                tally(ui, "NOW", PROGRAM_RED, "The step taken last");
            }
            if rundown.next.as_deref() == Some(uuid) {
                tally(ui, "NEXT", PREVIEW_GREEN, "The step GO takes");
            }
            let name = ui
                .add(
                    egui::Label::new(egui::RichText::new(&step.step.name).strong().size(15.0))
                        .sense(egui::Sense::click()),
                )
                .on_hover_text("Double-click for what it sets; right-click for more");
            if name.double_clicked() {
                open_editor(ui.ctx(), &step.step);
            }
            name.context_menu(|ui| step_menu(ui, step, data, actions));
            if !step.missing.is_empty() {
                ui.label(egui::RichText::new("⚠").color(WARN))
                    .on_hover_text(why_not(step));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let takeable = step.missing.is_empty();
                let button = |text: &str| egui::Button::new(text).min_size(TAKE_BUTTON);
                let fade = ui
                    .add_enabled(takeable && step.fading.is_empty(), button("Fade"))
                    .on_hover_text(format!(
                        "Fade to {} over {}",
                        step.step.name,
                        seconds(rundown.fade_secs)
                    ))
                    .on_disabled_hover_text(if takeable {
                        why_not_faded(step)
                    } else {
                        why_not(step)
                    });
                let cut = ui
                    .add_enabled(takeable, button("Cut"))
                    .on_hover_text(format!("Cut to {}", step.step.name))
                    .on_disabled_hover_text(why_not(step));
                for (pressed, fade) in [(fade, true), (cut, false)] {
                    if pressed.clicked() {
                        actions.commands.push(EngineCommand::TakeRundownStep {
                            uuid: uuid.clone(),
                            fade,
                        });
                    }
                }
            });
        });
        // Not small, not dim: read at arm's length in the middle of a show.
        ui.add(egui::Label::new(summary(data, &step.step)).wrap());
    });
}

fn step_menu(
    ui: &mut egui::Ui,
    step: &RundownStepSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let uuid = &step.step.uuid;
    if ui.button("Edit…").clicked() {
        open_editor(ui.ctx(), &step.step);
        ui.close();
    }
    let at = data
        .rundown
        .steps
        .iter()
        .position(|s| &s.step.uuid == uuid)
        .unwrap_or(0);
    let last = at + 1 >= data.rundown.steps.len();
    let move_step = |later| EngineCommand::MoveRundownStep {
        uuid: uuid.clone(),
        later,
    };
    // Each: its label, what it does, whether it can be done, the command.
    let items = [
        (
            "Update from what is on now",
            "The screens it sets, and Main if it sets it, become what is on now",
            true,
            EngineCommand::CaptureRundownStep { uuid: uuid.clone() },
        ),
        (
            "GO takes this next",
            "Without taking anything now",
            true,
            EngineCommand::SetRundownNext { uuid: uuid.clone() },
        ),
        (
            "Duplicate",
            "A copy right after it, to change into the next moment",
            true,
            EngineCommand::DuplicateRundownStep { uuid: uuid.clone() },
        ),
        ("Move earlier", "One place up the rundown", at > 0, move_step(false)),
        ("Move later", "One place down the rundown", !last, move_step(true)),
        (
            "Remove",
            "Undo brings it back",
            true,
            EngineCommand::RemoveRundownStep { uuid: uuid.clone() },
        ),
    ];
    for (label, hover, possible, cmd) in items {
        if label == "Remove" {
            ui.separator();
        }
        if ui
            .add_enabled(possible, egui::Button::new(label))
            .on_hover_text(hover)
            .clicked()
        {
            actions.commands.push(cmd);
            ui.close();
        }
    }
}

/// The key bound to `GO` takes the next step with a fade — when no text
/// field is being typed into, and not while a key is being learned.
pub(super) fn go_shortcut(ui: &egui::Ui, data: &UIData, actions: &mut UIActions) {
    use crate::keymap::{ActionId, KeyTarget};
    if data.keyboard_learn_active || data.stage_editor_open || super::typing(ui.ctx()) {
        return;
    }
    for (combo, repeat) in crate::usecases::ui::keyboard::collect_key_downs(ui.ctx()) {
        if data.keymap_bindings.get(&combo) != Some(&KeyTarget::Action(ActionId::RundownGo)) {
            continue;
        }
        // The key is GO's alone: it must not also press a focused button —
        // held, as it repeats, either. One GO a press.
        super::consume(ui.ctx(), &combo);
        if repeat {
            continue;
        }
        actions.commands.push(EngineCommand::RundownGo {
            fade: true,
            expect: data.rundown.next.clone(),
        });
    }
}

// ── A new step ─────────────────────────────────────────────────────

/// The step being made: its name, the buses it keeps, and whether Main.
#[derive(Clone)]
struct NewStep {
    name: String,
    /// Each bus by UUID, and whether the step keeps what it shows.
    buses: Vec<(String, bool)>,
    main: bool,
    at: egui::Pos2,
    /// Not yet drawn: the name is selected, to be typed over.
    fresh: bool,
}

fn new_step_id() -> egui::Id {
    egui::Id::new("rundown_new_step")
}

fn new_step_field() -> egui::Id {
    egui::Id::new("rundown_new_step_name")
}

fn open_new_step(ctx: &egui::Context, data: &UIData) {
    let names: Vec<&str> = data
        .rundown
        .steps
        .iter()
        .map(|s| s.step.name.as_str())
        .collect();
    let state = NewStep {
        name: crate::rundown::new_name(&names),
        // A step is for the screens: what the stream or the recording takes
        // is left out unless ticked, so a step made for the wall cannot
        // change what goes out.
        buses: data
            .buses
            .iter()
            .map(|b| (b.bus.uuid.clone(), !on_air_bus(data, b)))
            .collect(),
        main: false,
        at: ctx
            .pointer_latest_pos()
            .unwrap_or_else(|| ctx.content_rect().center()),
        fresh: true,
    };
    ctx.data_mut(|d| d.insert_temp(new_step_id(), state));
}

fn render_new_step(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let Some(mut state) = ctx.data(|d| d.get_temp::<NewStep>(new_step_id())) else {
        return;
    };
    let mut done = false;
    let mut open = true;
    egui::Window::new("New step")
        .id(egui::Id::new("rundown_new_step_window"))
        .default_pos(state.at)
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .show(ctx, |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut state.name)
                    .id(new_step_field())
                    .desired_width(240.0),
            );
            if state.fresh {
                edit.request_focus();
                super::rename::select_all_in(ui.ctx(), new_step_field(), &state.name);
                state.fresh = false;
            }
            let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Keeps what is on now:").small().weak());
            for (uuid, keep) in &mut state.buses {
                let Some(bus) = data.buses.iter().find(|b| &b.bus.uuid == uuid) else {
                    continue;
                };
                let shows = bus
                    .bus
                    .transition
                    .as_ref()
                    .map_or(&bus.bus.program, |t| &t.to);
                let mut label = format!("{} — {}", bus.bus.name, input_name(data, shows));
                let on_air = on_air_bus(data, bus);
                if on_air {
                    label.push_str("  (the stream or recording takes it)");
                }
                let tick = ui.checkbox(keep, label);
                if on_air {
                    tick.on_hover_text(
                        "Ticked, taking the step changes what is streamed or recorded",
                    );
                }
            }
            ui.checkbox(&mut state.main, "Main, and every overlay on or off")
                .on_hover_text("Ticked, taking the step takes Main too");
            ui.add_space(4.0);
            let name = state.name.trim();
            let add = ui
                .add_enabled(!name.is_empty(), egui::Button::new("Add step"))
                .clicked();
            if (add || entered) && !name.is_empty() {
                actions.commands.push(EngineCommand::AddRundownStep {
                    name: name.to_string(),
                    buses: Some(
                        state
                            .buses
                            .iter()
                            .filter(|(_, keep)| *keep)
                            .map(|(uuid, _)| uuid.clone())
                            .collect(),
                    ),
                    main: state.main,
                });
                done = true;
            }
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                done = true;
            }
        });
    if done || !open {
        ctx.data_mut(|d| d.remove::<NewStep>(new_step_id()));
    } else {
        ctx.data_mut(|d| d.insert_temp(new_step_id(), state));
    }
}

// ── What a step sets ───────────────────────────────────────────────

/// The step whose settings are open, and its name as typed so far.
#[derive(Clone)]
struct Editing {
    uuid: String,
    /// The name as typed so far.
    name: String,
    /// The step's name the typing started from: renamed meanwhile from
    /// elsewhere, an untouched field takes the new name rather than putting
    /// the old one back when it is left.
    name_was: String,
    at: egui::Pos2,
}

fn editing_id() -> egui::Id {
    egui::Id::new("rundown_editing")
}

/// Room the step's window takes across, to open it clear of the list.
const EDITOR_WIDTH: f32 = 380.0;

fn open_editor(ctx: &egui::Context, step: &Step) {
    // Beside the panel — to its left, or its right where the panel is at
    // the left of the window — so the list, and the step's own row, which
    // says what it sets, stay in view.
    let pointer = ctx
        .pointer_latest_pos()
        .unwrap_or_else(|| ctx.content_rect().center());
    let rect_id = panel_rect_id(ctx);
    let x = match ctx.data(|d| d.get_temp::<egui::Rect>(rect_id)) {
        Some(panel) if panel.left() >= EDITOR_WIDTH + 8.0 => panel.left() - EDITOR_WIDTH - 8.0,
        Some(panel) => panel.right() + 8.0,
        None => pointer.x,
    };
    let state = Editing {
        uuid: step.uuid.clone(),
        name: step.name.clone(),
        name_was: step.name.clone(),
        at: egui::pos2(x, (pointer.y - 60.0).max(0.0)),
    };
    ctx.data_mut(|d| d.insert_temp(editing_id(), state));
}

/// The step whose settings are open, by UUID, for tests.
#[cfg(test)]
fn editing(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<Editing>(editing_id()))
        .map(|e| e.uuid)
}

/// A menu of choices for one thing a step sets: `shown` is what it is set to
/// now, `choices` each a label, whether it is the one set, and the command
/// that sets it.
fn choice(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    shown: &str,
    choices: Vec<(String, bool, EngineCommand)>,
    actions: &mut UIActions,
) {
    egui::ComboBox::from_id_salt(salt)
        .selected_text(shown)
        .width(190.0)
        .show_ui(ui, |ui| {
            for (label, current, cmd) in choices {
                if ui.selectable_label(current, label).clicked() && !current {
                    actions.commands.push(cmd);
                }
            }
        });
}

const LEAVE: &str = "Leave as it is";

fn render_editor(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let Some(mut state) = ctx.data(|d| d.get_temp::<Editing>(editing_id())) else {
        return;
    };
    // Gone meanwhile: nothing to edit.
    let Some(snapshot) = step_called(data, Some(&state.uuid)) else {
        ctx.data_mut(|d| d.remove::<Editing>(editing_id()));
        return;
    };
    let step = &snapshot.step;
    let mut open = true;
    // The first time, the window takes the size of what it has; from then
    // on it is sized by hand — wider for long names, or shorter, its
    // contents then scrolled — and opens again at that size.
    let fitted = ctx.data(|d| d.get_temp::<bool>(editor_fitted_id())).unwrap_or(false);
    egui::Window::new(format!("Step: {}", step.name))
        .id(editor_window_id())
        .default_pos(state.at)
        .default_size(egui::vec2(EDITOR_WIDTH, 0.0))
        .collapsible(false)
        .resizable(fitted)
        .open(&mut open)
        .show(ctx, |ui| {
            if fitted {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| edit_step(ui, &mut state, snapshot, data, actions));
            } else {
                edit_step(ui, &mut state, snapshot, data, actions);
            }
        });
    if !fitted {
        ctx.data_mut(|d| d.insert_temp(editor_fitted_id(), true));
    }
    if open {
        ctx.data_mut(|d| d.insert_temp(editing_id(), state));
    } else {
        ctx.data_mut(|d| d.remove::<Editing>(editing_id()));
    }
}

fn editor_window_id() -> egui::Id {
    egui::Id::new("rundown_editor_window")
}

fn editor_fitted_id() -> egui::Id {
    egui::Id::new("rundown_editor_fitted")
}

/// What the step's window has: its name, what it sets, and what it starts.
fn edit_step(
    ui: &mut egui::Ui,
    state: &mut Editing,
    snapshot: &RundownStepSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let step = &snapshot.step;
    let uuid = &step.uuid;
    ui.horizontal(|ui| {
        ui.label("Name");
        let field = egui::Id::new("rundown_editor_name");
        let typing = ui.ctx().memory(|m| m.has_focus(field));
        if step.name != state.name_was && !typing {
            state.name.clone_from(&step.name);
            state.name_was.clone_from(&step.name);
        }
        let edit = ui.add(
            egui::TextEdit::singleline(&mut state.name)
                .id(field)
                .desired_width(220.0),
        );
        let name = state.name.trim();
        if edit.lost_focus() && !name.is_empty() && name != step.name {
            actions.commands.push(EngineCommand::RenameRundownStep {
                uuid: uuid.clone(),
                name: name.to_string(),
            });
        }
    });
    if !snapshot.missing.is_empty() {
        ui.add(egui::Label::new(egui::RichText::new(why_not(snapshot)).color(WARN)).wrap());
    }
    ui.separator();
    edit_screens(ui, step, data, actions);
    ui.separator();
    edit_main(ui, step, data, actions);
    ui.separator();
    edit_plays(ui, step, data, actions);
    ui.separator();
    edit_then(ui, step, data, actions);
    edit_at(ui, step, actions);
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button("Update from what is on now")
            .on_hover_text(
                "The screens it sets, and Main if it sets it, become what is on now",
            )
            .clicked()
        {
            actions
                .commands
                .push(EngineCommand::CaptureRundownStep { uuid: uuid.clone() });
        }
        if ui
            .button("Remove step")
            .on_hover_text("Take this step out of the rundown — Undo brings it back")
            .clicked()
        {
            actions
                .commands
                .push(EngineCommand::RemoveRundownStep { uuid: uuid.clone() });
        }
    });
}

/// What each bus shows in the step.
fn edit_screens(ui: &mut egui::Ui, step: &Step, data: &UIData, actions: &mut UIActions) {
    ui.label(egui::RichText::new("Screens").strong());
    if data.buses.is_empty() {
        ui.label(egui::RichText::new("No buses yet").small().weak());
    }
    let set = |bus: &str, input: Option<BusInputRef>| EngineCommand::SetRundownScreen {
        uuid: step.uuid.clone(),
        bus_uuid: bus.to_string(),
        input,
    };
    egui::Grid::new("rundown_editor_screens")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            for bus in &data.buses {
                let bus_uuid = &bus.bus.uuid;
                let now = step.screens.iter().find(|s| &s.bus == bus_uuid).map(|s| &s.input);
                ui.label(&bus.bus.name);
                let mut choices = vec![(LEAVE.to_string(), now.is_none(), set(bus_uuid, None))];
                let mut inputs = vec![BusInputRef::Black, BusInputRef::Master];
                inputs.extend(data.channels.iter().map(|ch| BusInputRef::Channel(ch.uuid.clone())));
                // One layer alone, set from what was on: kept on the list.
                if let Some(deck @ BusInputRef::Deck(_)) = now {
                    inputs.push(deck.clone());
                }
                choices.extend(inputs.into_iter().map(|input| {
                    let label = input_name(data, &input);
                    let current = now == Some(&input);
                    (label, current, set(bus_uuid, Some(input)))
                }));
                let shown = now.map_or_else(|| LEAVE.to_string(), |input| input_name(data, input));
                choice(ui, ("rundown_screen", bus_uuid), &shown, choices, actions);
                ui.end_row();
            }
            // A bus that was removed: said, and taken out here.
            for screen in &step.screens {
                if bus_name(data, &screen.bus).is_some() {
                    continue;
                }
                ui.label(egui::RichText::new("⚠ a bus that is gone").color(WARN));
                if ui.small_button("Take out").clicked() {
                    actions.commands.push(set(&screen.bus, None));
                }
                ui.end_row();
            }
        });
}

/// What the step takes to Main, and the overlays it switches.
fn edit_main(ui: &mut egui::Ui, step: &Step, data: &UIData, actions: &mut UIActions) {
    ui.label(egui::RichText::new("Main").strong());
    let set_main = |main: Option<MainSource>| EngineCommand::SetRundownMain {
        uuid: step.uuid.clone(),
        main,
    };
    egui::Grid::new("rundown_editor_main")
        .num_columns(2)
        .spacing([10.0, 4.0])
        .show(ui, |ui| {
            ui.label("Takes");
            let now = step.main.as_ref();
            let mut sources = vec![MainSource::Black];
            sources.extend(
                data.channels
                    .iter()
                    .filter(|ch| !ch.overlay)
                    .map(|ch| MainSource::Channel(ch.uuid.clone())),
            );
            if let Some(deck @ MainSource::Deck(_)) = now {
                sources.push(deck.clone());
            }
            let mut choices = vec![(LEAVE.to_string(), now.is_none(), set_main(None))];
            choices.extend(sources.into_iter().map(|source| {
                let label = main_name(data, &source);
                let current = now == Some(&source);
                (label, current, set_main(Some(source)))
            }));
            let shown = now.map_or_else(|| LEAVE.to_string(), |source| main_name(data, source));
            choice(ui, "rundown_main", &shown, choices, actions);
            ui.end_row();

            for ch in data.channels.iter().filter(|ch| ch.overlay) {
                let now = step.overlays.iter().find(|o| o.channel == ch.uuid).map(|o| o.on);
                let set = |on: Option<bool>| EngineCommand::SetRundownOverlay {
                    uuid: step.uuid.clone(),
                    channel_uuid: ch.uuid.clone(),
                    on,
                };
                ui.label(ch.numbered());
                let choices = vec![
                    (LEAVE.to_string(), now.is_none(), set(None)),
                    ("On".to_string(), now == Some(true), set(Some(true))),
                    ("Off".to_string(), now == Some(false), set(Some(false))),
                ];
                let shown = match now {
                    None => LEAVE,
                    Some(true) => "On",
                    Some(false) => "Off",
                };
                choice(ui, ("rundown_overlay", &ch.uuid), shown, choices, actions);
                ui.end_row();
            }
            // An overlay the step switches that is one no longer, or is
            // gone: said, and taken out here — the step waits until it is.
            for overlay in &step.overlays {
                let channel = data.channels.iter().find(|ch| ch.uuid == overlay.channel);
                if channel.is_some_and(|ch| ch.overlay) {
                    continue;
                }
                let what = channel.map_or_else(
                    || "⚠ an overlay that is gone".to_string(),
                    |ch| format!("⚠ {} — not an overlay now", ch.numbered()),
                );
                ui.label(egui::RichText::new(what).color(WARN));
                if ui.small_button("Take out").clicked() {
                    actions.commands.push(EngineCommand::SetRundownOverlay {
                        uuid: step.uuid.clone(),
                        channel_uuid: overlay.channel.clone(),
                        on: None,
                    });
                }
                ui.end_row();
            }
        });
    ui.label(
        egui::RichText::new("Main is taken as its own Cut and Fade take it, with its own fade time.")
            .small(),
    );
}

/// Why a clip a step starts may not be heard, if so.
fn unheard(data: &UIData, deck: &str) -> Option<&'static str> {
    use crate::audio_mix::AudioSourceKind;
    let strip = data.audio_mix.sources.iter().find(
        |s| matches!(&s.kind, AudioSourceKind::Clip { deck_uuid } if deck_uuid == deck),
    )?;
    if strip.muted {
        Some("Its sound is off in the Audio Mixer")
    } else if strip.auto {
        Some("Heard only while its input is on Main or a screen — Audio Mixer ▸ Auto")
    } else {
        None
    }
}

/// What a step starts and stops of a deck, as the panel sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Runs {
    /// A clip or a playlist: a picture, a sound, or both.
    Media,
    Timer,
}

fn runs_of(data: &UIData, deck: &str) -> Option<Runs> {
    let deck = data
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .find(|d| d.uuid == deck)?;
    if deck.timer.is_some() {
        Some(Runs::Timer)
    } else if deck.playlist.is_some() || deck.video_playback.is_some() {
        Some(Runs::Media)
    } else {
        None
    }
}

/// How wide a name is in the step's window, and what is said under an
/// action: cut short, or wrapped, rather than pushing the window wider. A
/// name takes the width the window has beyond the rest of its row — more
/// than ×, an action and its notes ever take, so a name never makes the
/// window wider.
const PLAY_NAME_WIDTH: f32 = 150.0;
const PLAY_NAME_MAX: f32 = 600.0;
const PLAY_ROW_REST: f32 = 330.0;
const PLAY_NOTE_WIDTH: f32 = 230.0;

/// `text` in at most `width`, cut short with "…" — all of it on hover.
fn short_label(ui: &mut egui::Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_max_width(width);
            ui.add(egui::Label::new(text).truncate()).on_hover_text(text);
        },
    );
}

/// A small note under an action, wrapped to `PLAY_NOTE_WIDTH` — from the
/// top of its cell: a grid's row is centred, and a note begun at its middle
/// ran its second line under the row below.
fn play_note(ui: &mut egui::Ui, text: &str, colour: egui::Color32) {
    ui.vertical(|ui| {
        ui.set_max_width(PLAY_NOTE_WIDTH);
        ui.add(egui::Label::new(egui::RichText::new(text).small().color(colour)).wrap());
    });
}

/// The clips, playlists and timers the step starts and stops.
fn edit_plays(ui: &mut egui::Ui, step: &Step, data: &UIData, actions: &mut UIActions) {
    ui.label(egui::RichText::new("Starts and stops").strong());
    let set = |deck: &str, action: Option<PlayAction>| EngineCommand::SetRundownPlay {
        uuid: step.uuid.clone(),
        deck_uuid: deck.to_string(),
        action,
    };
    // Names take what a wider window has beyond the rest of a row.
    let name_width = (ui.available_width() - PLAY_ROW_REST).clamp(PLAY_NAME_WIDTH, PLAY_NAME_MAX);
    egui::Grid::new("rundown_editor_plays")
        .num_columns(3)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            for play in &step.plays {
                // Taken out first in the row: a long name never pushes it out
                // of a narrow window.
                if ui
                    .small_button("×")
                    .on_hover_text("Remove it from this step: the step leaves it alone")
                    .clicked()
                {
                    actions.commands.push(set(&play.deck, None));
                }
                let name = deck_name(data, &play.deck).unwrap_or_else(|| MISSING.into());
                short_label(ui, &name, name_width);
                let choices = [
                    PlayAction::Start,
                    PlayAction::Resume,
                    PlayAction::Pause,
                    PlayAction::Stop,
                ]
                .into_iter()
                .map(|action| {
                    (
                        action_name(action).to_string(),
                        play.action == action,
                        set(&play.deck, Some(action)),
                    )
                })
                .collect();
                choice(
                    ui,
                    ("rundown_play", &play.deck),
                    action_name(play.action),
                    choices,
                    actions,
                );
                ui.end_row();
                let playing = matches!(play.action, PlayAction::Start | PlayAction::Resume);
                match runs_of(data, &play.deck) {
                    Some(Runs::Media) if playing => {
                        ui.label("");
                        ui.label("");
                        let mut on = play.until_next;
                        if ui
                            .checkbox(&mut on, "Fade out at the next step")
                            .on_hover_text(
                                "When another step is taken — one that does not start or stop \
                                 it itself — its sound fades out over that step's fade, then it \
                                 stops, back at its beginning: music for this moment only",
                            )
                            .changed()
                        {
                            actions.commands.push(EngineCommand::SetRundownUntilNext {
                                uuid: step.uuid.clone(),
                                deck_uuid: play.deck.clone(),
                                on,
                            });
                        }
                        ui.end_row();
                    }
                    Some(Runs::Timer) if play.action == PlayAction::Start => {
                        ui.label("");
                        ui.label("Time for this step");
                        edit_timer_set(ui, step, play, data, actions);
                        ui.end_row();
                    }
                    _ => {}
                }
                if let Some(why) = unheard(data, &play.deck).filter(|_| playing) {
                    ui.label("");
                    ui.label("");
                    play_note(ui, why, WARN);
                    ui.end_row();
                }
            }
        });
    // What there is to start that the step does not start yet; each with
    // whether a step can start it.
    let on_timeline = |deck: &super::super::DeckUIInfo| {
        let chasing = deck
            .video_playback
            .as_ref()
            .is_some_and(|v| v.transport_sync.mode.is_chasing(data.transport.running));
        let has_lane = data
            .arrangement
            .as_ref()
            .is_some_and(|a| a.config.lane(&deck.uuid).is_some());
        // A timer counts whatever the timeline does with its picture.
        chasing || (has_lane && deck.timer.is_none())
    };
    let runnable: Vec<(String, &str, &str, bool)> = data
        .channels
        .iter()
        // Front first, as the lists of a channel's layers.
        .flat_map(|ch| ch.decks.iter().rev().map(move |d| (ch, d)))
        .filter(|(_, d)| {
            d.video_playback.is_some()
                || d.playlist.is_some()
                || d.timer
                    .as_ref()
                    .is_some_and(|t| t.settings.mode.has_transport())
        })
        .filter(|(_, d)| !step.plays.iter().any(|p| p.deck == d.uuid))
        .map(|(ch, d)| (ch.numbered(), d.name.as_str(), d.uuid.as_str(), !on_timeline(d)))
        .collect();
    ui.add_enabled_ui(!runnable.is_empty(), |ui| {
        ui.menu_button("+ Clip, playlist or timer", |ui| {
            for (channel, name, deck, startable) in &runnable {
                if ui
                    .add_enabled(*startable, egui::Button::new(format!("{channel} ▸ {name}")))
                    .on_disabled_hover_text(
                        "It follows the timeline, which starts and stops it",
                    )
                    .clicked()
                {
                    actions.commands.push(set(deck, Some(PlayAction::Start)));
                    ui.close();
                }
            }
        })
        .response
        .on_disabled_hover_text("The show has no clip, playlist or timer it does not start");
    });
}

/// How the step sets a timer it starts: as the timer is set, down from a
/// length of its own — this talk's — or up.
fn edit_timer_set(
    ui: &mut egui::Ui,
    step: &Step,
    play: &PlaySet,
    data: &UIData,
    actions: &mut UIActions,
) {
    let send = |timer: Option<TimerSet>| EngineCommand::SetRundownTimer {
        uuid: step.uuid.clone(),
        deck_uuid: play.deck.clone(),
        timer,
    };
    let settings = data
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .find(|d| d.uuid == play.deck)
        .and_then(|d| d.timer.as_ref())
        .map(|t| &t.settings);
    // A count down chosen here starts from the timer's own length.
    let length = match play.timer {
        Some(TimerSet::Down { secs }) => secs,
        _ => settings.map_or(15 * 60, |t| t.duration_secs),
    };
    let as_set = as_set(settings);
    let named = |set: Option<TimerSet>| match set {
        None => as_set.as_str(),
        Some(TimerSet::Down { .. }) => "Count down from",
        Some(TimerSet::Up) => "Count up",
    };
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(("rundown_timer_set", &play.deck))
            .selected_text(named(play.timer))
            .width(150.0)
            .show_ui(ui, |ui| {
                for set in [None, Some(TimerSet::Down { secs: length }), Some(TimerSet::Up)] {
                    let current = named(set) == named(play.timer);
                    if ui.selectable_label(current, named(set)).clicked() && !current {
                        actions.commands.push(send(set));
                    }
                }
            })
            .response
            .on_hover_text(
                "This step's own: down from a length of its own, or up — or as the timer is \
                 set in its input",
            );
        if let Some(TimerSet::Down { secs }) = play.timer {
            let mut now = secs;
            let drag = ui
                .add(
                    egui::DragValue::new(&mut now)
                        .range(1..=MAX_DURATION_SECS)
                        .speed(5.0)
                        .custom_formatter(|v, _| {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            span_text(v.max(0.0) as u64, TimeFormat::Auto)
                        })
                        .custom_parser(|text| parse_span(text).map(f64::from)),
                )
                .on_hover_text("Drag, or click and type: 15, 15:00 or 1:30:00");
            // A drag is one change to undo, not one a frame.
            if drag.dragged() {
                actions.session.gesture_active = true;
            }
            if drag.changed() && now != secs {
                actions.commands.push(send(Some(TimerSet::Down { secs: now })));
            }
        }
    });
}

/// What a timer does as it is set in its input, said short.
fn as_set(settings: Option<&crate::engine::value::timer::TimerSettings>) -> String {
    use crate::engine::value::timer::TimerMode;
    match settings {
        Some(t) if t.mode == TimerMode::Countdown => {
            format!("As set: {} down", span_text(u64::from(t.duration_secs), TimeFormat::Auto))
        }
        Some(t) if t.mode == TimerMode::CountUp => "As set: up".to_string(),
        _ => "As set".to_string(),
    }
}

/// Whether the step is taken by itself at a time of day, and at which.
fn edit_at(ui: &mut egui::Ui, step: &Step, actions: &mut UIActions) {
    let set = |at_secs: Option<u32>| EngineCommand::SetRundownAt {
        uuid: step.uuid.clone(),
        at_secs,
        always: None,
    };
    ui.horizontal(|ui| {
        let mut on = step.at_secs.is_some();
        if ui
            .checkbox(&mut on, "Start by itself at")
            .on_hover_text(
                "At that time of day, while it is the step GO takes next, it is taken as GO \
                 takes it — Hold stops it",
            )
            .changed()
        {
            // The next quarter of an hour, to change.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let day = crate::deck::timer::time_of_day().max(0.0) as u32;
            let quarter = (day / 900 + 1) * 900 % DAY_SECS;
            actions.commands.push(set(on.then_some(quarter)));
        }
        if let Some(at) = step.at_secs {
            let mut now = at;
            let drag = ui
                .add(
                    egui::DragValue::new(&mut now)
                        .range(0..=DAY_SECS - 1)
                        .speed(60.0)
                        .custom_formatter(|v, _| {
                            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                            target_text(v.max(0.0) as u32)
                        })
                        .custom_parser(|text| parse_time_of_day(text).map(f64::from)),
                )
                .on_hover_text("Drag, or click and type: 9:15, 14:05 or 1405");
            if drag.dragged() {
                actions.session.gesture_active = true;
            }
            if drag.changed() && now != at {
                actions.commands.push(set(Some(now)));
            }
        }
    });
    if let Some(at) = step.at_secs {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            let mut always = step.at_always;
            if ui
                .checkbox(&mut always, "Even if it is not next")
                .on_hover_text(
                    "At its time it is taken wherever the rundown is — the steps before it \
                     left out — and GO goes on from it. Hold stops it.",
                )
                .changed()
            {
                actions.commands.push(EngineCommand::SetRundownAt {
                    uuid: step.uuid.clone(),
                    at_secs: Some(at),
                    always: Some(always),
                });
            }
        });
    }
}

/// Whether the step goes on to the next by itself, and after how long.
fn edit_then(ui: &mut egui::Ui, step: &Step, data: &UIData, actions: &mut UIActions) {
    let set = |secs: Option<f32>, when_ends: Option<String>| EngineCommand::SetRundownThen {
        uuid: step.uuid.clone(),
        secs,
        when_ends,
    };
    // What it can go on at the end of: the clips and playlists it starts or
    // carries on.
    let endable: Vec<&str> = step
        .plays
        .iter()
        .filter(|p| step.plays_on(&p.deck) && runs_of(data, &p.deck) == Some(Runs::Media))
        .map(|p| p.deck.as_str())
        .collect();
    let ends_name = |deck: &str| {
        format!("when {} ends", deck_name(data, deck).unwrap_or_else(|| MISSING.into()))
    };
    ui.horizontal(|ui| {
        let mut on = step.then_secs.is_some() || step.then_ends.is_some();
        if ui
            .checkbox(&mut on, "Then go on to the next step")
            .on_hover_text("By itself, without GO — GO sooner goes on at once")
            .changed()
        {
            actions.commands.push(set(on.then_some(5.0), None));
        }
        if !on {
            return;
        }
        let shown = step.then_ends.as_deref().map_or_else(|| "after".to_string(), ends_name);
        egui::ComboBox::from_id_salt(("rundown_then", &step.uuid))
            .selected_text(shown)
            .width(if step.then_ends.is_some() { 260.0 } else { 70.0 })
            .show_ui(ui, |ui| {
                let after = step.then_ends.is_none();
                if ui.selectable_label(after, "after a time").clicked() && !after {
                    actions.commands.push(set(Some(5.0), None));
                }
                for deck in &endable {
                    let current = step.then_ends.as_deref() == Some(*deck);
                    if ui.selectable_label(current, ends_name(deck)).clicked() && !current {
                        actions.commands.push(set(None, Some((*deck).to_string())));
                    }
                }
            })
            .response
            .on_hover_text(
                "After a time from when the step is taken, or when a clip or playlist it starts \
                 has played to its end — the music, say, while a clip on screen loops",
            );
        if let Some(now) = step.then_secs {
            let mut secs = now;
            let drag = ui
                .add(
                    egui::DragValue::new(&mut secs)
                        .speed(0.1)
                        .range(0.1..=MAX_THEN_SECS)
                        .custom_formatter(|v, _| wait_text(v))
                        .custom_parser(parse_wait),
                )
                .on_hover_text("Drag, or click and type: 90, 1:30 or 4.5");
            if drag.changed() && (secs - now).abs() > f32::EPSILON {
                actions.commands.push(set(Some(secs), None));
            }
        }
    });
    if step.then_secs.is_some_and(|secs| secs < data.rundown.fade_secs) {
        ui.label(
            egui::RichText::new(format!(
                "Faded to, it goes on when its fade is done ({}).",
                seconds(data.rundown.fade_secs)
            ))
            .small(),
        );
    }
    if let Some(deck) = step.then_ends.as_deref() {
        if loops(data, deck) {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "⚠ It loops, so it never ends: set it to play once (its Loop setting), \
                         or pick what ends",
                    )
                    .small()
                    .color(WARN),
                )
                .wrap(),
            );
        }
    }
}

/// Whether the clip or playlist `deck` starts again at its end — and so
/// never ends.
fn loops(data: &UIData, deck: &str) -> bool {
    use crate::video::LoopMode;
    let Some(deck) = data.channels.iter().flat_map(|ch| &ch.decks).find(|d| d.uuid == deck) else {
        return false;
    };
    if let Some(list) = &deck.playlist {
        return list.settings.loop_list;
    }
    deck.video_playback
        .as_ref()
        .is_some_and(|v| matches!(v.loop_mode, LoopMode::Loop | LoopMode::PingPong))
}

/// A wait as it is shown: "4.5 s", or "1:30" from a minute.
fn wait_text(secs: f64) -> String {
    if secs >= 60.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let whole = secs.round() as u64;
        format!("{}:{:02}", whole / 60, whole % 60)
    } else {
        format!("{secs:.1} s")
    }
}

/// A wait as typed: seconds ("90", "4.5", "4.5 s") or minutes and seconds
/// ("1:30", "2:37").
fn parse_wait(text: &str) -> Option<f64> {
    let text = text.trim().trim_end_matches('s').trim();
    let secs = match text.split_once(':') {
        Some((m, s)) => m.trim().parse::<f64>().ok()? * 60.0 + s.trim().parse::<f64>().ok()?,
        None => text.parse::<f64>().ok()?,
    };
    (secs.is_finite() && secs > 0.0).then_some(secs)
}

/// The windows the panel opens: a new step, and what a step sets.
pub(super) fn render_windows(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    render_new_step(ctx, data, actions);
    render_editor(ctx, data, actions);
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{RundownSnapshot, RundownWaitSnapshot};
    use crate::rundown::{OverlaySet, PlaySet, ScreenSet};
    use egui_kittest::kittest::{NodeT, Queryable};

    /// The fixture's one bus, "TV", and its two channels.
    const TV: &str = "bus00001";
    const CH_A: &str = "ca000001";
    const CH_B: &str = "cb000001";

    fn step(uuid: &str, name: &str) -> RundownStepSnapshot {
        RundownStepSnapshot {
            step: Step {
                uuid: uuid.to_string(),
                ..Step::new(name)
            },
            missing: Vec::new(),
            fading: Vec::new(),
            shown: false,
        }
    }

    /// The fixture with steps A, B and C; nothing taken, so A is next.
    fn show() -> UIData {
        let mut data = UIData::test_fixture();
        data.rundown = RundownSnapshot {
            fade_secs: 1.0,
            steps: vec![step("s1", "A"), step("s2", "B"), step("s3", "C")],
            last: None,
            next: Some("s1".into()),
            wait: None,
            at_held: None,
            held_always: vec![],
        };
        data
    }

    type Harness<'a> = egui_kittest::Harness<'a, Vec<EngineCommand>>;

    /// The panel and its windows, the commands they send kept as state.
    fn harness(data: &UIData) -> Harness<'_> {
        harness_sized(data, egui::vec2(460.0, 620.0))
    }

    fn harness_sized(data: &UIData, size: egui::Vec2) -> Harness<'_> {
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_ui_state(
                |ui, sent: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_rundown(ui, data, &mut actions);
                    go_shortcut(ui, data, &mut actions);
                    render_windows(ui.ctx(), data, &mut actions);
                    sent.append(&mut actions.commands);
                },
                Vec::new(),
            )
    }

    fn settle(harness: &mut Harness<'_>) {
        harness.run_steps(4);
    }

    /// The buttons called `label`, top to bottom.
    fn buttons<'h>(harness: &'h Harness<'_>, label: &'h str) -> Vec<egui_kittest::Node<'h>> {
        let mut found: Vec<_> = harness
            .get_all_by_role_and_label(egui::accesskit::Role::Button, label)
            .collect();
        found.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        found
    }

    fn sent(harness: &Harness<'_>) -> String {
        format!("{:?}", harness.state())
    }

    /// GO names the step it takes and takes that one, faded; the Cut beside
    /// it cuts to it. Both say which step they were pressed for.
    #[test]
    fn go_takes_the_step_it_names() {
        let data = show();
        let mut harness = harness(&data);
        harness.get_by_label("GO ▸ A").click();
        settle(&mut harness);
        buttons(&harness, "Cut")[0].click();
        settle(&mut harness);
        assert_eq!(
            sent(&harness),
            r#"[RundownGo { fade: true, expect: Some("s1") }, RundownGo { fade: false, expect: Some("s1") }]"#
        );
        // The step taken last and the next are marked.
        let mut data = show();
        data.rundown.last = Some("s1".into());
        data.rundown.next = Some("s2".into());
        let harness = self::harness(&data);
        harness.get_by_label("GO ▸ B");
        let (now, next) = (harness.get_by_label(" NOW "), harness.get_by_label(" NEXT "));
        let (a, b) = (harness.get_by_label("A"), harness.get_by_label("B"));
        assert!((now.rect().center().y - a.rect().center().y).abs() < 8.0, "NOW is on A's row");
        assert!((next.rect().center().y - b.rect().center().y).abs() < 8.0, "NEXT is on B's");
    }

    /// Each step has a Cut and a Fade of its own, for a step out of order.
    #[test]
    fn a_steps_own_buttons_take_that_step() {
        let data = show();
        let mut harness = harness(&data);
        buttons(&harness, "Fade")[1].click();
        settle(&mut harness);
        // The first Cut is GO's; then one a step.
        buttons(&harness, "Cut")[3].click();
        settle(&mut harness);
        assert_eq!(
            sent(&harness),
            r#"[TakeRundownStep { uuid: "s2", fade: true }, TakeRundownStep { uuid: "s3", fade: false }]"#
        );
    }

    /// A step that cannot be taken says so on its row before it is pressed,
    /// and cannot be pressed; one whose screens are still fading can be cut
    /// to, not faded to. GO goes by the same.
    #[test]
    fn a_step_that_cannot_be_taken_says_why_and_cannot_be_pressed() {
        let mut data = show();
        data.rundown.steps[0].fading = vec!["TV".into()];
        data.rundown.steps[1].missing = vec!["TV's input".into()];
        let mut harness = harness(&data);
        let disabled = |harness: &Harness<'_>, label: &str| -> Vec<bool> {
            buttons(harness, label)
                .iter()
                .map(|b| b.accesskit_node().is_disabled())
                .collect()
        };
        assert_eq!(disabled(&harness, "Fade"), [true, true, false], "A fading, B missing");
        assert_eq!(disabled(&harness, "Cut"), [false, false, true, false], "GO's cut, then A, B, C");
        assert!(harness.get_by_label("GO ▸ A").accesskit_node().is_disabled());
        harness.get_by_label("⚠");
        // Pressed all the same, nothing is sent.
        harness.get_by_label("GO ▸ A").click();
        buttons(&harness, "Fade")[0].click();
        buttons(&harness, "Cut")[2].click();
        settle(&mut harness);
        assert_eq!(sent(&harness), "[]");
        drop(harness);

        // The next step with something missing: GO and its cut both wait.
        data.rundown.next = Some("s2".into());
        let harness = self::harness(&data);
        assert!(harness.get_by_label("GO ▸ B").accesskit_node().is_disabled());
        assert!(disabled(&harness, "Cut")[0]);
    }

    /// At the end GO takes nothing, and says it is the end; with no steps
    /// the panel says what a rundown is.
    #[test]
    fn at_the_end_go_takes_nothing() {
        let mut data = show();
        data.rundown.last = Some("s3".into());
        data.rundown.next = None;
        let mut harness = harness(&data);
        let go = harness.get_by_label("GO — the end");
        assert!(go.accesskit_node().is_disabled());
        go.click();
        settle(&mut harness);
        assert_eq!(sent(&harness), "[]");
        drop(harness);

        data.rundown = RundownSnapshot::default();
        let harness = self::harness(&data);
        assert!(harness.get_by_label("GO").accesskit_node().is_disabled());
        harness.get_by_label_contains("The show in order");
    }

    /// A step going on by itself shows where to and in how long, and Hold
    /// stops it.
    #[test]
    fn a_wait_shows_its_countdown_and_hold_stops_it() {
        let mut data = show();
        data.rundown.last = Some("s1".into());
        data.rundown.next = Some("s2".into());
        data.rundown.wait = Some(RundownWaitSnapshot {
            from: "s1".into(),
            to: "s2".into(),
            secs_left: 2.4,
            fade: true,
            until: None,
        });
        let mut harness = harness(&data);
        harness.get_by_label("Then B in 3 s");
        harness.get_by_label("Hold").click();
        settle(&mut harness);
        assert_eq!(sent(&harness), "[RundownHold]");
    }

    #[test]
    fn the_fade_time_is_picked_from_a_menu() {
        let data = show();
        let mut harness = harness(&data);
        harness.get_by_value("1 s").click();
        settle(&mut harness);
        harness.get_by_label("3 s").click();
        settle(&mut harness);
        assert_eq!(sent(&harness), "[SetRundownFade { secs: 3.0 }]");
    }

    /// A new step asks for a name — the next letter, ready to type over —
    /// and what to keep: each bus, ticked unless the stream takes it, and
    /// Main, unticked.
    #[test]
    fn a_new_step_asks_for_a_name_and_what_to_keep() {
        let mut data = show();
        let mut harness = harness(&data);
        harness.get_by_label("+ Step").click();
        settle(&mut harness);
        harness.get_by_label("TV — 1  Ch A");
        harness.get_by_label("Main, and every overlay on or off").click();
        settle(&mut harness);
        harness.get_by_label("Add step").click();
        settle(&mut harness);
        assert_eq!(
            sent(&harness),
            r#"[AddRundownStep { name: "D", buses: Some(["bus00001"]), main: true }]"#
        );
        assert!(harness.query_by_label("Add step").is_none(), "the window closed");
        drop(harness);

        // The bus the stream takes: there, unticked, and said why.
        data.stream_settings.options.source = OutputSource::BusProgram(TV.into());
        let mut harness = self::harness(&data);
        harness.get_by_label("+ Step").click();
        settle(&mut harness);
        harness.get_by_label("TV — 1  Ch A  (the stream or recording takes it)");
        harness.get_by_label("Add step").click();
        settle(&mut harness);
        assert_eq!(
            sent(&harness),
            r#"[AddRundownStep { name: "D", buses: Some([]), main: false }]"#
        );
    }

    /// A step's right-click menu changes the list; the first cannot move
    /// earlier.
    #[test]
    fn a_steps_menu_changes_the_list() {
        let data = show();
        let mut harness = harness(&data);
        for item in ["Update from what is on now", "GO takes this next", "Duplicate", "Move later", "Remove"] {
            harness.get_by_label("A").click_secondary();
            settle(&mut harness);
            assert!(harness.get_by_label("Move earlier").accesskit_node().is_disabled());
            harness.get_by_label(item).click();
            settle(&mut harness);
        }
        assert_eq!(
            sent(&harness),
            r#"[CaptureRundownStep { uuid: "s1" }, SetRundownNext { uuid: "s1" }, DuplicateRundownStep { uuid: "s1" }, MoveRundownStep { uuid: "s1", later: true }, RemoveRundownStep { uuid: "s1" }]"#
        );
        harness.get_by_label("C").click_secondary();
        settle(&mut harness);
        assert!(harness.get_by_label("Move later").accesskit_node().is_disabled());
        harness.get_by_label("Move earlier").click();
        settle(&mut harness);
        assert!(sent(&harness).ends_with(r#"MoveRundownStep { uuid: "s3", later: false }]"#));
    }

    /// What a step sets is read under its name.
    #[test]
    fn a_step_says_what_it_sets() {
        let mut data = show();
        data.channels[1].overlay = true;
        let step = &mut data.rundown.steps[0].step;
        step.screens = vec![
            ScreenSet { bus: TV.into(), input: BusInputRef::Channel(CH_B.into()) },
            ScreenSet { bus: "gone".into(), input: BusInputRef::Black },
        ];
        step.main = Some(MainSource::Channel(CH_A.into()));
        step.overlays = vec![OverlaySet { channel: CH_B.into(), on: false }];
        step.plays = vec![PlaySet::new("a0000001", PlayAction::Start)];
        step.then_secs = Some(5.0);
        let harness = harness(&data);
        harness.get_by_label(
            "TV: 2  Ch B  ·  ⚠ a bus that is gone: Black  ·  Main: 1  Ch A  ·  2  Ch B off  ·  \
             ▶ 1  test_generator_a  ·  then the next after 5 s",
        );
        assert_eq!(harness.query_all_by_label_contains("Sets nothing yet").count(), 2, "B and C");
    }

    /// The fixture's first layer made a count down, so there is something
    /// to start.
    fn with_timer(mut data: UIData) -> UIData {
        use crate::engine::value::timer::{TimerSettings, TimerSnapshot, TimerState};
        data.channels[0].decks[0].timer = Some(TimerSnapshot {
            text: "5:00".into(),
            state: TimerState::Normal,
            remaining: Some(300.0),
            elapsed: 0.0,
            running: false,
            panel: 7,
            settings: TimerSettings::default(),
        });
        data
    }

    fn open_editor_of(harness: &mut Harness<'_>, name: &str) {
        harness.get_by_label(name).click_secondary();
        settle(harness);
        harness.get_by_label("Edit…").click();
        settle(harness);
    }

    /// The step's window sets one thing with one choice: a screen, Main, an
    /// overlay, what it starts, and whether it goes on by itself.
    #[test]
    fn a_steps_window_sets_one_thing_with_each_choice() {
        let mut data = with_timer(show());
        data.channels[1].overlay = true;
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        assert_eq!(editing(&harness.ctx), Some("s2".into()));

        // Screens: TV. Every menu reads "Leave as it is" until set.
        let pick = |harness: &mut Harness<'_>, menu: usize, item: &str| {
            let mut menus: Vec<_> = harness.get_all_by_value(LEAVE).collect();
            menus.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
            menus[menu].click();
            settle(harness);
            harness.get_by_label(item).click();
            settle(harness);
        };
        pick(&mut harness, 0, "1  Ch A");
        pick(&mut harness, 1, "Black");
        pick(&mut harness, 2, "Off");
        harness.get_by_label("+ Clip, playlist or timer").click();
        settle(&mut harness);
        harness.get_by_label("1  Ch A ▸ test_generator_a").click();
        settle(&mut harness);
        harness.get_by_label("Then go on to the next step").click();
        settle(&mut harness);
        harness.get_by_label("Update from what is on now").click();
        settle(&mut harness);
        assert_eq!(
            sent(&harness),
            r#"[SetRundownScreen { uuid: "s2", bus_uuid: "bus00001", input: Some(Channel("ca000001")) }, SetRundownMain { uuid: "s2", main: Some(Black) }, SetRundownOverlay { uuid: "s2", channel_uuid: "cb000001", on: Some(false) }, SetRundownPlay { uuid: "s2", deck_uuid: "a0000001", action: Some(Start) }, SetRundownThen { uuid: "s2", secs: Some(5.0), when_ends: None }, CaptureRundownStep { uuid: "s2" }]"#
        );
        // An overlay is not offered for Main: Ch B is not on Main's list.
        let mut menus: Vec<_> = harness.get_all_by_value(LEAVE).collect();
        menus.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
        menus[1].click();
        settle(&mut harness);
        assert_eq!(harness.query_all_by_label("2  Ch B").count(), 1, "its own row only");
    }

    /// Set, each thing reads as it is set and is put back to "Leave as it
    /// is" with one choice; a bus that is gone is said and taken out.
    #[test]
    fn a_steps_window_shows_what_is_set_and_unsets_it() {
        let mut data = with_timer(show());
        let step = &mut data.rundown.steps[1].step;
        step.screens = vec![
            ScreenSet { bus: TV.into(), input: BusInputRef::Channel(CH_B.into()) },
            ScreenSet { bus: "gone".into(), input: BusInputRef::Black },
        ];
        step.plays = vec![PlaySet::new("a0000001", PlayAction::Start)];
        step.then_secs = Some(2.0);
        data.rundown.steps[1].missing = vec!["a bus that is gone".into()];
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        harness.get_by_label_contains("Cannot be taken — not in the show: a bus that is gone");

        harness.get_by_value("2  Ch B").click();
        settle(&mut harness);
        harness.get_by_label(LEAVE).click();
        settle(&mut harness);
        harness.get_by_label("Take out").click();
        settle(&mut harness);
        harness.get_by_value(action_name(PlayAction::Start)).click();
        settle(&mut harness);
        harness.get_by_label(action_name(PlayAction::Stop)).click();
        settle(&mut harness);
        harness.get_by_label("×").click();
        settle(&mut harness);
        harness.get_by_label("Then go on to the next step").click();
        settle(&mut harness);
        assert_eq!(
            sent(&harness),
            r#"[SetRundownScreen { uuid: "s2", bus_uuid: "bus00001", input: None }, SetRundownScreen { uuid: "s2", bus_uuid: "gone", input: None }, SetRundownPlay { uuid: "s2", deck_uuid: "a0000001", action: Some(Stop) }, SetRundownPlay { uuid: "s2", deck_uuid: "a0000001", action: None }, SetRundownThen { uuid: "s2", secs: None, when_ends: None }]"#
        );
    }

    /// An overlay a step switches that is an overlay no longer — or is gone
    /// from the show — keeps the step from being taken, so it is shown in
    /// the step's window and taken out there.
    #[test]
    fn an_overlay_that_is_one_no_longer_is_shown_and_taken_out() {
        let mut data = show();
        data.rundown.steps[1].step.overlays = vec![
            OverlaySet { channel: CH_B.into(), on: true },
            OverlaySet { channel: "gone".into(), on: false },
        ];
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        harness.get_by_label("⚠ 2  Ch B — not an overlay now");
        harness.get_by_label("⚠ an overlay that is gone");
        for _ in 0..2 {
            let mut out: Vec<_> = harness.get_all_by_label("Take out").collect();
            out.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
            out[0].click();
            settle(&mut harness);
        }
        let mut said: Vec<String> = harness.state().iter().map(|c| format!("{c:?}")).collect();
        said.dedup();
        assert_eq!(
            said[0],
            r#"SetRundownOverlay { uuid: "s2", channel_uuid: "cb000001", on: None }"#
        );
        drop(harness);
        // An overlay that is one still has its menu, and no warning.
        data.channels[1].overlay = true;
        data.rundown.steps[1].step.overlays.truncate(1);
        let mut harness = self::harness(&data);
        open_editor_of(&mut harness, "B");
        assert!(harness.query_by_label_contains("not an overlay now").is_none());
        harness.get_by_value("On");
    }

    /// A clip the timeline starts and stops is on the list of what a step
    /// could start, greyed: picking it would be refused.
    #[test]
    fn a_clip_on_the_timeline_cannot_be_picked_to_start() {
        use crate::video::{DeckTransportSync, TransportSyncMode};
        let mut data = with_timer(show());
        let playback = |mode| crate::usecases::ui::VideoPlaybackUI {
            playing: false,
            position: 0.0,
            duration: 10.0,
            speed: 1.0,
            effective_speed: 1.0,
            position_offset: 0.0,
            loop_mode: crate::video::LoopMode::default(),
            in_point: 0.0,
            out_point: 10.0,
            frame_rate: 25.0,
            transport_sync: DeckTransportSync {
                mode,
                ..DeckTransportSync::default()
            },
            hardware_decode: false,
            auto: crate::engine::value::video::AutoPlayback::default(),
            sound_only: false,
        };
        data.channels[0].decks[1].video_playback = Some(playback(TransportSyncMode::Always));
        data.channels[1].decks[0].video_playback = Some(playback(TransportSyncMode::Never));
        let free = data.channels[1].decks[0].name.clone();
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        harness.get_by_label("+ Clip, playlist or timer").click();
        settle(&mut harness);
        let chasing = harness.get_by_label("1  Ch A ▸ test_generator_b");
        assert!(chasing.accesskit_node().is_disabled());
        chasing.click();
        settle(&mut harness);
        assert_eq!(sent(&harness), "[]");
        // (The press closed the menu.)
        if harness.query_by_label(&format!("2  Ch B ▸ {free}")).is_none() {
            harness.get_by_label("+ Clip, playlist or timer").click();
            settle(&mut harness);
        }
        harness.get_by_label(&format!("2  Ch B ▸ {free}")).click();
        settle(&mut harness);
        assert!(sent(&harness).contains("action: Some(Start)"), "{}", sent(&harness));
        drop(harness);

        // With a row on the timeline, a clip is the timeline's whatever it
        // follows — and a timer is not: it counts all the same.
        let lane = |deck: &crate::usecases::ui::DeckUIInfo| {
            crate::arrangement::LaneConfig::new(deck.uuid.clone())
        };
        data.arrangement = Some(crate::engine::types::ArrangementSnapshot {
            config: crate::arrangement::ArrangementConfig {
                lanes: vec![
                    lane(&data.channels[1].decks[0]),
                    lane(&data.channels[0].decks[0]),
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut harness = self::harness(&data);
        open_editor_of(&mut harness, "B");
        harness.get_by_label("+ Clip, playlist or timer").click();
        settle(&mut harness);
        let with_a_row = format!("2  Ch B ▸ {free}");
        let clip = harness.get_by_label(&with_a_row);
        assert!(clip.accesskit_node().is_disabled(), "a clip with a row");
        let timer = harness.get_by_label("1  Ch A ▸ test_generator_a");
        assert!(!timer.accesskit_node().is_disabled(), "a timer with a row");
        drop(harness);

        // A playlist with a row is the timeline's as a clip is.
        data.channels[1].decks[0].video_playback = None;
        data.channels[1].decks[0].playlist =
            Some(crate::engine::value::playlist::PlaylistSnapshot {
                items: vec![],
                settings: crate::engine::value::playlist::PlaylistSettings::default(),
                playing: false,
                shown: None,
                coming: None,
                next_ready: None,
                failed: vec![],
                remaining: None,
            });
        let mut harness = self::harness(&data);
        open_editor_of(&mut harness, "B");
        harness.get_by_label("+ Clip, playlist or timer").click();
        settle(&mut harness);
        let list = harness.get_by_label(&with_a_row);
        assert!(list.accesskit_node().is_disabled(), "a playlist with a row");
    }

    /// A clip a step starts that would not be heard says so: off in the
    /// Audio Mixer, or on Audio Auto, which follows the picture.
    #[test]
    fn a_clip_that_would_not_be_heard_says_so() {
        use crate::audio_mix::{AudioSourceKind, AudioSourceSnapshot};
        let strip = |muted: bool, auto: bool| AudioSourceSnapshot {
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            uuid: "strip001".into(),
            name: "Sting".into(),
            kind: AudioSourceKind::Clip { deck_uuid: "a0000001".into() },
            gain_db: 0.0,
            muted,
            sends: vec![],
            auto,
            follow: None,
            balance: 0.0,
            delay_ms: 0.0,
            on_air: 0.0,
            gate: 1.0,
            solo: false,
            live: true,
            meter_db: [-96.0, -96.0],
            signal_db: [-96.0, -96.0],
            peak_db: [-96.0;2],
            refused: None,
        };
        let mut data = show();
        data.rundown.steps[1].step.plays =
            vec![PlaySet::new("a0000001", PlayAction::Start)];
        for (muted, auto, said) in [
            (true, true, Some("Its sound is off in the Audio Mixer")),
            (false, true, Some("Heard only while its input is on Main or a screen")),
            (false, false, None),
        ] {
            data.audio_mix.sources = vec![strip(muted, auto)];
            let mut harness = harness(&data);
            open_editor_of(&mut harness, "B");
            match said {
                Some(text) => {
                    harness.get_by_label_contains(text);
                }
                None => assert!(harness.query_by_label_contains("Audio Mixer").is_none()),
            }
        }
        // Stopped by the step, there is nothing to hear and nothing said.
        data.audio_mix.sources = vec![strip(true, true)];
        data.rundown.steps[1].step.plays[0].action = PlayAction::Stop;
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        assert!(harness.query_by_label_contains("Audio Mixer").is_none());
    }

    /// The name is changed in the step's window, kept when the field is
    /// left; a blank one is not sent.
    #[test]
    fn a_step_is_renamed_in_its_window() {
        let data = show();
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        let field = harness.get_by_role(egui::accesskit::Role::TextInput);
        field.focus();
        settle(&mut harness);
        harness.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
        harness.get_by_role(egui::accesskit::Role::TextInput).type_text("B Walk-on");
        settle(&mut harness);
        harness.key_press(egui::Key::Tab);
        settle(&mut harness);
        // Said once a pass of the frame the field is left on; the window
        // keeps the last pass's (see `runner::run_ui`).
        let mut renames = harness.state().iter().map(|c| format!("{c:?}")).collect::<Vec<_>>();
        renames.dedup();
        assert_eq!(renames, [r#"RenameRundownStep { uuid: "s2", name: "B Walk-on" }"#]);
    }

    /// Renamed from elsewhere while its window is open, a step's name field
    /// shows the new name, and leaving it puts nothing back.
    #[test]
    fn a_name_changed_elsewhere_is_not_put_back() {
        let mut data = show();
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        let editing = harness.ctx.data(|d| d.get_temp::<Editing>(editing_id())).expect("open");
        drop(harness);
        data.rundown.steps[1].step.name = "B Walk-on".into();
        let mut harness = self::harness(&data);
        harness.ctx.data_mut(|d| d.insert_temp(editing_id(), editing));
        settle(&mut harness);
        let field = harness.get_by_role(egui::accesskit::Role::TextInput);
        assert_eq!(field.accesskit_node().value().as_deref(), Some("B Walk-on"));
        field.focus();
        settle(&mut harness);
        harness.key_press(egui::Key::Tab);
        settle(&mut harness);
        assert!(!sent(&harness).contains("RenameRundownStep"), "{}", sent(&harness));
    }

    /// The step the show is on, made the one GO takes next, shows both.
    #[test]
    fn a_step_both_now_and_next_shows_both() {
        let mut data = show();
        data.rundown.last = Some("s2".into());
        data.rundown.next = Some("s2".into());
        let harness = harness(&data);
        let (now, next, b) = (
            harness.get_by_label(" NOW "),
            harness.get_by_label(" NEXT "),
            harness.get_by_label("B"),
        );
        for tag in [now, next] {
            assert!((tag.rect().center().y - b.rect().center().y).abs() < 8.0, "on B's row");
        }
    }

    /// The key learned for GO takes the next step with a fade — but not
    /// while a name is being typed; and in keyboard-learn mode the GO button
    /// is picked as the key's target rather than pressed.
    #[test]
    fn the_go_key_takes_the_next_step() {
        use crate::keymap::{ActionId, KeyCombo, KeyTarget};
        let mut data = show();
        data.keymap_bindings.insert(
            KeyCombo { key: "G".into(), command: false, shift: false, alt: false },
            KeyTarget::Action(ActionId::RundownGo),
        );
        let mut harness = harness(&data);
        harness.key_press(egui::Key::G);
        settle(&mut harness);
        assert_eq!(sent(&harness), r#"[RundownGo { fade: true, expect: Some("s1") }]"#);

        // Typing a step's name, G is a letter.
        harness.get_by_label("+ Step").click();
        settle(&mut harness);
        harness.key_press(egui::Key::G);
        settle(&mut harness);
        assert_eq!(sent(&harness), r#"[RundownGo { fade: true, expect: Some("s1") }]"#);
        drop(harness);

        data.keyboard_learn_active = true;
        let mut picked = None;
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(460.0, 620.0))
                .build_ui(|ui| {
                    let mut actions = UIActions::new();
                    render_rundown(ui, &data, &mut actions);
                    go_shortcut(ui, &data, &mut actions);
                    assert!(actions.commands.is_empty(), "nothing is taken while learning");
                    if let Some(target) = actions.session.keyboard_learn_select.take() {
                        picked = Some(target.to_string());
                    }
                });
            harness.get_by_label("GO ▸ A").click();
            harness.key_press(egui::Key::G);
            harness.run_steps(4);
        }
        assert_eq!(picked.as_deref(), Some(GO_ACTION));
    }
    /// A clip's playback as the step's window reads it: sound only, music.
    fn music() -> crate::usecases::ui::VideoPlaybackUI {
        crate::usecases::ui::VideoPlaybackUI {
            playing: false,
            position: 0.0,
            duration: 180.0,
            speed: 1.0,
            effective_speed: 1.0,
            position_offset: 0.0,
            loop_mode: crate::video::LoopMode::default(),
            in_point: 0.0,
            out_point: 180.0,
            frame_rate: 25.0,
            transport_sync: crate::video::DeckTransportSync::default(),
            hardware_decode: false,
            auto: crate::engine::value::video::AutoPlayback::default(),
            sound_only: true,
        }
    }

    /// What a step starts is taken out with × at the head of its row — a
    /// long name cut short after it, never pushing it out of a narrow
    /// window; music is faded out at the next step; a timer counts down
    /// from a length of the step's own, or up; and the step is started by
    /// itself at a time of day.
    #[test]
    fn a_steps_window_sets_how_it_plays_and_when() {
        let mut data = with_timer(show());
        let long = "Nida 2026-10-26 10-10-13 — the walk-in music for the morning.mp3";
        data.channels[1].decks[0].video_playback = Some(music());
        data.channels[1].decks[0].name = long.into();
        let clip = data.channels[1].decks[0].uuid.clone();
        let length = data.channels[0].decks[0]
            .timer
            .as_ref()
            .map(|t| t.settings.duration_secs)
            .unwrap();
        data.rundown.steps[1].step.plays = vec![
            PlaySet::new(clip.clone(), PlayAction::Start),
            PlaySet::new("a0000001", PlayAction::Start),
        ];
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        let bin = harness.get_all_by_label("×").next().unwrap().rect();
        let name = harness.get_by_label(&format!("2  {long}")).rect();
        assert!(bin.right() <= name.left(), "× {bin:?} before the name {name:?}");
        assert!(name.width() <= PLAY_NAME_WIDTH + 0.5, "cut short: {name:?}");

        harness.get_by_label("Fade out at the next step").click();
        settle(&mut harness);
        // What the timer's own setting is, said; this step's own chosen.
        harness.get_by_label("Time for this step");
        let as_set = format!("As set: {} down", span_text(u64::from(length), TimeFormat::Auto));
        harness.get_by_value(&as_set).click();
        settle(&mut harness);
        harness.get_by_label("Count down from").click();
        settle(&mut harness);
        harness.get_by_label("Start by itself at").click();
        settle(&mut harness);
        let sent = sent(&harness);
        assert!(
            sent.contains(&format!(
                r#"SetRundownUntilNext {{ uuid: "s2", deck_uuid: {clip:?}, on: true }}"#
            )),
            "{sent}"
        );
        assert!(
            sent.contains(&format!(
                r#"SetRundownTimer {{ uuid: "s2", deck_uuid: "a0000001", timer: Some(Down {{ secs: {length} }}) }}"#
            )),
            "{sent}"
        );
        assert!(sent.contains(r#"SetRundownAt { uuid: "s2", at_secs: Some("#), "{sent}");
    }

    /// A step started by itself at a time says so on its row; next and held,
    /// the panel says it waits for GO — and offers its time again.
    #[test]
    fn a_step_at_a_time_of_day_says_so_and_a_held_one_waits_for_go() {
        let mut data = show();
        data.rundown.steps[1].step.at_secs = Some(9 * 3600 + 15 * 60);
        data.rundown.last = Some("s1".into());
        data.rundown.next = Some("s2".into());
        data.rundown.at_held = Some("s2".into());
        let mut harness = harness(&data);
        settle(&mut harness);
        harness.get_by_label("at 09:15");
        harness.get_by_label("B at 09:15 — held: GO takes it");
        harness.get_by_label("By its time again").click();
        settle(&mut harness);
        assert_eq!(sent(&harness), r#"[SetRundownNext { uuid: "s2" }]"#);
    }
    /// The step's window: made wider by its corner, a long name shows more
    /// of itself and the window stays as wide as it was made; made shorter,
    /// it stays short, its contents scrolled.
    #[test]
    fn a_steps_window_is_sized_by_hand() {
        let mut data = with_timer(show());
        let long = "Nida 2026-10-26 10-10-13 — the walk-in music for the morning.mp3";
        data.channels[1].decks[0].video_playback = Some(music());
        data.channels[1].decks[0].name = long.into();
        let clip = data.channels[1].decks[0].uuid.clone();
        data.rundown.steps[1].step.plays = vec![
            PlaySet::new(clip, PlayAction::Start),
            PlaySet::new("a0000001", PlayAction::Start),
        ];
        let mut harness = harness_sized(&data, egui::vec2(1400.0, 1000.0));
        open_editor_of(&mut harness, "B");
        let window = |h: &Harness<'_>| h.ctx.memory(|m| m.area_rect(editor_window_id())).unwrap();
        let before = window(&harness);
        // At first, as tall as what it has: no empty space under its last row.
        let last = harness.get_by_label("Remove step").rect();
        assert!(before.bottom() - last.bottom() < 24.0, "fits: {last:?} in {before:?}");
        let narrow = harness.get_by_label(&format!("2  {long}")).rect().width();
        assert!(narrow <= PLAY_NAME_WIDTH + 0.5, "cut short at first: {narrow}");

        let drag = |h: &mut Harness<'_>, by: egui::Vec2| {
            let corner = window(h).right_bottom() - egui::vec2(3.0, 3.0);
            h.hover_at(corner);
            h.run_steps(2);
            h.drag_at(corner);
            h.run_steps(2);
            for i in 1..=8 {
                #[allow(clippy::cast_precision_loss)]
                h.hover_at(corner + by * (i as f32 / 8.0));
                h.run_steps(1);
            }
            h.drop_at(corner + by);
            h.run_steps(4);
        };
        drag(&mut harness, egui::vec2(400.0, 0.0));
        let wider = window(&harness);
        assert!(wider.width() >= before.width() + 300.0, "wider: {before:?} → {wider:?}");
        let name = harness.get_by_label(&format!("2  {long}")).rect().width();
        assert!(name > narrow + 200.0, "the name shows more: {narrow} → {name}");
        harness.run_steps(10);
        let later = window(&harness);
        assert!((later.width() - wider.width()).abs() < 1.0, "it stays: {wider:?} → {later:?}");

        drag(&mut harness, egui::vec2(0.0, -300.0));
        let shorter = window(&harness);
        assert!(shorter.height() <= wider.height() - 200.0, "shorter: {wider:?} → {shorter:?}");
        harness.run_steps(10);
        let later = window(&harness);
        assert!((later.height() - shorter.height()).abs() < 1.0, "it stays: {shorter:?} → {later:?}");

        // Closed and opened again: at the size it was made, though what it
        // has is taller.
        harness.ctx.data_mut(|d| d.remove::<Editing>(editing_id()));
        settle(&mut harness);
        open_editor_of(&mut harness, "B");
        let again = window(&harness);
        assert!(
            (again.size() - shorter.size()).length() < 1.0,
            "as it was made: {shorter:?} → {again:?}"
        );
    }

    /// A note under what a step starts is all above the next row — its
    /// second line once ran under the timer's row.
    #[test]
    fn a_note_under_a_clip_is_above_the_next_row() {
        use crate::audio_mix::{AudioSourceKind, AudioSourceSnapshot};
        let mut data = with_timer(show());
        data.channels[1].decks[0].video_playback = Some(music());
        let clip = data.channels[1].decks[0].uuid.clone();
        data.audio_mix.sources = vec![AudioSourceSnapshot {
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            uuid: "strip001".into(),
            name: "Music".into(),
            kind: AudioSourceKind::Clip { deck_uuid: clip.clone() },
            gain_db: 0.0,
            muted: false,
            sends: vec![],
            auto: true,
            follow: None,
            balance: 0.0,
            delay_ms: 0.0,
            on_air: 0.0,
            gate: 1.0,
            solo: false,
            live: true,
            meter_db: [-96.0, -96.0],
            signal_db: [-96.0, -96.0],
            peak_db: [-96.0; 2],
            refused: None,
        }];
        data.rundown.steps[1].step.plays = vec![
            PlaySet::new(clip, PlayAction::Start),
            PlaySet::new("a0000001", PlayAction::Start),
        ];
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        let note = harness
            .get_by_label_contains("Heard only while its input is on Main or a screen")
            .rect();
        let fade = harness.get_by_label("Fade out at the next step").rect();
        let mut removes: Vec<_> = harness.get_all_by_label("×").map(|n| n.rect()).collect();
        removes.sort_by(|a, b| a.top().total_cmp(&b.top()));
        let next_row = removes[1];
        assert!(note.height() > 15.0, "two lines: {note:?}");
        assert!(note.top() >= fade.bottom(), "under the fade {fade:?}: {note:?}");
        assert!(note.bottom() <= next_row.top(), "above the next row {next_row:?}: {note:?}");
    }

    /// The step's window removes the step itself.
    #[test]
    fn a_steps_window_removes_the_step() {
        let data = show();
        let mut harness = harness(&data);
        open_editor_of(&mut harness, "B");
        harness.get_by_label("Remove step").click();
        settle(&mut harness);
        assert!(sent(&harness).contains(r#"RemoveRundownStep { uuid: "s2" }"#), "{}", sent(&harness));
    }
    /// A step at a time of day can be taken at it even when not next: a tick
    /// in its window, there only with a time; its row says so; with another
    /// step next, the panel counts down to it and Hold holds it.
    #[test]
    fn a_step_at_its_time_even_when_not_next_is_set_said_and_counted_down() {
        let mut data = show();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let now = crate::deck::timer::time_of_day().max(0.0) as u32;
        let soon = now + 600;
        data.rundown.steps[2].step.at_secs = Some(soon % DAY_SECS);
        data.rundown.last = Some("s1".into());
        data.rundown.next = Some("s2".into());
        {
            let mut harness = harness(&data);
            open_editor_of(&mut harness, "C");
            harness.get_by_label("Even if it is not next").click();
            settle(&mut harness);
            let want = format!(
                r#"SetRundownAt {{ uuid: "s3", at_secs: Some({}), always: Some(true) }}"#,
                soon % DAY_SECS
            );
            assert!(sent(&harness).contains(&want), "{}", sent(&harness));
            let timeless = harness_of_step_without_time(&data);
            assert!(timeless.query_by_label("Even if it is not next").is_none(), "only with a time");
        }

        data.rundown.steps[2].step.at_always = true;
        let mut harness = harness(&data);
        harness.get_by_label_contains(", even if not next");
        // Ten minutes from now, unless that is past midnight.
        if soon < DAY_SECS {
            harness.get_by_label_contains("C at ");
            harness.get_by_label("Hold").click();
            settle(&mut harness);
            assert!(sent(&harness).contains("RundownHold"), "{}", sent(&harness));
        }
    }

    fn harness_of_step_without_time(data: &UIData) -> Harness<'_> {
        let mut harness = harness(data);
        open_editor_of(&mut harness, "B");
        harness
    }
    /// One taken even when not next is counted down while a wait runs too;
    /// held, it says it is not taken by its time — GO takes the next.
    #[test]
    fn a_step_even_when_not_next_counts_down_in_a_wait_and_says_it_is_held() {
        use crate::engine::RundownWaitSnapshot;
        let mut data = show();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let now = crate::deck::timer::time_of_day().max(0.0) as u32;
        let soon = now + 600;
        if soon >= DAY_SECS {
            return;
        }
        data.rundown.steps[2].step.at_secs = Some(soon);
        data.rundown.steps[2].step.at_always = true;
        data.rundown.last = Some("s1".into());
        data.rundown.next = Some("s2".into());
        data.rundown.wait = Some(RundownWaitSnapshot {
            from: "s1".into(),
            to: "s2".into(),
            secs_left: 60.0,
            fade: true,
            until: None,
        });
        {
            let harness = harness(&data);
            harness.get_by_label_contains("Then B in");
            harness.get_by_label_contains("C at ");
        }
        data.rundown.wait = None;
        data.rundown.held_always = vec!["s3".into()];
        let harness = harness(&data);
        harness.get_by_label_contains("— held: not taken by its time");
    }
    /// A step goes on when what it plays ends: picked among the clips it
    /// starts; a looping one is said never to end; its row and the wait say
    /// what it waits for.
    #[test]
    fn a_step_goes_on_when_a_clip_it_starts_ends() {
        use crate::engine::RundownWaitSnapshot;
        let mut data = show();
        let mut clip = music();
        clip.loop_mode = crate::video::LoopMode::OneShot;
        data.channels[1].decks[0].video_playback = Some(clip);
        data.channels[1].decks[0].name = "music.mp3".into();
        let deck = data.channels[1].decks[0].uuid.clone();
        data.rundown.steps[1].step.plays = vec![PlaySet::new(deck.clone(), PlayAction::Start)];
        data.rundown.steps[1].step.then_secs = Some(5.0);
        let ends = "when 2  music.mp3 ends";
        {
            let mut harness = harness(&data);
            open_editor_of(&mut harness, "B");
            harness.get_by_value("after").click();
            settle(&mut harness);
            harness.get_by_label(ends).click();
            settle(&mut harness);
            let want = format!(
                r#"SetRundownThen {{ uuid: "s2", secs: None, when_ends: Some({deck:?}) }}"#
            );
            assert!(sent(&harness).contains(&want), "{}", sent(&harness));
            assert!(harness.query_by_label_contains("It loops").is_none(), "plays once");
        }
        // Set so, and looping: said. Its row says what it waits for.
        data.rundown.steps[1].step.then_secs = None;
        data.rundown.steps[1].step.then_ends = Some(deck.clone());
        if let Some(v) = data.channels[1].decks[0].video_playback.as_mut() {
            v.loop_mode = crate::video::LoopMode::Loop;
        }
        {
            let mut harness = harness(&data);
            harness.get_by_label_contains("then the next when 2  music.mp3 ends");
            open_editor_of(&mut harness, "B");
            harness.get_by_value(ends);
            harness.get_by_label_contains("It loops, so it never ends");
        }
        // Waiting: how long it has left.
        data.rundown.last = Some("s2".into());
        data.rundown.next = Some("s3".into());
        data.rundown.wait = Some(RundownWaitSnapshot {
            from: "s2".into(),
            to: "s3".into(),
            secs_left: 83.0,
            fade: true,
            until: Some(deck),
        });
        let harness = harness(&data);
        harness.get_by_label_contains("Then C when 2  music.mp3 ends — 1:23 left");
    }

    /// A wait is typed in seconds or in minutes and seconds, and shown so.
    #[test]
    fn a_wait_is_typed_as_seconds_or_minutes_and_seconds() {
        assert_eq!(parse_wait("90"), Some(90.0));
        assert_eq!(parse_wait("1:30"), Some(90.0));
        assert_eq!(parse_wait(" 2:37 "), Some(157.0));
        assert_eq!(parse_wait("4.5 s"), Some(4.5));
        assert_eq!(parse_wait("0"), None);
        assert_eq!(parse_wait("soon"), None);
        assert_eq!(wait_text(90.0), "1:30");
        assert_eq!(wait_text(157.0), "2:37");
        assert_eq!(wait_text(4.5), "4.5 s");
    }
}

