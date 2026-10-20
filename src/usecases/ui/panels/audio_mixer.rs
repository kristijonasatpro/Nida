//! The audio mixer, laid out as vMix's: **OUTPUTS** — Master and the other
//! buses — then **INPUTS**, one upright strip each.
//!
//! An input strip, top to bottom: its name (red while it is on air), the
//! balance, then beside its fader ⚙ settings, **S** solo (heard on the
//! headphones only, before the fader), Audio Auto (the arrows: the sound
//! comes on with the picture and goes off when it leaves) and On (the
//! speaker); under it a button per bus — **M** for Master, then **A**, **B**…
//! A bus strip has ⚙, On and its fader; Master has the headphones level too.
//!
//! Faders send a command every frame they move, so the sound follows the
//! hand; everything else is one command per click.

use super::super::{UIActions, UIData};
use crate::audio_mix::{AudioBusSnapshot, AudioSourceKind, AudioSourceSnapshot, FOLLOW_MAIN};
use crate::engine::EngineCommand;

const STRIP_WIDTH: f32 = 128.0;
const STRIP_GAP: f32 = 4.0;
const SECTION_WIDTH: f32 = 22.0;
const HEADER: f32 = 22.0;
const BALANCE: f32 = 18.0;
const BUTTON: f32 = 28.0;
const BUTTON_GAP: f32 = 5.0;
const METER_WIDTH: f32 = 38.0;
/// One row of bus buttons under an input strip.
const SEND_ROW: f32 = 24.0;
const SENDS_PER_ROW: usize = 3;
/// Master's headphones level under its fader.
const KNOB_ROW: f32 = 40.0;
const MIN_HEIGHT: f32 = 240.0;

/// Fader travel: the level at each point of the track, bottom to top — as a
/// desk's fader, with most of the travel round 0 dB. The bottom stop is off.
const TAPER: [(f32, f32); 10] = [
    (-96.0, 0.0),
    (-60.0, 0.05),
    (-40.0, 0.18),
    (-30.0, 0.28),
    (-20.0, 0.40),
    (-10.0, 0.56),
    (-5.0, 0.66),
    (0.0, 0.76),
    (6.0, 0.88),
    (12.0, 1.0),
];
/// Marks beside a fader, in dB; the labelled ones are named.
const TICKS: [f32; 8] = [12.0, 6.0, 0.0, -10.0, -20.0, -30.0, -40.0, -60.0];
const LABELLED_TICKS: [f32; 4] = [6.0, 0.0, -20.0, -40.0];
/// Delays offered for a bus, in milliseconds.
const DELAYS_MS: [f32; 9] = [0.0, 20.0, 40.0, 60.0, 80.0, 100.0, 150.0, 200.0, 300.0];

const STRIP_BG: egui::Color32 = egui::Color32::from_rgb(16, 18, 24);
const HEADER_BG: egui::Color32 = egui::Color32::from_rgb(6, 6, 8);
const MASTER_HEADER: egui::Color32 = egui::Color32::from_rgb(40, 110, 40);
const ON_AIR_HEADER: egui::Color32 = egui::Color32::from_rgb(160, 28, 28);
const SECTION_BLUE: egui::Color32 = egui::Color32::from_rgb(40, 100, 190);
const BUTTON_BG: egui::Color32 = egui::Color32::from_rgb(42, 46, 56);
const BUTTON_ON: egui::Color32 = egui::Color32::from_rgb(40, 125, 50);
const SOLO_ON: egui::Color32 = egui::Color32::from_rgb(225, 185, 40);
const ARROW_GREEN: egui::Color32 = egui::Color32::from_rgb(90, 220, 90);
const ARROW_ORANGE: egui::Color32 = egui::Color32::from_rgb(240, 150, 40);
const TICK: egui::Color32 = egui::Color32::from_rgb(120, 124, 134);

pub(super) fn render_audio_mixer_section(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
) {
    let mix = &data.audio_mix;
    let height = (ui.available_height() - 4.0).max(MIN_HEIGHT);
    egui::ScrollArea::both()
        .id_salt("audio_mixer_strips")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = STRIP_GAP;
                section_label(ui, "OUTPUTS", height);
                for (i, bus) in mix.buses.iter().enumerate() {
                    bus_strip(ui, bus, i == 0, height, data, actions);
                }
                section_label(ui, "INPUTS", height);
                for source in &mix.sources {
                    input_strip(ui, source, height, data, actions);
                }
                add_column(ui, data, actions);
            });
        });
    render_settings_window(ui.ctx(), data, actions);
}

/// Whose settings are open, from a strip's ⚙ (or the headphones).
#[derive(Clone, PartialEq)]
pub(super) enum Settings {
    Bus(String),
    Source(String),
    Headphones,
}

#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    General,
    Equalizer,
    Dynamics,
}

fn settings_id() -> egui::Id {
    egui::Id::new("audio_strip_settings")
}

/// Open a strip's settings beside its button, or close them if they are
/// open.
fn toggle_settings(ctx: &egui::Context, which: Settings, at: egui::Pos2) {
    let open = ctx.memory(|m| m.data.get_temp::<(Settings, egui::Pos2)>(settings_id()));
    ctx.memory_mut(|m| {
        if open.is_some_and(|(w, _)| w == which) {
            m.data.remove::<(Settings, egui::Pos2)>(settings_id());
        } else {
            m.data.insert_temp(settings_id(), (which, at));
        }
    });
}

/// The settings window of one strip, as vMix opens an input's audio
/// settings: small, beside the strip, closed with its ×.
fn render_settings_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let Some((which, at)) =
        ctx.memory(|m| m.data.get_temp::<(Settings, egui::Pos2)>(settings_id()))
    else {
        return;
    };
    let mix = &data.audio_mix;
    let title = match &which {
        Settings::Bus(uuid) => mix
            .buses
            .iter()
            .find(|b| &b.config.uuid == uuid)
            .map(|b| b.config.name.clone()),
        Settings::Source(uuid) => mix
            .sources
            .iter()
            .find(|s| &s.uuid == uuid)
            .map(|s| s.name.clone()),
        Settings::Headphones => Some("Headphones".to_string()),
    };
    // Its strip is gone: nothing to set.
    let Some(title) = title else {
        ctx.memory_mut(|m| m.data.remove::<(Settings, egui::Pos2)>(settings_id()));
        return;
    };
    let key = match &which {
        Settings::Bus(uuid) => format!("bus_{uuid}"),
        Settings::Source(uuid) => format!("source_{uuid}"),
        Settings::Headphones => "headphones".to_string(),
    };
    let view_id = egui::Id::new(("audio_settings_expanded", &key));
    let expanded = ctx.data(|data| data.get_temp::<bool>(view_id).unwrap_or(false));
    let category_id = egui::Id::new(("audio_settings_page", &key));
    let mut page = ctx.data(|data| {
        data.get_temp::<SettingsPage>(category_id)
            .unwrap_or(SettingsPage::General)
    });
    let bounds = ctx.content_rect();
    let width =
        (if expanded { 640.0_f32 } else { 280.0_f32 }).min((bounds.width() - 24.0).max(100.0));
    let position_id = egui::Id::new(("audio_settings_drag_position", &key, expanded));
    let pending_position = ctx.data_mut(|data| data.remove_temp::<egui::Pos2>(position_id));
    let mut open = true;
    let mut window = egui::Window::new(format!("{title} — Audio"))
        .id(egui::Id::new((
            "audio_strip_settings_window",
            &key,
            expanded,
        )))
        .default_pos(if expanded {
            bounds.center() - egui::vec2(width / 2.0, 180.0)
        } else {
            at + egui::vec2(6.0, 0.0)
        })
        .constrain_to(bounds)
        .default_width(width)
        .min_height(if expanded {
            300.0_f32.min(bounds.height() - 40.0).max(0.0)
        } else {
            0.0
        })
        .collapsible(false)
        .title_bar(false)
        .movable(false)
        .resizable(expanded);
    if expanded {
        window = window.default_height((bounds.height() - 96.0).clamp(0.0, 760.0));
    }
    if let Some(position) = pending_position {
        window = window.current_pos(position);
    }
    let shown = window.show(ctx, |ui| {
        ui.set_min_width(width);
        let header = ui.horizontal(|ui| {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let close = ui.add_sized([28.0, 24.0], egui::Button::new("×"));
                close.widget_info(|| {
                    egui::WidgetInfo::labeled(
                        egui::WidgetType::Button,
                        true,
                        "Close audio settings",
                    )
                });
                if close.on_hover_text("Close audio settings").clicked() {
                    open = false;
                }
                if settings_view_button(ui, expanded).clicked() {
                    ctx.data_mut(|data| data.insert_temp(view_id, !expanded));
                    ctx.request_repaint();
                }
                ui.add_sized(
                    [ui.available_width(), 24.0],
                    egui::Label::new(egui::RichText::new(format!("{title} — Audio")).strong())
                        .truncate()
                        .sense(egui::Sense::drag()),
                )
                .on_hover_text(&title)
                .on_hover_cursor(egui::CursorIcon::Grab)
                .drag_delta()
            })
            .inner
        });
        ui.separator();
        egui::ScrollArea::vertical()
            .id_salt(("audio_settings_content", &key, expanded))
            .max_height(
                ui.available_height()
                    .min((bounds.height() - 96.0).max(40.0))
                    .max(40.0),
            )
            .show(ui, |ui| {
                if expanded {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(112.0);
                            ui.set_min_height(230.0_f32.min((bounds.height() - 100.0).max(0.0)));
                            if ui
                                .selectable_label(page == SettingsPage::General, "General")
                                .clicked()
                            {
                                page = SettingsPage::General;
                            }
                            if which != Settings::Headphones
                                && ui
                                    .selectable_label(page == SettingsPage::Equalizer, "Equalizer")
                                    .clicked()
                            {
                                page = SettingsPage::Equalizer;
                            }
                            if which != Settings::Headphones
                                && ui
                                    .selectable_label(page == SettingsPage::Dynamics, "Dynamics")
                                    .clicked()
                            {
                                page = SettingsPage::Dynamics;
                            }
                        });
                        ui.separator();
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width());
                            if page == SettingsPage::Equalizer && which != Settings::Headphones {
                                super::audio_equalizer::render(ui, &which, data, actions);
                            } else if page == SettingsPage::Dynamics
                                && which != Settings::Headphones
                            {
                                render_dynamics_settings(ui, &which, data, actions);
                            } else {
                                ui.heading("General settings");
                                ui.add_space(8.0);
                                render_strip_settings(ui, &which, data, actions);
                            }
                        });
                    });
                } else {
                    render_strip_settings(ui, &which, data, actions);
                }
            });
        header.inner
    });
    if let Some(shown) = shown {
        if let Some(delta) = shown.inner.filter(|delta| *delta != egui::Vec2::ZERO) {
            ctx.data_mut(|data| data.insert_temp(position_id, shown.response.rect.min + delta));
            ctx.request_repaint();
        }
    }
    ctx.data_mut(|data| data.insert_temp(category_id, page));
    if !open {
        ctx.memory_mut(|m| m.data.remove::<(Settings, egui::Pos2)>(settings_id()));
    }
}

fn render_dynamics_settings(
    ui: &mut egui::Ui,
    which: &Settings,
    data: &UIData,
    actions: &mut UIActions,
) {
    let (uuid, bus, snapshot, mut config) = match which {
        Settings::Bus(uuid) => {
            let Some(bus) = data
                .audio_mix
                .buses
                .iter()
                .find(|bus| bus.config.uuid == *uuid)
            else {
                return;
            };
            (uuid, true, &bus.dynamics, bus.config.compressor)
        }
        Settings::Source(uuid) => {
            let Some(source) = data
                .audio_mix
                .sources
                .iter()
                .find(|source| source.uuid == *uuid)
            else {
                return;
            };
            (uuid, false, &source.dynamics, source.dynamics.config)
        }
        Settings::Headphones => return,
    };
    ui.heading("Dynamics settings");
    ui.label(
        egui::RichText::new("FFmpeg compressor · stereo linked · peak detector")
            .small()
            .weak(),
    );
    let before = config;
    ui.horizontal(|ui| {
        ui.checkbox(&mut config.enabled, "Enable compressor");
        if ui.button("Reset compressor").clicked() {
            config = crate::audio_mix::CompressorConfig::default();
        }
    });
    ui.separator();
    ui.add(
        egui::Slider::new(&mut config.threshold_db, -60.0..=0.0)
            .text("Threshold")
            .suffix(" dB"),
    );
    ui.add(
        egui::Slider::new(&mut config.ratio, 1.0..=20.0)
            .text("Ratio")
            .suffix(":1"),
    );
    ui.add(
        egui::Slider::new(&mut config.attack_ms, 0.1..=2000.0)
            .logarithmic(true)
            .text("Attack")
            .suffix(" ms"),
    );
    ui.add(
        egui::Slider::new(&mut config.release_ms, 1.0..=9000.0)
            .logarithmic(true)
            .text("Release")
            .suffix(" ms"),
    );
    ui.add(
        egui::Slider::new(&mut config.knee_db, 0.0..=18.0)
            .text("Knee")
            .suffix(" dB"),
    );
    ui.add(
        egui::Slider::new(&mut config.makeup_db, -12.0..=24.0)
            .text("Makeup")
            .suffix(" dB"),
    );
    ui.separator();
    ui.horizontal_top(|ui|{
        for (label,levels) in [("Pre comp",snapshot.pre_db),("Post comp",snapshot.post_db)] {
            ui.vertical(|ui|{
                ui.set_width(100.0);
                ui.label(label);
                let (rect,_)=ui.allocate_exact_size(egui::vec2(54.0,160.0),egui::Sense::hover());
                let _=meter(ui,rect,levels,None);
                ui.label(egui::RichText::new(format!("L {:+.1}  R {:+.1}",levels[0],levels[1])).small());
            });
        }
        ui.vertical(|ui|{
            ui.label("Gain reduction");
            ui.add(egui::ProgressBar::new((snapshot.reduction_db/24.0).clamp(0.0,1.0))
                .desired_width(135.0).text(format!("{:.1} dB",snapshot.reduction_db)));
            ui.label(egui::RichText::new("Peak hold · excludes makeup").small().weak());
        });
    }).response.on_hover_text("Pre/Post comp are stereo sample peaks around the compressor, after EQ and before the fader. Raw PFL remains unprocessed. Reduction excludes makeup, with transition smoothing and display decay.");
    if let Some(error) = &snapshot.error {
        ui.colored_label(egui::Color32::YELLOW, format!("Dry fallback: {error}"));
        ui.label("Toggle compression off/on to retry.");
    } else {
        ui.label(if !config.enabled {
            "Bypassed"
        } else if snapshot.ready {
            "Active"
        } else {
            "Waiting for audio source"
        });
    }
    ui.label(
        egui::RichText::new("No lookahead added. Not a clipping-prevention limiter.")
            .small()
            .weak(),
    );
    if config != before {
        actions.commands.push(if bus {
            EngineCommand::SetAudioBusCompressor {
                uuid: uuid.clone(),
                config,
            }
        } else {
            EngineCommand::SetAudioSourceCompressor {
                uuid: uuid.clone(),
                config,
            }
        });
    }
}

fn settings_view_button(ui: &mut egui::Ui, expanded: bool) -> egui::Response {
    let label = if expanded {
        "Compact audio settings"
    } else {
        "Expand audio settings"
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(28.0, 24.0), egui::Sense::click());
    let visuals = ui.style().interact(&response);
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, visuals.bg_fill);
    let icon = egui::Rect::from_center_size(rect.center(), egui::vec2(12.0, 10.0));
    if expanded {
        painter.rect_stroke(
            icon.translate(egui::vec2(2.0, -2.0)),
            0.0,
            visuals.fg_stroke,
            egui::StrokeKind::Inside,
        );
        painter.rect_filled(icon.translate(egui::vec2(-2.0, 2.0)), 0.0, visuals.bg_fill);
        painter.rect_stroke(
            icon.translate(egui::vec2(-2.0, 2.0)),
            0.0,
            visuals.fg_stroke,
            egui::StrokeKind::Inside,
        );
    } else {
        painter.rect_stroke(icon, 0.0, visuals.fg_stroke, egui::StrokeKind::Inside);
        painter.line_segment(
            [
                icon.left_top() + egui::vec2(1.0, 2.0),
                icon.right_top() + egui::vec2(-1.0, 2.0),
            ],
            visuals.fg_stroke,
        );
    }
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
    response.on_hover_text(label)
}

fn render_strip_settings(
    ui: &mut egui::Ui,
    which: &Settings,
    data: &UIData,
    actions: &mut UIActions,
) {
    let mix = &data.audio_mix;
    match which {
        Settings::Bus(uuid) => {
            if let Some((i, bus)) = mix
                .buses
                .iter()
                .enumerate()
                .find(|(_, b)| &b.config.uuid == uuid)
            {
                bus_settings(ui, bus, i == 0, data, actions);
            }
        }
        Settings::Source(uuid) => {
            if let Some(source) = mix.sources.iter().find(|s| &s.uuid == uuid) {
                source_settings(ui, source, data, actions);
            }
        }
        Settings::Headphones => monitor_settings(ui, data, actions),
    }
}

fn next_bus_name(existing: usize) -> String {
    // After Master come A, B, C… as in vMix.
    let letter = char::from(b'A' + u8::try_from(existing.saturating_sub(1) % 26).unwrap_or(0));
    format!("Bus {letter}")
}

/// What a bus's send button says: M for the first bus (Master), else its
/// name without "Bus ", cut short.
fn bus_letter(name: &str, index: usize) -> String {
    if index == 0 {
        return "M".into();
    }
    let short = name.strip_prefix("Bus ").unwrap_or(name);
    short.chars().take(3).collect()
}

/// The blue spine naming a section, read from the bottom up.
fn section_label(ui: &mut egui::Ui, text: &str, height: f32) {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(SECTION_WIDTH, height), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, SECTION_BLUE);
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
    let at = egui::pos2(
        rect.center().x - galley.size().y / 2.0,
        rect.center().y + galley.size().x / 2.0,
    );
    painter.add(
        egui::epaint::TextShape::new(at, galley, egui::Color32::WHITE)
            .with_angle(-std::f32::consts::FRAC_PI_2),
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text));
}

/// The parts of a strip, top to bottom and left to right.
struct StripLayout {
    header: egui::Rect,
    balance: Option<egui::Rect>,
    buttons: egui::Rect,
    fader: egui::Rect,
    meter: egui::Rect,
    footer: egui::Rect,
}

impl StripLayout {
    fn new(rect: egui::Rect, balance: bool, footer: f32) -> Self {
        let header = egui::Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER));
        let mut top = header.bottom() + 4.0;
        let balance = balance.then(|| {
            let r = egui::Rect::from_min_size(
                egui::pos2(rect.left() + 8.0, top),
                egui::vec2(rect.width() - 16.0, BALANCE - 4.0),
            );
            top += BALANCE;
            r
        });
        let footer = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 4.0, rect.bottom() - footer - 4.0),
            egui::pos2(rect.right() - 4.0, rect.bottom() - 4.0),
        );
        let body_bottom = footer.top() - 6.0;
        let buttons = egui::Rect::from_min_max(
            egui::pos2(rect.left() + 5.0, top + 4.0),
            egui::pos2(rect.left() + 5.0 + BUTTON, body_bottom),
        );
        let meter = egui::Rect::from_min_max(
            egui::pos2(rect.right() - 5.0 - METER_WIDTH, top + 4.0),
            egui::pos2(rect.right() - 5.0, body_bottom),
        );
        let fader = egui::Rect::from_min_max(
            egui::pos2(buttons.right() + 4.0, top + 4.0),
            egui::pos2(meter.left() - 3.0, body_bottom),
        );
        Self {
            header,
            balance,
            buttons,
            fader,
            meter,
            footer,
        }
    }

    /// The `i`th button of the column beside the fader.
    fn button(&self, i: usize) -> egui::Rect {
        let top = self.buttons.top() + i as f32 * (BUTTON + BUTTON_GAP);
        egui::Rect::from_min_size(
            egui::pos2(self.buttons.left(), top),
            egui::vec2(BUTTON, BUTTON - 2.0),
        )
    }
}

fn strip_rect(ui: &mut egui::Ui, height: f32) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(STRIP_WIDTH, height), egui::Sense::hover());
    ui.painter().rect_filled(rect, 3.0, STRIP_BG);
    rect
}

/// The name bar across the top of a strip.
fn header(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    name: &str,
    fill: egui::Color32,
    id: egui::Id,
) -> egui::Response {
    let response = ui.interact(rect, id, egui::Sense::click());
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, fill);
    painter.with_clip_rect(rect.shrink(2.0)).text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        name,
        egui::FontId::proportional(13.0),
        egui::Color32::WHITE,
    );
    let label = name.to_string();
    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &label));
    response
}

// ── Output strips ───────────────────────────────────────────────────

fn bus_strip(
    ui: &mut egui::Ui,
    bus: &AudioBusSnapshot,
    master: bool,
    height: f32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let cfg = &bus.config;
    let rect = strip_rect(ui, height);
    let footer = if master { KNOB_ROW } else { SEND_ROW };
    let layout = StripLayout::new(rect, false, footer);
    let id = egui::Id::new(("audio_bus_strip", &cfg.uuid));
    let fill = if master { MASTER_HEADER } else { HEADER_BG };
    let name = header(ui, layout.header, &cfg.name, fill, id.with("header"))
        .on_hover_text("Double-click to rename; right-click for more");
    if name.double_clicked() {
        super::rename::open_audio_bus(ui.ctx(), &cfg.uuid, &cfg.name);
    }
    name.context_menu(|ui| {
        if ui.button("Rename bus…").clicked() {
            super::rename::open_audio_bus(ui.ctx(), &cfg.uuid, &cfg.name);
            ui.close();
        }
        if !master && ui.button("Remove bus").clicked() {
            actions.commands.push(EngineCommand::RemoveAudioBus {
                uuid: cfg.uuid.clone(),
            });
            ui.close();
        }
    });

    let gear = icon_button(
        ui,
        layout.button(0),
        id.with("gear"),
        Icon::Gear,
        false,
        BUTTON_ON,
        &format!("Settings {}", cfg.name),
    )
    .on_hover_text(format!("{}: sound device and delay", cfg.name));
    if gear.clicked() {
        toggle_settings(
            ui.ctx(),
            Settings::Bus(cfg.uuid.clone()),
            gear.rect.right_top(),
        );
    }

    let on = !cfg.muted;
    if icon_button(
        ui,
        layout.button(1),
        id.with("on"),
        Icon::Speaker,
        on,
        BUTTON_ON,
        &format!("On {}", cfg.name),
    )
    .on_hover_text(if on {
        format!("{} is on — click to turn it off", cfg.name)
    } else {
        format!("{} is off — click to turn it on", cfg.name)
    })
    .clicked()
    {
        actions.commands.push(EngineCommand::SetAudioBusMute {
            uuid: cfg.uuid.clone(),
            muted: on,
        });
    }

    if let Some(db) = fader(ui, layout.fader, id.with("fader"), cfg.gain_db, &cfg.name) {
        actions.commands.push(EngineCommand::SetAudioBusGain {
            uuid: cfg.uuid.clone(),
            gain_db: db,
        });
    }
    if strip_meter(ui, layout.meter, bus.meter_db, bus.peak_db, &cfg.name) {
        actions.commands.push(EngineCommand::ResetAudioMeter {
            uuid: cfg.uuid.clone(),
            bus: true,
        });
    }

    if master {
        headphones(ui, layout.footer, data, actions);
    } else {
        let device = match (&cfg.device, bus.device_ok) {
            (None, _) => "no device".to_string(),
            (Some(d), Some(false)) => format!("⚠ {d}"),
            (Some(d), _) => d.clone(),
        };
        ui.painter().with_clip_rect(layout.footer).text(
            layout.footer.center(),
            egui::Align2::CENTER_CENTER,
            device,
            egui::FontId::proportional(10.0),
            egui::Color32::GRAY,
        );
    }
}

fn bus_settings(
    ui: &mut egui::Ui,
    bus: &AudioBusSnapshot,
    master: bool,
    data: &UIData,
    actions: &mut UIActions,
) {
    let cfg = &bus.config;
    ui.label(egui::RichText::new(&cfg.name).strong());
    if master {
        ui.label(
            egui::RichText::new("Recordings and streams take this bus unless set otherwise")
                .small()
                .weak(),
        );
    }
    ui.horizontal(|ui| {
        ui.label("Plays on:");
        if let Some((device, first_channel)) = device_picker(
            ui,
            &format!("bus_device_{}", cfg.uuid),
            cfg.device.as_deref(),
            cfg.first_channel,
            bus.device_ok,
            data,
        ) {
            actions.commands.push(EngineCommand::SetAudioBusDevice {
                uuid: cfg.uuid.clone(),
                device,
                first_channel,
            });
        }
    });
    ui.horizontal(|ui| {
        ui.label("Delay:");
        egui::ComboBox::from_id_salt(format!("bus_delay_{}", cfg.uuid))
            .selected_text(format!("{} ms", cfg.delay_ms))
            .width(80.0)
            .show_ui(ui, |ui| {
                for ms in DELAYS_MS {
                    let current = (cfg.delay_ms - ms).abs() < 0.5;
                    if ui.selectable_label(current, format!("{ms} ms")).clicked() && !current {
                        actions.commands.push(EngineCommand::SetAudioBusDelay {
                            uuid: cfg.uuid.clone(),
                            delay_ms: ms,
                        });
                    }
                }
            })
            .response
            .on_hover_text("Hold the sound back to match a picture that arrives late");
    });
    ui.horizontal(|ui| {
        ui.label("Level:");
        if let Some(db) = level_field(ui, cfg.gain_db) {
            actions.commands.push(EngineCommand::SetAudioBusGain {
                uuid: cfg.uuid.clone(),
                gain_db: db,
            });
        }
    });
    if !master {
        ui.separator();
        if ui.button("Remove bus").clicked() {
            actions.commands.push(EngineCommand::RemoveAudioBus {
                uuid: cfg.uuid.clone(),
            });
        }
    }
}

/// The operator's headphones under Master: the icon opens where they play
/// and what they hear; the knob is their level.
fn headphones(ui: &mut egui::Ui, rect: egui::Rect, data: &UIData, actions: &mut UIActions) {
    let monitor = &data.audio_mix.monitor;
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 4.0, rect.center().y - 13.0),
        egui::vec2(BUTTON, 26.0),
    );
    let soloing = monitor.soloing;
    let icon = icon_button(
        ui,
        icon_rect,
        egui::Id::new("audio_headphones"),
        Icon::Headphones,
        soloing,
        SOLO_ON,
        "Headphones",
    )
    .on_hover_text(if soloing {
        "Headphones: hearing the soloed inputs (S)"
    } else {
        "Headphones: where they play and what they hear"
    });
    if icon.clicked() {
        toggle_settings(ui.ctx(), Settings::Headphones, icon.rect.right_top());
    }

    let knob_rect = egui::Rect::from_center_size(
        egui::pos2(rect.right() - 24.0, rect.center().y),
        egui::vec2(34.0, 34.0),
    );
    if let Some(db) = knob(
        ui,
        knob_rect,
        egui::Id::new("audio_headphones_level"),
        monitor.config.gain_db,
        "Headphones level",
    ) {
        actions
            .commands
            .push(EngineCommand::SetAudioMonitorGain { gain_db: db });
    }
}

fn monitor_settings(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let monitor = &data.audio_mix.monitor;
    let cfg = &monitor.config;
    ui.label(egui::RichText::new("Headphones").strong());
    ui.label(
        egui::RichText::new("For the operator only: nothing here reaches a bus")
            .small()
            .weak(),
    );
    ui.horizontal(|ui| {
        ui.label("Plays on:");
        if let Some((device, first_channel)) = device_picker(
            ui,
            "monitor_device",
            cfg.device.as_deref(),
            cfg.first_channel,
            monitor.device_ok,
            data,
        ) {
            actions.commands.push(EngineCommand::SetAudioMonitorDevice {
                device,
                first_channel,
            });
        }
    });
    if monitor.soloing {
        ui.label(
            egui::RichText::new(" hearing the soloed inputs ")
                .strong()
                .color(egui::Color32::BLACK)
                .background_color(SOLO_ON),
        );
        return;
    }
    ui.horizontal(|ui| {
        ui.label("Listen to:");
        let listening = cfg
            .listen
            .as_ref()
            .and_then(|u| data.audio_mix.buses.iter().find(|b| &b.config.uuid == u))
            .or_else(|| data.audio_mix.buses.first())
            .map_or("—".to_string(), |b| b.config.name.clone());
        egui::ComboBox::from_id_salt("monitor_listen")
            .selected_text(listening)
            .width(110.0)
            .show_ui(ui, |ui| {
                for (i, bus) in data.audio_mix.buses.iter().enumerate() {
                    let on = cfg.listen.as_deref() == Some(bus.config.uuid.as_str())
                        || (cfg.listen.is_none() && i == 0);
                    if ui.selectable_label(on, &bus.config.name).clicked() && !on {
                        actions.commands.push(EngineCommand::SetAudioMonitorListen {
                            bus_uuid: Some(bus.config.uuid.clone()),
                        });
                    }
                }
            })
            .response
            .on_hover_text("The bus heard when nothing is soloed");
    });
}

// ── Input strips ────────────────────────────────────────────────────

fn input_strip(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    height: f32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let buses = &data.audio_mix.buses;
    let rows = buses.len().div_ceil(SENDS_PER_ROW).max(1);
    let rect = strip_rect(ui, height);
    let layout = StripLayout::new(rect, true, SEND_ROW * rows as f32);
    let id = egui::Id::new(("audio_source_strip", &source.uuid));
    let name = &source.name;
    let fill = if source.on_air > 0.0 {
        ON_AIR_HEADER
    } else {
        HEADER_BG
    };
    let title = header(ui, layout.header, name, fill, id.with("header"))
        .on_hover_text(describe(source, data));
    let clip_deck = match &source.kind {
        AudioSourceKind::Clip { deck_uuid } => Some(deck_uuid.as_str()),
        AudioSourceKind::Live { .. } | AudioSourceKind::Input { .. } => None,
    };
    // A clip, an NDI source's sound or a camera's: switched with its picture.
    let follows_a_picture = clip_deck.is_some()
        || matches!(
            &source.kind,
            AudioSourceKind::Live { .. } | AudioSourceKind::Input { deck: Some(_), .. }
        );
    if title.double_clicked() {
        if let Some(deck) = clip_deck {
            super::channel_settings::open_layer_playback(ui.ctx(), deck);
        }
    }
    if matches!(&source.kind, AudioSourceKind::Input { .. }) {
        title.context_menu(|ui| {
            if ui.button("Remove input").clicked() {
                actions.commands.push(EngineCommand::RemoveAudioInput {
                    uuid: source.uuid.clone(),
                });
                ui.close();
            }
        });
    }
    if !source.live {
        ui.painter().text(
            layout.header.center_bottom() + egui::vec2(0.0, 2.0),
            egui::Align2::CENTER_TOP,
            missing(source),
            egui::FontId::proportional(10.0),
            egui::Color32::from_rgb(230, 150, 60),
        );
    }

    if let Some(r) = layout.balance {
        if let Some(b) = balance(ui, r, id.with("balance"), source.balance, name) {
            actions.commands.push(EngineCommand::SetAudioBalance {
                uuid: source.uuid.clone(),
                balance: b,
            });
        }
    }

    let gear = icon_button(
        ui,
        layout.button(0),
        id.with("gear"),
        Icon::Gear,
        false,
        BUTTON_ON,
        &format!("Settings {name}"),
    )
    .on_hover_text(format!("{name}: level, balance, Audio Auto, buses"));
    if gear.clicked() {
        toggle_settings(
            ui.ctx(),
            Settings::Source(source.uuid.clone()),
            gear.rect.right_top(),
        );
    }

    if icon_button(
        ui,
        layout.button(1),
        id.with("solo"),
        Icon::Text("S"),
        source.solo,
        SOLO_ON,
        &format!("Solo {name}"),
    )
    .on_hover_text(if source.solo {
        format!("Hearing {name} on the headphones — click to stop")
    } else {
        format!("Hear {name} on the headphones only, before its fader — even off")
    })
    .clicked()
    {
        actions.commands.push(EngineCommand::SetAudioSolo {
            uuid: source.uuid.clone(),
            solo: !source.solo,
        });
    }

    let auto_rect = layout.button(2);
    if follows_a_picture {
        if icon_button(
            ui,
            auto_rect,
            id.with("auto"),
            Icon::Arrows,
            source.auto,
            BUTTON_ON,
            &format!("Audio Auto {name}"),
        )
        .on_hover_text(if source.auto {
            format!(
                "Audio Auto: {name} comes on as it goes on air and goes off as it leaves — \
                 click to switch it by hand only"
            )
        } else {
            format!("Audio Auto is off: click to switch {name} on and off with its picture")
        })
        .clicked()
        {
            actions.commands.push(EngineCommand::SetAudioAuto {
                uuid: source.uuid.clone(),
                auto: !source.auto,
            });
        }
    } else {
        let painter = ui.painter();
        painter.rect_filled(auto_rect, 3.0, BUTTON_BG.gamma_multiply(0.5));
        paint_icon(painter, auto_rect, Icon::Arrows, false, true);
    }

    let on = !source.muted;
    if icon_button(
        ui,
        layout.button(3),
        id.with("on"),
        Icon::Speaker,
        on,
        BUTTON_ON,
        &format!("On {name}"),
    )
    .on_hover_text(if on {
        format!("{name} is on — click to turn it off")
    } else {
        format!("{name} is off — click to turn it on")
    })
    .clicked()
    {
        actions.commands.push(EngineCommand::SetAudioSourceMute {
            uuid: source.uuid.clone(),
            muted: on,
        });
    }

    if let Some(db) = fader(ui, layout.fader, id.with("fader"), source.gain_db, name) {
        actions.commands.push(EngineCommand::SetAudioSourceGain {
            uuid: source.uuid.clone(),
            gain_db: db,
        });
    }
    if strip_meter(ui, layout.meter, source.meter_db, source.peak_db, name) {
        actions.commands.push(EngineCommand::ResetAudioMeter {
            uuid: source.uuid.clone(),
            bus: false,
        });
    }
    sends(ui, layout.footer, source, data, actions);
}

/// What a strip says while its source is not there: an app's sound is there
/// while the app runs and its sound can be taken, a device while it is
/// plugged in.
fn missing(source: &AudioSourceSnapshot) -> &'static str {
    match (&source.kind, &source.refused) {
        (AudioSourceKind::Input { app: Some(_), .. }, Some(_)) => "not taken",
        (AudioSourceKind::Input { app: Some(_), .. }, None) => "not running",
        _ => "not connected",
    }
}

/// What a source is, for its name's tooltip.
fn describe(source: &AudioSourceSnapshot, data: &UIData) -> String {
    match &source.kind {
        AudioSourceKind::Clip { deck_uuid } => {
            let channel = data
                .channels
                .iter()
                .find(|c| c.decks.iter().any(|d| &d.uuid == deck_uuid))
                .map_or_else(String::new, |c| format!(" on {}", c.name));
            format!("{}{channel} — double-click for its settings", source.name)
        }
        AudioSourceKind::Live { deck_uuid } => {
            let channel = data
                .channels
                .iter()
                .find(|c| c.decks.iter().any(|d| &d.uuid == deck_uuid))
                .map_or_else(String::new, |c| format!(" on {}", c.name));
            format!("{}{channel}: its sound over NDI", source.name)
        }
        AudioSourceKind::Input { app: Some(app), .. } => {
            let why = source
                .refused
                .as_ref()
                .map_or_else(String::new, |why| format!(" — not taken: {why}"));
            format!(
                "{}: the sound of the app {app}, taken straight from it while it runs{why}",
                source.name
            )
        }
        AudioSourceKind::Input {
            device,
            left,
            right,
            ..
        } if left == right => format!("{device}, input {} (mono)", left + 1),
        AudioSourceKind::Input {
            device,
            left,
            right,
            ..
        } => format!("{device}, inputs {}-{}", left + 1, right + 1),
    }
}

/// A button per bus, lettered, lit where the source is sent.
fn sends(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    source: &AudioSourceSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let buses = &data.audio_mix.buses;
    let width = (rect.width() - 4.0 * (SENDS_PER_ROW as f32 - 1.0)) / SENDS_PER_ROW as f32;
    for (i, bus) in buses.iter().enumerate() {
        let (row, col) = (i / SENDS_PER_ROW, i % SENDS_PER_ROW);
        let r = egui::Rect::from_min_size(
            egui::pos2(
                rect.left() + col as f32 * (width + 4.0),
                rect.top() + row as f32 * SEND_ROW,
            ),
            egui::vec2(width, SEND_ROW - 3.0),
        );
        let on = source.sends.contains(&bus.config.uuid);
        let letter = bus_letter(&bus.config.name, i);
        let response = icon_button(
            ui,
            r,
            egui::Id::new(("audio_send", &source.uuid, &bus.config.uuid)),
            Icon::Text(&letter),
            on,
            BUTTON_ON,
            &format!("Send {} to {}", source.name, bus.config.name),
        )
        .on_hover_text(if on {
            format!("Sent to {} — click to stop", bus.config.name)
        } else {
            format!("Send to {}", bus.config.name)
        });
        if response.clicked() {
            actions.commands.push(EngineCommand::SetAudioSend {
                uuid: source.uuid.clone(),
                bus_uuid: bus.config.uuid.clone(),
                on: !on,
            });
        }
    }
}

fn source_settings(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new(&source.name).strong());
    ui.label(egui::RichText::new(describe(source, data)).small().weak());
    ui.horizontal(|ui| {
        ui.label("Level:");
        if let Some(db) = level_field(ui, source.gain_db) {
            actions.commands.push(EngineCommand::SetAudioSourceGain {
                uuid: source.uuid.clone(),
                gain_db: db,
            });
        }
    });
    ui.horizontal(|ui| {
        ui.label("Balance:");
        let mut b = source.balance;
        let changed = ui
            .add(egui::Slider::new(&mut b, -1.0..=1.0).show_value(false))
            .changed();
        if changed {
            actions.commands.push(EngineCommand::SetAudioBalance {
                uuid: source.uuid.clone(),
                balance: b,
            });
        }
        if ui.small_button("Centre").clicked() {
            actions.commands.push(EngineCommand::SetAudioBalance {
                uuid: source.uuid.clone(),
                balance: 0.0,
            });
        }
    });
    ui.horizontal(|ui| {
        ui.label("Delay:");
        let mut ms = source.delay_ms;
        let changed = ui
            .add(
                egui::DragValue::new(&mut ms)
                    .range(0.0..=crate::audio_mix::MAX_DELAY_MS)
                    .speed(1.0)
                    .max_decimals(0)
                    .suffix(" ms"),
            )
            .on_hover_text(
                "Hold this input's sound back to meet a picture that arrives later — \
                 a microphone with a camera over USB. Pre-fader listen hears it at once.",
            )
            .changed();
        if changed {
            actions.commands.push(EngineCommand::SetAudioSourceDelay {
                uuid: source.uuid.clone(),
                delay_ms: ms.round(),
            });
        }
    });
    if matches!(
        &source.kind,
        AudioSourceKind::Live { .. } | AudioSourceKind::Input { deck: Some(_), .. }
    ) {
        ui.separator();
        audio_auto_settings(ui, source, data, actions);
    }
    if let AudioSourceKind::Clip { deck_uuid } = &source.kind {
        ui.separator();
        audio_auto_settings(ui, source, data, actions);
        ui.separator();
        if ui
            .button("Input settings…")
            .on_hover_text("Play, pause and restart as it goes on and off air")
            .clicked()
        {
            super::channel_settings::open_layer_playback(ui.ctx(), deck_uuid);
            ui.ctx()
                .memory_mut(|m| m.data.remove::<(Settings, egui::Pos2)>(settings_id()));
        }
    } else {
        ui.separator();
        if ui.button("Remove input").clicked() {
            actions.commands.push(EngineCommand::RemoveAudioInput {
                uuid: source.uuid.clone(),
            });
        }
    }
}

/// Audio Auto and where it looks, for a clip: in the strip's settings and in
/// the clip's Input Settings.
pub(super) fn audio_auto_settings(
    ui: &mut egui::Ui,
    source: &AudioSourceSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let mut auto = source.auto;
    if ui
        .checkbox(&mut auto, "Sound on while on air (Audio Auto)")
        .on_hover_text(
            "The sound comes on as it goes on air and goes off as it leaves, fading with a \
             fade. Its On button can still be pressed in between",
        )
        .changed()
    {
        actions.commands.push(EngineCommand::SetAudioAuto {
            uuid: source.uuid.clone(),
            auto,
        });
    }
    ui.add_enabled_ui(source.auto, |ui| {
        ui.horizontal(|ui| {
            ui.label("On air on:");
            let current = match source.follow.as_deref() {
                None => "Main or any bus".to_string(),
                Some(FOLLOW_MAIN) => "Main".to_string(),
                Some(uuid) => data
                    .buses
                    .iter()
                    .find(|b| b.bus.uuid == uuid)
                    .map_or_else(|| "⚠ removed bus".to_string(), |b| b.bus.name.clone()),
            };
            let mut pick = |follow: Option<String>| {
                actions.commands.push(EngineCommand::SetAudioFollow {
                    uuid: source.uuid.clone(),
                    bus_uuid: follow,
                });
            };
            egui::ComboBox::from_id_salt(("audio_follow", &source.uuid))
                .selected_text(current)
                .width(130.0)
                .show_ui(ui, |ui| {
                    let options = std::iter::once((None, "Main or any bus".to_string()))
                        .chain(std::iter::once((
                            Some(FOLLOW_MAIN.to_string()),
                            "Main".to_string(),
                        )))
                        .chain(
                            data.buses
                                .iter()
                                .map(|b| (Some(b.bus.uuid.clone()), b.bus.name.clone())),
                        );
                    for (follow, label) in options {
                        let on = source.follow == follow;
                        if ui.selectable_label(on, label).clicked() && !on {
                            pick(follow);
                        }
                    }
                });
        });
    });
}

// ── The last column: adding things ──────────────────────────────────

fn add_column(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    ui.vertical(|ui| {
        ui.set_width(96.0);
        render_add_input(ui, data, actions);
        if ui
            .button("+ Bus")
            .on_hover_text(
                "Add an audio bus: a mix of its own for a device, a stream or a recording",
            )
            .clicked()
        {
            actions.commands.push(EngineCommand::AddAudioBus {
                name: next_bus_name(data.audio_mix.buses.len()),
            });
        }
        if ui
            .button("Rescan")
            .on_hover_text("Look for sound devices again, after plugging one in")
            .clicked()
        {
            actions.commands.push(EngineCommand::RescanAudioDevices);
        }
        if data.audio_mix.sources.is_empty() {
            ui.label(
                egui::RichText::new("Clips with sound appear here; add a microphone with + Input.")
                    .small()
                    .weak(),
            );
        }
    });
}

fn render_add_input(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    ui.menu_button("+ Input", |ui| {
        if data.audio_mix.input_devices.is_empty() {
            ui.label(egui::RichText::new("No input devices listed — try Rescan").small());
        }
        for dev in &data.audio_mix.input_devices {
            ui.menu_button(&dev.name, |ui| {
                let mut add = |ui: &mut egui::Ui, label: String, left: u16, right: u16| {
                    if ui.button(&label).clicked() {
                        actions.commands.push(EngineCommand::AddAudioInput {
                            name: if dev.channels > 2 {
                                format!("{} {label}", dev.name)
                            } else {
                                dev.name.clone()
                            },
                            device: dev.name.clone(),
                            left,
                            right,
                        });
                        ui.close();
                    }
                };
                for pair in (0..dev.channels.saturating_sub(1)).step_by(2) {
                    add(
                        ui,
                        format!("Stereo {}-{}", pair + 1, pair + 2),
                        pair,
                        pair + 1,
                    );
                }
                for ch in 0..dev.channels {
                    add(ui, format!("Mono {}", ch + 1), ch, ch);
                }
            });
        }
        ui.separator();
        ui.menu_button("App sound", |ui| {
            if data.audio_mix.apps.is_empty() {
                ui.label(
                    egui::RichText::new("No app has made sound yet — start the call first").small(),
                );
            }
            for app in &data.audio_mix.apps {
                let label = if app.playing {
                    format!("{}  ● sound", app.name)
                } else {
                    app.name.clone()
                };
                if ui.button(label).on_hover_text(&app.bundle_id).clicked() {
                    actions.commands.push(EngineCommand::AddAppSound {
                        app: app.bundle_id.clone(),
                        name: app.name.clone(),
                    });
                    ui.close();
                }
            }
        })
        .response
        .on_hover_text(
            "An app's sound — a Zoom or Teams call's — straight from the app, no cable or \
             driver. macOS asks once whether Nida may record other apps' sound.",
        );
    })
    .response
    .on_hover_text("Add a microphone, line input, a USB mixer's return, or an app's sound");
}

/// A sound-device menu, with the output pair for multi-channel interfaces.
/// Returns the new choice when one is picked.
fn device_picker(
    ui: &mut egui::Ui,
    id: &str,
    device: Option<&str>,
    first_channel: u16,
    device_ok: Option<bool>,
    data: &UIData,
) -> Option<(Option<String>, u16)> {
    let label = match (device, device_ok) {
        (None, _) => "Out: none".to_string(),
        (Some(d), Some(false)) => format!("⚠ {d}"),
        (Some(d), _) if first_channel > 0 => {
            format!("{d} {}-{}", first_channel + 1, first_channel + 2)
        }
        (Some(d), _) => d.to_string(),
    };
    let mut picked = None;
    egui::ComboBox::from_id_salt(id)
        .selected_text(label)
        .width(140.0)
        .show_ui(ui, |ui| {
            if ui.selectable_label(device.is_none(), "None").clicked() && device.is_some() {
                picked = Some((None, 0));
            }
            for name in &data.audio_mix.output_devices {
                for pair in 0..4u16 {
                    let first = pair * 2;
                    let text = if pair == 0 {
                        name.clone()
                    } else {
                        format!("{name} — outputs {}-{}", first + 1, first + 2)
                    };
                    let on = device == Some(name.as_str()) && first_channel == first;
                    if ui.selectable_label(on, text).clicked() && !on {
                        picked = Some((Some(name.clone()), first));
                    }
                }
            }
            if data.audio_mix.output_devices.is_empty() {
                ui.label(
                    egui::RichText::new("No sound devices listed yet")
                        .small()
                        .weak(),
                );
            }
        })
        .response
        .on_hover_text("The sound device this plays on");
    picked
}

// ── Controls ────────────────────────────────────────────────────────

/// A level in dB typed or dragged; -96 is off.
fn level_field(ui: &mut egui::Ui, gain_db: f32) -> Option<f32> {
    let mut db = gain_db;
    let changed = ui
        .add(
            egui::DragValue::new(&mut db)
                .range(-96.0..=12.0)
                .speed(0.2)
                .fixed_decimals(1)
                .suffix(" dB"),
        )
        .changed();
    changed.then_some(db)
}

/// Where on the track a level sits, 0 (bottom, off) to 1.
fn db_to_travel(db: f32) -> f32 {
    let db = db.clamp(TAPER[0].0, TAPER[TAPER.len() - 1].0);
    for pair in TAPER.windows(2) {
        let ((d0, t0), (d1, t1)) = (pair[0], pair[1]);
        if db <= d1 {
            return t0 + (t1 - t0) * (db - d0) / (d1 - d0);
        }
    }
    1.0
}

/// The level at a point of the track.
fn travel_to_db(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t <= TAPER[1].1 * 0.5 {
        // The bottom of the travel is the off stop.
        return -96.0;
    }
    for pair in TAPER.windows(2) {
        let ((d0, t0), (d1, t1)) = (pair[0], pair[1]);
        if t <= t1 {
            let db = d0 + (d1 - d0) * (t - t0) / (t1 - t0);
            return db.max(-60.0);
        }
    }
    12.0
}

fn db_text(db: f32) -> String {
    if db <= -60.0 {
        "off".into()
    } else {
        format!("{db:+.1}")
    }
}

/// An upright fader: drag the cap (Shift for fine), double-click for 0 dB.
/// Returns the new level while it moves.
fn fader(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    gain_db: f32,
    what: &str,
) -> Option<f32> {
    let value_h = 13.0;
    let track = egui::Rect::from_min_max(
        rect.min + egui::vec2(0.0, 8.0),
        egui::pos2(rect.right(), rect.bottom() - value_h - 6.0),
    );
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    let travel = db_to_travel(gain_db);
    let mut new_db = None;
    if response.double_clicked() {
        new_db = Some(0.0);
    } else if response.drag_started() {
        ui.memory_mut(|m| m.data.insert_temp(id, travel));
    } else if response.dragged() {
        let start = ui.memory(|m| m.data.get_temp::<f32>(id)).unwrap_or(travel);
        let (origin, now, fine) = ui.input(|i| {
            (
                i.pointer.press_origin(),
                i.pointer.interact_pos(),
                i.modifiers.shift,
            )
        });
        if let (Some(origin), Some(now)) = (origin, now) {
            let scale = if fine { 0.2 } else { 1.0 };
            let t = start - (now.y - origin.y) / track.height().max(1.0) * scale;
            let db = travel_to_db(t);
            if (db - gain_db).abs() > 0.01 {
                new_db = Some(db);
            }
        }
    }

    let painter = ui.painter();
    let x = track.center().x;
    for db in TICKS {
        let y = track.bottom() - db_to_travel(db) * track.height();
        let long = db == 0.0;
        let half = if long { 13.0 } else { 9.0 };
        painter.line_segment(
            [egui::pos2(x - half, y), egui::pos2(x - 5.0, y)],
            egui::Stroke::new(1.0, TICK),
        );
        painter.line_segment(
            [egui::pos2(x + 5.0, y), egui::pos2(x + half, y)],
            egui::Stroke::new(1.0, TICK),
        );
        if LABELLED_TICKS.contains(&db) && track.height() > 150.0 {
            painter.text(
                egui::pos2(track.left(), y),
                egui::Align2::LEFT_CENTER,
                format!("{}", db.abs()),
                egui::FontId::proportional(8.5),
                TICK,
            );
        }
    }
    painter.rect_filled(
        egui::Rect::from_center_size(
            egui::pos2(x, track.center().y),
            egui::vec2(3.0, track.height()),
        ),
        1.0,
        egui::Color32::from_rgb(4, 4, 6),
    );
    let cap_y = track.bottom() - travel * track.height();
    let cap = egui::Rect::from_center_size(egui::pos2(x, cap_y), egui::vec2(30.0, 14.0));
    let lit = response.hovered() || response.dragged();
    painter.rect_filled(
        cap,
        2.0,
        if lit {
            egui::Color32::from_rgb(235, 238, 242)
        } else {
            egui::Color32::from_rgb(200, 204, 210)
        },
    );
    painter.line_segment(
        [
            egui::pos2(cap.left() + 3.0, cap_y),
            egui::pos2(cap.right() - 3.0, cap_y),
        ],
        egui::Stroke::new(1.5, egui::Color32::from_rgb(40, 40, 46)),
    );
    painter.text(
        egui::pos2(x, rect.bottom() - value_h / 2.0),
        egui::Align2::CENTER_CENTER,
        db_text(gain_db),
        egui::FontId::proportional(10.0),
        egui::Color32::LIGHT_GRAY,
    );
    let label = format!("Level {what}");
    response.widget_info(move || egui::WidgetInfo::slider(true, f64::from(gain_db), &label));
    let _ = response.on_hover_text(format!(
        "{what}: {} dB — drag (Shift: fine), double-click for 0 dB",
        db_text(gain_db)
    ));
    new_db
}

/// Left–right balance: drag sideways, double-click to centre.
fn balance(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    value: f32,
    what: &str,
) -> Option<f32> {
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    let mut new = None;
    if response.double_clicked() {
        new = Some(0.0);
    } else if response.drag_started() {
        ui.memory_mut(|m| m.data.insert_temp(id, value));
    } else if response.dragged() {
        let start = ui.memory(|m| m.data.get_temp::<f32>(id)).unwrap_or(value);
        let (origin, now) = ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos()));
        if let (Some(origin), Some(now)) = (origin, now) {
            let b = (start + (now.x - origin.x) / (rect.width() / 2.0).max(1.0)).clamp(-1.0, 1.0);
            // A little catch at the centre, where it is wanted most.
            let b = if b.abs() < 0.04 { 0.0 } else { b };
            if (b - value).abs() > 0.001 {
                new = Some(b);
            }
        }
    }
    let painter = ui.painter();
    let y = rect.center().y;
    painter.rect_filled(
        egui::Rect::from_center_size(rect.center(), egui::vec2(rect.width(), 4.0)),
        2.0,
        egui::Color32::from_rgb(50, 54, 64),
    );
    painter.line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        egui::Stroke::new(1.0, TICK),
    );
    let x = rect.center().x + value.clamp(-1.0, 1.0) * (rect.width() / 2.0 - 5.0);
    painter.rect_filled(
        egui::Rect::from_center_size(egui::pos2(x, y), egui::vec2(9.0, rect.height())),
        2.0,
        egui::Color32::from_rgb(200, 204, 210),
    );
    let label = format!("Balance {what}");
    response.widget_info(move || egui::WidgetInfo::slider(true, f64::from(value), &label));
    let text = if value.abs() < 0.005 {
        "centre".to_string()
    } else if value < 0.0 {
        format!("left {:.0}%", -value * 100.0)
    } else {
        format!("right {:.0}%", value * 100.0)
    };
    let _ = response.on_hover_text(format!("Balance: {text} — drag, double-click to centre"));
    new
}

/// A round level knob: drag up or down, double-click for 0 dB.
fn knob(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    gain_db: f32,
    what: &str,
) -> Option<f32> {
    let response = ui.interact(rect, id, egui::Sense::click_and_drag());
    let travel = db_to_travel(gain_db);
    let mut new = None;
    if response.double_clicked() {
        new = Some(0.0);
    } else if response.drag_started() {
        ui.memory_mut(|m| m.data.insert_temp(id, travel));
    } else if response.dragged() {
        let start = ui.memory(|m| m.data.get_temp::<f32>(id)).unwrap_or(travel);
        let (origin, now) = ui.input(|i| (i.pointer.press_origin(), i.pointer.interact_pos()));
        if let (Some(origin), Some(now)) = (origin, now) {
            let db = travel_to_db(start - (now.y - origin.y) / 120.0);
            if (db - gain_db).abs() > 0.01 {
                new = Some(db);
            }
        }
    }
    let painter = ui.painter();
    let c = rect.center();
    let r = rect.width().min(rect.height()) / 2.0 - 1.0;
    painter.circle_filled(c, r, egui::Color32::from_rgb(52, 56, 66));
    painter.circle_stroke(
        c,
        r,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(90, 94, 104)),
    );
    // -135° (off) to +135° (+12 dB), from straight up.
    let angle = (-135.0 + 270.0 * travel).to_radians();
    let tip = c + egui::vec2(angle.sin(), -angle.cos()) * (r - 4.0);
    painter.line_segment([c, tip], egui::Stroke::new(2.5, egui::Color32::WHITE));
    let label = what.to_string();
    response.widget_info(move || egui::WidgetInfo::slider(true, f64::from(gain_db), &label));
    let _ = response.on_hover_text(format!(
        "{what}: {} dB — drag up or down, double-click for 0 dB",
        db_text(gain_db)
    ));
    new
}

/// Stereo sample peaks use a dBFS scale independent of fader gain.
fn meter_travel(db: f32) -> f32 {
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

fn meter_color(db: f32) -> egui::Color32 {
    if db >= 0.0 {
        egui::Color32::from_rgb(230, 60, 50)
    } else if db >= -12.0 {
        egui::Color32::from_rgb(225, 200, 60)
    } else {
        egui::Color32::from_rgb(70, 215, 90)
    }
}

fn peak_text(db: f32) -> String {
    if db <= -96.0 {
        "−∞".into()
    } else {
        format!("{db:.1}")
    }
}

fn strip_meter(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    levels: [f32; 2],
    peaks: [f32; 2],
    name: &str,
) -> bool {
    let response = meter(ui, rect, levels, Some(peaks));
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Reset peak {name}"))
    });
    response.clicked()
}

pub(super) fn meter(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    meter_db: [f32; 2],
    held: Option<[f32; 2]>,
) -> egui::Response {
    let response = ui.interact(
        rect,
        ui.id()
            .with(("audio_meter", rect.left().to_bits(), rect.top().to_bits())),
        if held.is_some() {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let peaks = held.unwrap_or(meter_db);
    let maximum = peaks[0].max(peaks[1]);
    let track = egui::Rect::from_min_max(
        rect.min + egui::vec2(24.0, 20.0),
        egui::pos2(rect.right(), rect.bottom() - 30.0),
    );
    let painter = ui.painter();
    painter.text(
        egui::pos2(rect.left(), rect.top() + 6.0),
        egui::Align2::LEFT_CENTER,
        "dBFS",
        egui::FontId::proportional(8.0),
        TICK,
    );
    let clip = egui::Rect::from_min_max(
        egui::pos2(track.left(), rect.top() + 2.0),
        egui::pos2(track.right(), rect.top() + 11.0),
    );
    painter.rect_filled(
        clip,
        1.0,
        if maximum >= 0.0 {
            meter_color(0.0)
        } else {
            egui::Color32::from_rgb(45, 30, 30)
        },
    );
    for db in [0_i16, -3, -6, -12, -18, -24, -36, -48] {
        let y = track.bottom() - meter_travel(f32::from(db)) * track.height();
        if db != -3 || track.height() >= 200.0 {
            painter.text(
                egui::pos2(track.left() - 3.0, y),
                egui::Align2::RIGHT_CENTER,
                format!("{db}"),
                egui::FontId::proportional(8.5),
                if db == -6 { egui::Color32::WHITE } else { TICK },
            );
        }
        painter.line_segment(
            [
                egui::pos2(track.left() - 2.0, y),
                egui::pos2(track.right(), y),
            ],
            egui::Stroke::new(0.5, TICK.gamma_multiply(0.35)),
        );
    }
    let half = track.width() / 2.0;
    for (i, db) in meter_db.iter().enumerate() {
        let bar = egui::Rect::from_min_size(
            egui::pos2(track.left() + half * i as f32 + 0.5, track.top()),
            egui::vec2(half - 1.5, track.height()),
        );
        painter.rect_filled(bar, 1.0, egui::Color32::from_rgb(8, 30, 12));
        for (low, high, color) in [
            (-60.0, -12.0, meter_color(-24.0)),
            (-12.0, 0.0, meter_color(-6.0)),
        ] {
            let top = (*db).min(high);
            if top > low {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(bar.left(), bar.bottom() - meter_travel(top) * bar.height()),
                        egui::pos2(bar.right(), bar.bottom() - meter_travel(low) * bar.height()),
                    ),
                    1.0,
                    color,
                );
            }
        }
        if held.is_some() && peaks[i] > -60.0 {
            let y = bar.bottom() - meter_travel(peaks[i]) * bar.height();
            painter.line_segment(
                [egui::pos2(bar.left(), y), egui::pos2(bar.right(), y)],
                egui::Stroke::new(1.5, egui::Color32::WHITE),
            );
        }
    }
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 19.0),
        egui::Align2::CENTER_CENTER,
        if held.is_some() { "Peak" } else { "Now" },
        egui::FontId::proportional(8.0),
        TICK,
    );
    painter.text(
        egui::pos2(rect.center().x, rect.bottom() - 6.0),
        egui::Align2::CENTER_CENTER,
        peak_text(maximum),
        egui::FontId::proportional(9.0),
        if maximum >= 0.0 {
            meter_color(0.0)
        } else {
            egui::Color32::WHITE
        },
    );
    response.on_hover_text(format!("Stereo sample peaks, dBFS (not fader gain or true peak).\nNow L {} / R {}\nPeak L {} / R {}{}",
        peak_text(meter_db[0]),peak_text(meter_db[1]),peak_text(peaks[0]),peak_text(peaks[1]),
        if held.is_some(){"\nPeaks and 0 dBFS warning latch on the audio thread. Click to reset; sound is unchanged."}else{""}))
}

#[derive(Clone, Copy)]
enum Icon<'a> {
    Gear,
    Speaker,
    Arrows,
    Headphones,
    Text(&'a str),
}

/// A square button with a drawn icon, lit when `on`.
fn icon_button(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    icon: Icon<'_>,
    on: bool,
    on_color: egui::Color32,
    label: &str,
) -> egui::Response {
    let response = ui.interact(rect, id, egui::Sense::click());
    let fill = match (on, response.hovered()) {
        (true, _) => on_color,
        (false, true) => BUTTON_BG.gamma_multiply(1.4),
        (false, false) => BUTTON_BG,
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 3.0, fill);
    paint_icon(painter, rect, icon, on && on_color == SOLO_ON, false);
    let label = label.to_string();
    response.widget_info(move || {
        egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, &label)
    });
    response
}

fn paint_icon(painter: &egui::Painter, rect: egui::Rect, icon: Icon<'_>, dark: bool, dim: bool) {
    let ink = if dark {
        egui::Color32::BLACK
    } else if dim {
        egui::Color32::from_gray(80)
    } else {
        egui::Color32::WHITE
    };
    let stroke = egui::Stroke::new(1.6, ink);
    let c = rect.center();
    match icon {
        Icon::Text(text) => {
            painter.text(
                c,
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(13.0),
                ink,
            );
        }
        Icon::Gear => {
            for i in 0..8 {
                let a = i as f32 * std::f32::consts::FRAC_PI_4;
                let dir = egui::vec2(a.cos(), a.sin());
                painter.line_segment([c + dir * 5.0, c + dir * 8.5], egui::Stroke::new(2.6, ink));
            }
            painter.circle_filled(c, 6.0, ink);
            painter.circle_filled(c, 2.6, BUTTON_BG);
        }
        Icon::Speaker => {
            let body = [
                c + egui::vec2(-8.0, -3.0),
                c + egui::vec2(-4.5, -3.0),
                c + egui::vec2(0.5, -7.5),
                c + egui::vec2(0.5, 7.5),
                c + egui::vec2(-4.5, 3.0),
                c + egui::vec2(-8.0, 3.0),
            ];
            painter.add(egui::Shape::convex_polygon(
                body.to_vec(),
                ink,
                egui::Stroke::NONE,
            ));
            for r in [4.5_f32, 8.0] {
                let arc: Vec<egui::Pos2> = (-4..=4)
                    .map(|k| {
                        let a = k as f32 * 0.17;
                        c + egui::vec2(1.5 + r * a.cos(), r * a.sin())
                    })
                    .collect();
                painter.add(egui::Shape::line(arc, stroke));
            }
        }
        Icon::Arrows => {
            let (green, orange) = if dim {
                (ink, ink)
            } else {
                (ARROW_GREEN, ARROW_ORANGE)
            };
            let arrow = |y: f32, right: bool, color: egui::Color32| {
                let s = egui::Stroke::new(2.4, color);
                let (from, to) = if right { (-8.0, 8.0) } else { (8.0, -8.0) };
                let head = if right { -1.0 } else { 1.0 };
                painter.line_segment([c + egui::vec2(from, y), c + egui::vec2(to, y)], s);
                painter.line_segment(
                    [
                        c + egui::vec2(to, y),
                        c + egui::vec2(to + head * 4.5, y - 3.5),
                    ],
                    s,
                );
                painter.line_segment(
                    [
                        c + egui::vec2(to, y),
                        c + egui::vec2(to + head * 4.5, y + 3.5),
                    ],
                    s,
                );
            };
            arrow(-4.0, true, green);
            arrow(4.0, false, orange);
        }
        Icon::Headphones => {
            let arc: Vec<egui::Pos2> = (0..=12)
                .map(|k| {
                    let a = std::f32::consts::PI * (1.0 + k as f32 / 12.0);
                    c + egui::vec2(7.5 * a.cos(), 7.5 * a.sin() + 2.0)
                })
                .collect();
            painter.add(egui::Shape::line(arc, egui::Stroke::new(2.0, ink)));
            for side in [-1.0_f32, 1.0] {
                painter.rect_filled(
                    egui::Rect::from_center_size(
                        c + egui::vec2(side * 7.0, 5.0),
                        egui::vec2(4.0, 7.0),
                    ),
                    1.0,
                    ink,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    fn bus(uuid: &str, name: &str) -> AudioBusSnapshot {
        AudioBusSnapshot {
            equalizer: crate::audio_mix::EqualizerSnapshot::default(),
            peak_db: [-20.0; 2],
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            config: crate::audio_mix::AudioBusConfig {
                equalizer: crate::audio_mix::EqualizerConfig::default(),
                compressor: crate::audio_mix::CompressorConfig::default(),
                uuid: uuid.into(),
                name: name.into(),
                gain_db: 0.0,
                muted: false,
                delay_ms: 0.0,
                device: None,
                first_channel: 0,
            },
            device_ok: None,
            meter_db: [-20.0, -20.0],
        }
    }

    fn clip(uuid: &str, name: &str) -> AudioSourceSnapshot {
        AudioSourceSnapshot {
            equalizer: crate::audio_mix::EqualizerSnapshot::default(),
            peak_db: [-96.0; 2],
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            uuid: uuid.into(),
            name: name.into(),
            kind: AudioSourceKind::Clip {
                deck_uuid: uuid.into(),
            },
            gain_db: 0.0,
            muted: true,
            sends: vec!["amaster1".into()],
            auto: true,
            follow: None,
            balance: 0.0,
            delay_ms: 0.0,
            on_air: 0.0,
            gate: 1.0,
            solo: false,
            live: true,
            meter_db: [-96.0, -96.0],
            refused: None,
        }
    }

    fn mixer_data() -> UIData {
        let mut data = UIData::test_fixture();
        let mut mic = clip("mic00001", "Lectern mic");
        mic.kind = AudioSourceKind::Input {
            device: "USB Mixer".into(),
            left: 0,
            right: 0,
            deck: None,
            app: None,
        };
        mic.auto = false;
        mic.muted = false;
        data.audio_mix = crate::audio_mix::AudioMixSnapshot {
            buses: vec![bus("amaster1", "Master"), bus("astream1", "Bus A")],
            sources: vec![clip("a0000001", "intro.mp4"), mic],
            monitor: crate::audio_mix::AudioMonitorSnapshot::default(),
            output_devices: vec!["Mac mini Speakers".into(), "BMD HDMI".into()],
            input_devices: vec![crate::audio_mix::AudioInputDevice {
                name: "USB Mixer".into(),
                channels: 2,
            }],
            apps: vec![
                crate::audio::app_sound::RunningApp {
                    bundle_id: "us.zoom.xos".into(),
                    name: "zoom.us".into(),
                    playing: true,
                },
                crate::audio::app_sound::RunningApp {
                    bundle_id: "com.microsoft.teams2".into(),
                    name: "Microsoft Teams".into(),
                    playing: false,
                },
            ],
        };
        data
    }

    fn harness<'a>(data: &'a UIData, actions: &'a mut UIActions) -> egui_kittest::Harness<'a> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(760.0, 420.0))
            // Short frames, so two clicks fall within egui's double-click time.
            .with_step_dt(1.0 / 60.0)
            .build_ui(move |ui| render_audio_mixer_section(ui, data, actions))
    }

    fn run(data: &UIData, clicks: &[&str]) -> UIActions {
        let mut actions = UIActions::new();
        {
            let mut harness = harness(data, &mut actions);
            harness.run();
            for click in clicks {
                harness.get_by_label(click).click();
                harness.run_steps(3);
            }
        }
        actions
    }

    fn pushed(actions: &UIActions, what: impl Fn(&EngineCommand) -> bool) -> bool {
        actions.commands.iter().any(what)
    }

    /// An audio bus's name offers Rename… on a right-click — Master's too —
    /// and asks for the new name; a double-click does the same.
    #[test]
    fn settings_views_share_controls_and_remember_the_last_view_without_commands() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            harness.get_by_label("Settings Master").click();
            harness.run();
            let expand = harness.get_by_label("Expand audio settings").rect();
            let close = harness.get_by_label("Close audio settings").rect();
            assert!(expand.right() <= close.left());
            assert!((expand.center().y - close.center().y).abs() < 1.0);
            harness.get_by_label("Expand audio settings").click();
            harness.run();
            let restore = harness.get_by_label("Compact audio settings").rect();
            let close = harness.get_by_label("Close audio settings").rect();
            assert!(restore.right() <= close.left());
            let category = harness.get_by_label("General").rect();
            let content = harness.get_by_label("General settings").rect();
            assert!(category.right() < content.left());
            harness.get_by_label("General").click();
            harness.run();
            harness.get_by_value("Out: none");
            harness.get_by_value("0 ms");
            harness.get_by_label("Close audio settings").click();
            harness.run();
            harness.get_by_label("Settings Master").click();
            harness.run();
            harness.get_by_label("Compact audio settings").click();
            harness.run();
            harness.get_by_label("Expand audio settings");
            assert!(harness.query_by_label("General settings").is_none());
            harness.get_by_value("Out: none");
        }
        assert!(actions.commands.is_empty());
    }

    #[test]
    fn dynamics_navigation_is_local_and_enable_targets_only_the_selected_bus() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            for label in ["Settings Master", "Expand audio settings", "Dynamics"] {
                harness.get_by_label(label).click();
                harness.run();
            }
            assert!(
                harness.get_by_label("General").rect().top()
                    < harness.get_by_label("Dynamics").rect().top()
            );
            harness.get_by_label("Pre comp");
            harness.get_by_label("Post comp");
            harness.get_by_label("Enable compressor").click();
            harness.run();
        }
        assert_eq!(actions.commands.len(), 1, "{:?}", actions.commands);
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::SetAudioBusCompressor{uuid,config}
            if uuid=="amaster1" && config.enabled)
        ));
    }

    #[test]
    fn dynamics_reset_targets_the_source_and_disables_compression() {
        let mut data = mixer_data();
        data.audio_mix.sources[0].dynamics.config.enabled = true;
        data.audio_mix.sources[0].dynamics.config.ratio = 9.0;
        let uuid = data.audio_mix.sources[0].uuid.clone();
        let name = data.audio_mix.sources[0].name.clone();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            for label in [
                format!("Settings {name}"),
                "Expand audio settings".into(),
                "Dynamics".into(),
                "Reset compressor".into(),
            ] {
                harness.get_by_label(&label).click();
                harness.run();
            }
        }
        assert_eq!(actions.commands.len(), 1, "{:?}", actions.commands);
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::SetAudioSourceCompressor{uuid:target,config}
            if target==&uuid && *config==crate::audio_mix::CompressorConfig::default())
        ));
    }

    #[test]
    fn equalizer_navigation_is_local_and_enable_targets_the_selected_bus() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            for label in ["Settings Master", "Expand audio settings", "Equalizer"] {
                harness.get_by_label(label).click();
                harness.run();
            }
            harness.get_by_label("Equalizer settings");
            harness.get_by_label("EQ 2 point");
            harness.get_by_label("Enable EQ").click();
            harness.run();
        }
        assert_eq!(actions.commands.len(), 1, "{:?}", actions.commands);
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::SetAudioBusEqualizer{uuid,config} if uuid=="amaster1" && config.enabled)
        ));
    }

    #[test]
    fn equalizer_reset_targets_only_the_selected_source_and_disables_eq() {
        let mut data = mixer_data();
        data.audio_mix.sources[0].equalizer.config.enabled = true;
        data.audio_mix.sources[0].equalizer.config.bands[1].gain_db = 6.0;
        let uuid = data.audio_mix.sources[0].uuid.clone();
        let name = data.audio_mix.sources[0].name.clone();
        let actions = run(
            &data,
            &[
                &format!("Settings {name}"),
                "Expand audio settings",
                "Equalizer",
                "Reset EQ",
            ],
        );
        assert_eq!(actions.commands.len(), 1, "{:?}", actions.commands);
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::SetAudioSourceEqualizer{uuid:target,config} if target==&uuid && *config==crate::audio_mix::EqualizerConfig::default())
        ));
    }

    #[test]
    fn dragging_an_eq_point_changes_its_real_band_without_enabling_the_whole_eq() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            for label in ["Settings Master", "Expand audio settings", "Equalizer"] {
                harness.get_by_label(label).click();
                harness.run();
            }
            let from = harness.get_by_label("EQ 2 point").rect().center();
            harness.hover_at(from);
            harness.run_steps(2);
            harness.drag_at(from);
            harness.run_steps(2);
            harness.hover_at(from + egui::vec2(15.0, -12.0));
            harness.run_steps(2);
            harness.drop_at(from + egui::vec2(15.0, -12.0));
            harness.run_steps(2);
        }
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::SetAudioBusEqualizer{uuid,config}
            if uuid=="amaster1" && !config.enabled && config.bands[1].frequency_hz>1000.0 && config.bands[1].gain_db>1.0)
        ));
        assert!(actions
            .commands
            .iter()
            .all(|command| matches!(command, EngineCommand::SetAudioBusEqualizer { .. })));
    }

    #[test]
    #[ignore = "manual screenshot of the actual egui EQ implementation"]
    fn render_equalizer_preview_image() {
        let path = std::env::var("NIDA_EQ_PREVIEW_IMAGE")
            .expect("Set NIDA_EQ_PREVIEW_IMAGE to a PNG output path");
        let mut data = mixer_data();
        let config = &mut data.audio_mix.buses[0].equalizer.config;
        config.enabled = true;
        config.high_pass = true;
        config.low_shelf.gain_db = -2.0;
        config.high_shelf.gain_db = 3.0;
        for (band, gain) in config.bands.iter_mut().zip([-3.0, 2.0, -1.0]) {
            band.gain_db = gain;
        }
        config.output_db = -2.0;
        data.audio_mix.buses[0].equalizer.pre_db = [-9.0, -12.0];
        data.audio_mix.buses[0].equalizer.post_db = [-11.0, -14.0];
        data.audio_mix.buses[0].equalizer.ready = true;
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1100.0, 1000.0))
                .build_ui(|ui| render_audio_mixer_section(ui, &data, &mut actions));
            harness.run();
            for label in ["Settings Master", "Expand audio settings", "Equalizer"] {
                harness.get_by_label(label).click();
                harness.run();
            }
            harness.hover_at(egui::pos2(2.0, 2.0));
            harness.run();
            harness
                .render()
                .expect("GPU rendered EQ")
                .save(path)
                .expect("saved EQ PNG");
        }
        assert!(actions.commands.is_empty());
    }

    #[test]
    fn meter_scale_and_colours_distinguish_headroom_from_full_scale() {
        assert_eq!(meter_travel(0.0), 1.0);
        assert!((meter_travel(-6.0) - 0.9).abs() < 1e-6);
        assert_eq!(meter_travel(-96.0), 0.0);
        assert_eq!(meter_travel(12.0), 1.0);
        assert_ne!(meter_color(-6.0), meter_color(0.0));
        assert_eq!(peak_text(-6.0), "-6.0");
        assert_eq!(peak_text(-96.0), "−∞");
        let layout = StripLayout::new(
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(STRIP_WIDTH, 400.0)),
            true,
            SEND_ROW,
        );
        assert!(layout.fader.right() < layout.meter.left());
        assert!(layout.fader.width() >= 40.0);
    }

    #[test]
    fn clicking_a_strip_meter_only_resets_that_targets_peak() {
        let data = mixer_data();
        let actions = run(&data, &["Reset peak Master", "Reset peak Lectern mic"]);
        assert_eq!(actions.commands.len(), 2);
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::ResetAudioMeter{uuid,bus:true} if uuid=="amaster1")
        ));
        assert!(pushed(
            &actions,
            |command| matches!(command,EngineCommand::ResetAudioMeter{uuid,bus:false} if uuid=="mic00001")
        ));
    }

    #[test]
    fn settings_can_be_moved_by_the_shared_header_and_closed_without_audio_commands() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            harness.get_by_label("Settings Master").click();
            harness.run();
            let before = harness.get_by_label("Close audio settings").rect();
            let start = before.center() - egui::vec2(130.0, 0.0);
            let offset = egui::vec2(35.0, 25.0);
            harness.event(egui::Event::PointerMoved(start));
            harness.event(egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(3);
            harness.event(egui::Event::PointerMoved(start + offset));
            harness.run_steps(3);
            harness.event(egui::Event::PointerButton {
                pos: start + offset,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(3);
            let after = harness.get_by_label("Close audio settings").rect();
            assert!((after.center() - before.center() - offset).length() < 2.0);
            harness.get_by_label("Close audio settings").click();
            harness.run();
            assert!(harness.query_by_label("Expand audio settings").is_none());
        }
        assert!(actions.commands.is_empty());
    }

    #[test]
    fn an_audio_bus_can_be_renamed_from_its_name() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        let mut harness = harness(&data, &mut actions);
        harness.run();
        harness.get_by_label("Bus A").click_secondary();
        harness.run_steps(3);
        harness.get_by_label("Rename bus…").click();
        harness.run_steps(3);
        assert_eq!(
            super::super::rename::renaming(&harness.ctx).as_deref(),
            Some("astream1")
        );
        harness.get_by_label("Master").click_secondary();
        harness.run_steps(3);
        harness.get_by_label("Rename bus…").click();
        harness.run_steps(3);
        assert_eq!(
            super::super::rename::renaming(&harness.ctx).as_deref(),
            Some("amaster1")
        );
    }

    #[test]
    fn the_strips_are_laid_out_as_vmix_outputs_then_inputs() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        let harness = harness(&data, &mut actions);
        let x = |label: &str| harness.get_by_label(label).rect().center().x;
        assert!(x("OUTPUTS") < x("Master"));
        assert!(x("Master") < x("Bus A"));
        assert!(x("Bus A") < x("INPUTS"));
        assert!(x("INPUTS") < x("intro.mp4"));
        assert!(x("intro.mp4") < x("Lectern mic"));
        // Buttons stacked beside the fader, bus buttons below.
        let y = |label: &str| harness.get_by_label(label).rect().center().y;
        assert!(y("Settings intro.mp4") < y("Solo intro.mp4"));
        assert!(y("Solo intro.mp4") < y("Audio Auto intro.mp4"));
        assert!(y("Audio Auto intro.mp4") < y("On intro.mp4"));
        assert!(y("On intro.mp4") < y("Send intro.mp4 to Master"));
    }

    #[test]
    fn the_bus_buttons_are_lettered_m_a_b() {
        assert_eq!(bus_letter("Master", 0), "M");
        assert_eq!(bus_letter("Bus A", 1), "A");
        assert_eq!(bus_letter("STREAM", 2), "STR");
        assert_eq!(next_bus_name(1), "Bus A");
        assert_eq!(next_bus_name(3), "Bus C");
    }

    #[test]
    fn a_bus_button_sends_the_source_to_that_bus() {
        let data = mixer_data();
        let actions = run(&data, &["Send intro.mp4 to Bus A"]);
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioSend { uuid, bus_uuid, on: true }
                    if uuid == "a0000001" && bus_uuid == "astream1"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn the_arrows_switch_audio_auto() {
        let data = mixer_data();
        let actions = run(&data, &["Audio Auto intro.mp4"]);
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioAuto { uuid, auto: false } if uuid == "a0000001"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_microphone_has_no_audio_auto_since_it_never_goes_on_air() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        let harness = harness(&data, &mut actions);
        assert!(harness.query_by_label("Audio Auto Lectern mic").is_none());
        harness.get_by_label("On Lectern mic");
    }

    #[test]
    fn the_speaker_turns_the_source_on_and_off() {
        let data = mixer_data();
        let actions = run(&data, &["On intro.mp4", "On Lectern mic"]);
        assert!(pushed(&actions, |c| matches!(
            c,
            EngineCommand::SetAudioSourceMute { uuid, muted: false } if uuid == "a0000001"
        )));
        assert!(pushed(&actions, |c| matches!(
            c,
            EngineCommand::SetAudioSourceMute { uuid, muted: true } if uuid == "mic00001"
        )));
    }

    #[test]
    fn s_solos_a_source_to_the_headphones() {
        let data = mixer_data();
        let actions = run(&data, &["Solo intro.mp4"]);
        assert!(pushed(&actions, |c| matches!(
            c,
            EngineCommand::SetAudioSolo { uuid, solo: true } if uuid == "a0000001"
        )));
    }

    #[test]
    fn dragging_a_fader_up_raises_the_level() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run_steps(2);
            let rect = harness.get_by_label("Level intro.mp4").rect();
            let from = egui::pos2(rect.center().x, rect.center().y);
            harness.hover_at(from);
            harness.run_steps(2);
            harness.drag_at(from);
            harness.run_steps(2);
            for step in 1..=4 {
                harness.hover_at(from - egui::vec2(0.0, 10.0 * step as f32));
                harness.run_steps(2);
            }
            harness.drop_at(from - egui::vec2(0.0, 40.0));
            harness.run_steps(2);
        }
        let last = actions
            .commands
            .iter()
            .rev()
            .find_map(|c| match c {
                EngineCommand::SetAudioSourceGain { uuid, gain_db } if uuid == "a0000001" => {
                    Some(*gain_db)
                }
                _ => None,
            })
            .expect("the fader moved");
        assert!(last > 0.5, "up from 0 dB: {last}");
    }

    #[test]
    fn double_clicking_the_balance_centres_it() {
        let mut data = mixer_data();
        data.audio_mix.sources[0].balance = -0.5;
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            let at = harness.get_by_label("Balance intro.mp4").rect().center();
            harness.hover_at(at);
            harness.run();
            for _ in 0..2 {
                harness.drag_at(at);
                harness.step();
                harness.event(egui::Event::PointerButton {
                    pos: at,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: egui::Modifiers::NONE,
                });
                harness.step();
            }
        }
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioBalance { uuid, balance } if uuid == "a0000001" && balance.abs() < 1e-6
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn the_travel_of_a_fader_puts_0_db_three_quarters_up() {
        assert!((db_to_travel(0.0) - 0.76).abs() < 1e-6);
        assert!(db_to_travel(-96.0).abs() < 1e-6);
        assert!((db_to_travel(12.0) - 1.0).abs() < 1e-6);
        for db in [-50.0, -25.0, -7.5, 0.0, 3.0, 12.0] {
            assert!((travel_to_db(db_to_travel(db)) - db).abs() < 1e-3, "{db}");
        }
        assert!((travel_to_db(0.0) + 96.0).abs() < 1e-6, "the bottom is off");
    }

    #[test]
    fn a_source_on_air_shows_red() {
        let mut data = mixer_data();
        data.audio_mix.sources[0].on_air = 1.0;
        data.audio_mix.sources[0].muted = false;
        let mut actions = UIActions::new();
        let mut harness = harness(&data, &mut actions);
        harness.run();
        let image = harness.render().expect("rendered");
        let rect = harness.get_by_label("intro.mp4").rect();
        let ppp = harness.ctx.pixels_per_point();
        let px = image.get_pixel(
            ((rect.left() + 4.0) * ppp) as u32,
            ((rect.top() + 4.0) * ppp) as u32,
        );
        assert!(px[0] > 120 && px[1] < 60, "red header: {px:?}");
    }

    #[test]
    fn the_settings_pick_where_audio_auto_looks() {
        // The fixture's video bus is TV.
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            harness.get_by_label("Settings intro.mp4").click();
            harness.run();
            harness.get_by_value("Main or any bus").click();
            harness.run();
            harness.get_by_label("TV").click();
            harness.run();
        }
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioFollow { uuid, bus_uuid: Some(b) }
                    if uuid == "a0000001" && b == "bus00001"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_microphone_is_held_back_to_meet_a_late_picture() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            harness.get_by_label("Settings Lectern mic").click();
            harness.run();
            harness.get_by_value("0 ms").click();
            harness.run();
            harness.event(egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            });
            harness.event(egui::Event::Text("170".into()));
            harness.run();
            harness.key_press(egui::Key::Enter);
            harness.run();
        }
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioSourceDelay { uuid, delay_ms }
                    if uuid == "mic00001" && (*delay_ms - 170.0).abs() < 0.5
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_bus_plays_on_the_device_picked_for_it() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            harness.get_by_label("Settings Master").click();
            harness.run();
            harness.get_by_value("Out: none").click();
            harness.run();
            harness.get_by_label("BMD HDMI").click();
            harness.run();
        }
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioBusDevice { uuid, device: Some(d), first_channel: 0 }
                    if uuid == "amaster1" && d == "BMD HDMI"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_microphone_is_added_from_an_input_device() {
        let data = mixer_data();
        // A submenu's label carries its arrow.
        let actions = run(&data, &["+ Input", "USB Mixer ⏵", "Mono 2"]);
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::AddAudioInput { device, left: 1, right: 1, .. } if device == "USB Mixer"
            )),
            "{:?}",
            actions.commands
        );
    }

    /// An app's sound — a call's — is added from the apps listed, those
    /// making sound marked; its strip says "not running" while it is not,
    /// and "not taken", with why, when it runs but its sound was refused.
    #[test]
    fn an_apps_sound_is_added_from_the_apps_listed() {
        let data = mixer_data();
        let actions = run(&data, &["+ Input", "App sound ⏵", "zoom.us  ● sound"]);
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::AddAppSound { app, name } if app == "us.zoom.xos" && name == "zoom.us"
            )),
            "{:?}",
            actions.commands
        );

        let mut call = clip("app00001", "zoom.us");
        call.kind = AudioSourceKind::Input {
            device: crate::audio::app_sound::device_name("us.zoom.xos"),
            left: 0,
            right: 1,
            deck: None,
            app: Some("us.zoom.xos".into()),
        };
        assert_eq!(missing(&call), "not running");
        assert_eq!(missing(&data.audio_mix.sources[0]), "not connected");
        assert!(describe(&call, &data).contains("the sound of the app us.zoom.xos"));
        call.refused = Some("the tap was refused (status 1)".into());
        assert_eq!(missing(&call), "not taken", "running, but refused");
        assert!(describe(&call, &data).contains("not taken: the tap was refused"));
    }

    #[test]
    fn the_headphones_can_listen_to_another_bus() {
        let data = mixer_data();
        let mut actions = UIActions::new();
        {
            let mut harness = harness(&data, &mut actions);
            harness.run();
            harness.get_by_label("Headphones").click();
            harness.run();
            harness
                .get_all_by_value("Master")
                .find(|n| n.accesskit_node().role() == egui::accesskit::Role::ComboBox)
                .expect("the Listen menu")
                .click();
            harness.run();
            harness
                .get_all_by_role_and_label(egui::accesskit::Role::Button, "Bus A")
                .last()
                .expect("the menu entry")
                .click();
            harness.run();
        }
        assert!(
            pushed(&actions, |c| matches!(
                c,
                EngineCommand::SetAudioMonitorListen { bus_uuid: Some(b) } if b == "astream1"
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn a_soloed_source_lights_the_headphones() {
        let mut data = mixer_data();
        data.audio_mix.sources[0].solo = true;
        data.audio_mix.monitor.soloing = true;
        let mut actions = UIActions::new();
        let mut harness = harness(&data, &mut actions);
        harness.run();
        harness.get_by_label("Headphones").click();
        harness.run();
        harness.get_by_label(" hearing the soloed inputs ");
    }
}
