//! Timers in the UI: a control panel for each — what it reads, start,
//! pause, reset, time put on or taken off, its length or the time it counts
//! to — which lives in the dock like any other panel, so it can sit beside
//! whatever the operator likes; a settings window for what it counts and
//! how it looks; and start/pause on the picture of every channel holding
//! one.

use super::super::{ChannelUIInfo, DeckUIInfo, UIActions, UIData};
use crate::engine::value::timer::{
    parse_span, parse_time_of_day, span_text, target_text, Anchor, AtZero, TimeFormat, TimerAction,
    TimerMode, TimerSettings, TimerSnapshot, TimerState, MAX_DURATION_SECS, MAX_OUTLINE, MAX_SIZE,
    MIN_SIZE,
};
use crate::engine::EngineCommand;
use crate::usecases::ui::screen_layout::Pane;

const RUNNING_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 150, 60);
const WARN_AMBER: egui::Color32 = egui::Color32::from_rgb(240, 180, 20);
const OVER_RED: egui::Color32 = egui::Color32::from_rgb(230, 70, 70);
/// Lengths a count down is set to in one click, in minutes.
const PRESET_MINUTES: [u32; 7] = [5, 10, 15, 20, 30, 45, 60];

/// A channel's timer layer, if it has one.
pub(super) fn of_channel(ch: &ChannelUIInfo) -> Option<(&DeckUIInfo, &TimerSnapshot)> {
    ch.decks
        .iter()
        .find_map(|d| d.timer.as_ref().map(|timer| (d, timer)))
}

/// Every timer in the show, with the channel it is in.
fn timers(data: &UIData) -> impl Iterator<Item = (&ChannelUIInfo, &DeckUIInfo, &TimerSnapshot)> {
    data.channels.iter().flat_map(|ch| {
        ch.decks
            .iter()
            .filter_map(move |d| d.timer.as_ref().map(|timer| (ch, d, timer)))
    })
}

/// The control panels of the timers in the show, in channel order.
pub(super) fn panels(data: &UIData) -> Vec<u64> {
    timers(data).map(|(_, _, timer)| timer.panel).collect()
}

/// The timer whose control panel is `panel`.
fn by_panel(data: &UIData, panel: u64) -> Option<(&ChannelUIInfo, &DeckUIInfo, &TimerSnapshot)> {
    timers(data).find(|(_, _, timer)| timer.panel == panel)
}

fn by_deck<'a>(
    data: &'a UIData,
    deck_uuid: &str,
) -> Option<(&'a ChannelUIInfo, &'a DeckUIInfo, &'a TimerSnapshot)> {
    timers(data).find(|(_, d, _)| d.uuid == deck_uuid)
}

/// The name on a timer's panel: its channel's, and its layer's where the
/// channel shows more than the timer.
pub(super) fn panel_title(data: &UIData, panel: u64) -> String {
    match by_panel(data, panel) {
        Some((ch, deck, _)) if ch.decks.len() > 1 => format!("⏱ {} · {}", ch.name, deck.name),
        Some((ch, _, _)) => format!("⏱ {}", ch.name),
        None => "⏱ Timer (gone)".to_string(),
    }
}

fn control(actions: &mut UIActions, deck_uuid: &str, action: TimerAction) {
    actions.commands.push(EngineCommand::ControlTimer {
        deck_uuid: deck_uuid.to_string(),
        action,
    });
}

fn set_settings(actions: &mut UIActions, deck_uuid: &str, settings: TimerSettings) {
    actions.commands.push(EngineCommand::SetTimerSettings {
        deck_uuid: deck_uuid.to_string(),
        settings,
    });
}

/// The timer's settings as an edit already made this frame left them — a
/// length preset clicked, a number stepped in the window — else as they
/// are. Each send is the whole settings, so a control sending its own
/// change must start from these, or it puts back what the other changed.
fn shown_settings(actions: &UIActions, deck_uuid: &str, timer: &TimerSnapshot) -> TimerSettings {
    actions
        .commands
        .iter()
        .rev()
        .find_map(|command| match command {
            EngineCommand::SetTimerSettings {
                deck_uuid: uuid,
                settings,
            } if uuid == deck_uuid => Some(settings.clone()),
            _ => None,
        })
        .unwrap_or_else(|| timer.settings.clone())
}

/// The colour the UI shows a reading in.
fn state_color(ui: &egui::Ui, timer: &TimerSnapshot) -> egui::Color32 {
    match timer.state {
        TimerState::Normal if timer.running => ui.visuals().strong_text_color(),
        TimerState::Normal => ui.visuals().text_color(),
        TimerState::Warn => WARN_AMBER,
        TimerState::Over => OVER_RED,
    }
}

// ── Buttons ─────────────────────────────────────────────────────────

/// ▶ for a paused count, ⏸ (green) for a running one. Nothing for the time
/// of day or a count to a time, which run by the clock.
pub(super) fn start_button(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    timer: &TimerSnapshot,
    size: egui::Vec2,
    actions: &mut UIActions,
) {
    if !timer.settings.mode.has_transport() {
        return;
    }
    let (text, hover, action) = if timer.running {
        ("⏸", "Pause the timer", TimerAction::Pause)
    } else {
        ("▶", "Start the timer", TimerAction::Start)
    };
    let button = egui::Button::new(
        egui::RichText::new(text)
            .size(size.y * 0.6)
            .strong()
            .color(egui::Color32::WHITE),
    )
    .min_size(size)
    .fill(if timer.running {
        RUNNING_GREEN
    } else {
        egui::Color32::from_black_alpha(170)
    });
    if ui.add(button).on_hover_text(hover).clicked() {
        control(actions, deck_uuid, action);
    }
}

/// ⚙: the timer's settings window.
pub(super) fn settings_button(ui: &mut egui::Ui, deck_uuid: &str, size: egui::Vec2) {
    let response = ui
        .add(
            egui::Button::new(
                egui::RichText::new("⚙")
                    .size(size.y * 0.6)
                    .color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_black_alpha(170))
            .min_size(size),
        )
        .on_hover_text("The timer: what it counts, its font, colours and place");
    if response.clicked() {
        open(ui.ctx(), deck_uuid);
    }
}

/// Start/pause and settings over the foot of a channel's picture: for a
/// tile or a column showing a timer.
pub(super) fn overlay_on_picture(
    ui: &mut egui::Ui,
    picture: egui::Rect,
    ch: &ChannelUIInfo,
    actions: &mut UIActions,
) {
    let Some((deck, timer)) = of_channel(ch) else {
        return;
    };
    let size = egui::vec2(30.0, 24.0);
    let row = egui::Rect::from_min_size(
        picture.left_bottom() + egui::vec2(4.0, -size.y - 4.0),
        egui::vec2(picture.width() - 8.0, size.y),
    );
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row_ui.spacing_mut().item_spacing.x = 3.0;
    start_button(&mut row_ui, &deck.uuid, timer, size, actions);
    settings_button(&mut row_ui, &deck.uuid, size);
}

/// Start, pause and reset in a row, for a count down or up.
fn transport(ui: &mut egui::Ui, deck_uuid: &str, timer: &TimerSnapshot, actions: &mut UIActions) {
    if !timer.settings.mode.has_transport() {
        return;
    }
    let size = egui::vec2(72.0, 30.0);
    let big = |text: &str| egui::RichText::new(text).size(15.0);
    let start = egui::Button::new(big("▶ Start").color(if timer.running {
        ui.visuals().weak_text_color()
    } else {
        egui::Color32::WHITE
    }))
    .min_size(size)
    .fill(if timer.running {
        ui.visuals().widgets.inactive.weak_bg_fill
    } else {
        RUNNING_GREEN
    });
    if ui
        .add_enabled(!timer.running, start)
        .on_hover_text("Count on from here — from the top, at zero")
        .clicked()
    {
        control(actions, deck_uuid, TimerAction::Start);
    }
    if ui
        .add_enabled(
            timer.running,
            egui::Button::new(big("⏸ Pause")).min_size(size),
        )
        .clicked()
    {
        control(actions, deck_uuid, TimerAction::Pause);
    }
    if ui
        .add(egui::Button::new(big("⏮ Reset")).min_size(size))
        .on_hover_text(match timer.settings.mode {
            TimerMode::Countdown => "Back to its full length, paused",
            _ => "Back to zero, paused",
        })
        .clicked()
    {
        control(actions, deck_uuid, TimerAction::Reset);
    }
}

/// −1 min, +1 min, −10 s, +10 s: time put on or taken off what it shows.
fn adjust(ui: &mut egui::Ui, deck_uuid: &str, timer: &TimerSnapshot, actions: &mut UIActions) {
    if !timer.settings.mode.has_transport() {
        return;
    }
    for (text, secs) in [("−1 min", -60), ("+1 min", 60), ("−10 s", -10), ("+10 s", 10)] {
        if ui
            .add(egui::Button::new(text).min_size(egui::vec2(54.0, 24.0)))
            .on_hover_text(if secs > 0 {
                "Put this on what it shows"
            } else {
                "Take this off what it shows"
            })
            .clicked()
        {
            control(actions, deck_uuid, TimerAction::Adjust { secs });
        }
    }
}

/// A length, dragged or typed as 5, 5:00 or 1:30:00.
fn length_value(
    ui: &mut egui::Ui,
    secs: &mut u32,
    actions: &mut UIActions,
) -> egui::Response {
    let response = ui.add(
        egui::DragValue::new(secs)
            .range(1..=MAX_DURATION_SECS)
            // Kept as it is till edited: shown, a length outside the range
            // would be changed, and sent.
            .clamp_existing_to_range(false)
            .speed(1.0)
            .custom_formatter(|v, _| span_text(v.max(0.0) as u64, TimeFormat::Auto))
            .custom_parser(|text| parse_span(text).map(f64::from)),
    );
    if response.dragged() {
        actions.session.gesture_active = true;
    }
    response.on_hover_text("Drag, or click and type: 5, 5:00 or 1:30:00")
}

/// A time of day, dragged or typed as 9:30, 09:30:15 or 930.
fn time_value(ui: &mut egui::Ui, secs: &mut u32, actions: &mut UIActions) -> egui::Response {
    let response = ui.add(
        egui::DragValue::new(secs)
            .range(0..=24 * 3600 - 1)
            .speed(30.0)
            .custom_formatter(|v, _| target_text(v.max(0.0) as u32))
            .custom_parser(|text| parse_time_of_day(text).map(f64::from)),
    );
    if response.dragged() {
        actions.session.gesture_active = true;
    }
    response.on_hover_text("Drag, or click and type: 9:30, 14:05:30 or 1405")
}

/// The count down's length, and a row of lengths set in one click.
fn length_row(ui: &mut egui::Ui, deck_uuid: &str, timer: &TimerSnapshot, actions: &mut UIActions) {
    let settings = &shown_settings(actions, deck_uuid, timer);
    match settings.mode {
        TimerMode::Countdown => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Length");
                let mut secs = settings.duration_secs;
                length_value(ui, &mut secs, actions);
                if secs != settings.duration_secs {
                    set_settings(
                        actions,
                        deck_uuid,
                        TimerSettings {
                            duration_secs: secs,
                            ..settings.clone()
                        },
                    );
                }
                ui.add_space(6.0);
                for minutes in PRESET_MINUTES {
                    let length = minutes * 60;
                    let button = egui::Button::selectable(
                        settings.duration_secs == length,
                        format!("{minutes}"),
                    )
                    .min_size(egui::vec2(28.0, 22.0));
                    if ui
                        .add(button)
                        .on_hover_text(format!("{minutes}:00, from the top, ready to start"))
                        .clicked()
                    {
                        set_settings(
                            actions,
                            deck_uuid,
                            TimerSettings {
                                duration_secs: length,
                                ..settings.clone()
                            },
                        );
                        control(actions, deck_uuid, TimerAction::Reset);
                    }
                }
                ui.label(egui::RichText::new("min").small().weak());
            });
        }
        TimerMode::CountTo => {
            ui.horizontal(|ui| {
                ui.label("Counts to");
                let mut secs = settings.target_secs;
                time_value(ui, &mut secs, actions);
                if secs != settings.target_secs {
                    set_settings(
                        actions,
                        deck_uuid,
                        TimerSettings {
                            target_secs: secs,
                            ..settings.clone()
                        },
                    );
                }
            });
        }
        TimerMode::CountUp | TimerMode::Clock => {}
    }
}

/// "Count down of 5:00 · paused".
fn status_text(timer: &TimerSnapshot) -> String {
    let settings = &timer.settings;
    let what = match settings.mode {
        TimerMode::Countdown => format!(
            "Count down of {}",
            span_text(u64::from(settings.duration_secs), settings.format)
        ),
        TimerMode::CountTo => format!("Count to {}", target_text(settings.target_secs)),
        TimerMode::CountUp => "Count up".to_string(),
        TimerMode::Clock => "Time of day".to_string(),
    };
    let how = match (settings.mode.has_transport(), timer.running, timer.state) {
        (_, _, TimerState::Over) if settings.mode.counts_down() => " · time's up",
        (true, true, _) => " · running",
        (true, false, _) => " · paused",
        (false, _, _) => "",
    };
    format!("{what}{how}")
}

// ── The control panel ───────────────────────────────────────────────

/// A timer's control panel, in the dock: what it reads, big, its controls,
/// and its settings — there too, not only in its window, for whoever
/// changes them often (an operator's ask).
pub(super) fn render_timer_panel(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
    panel: u64,
) {
    let Some((_, deck, timer)) = by_panel(data, panel) else {
        ui.add_space(8.0);
        ui.label(egui::RichText::new("This timer is not in this show.").weak());
        ui.label(
            egui::RichText::new("Close the panel, or add a timer from Add Input.")
                .small()
                .weak(),
        );
        return;
    };
    ui.add_space(4.0);
    readout(ui, timer, 72.0);
    ui.label(egui::RichText::new(status_text(timer)).small().weak());
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| transport(ui, &deck.uuid, timer, actions));
    ui.horizontal_wrapped(|ui| adjust(ui, &deck.uuid, timer, actions));
    ui.add_space(2.0);
    length_row(ui, &deck.uuid, timer, actions);
    ui.add_space(6.0);
    egui::CollapsingHeader::new("Settings")
        .id_salt(("timer_panel_settings", panel))
        .default_open(true)
        .show(ui, |ui| settings(ui, &deck.uuid, timer, actions));
}

/// What the timer reads, as big as `max` points or the width allows.
fn readout(ui: &mut egui::Ui, timer: &TimerSnapshot, max: f32) {
    #[allow(clippy::cast_precision_loss)]
    let chars = timer.text.chars().count().max(4) as f32;
    let size = (ui.available_width() / (chars * 0.62)).clamp(14.0, max);
    ui.label(
        egui::RichText::new(&timer.text)
            .size(size)
            .strong()
            .color(state_color(ui, timer)),
    );
}

/// The layer's settings column for a timer: what it reads, start and
/// pause, and its window and panel.
pub(super) fn render_deck_column(
    ui: &mut egui::Ui,
    data: &UIData,
    deck: &DeckUIInfo,
    timer: &TimerSnapshot,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("Timer").strong());
    readout(ui, timer, 30.0);
    ui.label(egui::RichText::new(status_text(timer)).small().weak());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.0;
        start_button(ui, &deck.uuid, timer, egui::vec2(30.0, 24.0), actions);
        if timer.settings.mode.has_transport()
            && ui
                .add(egui::Button::new("⏮").min_size(egui::vec2(30.0, 24.0)))
                .on_hover_text("Reset")
                .clicked()
        {
            control(actions, &deck.uuid, TimerAction::Reset);
        }
    });
    if ui.button("⚙ Settings…").clicked() {
        open(ui.ctx(), &deck.uuid);
    }
    panel_checkbox(ui, data, timer, actions);
}

/// "Control panel": the timer's panel in the dock, or not.
fn panel_checkbox(ui: &mut egui::Ui, data: &UIData, timer: &TimerSnapshot, actions: &mut UIActions) {
    let pane = Pane::Timer(timer.panel);
    let mut open = data.screen.has(pane);
    if ui
        .checkbox(&mut open, "Control panel")
        .on_hover_text(
            "A panel of its own with start, pause, reset and its length — drag its tab \
             beside any panel. Also under Layout ▸ Timers",
        )
        .changed()
    {
        actions.session.panel_requests.push((pane, open));
    }
}

// ── The settings window ─────────────────────────────────────────────

fn window_id() -> egui::Id {
    egui::Id::new("timer_window")
}

/// Open the settings window on the timer of deck `deck_uuid`.
pub(super) fn open(ctx: &egui::Context, deck_uuid: &str) {
    ctx.data_mut(|d| d.insert_temp(window_id(), deck_uuid.to_string()));
}

fn showing(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(window_id()))
}

fn close(ctx: &egui::Context) {
    ctx.data_mut(|d| d.remove::<String>(window_id()));
}

pub(super) fn render_timer_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    open_new_timer_panel(ctx, data, actions);
    let Some(deck_uuid) = showing(ctx) else {
        return;
    };
    // Its layer went away.
    let Some((ch, _, timer)) = by_deck(data, &deck_uuid) else {
        close(ctx);
        return;
    };
    let mut open = true;
    egui::Window::new(format!("Timer — {}", ch.name))
        .id(egui::Id::new("timer_window_frame"))
        .open(&mut open)
        .default_size(egui::vec2(560.0, 470.0))
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| window_body(ui, data, &deck_uuid, timer, actions));
    if !open {
        close(ctx);
    }
}

fn window_body(
    ui: &mut egui::Ui,
    data: &UIData,
    deck_uuid: &str,
    timer: &TimerSnapshot,
    actions: &mut UIActions,
) {
    ui.horizontal(|ui| {
        readout(ui, timer, 40.0);
        ui.add_space(8.0);
        ui.vertical(|ui| {
            ui.label(egui::RichText::new(status_text(timer)).small().weak());
            ui.horizontal(|ui| transport(ui, deck_uuid, timer, actions));
        });
    });
    panel_checkbox(ui, data, timer, actions);
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("timer_settings")
        .auto_shrink([false, true])
        .show(ui, |ui| settings(ui, deck_uuid, timer, actions));
}

/// What the timer counts and how it looks — side by side where there is
/// room, else one above the other. Sent only when changed here, and from
/// the settings as this frame's edits left them (see `shown_settings`).
fn settings(ui: &mut egui::Ui, deck_uuid: &str, timer: &TimerSnapshot, actions: &mut UIActions) {
    let shown = &shown_settings(actions, deck_uuid, timer);
    let mut next = shown.clone();
    if ui.available_width() >= 520.0 {
        ui.columns(2, |columns| {
            counting(&mut columns[0], &mut next, actions);
            look(&mut columns[1], &mut next, actions);
        });
    } else {
        counting(ui, &mut next, actions);
        ui.add_space(8.0);
        look(ui, &mut next, actions);
    }
    if next != *shown {
        set_settings(actions, deck_uuid, next);
    }
}

/// What it counts: the mode, a length or a time, what happens at zero, the
/// warning, how it is written.
fn counting(ui: &mut egui::Ui, next: &mut TimerSettings, actions: &mut UIActions) {
    ui.label(egui::RichText::new("Counts").strong());
    for mode in TimerMode::ALL {
        ui.radio_value(&mut next.mode, mode, mode.label());
    }
    ui.add_space(6.0);
    egui::Grid::new("timer_counting")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            match next.mode {
                TimerMode::Countdown => {
                    ui.label("Length");
                    length_value(ui, &mut next.duration_secs, actions);
                    ui.end_row();
                }
                TimerMode::CountTo => {
                    ui.label("Counts to");
                    time_value(ui, &mut next.target_secs, actions);
                    ui.end_row();
                }
                TimerMode::CountUp | TimerMode::Clock => {}
            }
            if next.mode.counts_down() {
                ui.label("At zero");
                egui::ComboBox::from_id_salt("timer_at_zero")
                    .selected_text(next.at_zero.label())
                    .show_ui(ui, |ui| {
                        for at_zero in AtZero::ALL {
                            ui.selectable_value(&mut next.at_zero, at_zero, at_zero.label());
                        }
                    });
                ui.end_row();
                ui.label("Warn");
                ui.horizontal(|ui| {
                    let response = ui.add(
                        egui::DragValue::new(&mut next.warn_secs)
                            .range(0..=MAX_DURATION_SECS)
                            .speed(1.0)
                            .suffix(" s before"),
                    );
                    if response.dragged() {
                        actions.session.gesture_active = true;
                    }
                    response.on_hover_text("Its warning colour this long before zero; 0 for never");
                });
                ui.end_row();
            }
            if next.mode == TimerMode::Clock {
                ui.label("Shown as");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        ui.selectable_value(&mut next.clock_24h, true, "14:05");
                        ui.selectable_value(&mut next.clock_24h, false, "2:05 PM");
                    });
                    ui.checkbox(&mut next.clock_seconds, "Seconds");
                });
                ui.end_row();
            } else {
                ui.label("Shown as");
                egui::ComboBox::from_id_salt("timer_format")
                    .selected_text(next.format.label())
                    .show_ui(ui, |ui| {
                        for format in TimeFormat::ALL {
                            ui.selectable_value(&mut next.format, format, format.label());
                        }
                    });
                ui.end_row();
            }
        });
}

/// How it looks: font, size, place, colours, outline, box, caption.
fn look(ui: &mut egui::Ui, next: &mut TimerSettings, actions: &mut UIActions) {
    ui.label(egui::RichText::new("Look").strong());
    let counts_down = next.mode.counts_down();
    let look = &mut next.look;
    egui::Grid::new("timer_look")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Font");
            ui.vertical(|ui| {
                font_picker(ui, &mut look.font);
                ui.horizontal(|ui| {
                    ui.checkbox(&mut look.bold, "Bold");
                    ui.checkbox(&mut look.italic, "Italic");
                });
            });
            ui.end_row();

            ui.label("Size");
            // Each control here writes only what it changes, and rounds
            // nothing it merely shows (see `settings`).
            let mut percent = look.size * 100.0;
            let response = ui.add(
                egui::Slider::new(&mut percent, MIN_SIZE * 100.0..=MAX_SIZE * 100.0)
                    .suffix(" %")
                    .max_decimals(0)
                    .clamping(egui::SliderClamping::Edits),
            );
            if response.dragged() {
                actions.session.gesture_active = true;
            }
            if response.changed() {
                look.size = percent / 100.0;
            }
            response.on_hover_text("The numbers' height, as a share of the picture's");
            ui.end_row();

            ui.label("Place");
            // From the row's top: a row lays a cell's contents out from a
            // line's height centred in it, and the picker, taller, hung
            // into the Colour row under it.
            ui.horizontal_top(|ui| {
                anchor_picker(ui, &mut look.anchor);
                ui.vertical(|ui| {
                    ui.label(egui::RichText::new(look.anchor.label()).small());
                    let mut margin = look.margin * 100.0;
                    let response = ui.add(
                        egui::DragValue::new(&mut margin)
                            .range(0.0..=40.0)
                            .speed(0.2)
                            .max_decimals(1)
                            .prefix("in ")
                            .suffix(" %"),
                    );
                    if response.dragged() {
                        actions.session.gesture_active = true;
                    }
                    if response.changed() {
                        look.margin = margin / 100.0;
                    }
                    response.on_hover_text("Room kept from the edges, as a share of the height");
                });
            });
            ui.end_row();

            ui.label("Colour");
            ui.horizontal(|ui| {
                colour(ui, &mut look.color, "The numbers");
                if counts_down {
                    ui.label(egui::RichText::new("warn").small().weak());
                    colour(ui, &mut look.warn_color, "In the last seconds before zero");
                    ui.label(egui::RichText::new("zero").small().weak());
                    colour(ui, &mut look.over_color, "At zero and over time");
                }
            });
            ui.end_row();

            ui.label("Outline");
            ui.horizontal(|ui| {
                let mut width = look.outline * 100.0;
                let response = ui.add(
                    egui::Slider::new(&mut width, 0.0..=MAX_OUTLINE * 100.0)
                        .max_decimals(1)
                        .suffix(" %")
                        .clamping(egui::SliderClamping::Edits),
                );
                if response.dragged() {
                    actions.session.gesture_active = true;
                }
                if response.changed() {
                    look.outline = width / 100.0;
                }
                response.on_hover_text("Round the numbers, to read them over a picture; 0 for none");
                colour(ui, &mut look.outline_color, "The outline");
            });
            ui.end_row();

            ui.label("Box");
            ui.horizontal(|ui| {
                let mut opacity = look.box_opacity * 100.0;
                let response = ui.add(
                    egui::Slider::new(&mut opacity, 0.0..=100.0)
                        .max_decimals(0)
                        .suffix(" %")
                        .clamping(egui::SliderClamping::Edits),
                );
                if response.dragged() {
                    actions.session.gesture_active = true;
                }
                if response.changed() {
                    look.box_opacity = opacity / 100.0;
                }
                response.on_hover_text("Behind the numbers; at 0 % none, and the background is see-through");
                colour(ui, &mut look.box_color, "The box");
            });
            ui.end_row();

            ui.label("Caption");
            let response = typed(ui, "caption", &mut look.caption, "a line above, e.g. Back in");
            if response.has_focus() {
                actions.session.gesture_active = true;
            }
            ui.end_row();
        });
}

/// A line typed into `text`, kept here while it is being typed: what is
/// typed goes to the engine at once, but the text shown is the typing, not
/// the engine's copy of it a frame behind.
fn typed(ui: &mut egui::Ui, salt: &str, text: &mut String, hint: &str) -> egui::Response {
    let id = ui.id().with(("timer_typing", salt));
    let mut typing: String = ui
        .ctx()
        .data(|d| d.get_temp(id))
        .unwrap_or_else(|| text.clone());
    let response = ui.add(
        egui::TextEdit::singleline(&mut typing)
            .id(id.with("field"))
            .hint_text(hint)
            .desired_width(field_width(ui)),
    );
    if response.changed() {
        text.clone_from(&typing);
    }
    if response.has_focus() {
        ui.ctx().data_mut(|d| d.insert_temp(id, typing));
    } else {
        ui.ctx().data_mut(|d| d.remove::<String>(id));
    }
    response
}

/// A text field's or a font list's width: 180 points, or less in a panel
/// docked narrow, which does not scroll sideways.
fn field_width(ui: &egui::Ui) -> f32 {
    ui.available_width().clamp(80.0, 180.0)
}

fn colour(ui: &mut egui::Ui, rgb: &mut [u8; 3], hover: &str) {
    egui::color_picker::color_edit_button_srgb(ui, rgb).on_hover_text(hover);
}

fn font_filter_id() -> egui::Id {
    egui::Id::new("timer_font_filter")
}

/// The installed fonts, by family, looked through by name.
fn font_picker(ui: &mut egui::Ui, font: &mut String) {
    let Some(families) = crate::deck::svg::font_families() else {
        // Still being read: typed by name meanwhile.
        typed(ui, "font", font, "a font's name").on_hover_text("A font installed on this computer, by name");
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        return;
    };
    let mut filter: String = ui
        .ctx()
        .data(|d| d.get_temp(font_filter_id()))
        .unwrap_or_default();
    egui::ComboBox::from_id_salt("timer_font")
        .selected_text(font.as_str())
        .width(field_width(ui))
        .height(320.0)
        .show_ui(ui, |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut filter)
                    .hint_text("Look for a font")
                    .desired_width(170.0),
            );
            let wanted = filter.to_lowercase();
            for family in families
                .iter()
                .filter(|f| wanted.is_empty() || f.to_lowercase().contains(&wanted))
            {
                if ui.selectable_label(family == font, family).clicked() {
                    font.clone_from(family);
                }
            }
        });
    ui.ctx().data_mut(|d| d.insert_temp(font_filter_id(), filter));
}

/// A little frame with its nine places: the one clicked is where the
/// timer goes.
fn anchor_picker(ui: &mut egui::Ui, anchor: &mut Anchor) {
    let size = egui::vec2(72.0, 42.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_stroke(
        rect.shrink(1.0),
        3.0,
        egui::Stroke::new(1.0, visuals.weak_text_color()),
        egui::StrokeKind::Inside,
    );
    for spot in Anchor::ALL {
        let (fx, fy) = spot.fractions();
        let at = egui::pos2(
            egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, fx),
            egui::lerp(rect.top() + 8.0..=rect.bottom() - 8.0, fy),
        );
        let hit = egui::Rect::from_center_size(at, egui::vec2(20.0, 14.0));
        let response = ui
            .interact(hit, ui.id().with(("timer_anchor", spot.label())), egui::Sense::click())
            .on_hover_text(spot.label());
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, true, *anchor == spot, spot.label())
        });
        if response.clicked() {
            *anchor = spot;
        }
        let chosen = *anchor == spot;
        let color = if chosen {
            RUNNING_GREEN
        } else if response.hovered() {
            visuals.strong_text_color()
        } else {
            visuals.weak_text_color()
        };
        if chosen {
            painter.rect_filled(egui::Rect::from_center_size(at, egui::vec2(14.0, 8.0)), 2.0, color);
        } else {
            painter.circle_filled(at, 2.5, color);
        }
    }
}

// ── A new timer's panel ─────────────────────────────────────────────

fn awaiting_new_id() -> egui::Id {
    egui::Id::new("timer_awaiting_new")
}

/// A timer is about to be made: open its control panel once it is there.
pub(super) fn open_panel_when_made(ctx: &egui::Context, data: &UIData) {
    ctx.data_mut(|d| d.insert_temp(awaiting_new_id(), (panels(data), 0_u32)));
}

/// Open the control panel of the timer just made, once it is there — given
/// up after a while if it never is.
fn open_new_timer_panel(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let Some((known, frames)) = ctx.data(|d| d.get_temp::<(Vec<u64>, u32)>(awaiting_new_id()))
    else {
        return;
    };
    let made = panels(data).into_iter().find(|p| !known.contains(p));
    match made {
        Some(panel) => {
            ctx.data_mut(|d| d.remove::<(Vec<u64>, u32)>(awaiting_new_id()));
            if !data.screen.locked {
                actions.session.panel_requests.push((Pane::Timer(panel), true));
            }
        }
        None if frames > 120 => {
            ctx.data_mut(|d| d.remove::<(Vec<u64>, u32)>(awaiting_new_id()));
        }
        None => {
            ctx.data_mut(|d| d.insert_temp(awaiting_new_id(), (known, frames + 1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    /// The fixture with a timer layer in its first channel.
    pub(crate) fn with_timer(settings: TimerSettings, running: bool) -> UIData {
        let mut data = UIData::test_fixture();
        let deck = &mut data.channels[0].decks[0];
        deck.timer = Some(TimerSnapshot {
            text: "4:59".into(),
            state: TimerState::Normal,
            remaining: Some(299.0),
            elapsed: 1.0,
            running,
            panel: 7,
            settings,
        });
        data
    }

    fn panel_harness(data: UIData) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        panel_harness_of_size(data, egui::vec2(420.0, 320.0))
    }

    /// As [`panel_harness`], the panel `size` — tall enough for its
    /// settings.
    fn panel_harness_of_size(
        data: UIData,
        size: egui::Vec2,
    ) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        // The panel holds the settings, and with them the font picker.
        fonts_listed();
        let data = std::rc::Rc::new(data);
        egui_kittest::Harness::builder()
            .with_size(size)
            .build_ui_state(
                move |ui, seen: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_timer_panel(ui, &data, &mut actions, 7);
                    seen.extend(actions.commands);
                },
                Vec::new(),
            )
    }

    fn deck_uuid(data: &UIData) -> String {
        data.channels[0].decks[0].uuid.clone()
    }

    /// Whether `seen` holds a `ControlTimer` doing `action` to `uuid`.
    fn controlled(seen: &[EngineCommand], uuid: &str, action: TimerAction) -> bool {
        seen.iter().any(|c| {
            matches!(c, EngineCommand::ControlTimer { deck_uuid, action: a }
                if deck_uuid == uuid && *a == action)
        })
    }

    #[test]
    fn the_panel_starts_a_paused_count_down() {
        let data = with_timer(TimerSettings::default(), false);
        let uuid = deck_uuid(&data);
        let mut harness = panel_harness(data);
        harness.run();
        harness.get_by_label("4:59");
        harness.get_by_label("▶ Start").click();
        harness.run();
        assert!(controlled(harness.state(), &uuid, TimerAction::Start));
    }

    #[test]
    fn a_running_count_pauses_and_takes_a_minute_off() {
        let data = with_timer(TimerSettings::default(), true);
        let uuid = deck_uuid(&data);
        let mut harness = panel_harness(data);
        harness.run();
        assert!(harness.get_by_label("▶ Start").accesskit_node().is_disabled());
        harness.get_by_label("⏸ Pause").click();
        harness.run();
        harness.get_by_label("−1 min").click();
        harness.run();
        assert!(controlled(harness.state(), &uuid, TimerAction::Pause));
        assert!(controlled(
            harness.state(),
            &uuid,
            TimerAction::Adjust { secs: -60 }
        ));
    }

    #[test]
    fn a_preset_sets_the_length_and_goes_back_to_the_top() {
        let data = with_timer(TimerSettings::default(), false);
        let uuid = deck_uuid(&data);
        let mut harness = panel_harness(data);
        harness.run();
        harness.get_by_label("15").click();
        harness.run();
        let seen = harness.state();
        let set = seen.iter().position(|c| {
            matches!(c, EngineCommand::SetTimerSettings { settings, .. }
                if settings.duration_secs == 900)
        });
        assert!(set.is_some(), "the length set");
        assert!(
            controlled(&seen[set.unwrap()..], &uuid, TimerAction::Reset),
            "then back to the top"
        );
    }

    #[test]
    fn the_time_of_day_has_no_start_button() {
        let data = with_timer(
            TimerSettings {
                mode: TimerMode::Clock,
                ..TimerSettings::default()
            },
            true,
        );
        let mut harness = panel_harness(data);
        harness.run();
        assert!(harness.query_by_label("▶ Start").is_none());
        assert!(harness.query_by_label("⏮ Reset").is_none());
    }

    #[test]
    fn a_panel_whose_timer_is_gone_says_so() {
        let mut harness = panel_harness(UIData::test_fixture());
        harness.run();
        harness.get_by_label("This timer is not in this show.");
    }

    /// Typed faster than the settings come back, every letter still counts:
    /// the settings here never change, as if the engine were a frame behind.
    #[test]
    fn a_caption_typed_ahead_of_the_engine_keeps_every_letter() {
        fn field(node: &egui_kittest::kittest::AccessKitNode<'_>) -> bool {
            node.role() == egui::accesskit::Role::TextInput
                && node.value().as_deref() == Some("Back")
        }
        // The font list is read on a thread; the window waits for it.
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while crate::deck::svg::font_families().is_none() {
            assert!(std::time::Instant::now() < until, "fonts never listed");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut settings = TimerSettings::default();
        settings.look.caption = "Back".into();
        let data = std::rc::Rc::new(with_timer(settings, false));
        let uuid = deck_uuid(&data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                move |ui, seen: &mut Vec<EngineCommand>| {
                    open(ui.ctx(), &uuid);
                    let mut actions = UIActions::new();
                    render_timer_window(ui.ctx(), &data, &mut actions);
                    seen.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        harness.get_by(field).click();
        harness.run_steps(2);
        // A letter a frame.
        for letter in [" ", "i", "n"] {
            harness
                .get_by_role(egui::accesskit::Role::TextInput)
                .type_text(letter);
            harness.step();
        }
        harness.run_steps(2);
        let last = harness.state().iter().rev().find_map(|c| match c {
            EngineCommand::SetTimerSettings { settings, .. } => Some(settings.look.caption.clone()),
            _ => None,
        });
        assert_eq!(last.as_deref(), Some("Back in"));
    }

    /// Waits for the font list, read on a thread: the settings' font picker
    /// asks for frames until it is there.
    fn fonts_listed() {
        let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while crate::deck::svg::font_families().is_none() {
            assert!(std::time::Instant::now() < until, "fonts never listed");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// The panel in the dock holds the timer's settings too — what it counts
    /// and how it looks — for whoever changes them often, not only its
    /// window.
    #[test]
    fn the_panel_changes_what_the_timer_counts() {
        let data = with_timer(TimerSettings::default(), false);
        let uuid = deck_uuid(&data);
        let mut harness = panel_harness_of_size(data, egui::vec2(420.0, 1100.0));
        harness.run();
        harness.get_by_label("Font");
        harness.get_by_label("Colour");
        harness.get_by_label("Count up").click();
        harness.run();
        assert!(
            harness.state().iter().any(|c| matches!(
                c,
                EngineCommand::SetTimerSettings { deck_uuid, settings }
                    if *deck_uuid == uuid && settings.mode == TimerMode::CountUp
            )),
            "{:?}",
            harness.state()
        );
    }

    /// The settings shown in the panel send nothing of their own — not even
    /// values their sliders would round — and a length preset clicked above
    /// them stays: each send is the whole settings, and one from settings
    /// merely shown put the old length back.
    #[test]
    fn the_settings_shown_leave_a_preset_alone() {
        let mut settings = TimerSettings {
            // Under the length field's least, as the engine allows.
            duration_secs: 0,
            ..TimerSettings::default()
        };
        settings.look.size = 0.155;
        // Not the same float once made a percentage and back.
        settings.look.margin = 0.041;
        settings.look.outline = 0.0123;
        settings.look.box_opacity = 0.333;
        let data = with_timer(settings, false);
        let mut harness = panel_harness_of_size(data, egui::vec2(420.0, 1100.0));
        harness.run();
        let sent = |seen: &[EngineCommand]| -> Vec<u32> {
            seen.iter()
                .filter_map(|c| match c {
                    EngineCommand::SetTimerSettings { settings, .. } => {
                        Some(settings.duration_secs)
                    }
                    _ => None,
                })
                .collect()
        };
        assert_eq!(
            sent(harness.state()),
            Vec::<u32>::new(),
            "nothing sent while shown"
        );
        harness.get_by_label("10").click();
        harness.run();
        assert_eq!(
            sent(harness.state()),
            vec![600],
            "the preset's length, and only it"
        );
    }

    /// The panel with an edit already made this frame — the length set to
    /// `pending`'s, or Warn to its — as a control drawn before it, or the
    /// window, leaves one.
    fn panel_after_an_edit(
        pending: TimerSettings,
    ) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        fonts_listed();
        let data = std::rc::Rc::new(with_timer(TimerSettings::default(), false));
        let uuid = deck_uuid(&data);
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 1100.0))
            .build_ui_state(
                move |ui, seen: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    set_settings(&mut actions, &uuid, pending.clone());
                    render_timer_panel(ui, &data, &mut actions, 7);
                    seen.extend(actions.commands.into_iter().skip(1));
                },
                Vec::new(),
            )
    }

    /// The last settings sent: (length, warning).
    fn last_sent(seen: &[EngineCommand]) -> Option<(u32, u32)> {
        seen.iter().rev().find_map(|c| match c {
            EngineCommand::SetTimerSettings { settings, .. } => {
                Some((settings.duration_secs, settings.warn_secs))
            }
            _ => None,
        })
    }

    /// A number stepped in the settings, or a length preset clicked, after
    /// another edit in the same frame keeps that edit: each sends the whole
    /// settings, on top of it rather than the settings shown before it.
    #[test]
    fn a_change_keeps_an_edit_made_before_it_in_the_frame() {
        use egui::accesskit::{Action, ActionRequest, Role};
        let ten_minutes = TimerSettings {
            duration_secs: 600,
            ..TimerSettings::default()
        };
        let mut harness = panel_after_an_edit(ten_minutes);
        harness.run();
        let warn = harness
            .get_all_by_role(Role::SpinButton)
            .find(|node| node.accesskit_node().numeric_value() == Some(60.0))
            .expect("Warn, 60 s");
        let (target_node, target_tree) = warn.accesskit_node().locate();
        harness.event(egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Increment,
            target_node,
            target_tree,
            data: None,
        }));
        harness.run();
        assert_eq!(last_sent(harness.state()), Some((600, 61)), "Warn stepped");

        let warned = TimerSettings {
            warn_secs: 61,
            ..TimerSettings::default()
        };
        let mut harness = panel_after_an_edit(warned);
        harness.run();
        harness.get_by_label("15").click();
        harness.run();
        assert_eq!(
            last_sent(harness.state()),
            Some((900, 61)),
            "a preset clicked"
        );
    }

    /// Look's rows each start below the one before: the Place row's nine
    /// spots, taller than a row, ran into the Colour swatch under them.
    #[test]
    fn the_place_row_clears_the_colour_row() {
        fonts_listed();
        let data = std::rc::Rc::new(with_timer(TimerSettings::default(), false));
        let uuid = deck_uuid(&data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                move |ui, seen: &mut Vec<EngineCommand>| {
                    open(ui.ctx(), &uuid);
                    let mut actions = UIActions::new();
                    render_timer_window(ui.ctx(), &data, &mut actions);
                    seen.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        let lowest = ["Bottom left", "Bottom", "Bottom right"]
            .iter()
            .map(|spot| harness.get_by_label(spot).rect().bottom())
            .fold(f32::MIN, f32::max);
        let colour = harness.get_by_label("Colour").rect();
        assert!(
            colour.top() >= lowest,
            "Colour at {colour:?}, the spots down to {lowest}"
        );
    }

    #[test]
    fn a_panel_is_named_after_its_timers_channel() {
        let data = with_timer(TimerSettings::default(), false);
        let name = &data.channels[0].name;
        let expected = if data.channels[0].decks.len() > 1 {
            format!("⏱ {name} · {}", data.channels[0].decks[0].name)
        } else {
            format!("⏱ {name}")
        };
        assert_eq!(panel_title(&data, 7), expected);
        assert_eq!(panel_title(&data, 8), "⏱ Timer (gone)");
    }
}
