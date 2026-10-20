use super::super::{UIActions, UIData};
use super::audio_mixer::{meter, Settings};
use crate::audio_mix::EqualizerConfig;
use crate::engine::EngineCommand;

pub(super) fn render(ui: &mut egui::Ui, which: &Settings, data: &UIData, actions: &mut UIActions) {
    let (uuid, bus, snapshot) = match which {
        Settings::Bus(uuid) => {
            let Some(bus) = data
                .audio_mix
                .buses
                .iter()
                .find(|bus| bus.config.uuid == *uuid)
            else {
                return;
            };
            (uuid, true, &bus.equalizer)
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
            (uuid, false, &source.equalizer)
        }
        Settings::Headphones => return,
    };
    let mut config = snapshot.config;
    let before = config;
    ui.heading("Equalizer settings");
    ui.label(
        egui::RichText::new("Stereo EQ → compressor → fader · 48 kHz")
            .small()
            .weak(),
    );
    ui.horizontal(|ui| {
        ui.checkbox(&mut config.enabled, "Enable EQ");
        if ui.button("Reset EQ").clicked() {
            config = EqualizerConfig::default();
        }
    });
    if let Some(error) = &snapshot.error {
        ui.colored_label(egui::Color32::YELLOW, format!("Dry fallback: {error}"));
        ui.label("Toggle EQ off/on to retry.");
    } else {
        ui.label(if !config.enabled {
            "Bypassed"
        } else if snapshot.updating {
            "Updating…"
        } else if snapshot.ready {
            "Active"
        } else {
            "Waiting for audio source"
        });
    }
    response_graph(ui, &mut config);
    ui.horizontal(|ui| {
        ui.checkbox(&mut config.high_pass, "High-pass (12 dB/oct)");
        ui.add(
            egui::Slider::new(&mut config.high_pass_hz, 20.0..=500.0)
                .logarithmic(true)
                .suffix(" Hz"),
        );
    });
    egui::Grid::new("equalizer_band_controls")
        .num_columns(4)
        .spacing([10.0, 5.0])
        .show(ui, |ui| {
            for title in ["Band", "Frequency (Hz)", "Gain (dB)", "Q"] {
                ui.strong(title);
            }
            ui.end_row();
            for index in 0..5 {
                let label = match index {
                    0 => "Low shelf",
                    1 => "Band 1",
                    2 => "Band 2",
                    3 => "Band 3",
                    _ => "High shelf",
                };
                let (enabled, frequency, gain, q) = match index {
                    0 => (
                        &mut config.low_shelf.enabled,
                        &mut config.low_shelf.frequency_hz,
                        &mut config.low_shelf.gain_db,
                        None,
                    ),
                    4 => (
                        &mut config.high_shelf.enabled,
                        &mut config.high_shelf.frequency_hz,
                        &mut config.high_shelf.gain_db,
                        None,
                    ),
                    _ => {
                        let band = &mut config.bands[index - 1];
                        (
                            &mut band.enabled,
                            &mut band.frequency_hz,
                            &mut band.gain_db,
                            Some(&mut band.q),
                        )
                    }
                };
                ui.checkbox(enabled, label);
                let range = match index {
                    0 => 20.0..=2000.0,
                    4 => 1000.0..=20_000.0,
                    _ => 20.0..=20_000.0,
                };
                ui.add(
                    egui::DragValue::new(frequency)
                        .range(range)
                        .speed(5.0)
                        .max_decimals(0),
                )
                .on_hover_text(format!(
                    "{label} frequency; drag the graph handle for logarithmic control"
                ));
                ui.add(
                    egui::DragValue::new(gain)
                        .range(-12.0..=12.0)
                        .speed(0.1)
                        .fixed_decimals(1),
                )
                .on_hover_text(format!("{label} gain, -12 to +12 dB"));
                if let Some(q) = q {
                    ui.add(
                        egui::DragValue::new(q)
                            .range(0.25..=10.0)
                            .speed(0.05)
                            .fixed_decimals(2),
                    )
                    .on_hover_text("Higher Q is a narrower band");
                } else {
                    ui.label("Fixed slope");
                }
                ui.end_row();
            }
        });
    ui.add_space(6.0);
    ui.add(
        egui::Slider::new(&mut config.output_db, -24.0..=12.0)
            .text("EQ output trim")
            .suffix(" dB"),
    );
    ui.label(
        egui::RichText::new(
            "Boosts consume headroom. Use output trim to compensate; this EQ is not a limiter.",
        )
        .small()
        .weak(),
    );
    ui.separator();
    ui.horizontal_top(|ui| {
        for (label, levels) in [("Pre EQ", snapshot.pre_db), ("Post EQ", snapshot.post_db)] {
            ui.vertical(|ui| {
                ui.set_width(115.0);
                ui.label(label);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(54.0, 150.0), egui::Sense::hover());
                let _ = meter(ui, rect, levels, None);
                ui.label(
                    egui::RichText::new(format!("L {:+.1}  R {:+.1} dBFS", levels[0], levels[1]))
                        .small(),
                );
            });
        }
        ui.vertical(|ui| {
            ui.label(
                egui::RichText::new(
                    "Sample peaks before compression and fader. Raw PFL stays unprocessed.",
                )
                .small()
                .weak(),
            );
            ui.label(
                egui::RichText::new(
                    "Edits briefly fade through dry audio. No lookahead or FFT analyzer.",
                )
                .small()
                .weak(),
            );
        });
    });
    if config != before {
        actions.commands.push(if bus {
            EngineCommand::SetAudioBusEqualizer {
                uuid: uuid.clone(),
                config,
            }
        } else {
            EngineCommand::SetAudioSourceEqualizer {
                uuid: uuid.clone(),
                config,
            }
        });
    }
}

fn frequency_x(frequency: f32, rect: egui::Rect) -> f32 {
    rect.left() + (frequency / 20.0).log10() / 3.0 * rect.width()
}

fn response_graph(ui: &mut egui::Ui, config: &mut EqualizerConfig) {
    let (outer, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(260.0), 185.0),
        egui::Sense::hover(),
    );
    let plot = egui::Rect::from_min_max(
        outer.min + egui::vec2(27.0, 12.0),
        outer.max - egui::vec2(7.0, 25.0),
    );
    let y = |db: f32| plot.center().y - db.clamp(-24.0, 24.0) / 48.0 * plot.height();
    let painter = ui.painter().clone();
    painter.rect_filled(plot, 2.0, egui::Color32::from_rgb(10, 13, 18));
    for db in [-24, -12, 0, 12, 24] {
        let height = y(db as f32);
        painter.line_segment(
            [
                egui::pos2(plot.left(), height),
                egui::pos2(plot.right(), height),
            ],
            egui::Stroke::new(
                if db == 0 { 1.0 } else { 0.5 },
                egui::Color32::from_gray(if db == 0 { 90 } else { 45 }),
            ),
        );
        painter.text(
            egui::pos2(plot.left() - 4.0, height),
            egui::Align2::RIGHT_CENTER,
            format!("{db}"),
            egui::FontId::proportional(9.0),
            egui::Color32::GRAY,
        );
    }
    for frequency in [
        20.0, 50.0, 100.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 10_000.0, 20_000.0,
    ] {
        let position = frequency_x(frequency, plot);
        painter.line_segment(
            [
                egui::pos2(position, plot.top()),
                egui::pos2(position, plot.bottom()),
            ],
            egui::Stroke::new(0.5, egui::Color32::from_gray(40)),
        );
        if plot.width() > 450.0 || frequency >= 100.0 {
            let text = if frequency >= 1000.0 {
                format!("{}k", frequency / 1000.0)
            } else {
                format!("{frequency:.0}")
            };
            painter.text(
                egui::pos2(position, plot.bottom() + 5.0),
                egui::Align2::CENTER_TOP,
                text,
                egui::FontId::proportional(9.0),
                egui::Color32::GRAY,
            );
        }
    }
    let cache_id = ui.id().with("equalizer_response_curve");
    let cached = ui
        .ctx()
        .data(|data| data.get_temp::<(EqualizerConfig, std::sync::Arc<Vec<f32>>)>(cache_id));
    let values = if let Some((old, values)) = cached.filter(|(old, _)| old == config) {
        let _ = old;
        values
    } else {
        let values = std::sync::Arc::new(
            (0..160)
                .map(|index| config.response_db(20.0 * 1000_f32.powf(index as f32 / 159.0)))
                .collect::<Vec<_>>(),
        );
        ui.ctx()
            .data_mut(|data| data.insert_temp(cache_id, (*config, values.clone())));
        values
    };
    let color = if config.enabled {
        egui::Color32::from_rgb(75, 175, 245)
    } else {
        egui::Color32::from_gray(125)
    };
    let points = values
        .iter()
        .enumerate()
        .map(|(index, db)| egui::pos2(plot.left() + index as f32 / 159.0 * plot.width(), y(*db)))
        .collect();
    painter.add(egui::Shape::line(points, egui::Stroke::new(2.0, color)));
    for index in 0..5 {
        let (label, enabled, frequency, gain) = match index {
            0 => (
                "L",
                config.low_shelf.enabled,
                config.low_shelf.frequency_hz,
                config.low_shelf.gain_db,
            ),
            4 => (
                "H",
                config.high_shelf.enabled,
                config.high_shelf.frequency_hz,
                config.high_shelf.gain_db,
            ),
            _ => {
                let band = config.bands[index - 1];
                (
                    ["", "1", "2", "3"][index],
                    band.enabled,
                    band.frequency_hz,
                    band.gain_db,
                )
            }
        };
        let position = egui::pos2(frequency_x(frequency, plot), y(gain));
        let response = ui.interact(
            egui::Rect::from_center_size(position, egui::vec2(15.0, 15.0)),
            ui.id().with(("eq_handle", index)),
            egui::Sense::drag(),
        );
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Slider, true, format!("EQ {label} point"))
        });
        painter.circle_filled(
            position,
            4.0,
            if enabled {
                color
            } else {
                egui::Color32::DARK_GRAY
            },
        );
        painter.text(
            position - egui::vec2(0.0, 8.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(10.0),
            color,
        );
        if response.dragged() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let frequency = 20.0
                    * 1000_f32.powf(((pointer.x - plot.left()) / plot.width()).clamp(0.0, 1.0));
                let gain =
                    ((plot.center().y - pointer.y) / plot.height() * 48.0).clamp(-12.0, 12.0);
                match index {
                    0 => {
                        config.low_shelf.frequency_hz = frequency.clamp(20.0, 2000.0);
                        config.low_shelf.gain_db = gain;
                        config.low_shelf.enabled = true;
                    }
                    4 => {
                        config.high_shelf.frequency_hz = frequency.clamp(1000.0, 20_000.0);
                        config.high_shelf.gain_db = gain;
                        config.high_shelf.enabled = true;
                    }
                    _ => {
                        let band = &mut config.bands[index - 1];
                        band.frequency_hz = frequency;
                        band.gain_db = gain;
                        band.enabled = true;
                    }
                }
            }
        }
    }
    painter.text(
        egui::pos2(plot.left() + 4.0, plot.top() + 4.0),
        egui::Align2::LEFT_TOP,
        if config.enabled {
            "Response · ±24 dB"
        } else {
            "Response preview · EQ bypassed"
        },
        egui::FontId::proportional(9.0),
        egui::Color32::GRAY,
    );
}
