//! The REC button and its window, as in vMix: the button sits beside STREAM
//! in the top bar — plain when idle, red with the time while recording,
//! yellow and blinking when something needs attention (the disk nearly full,
//! the encoder falling behind or restarted). Clicking it opens the window
//! with where files go, how they are encoded, and Start / Stop.

use super::super::{UIActions, UIData};
use super::monitoring::{BAD, WARN};
use super::stream::{
    button_text_width, clock, piece_text, pieces_width, source_label, Piece, LIVE_RED,
    STATS_SPACING, WARN_DIM, WARN_YELLOW,
};
use crate::engine::value::render::OutputSource;
use crate::engine::EngineCommand;
use crate::record::{RecordCodec, RecordFormat, RecordOptions};
use crate::streaming::StreamEncoder;

fn open_id() -> egui::Id {
    egui::Id::new("record_window_open")
}

fn stop_armed_id() -> egui::Id {
    egui::Id::new("record_stop_armed")
}

/// How long a first click on Stop waits for the second.
const STOP_CONFIRM_SECS: f64 = 3.0;

/// Video bit rates offered, with what they suit.
const BITRATE_PRESETS: [(u32, &str); 6] = [
    (8_000, "1080p, talks"),
    (16_000, "1080p, high"),
    (25_000, "1440p / 1080p60"),
    (40_000, "4K"),
    (60_000, "4K60"),
    (100_000, "4K60, for grading"),
];

/// `12.3 GB`, `456 MB`.
fn size(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    if b >= 1e9 {
        format!("{:.1} GB", b / 1e9)
    } else {
        format!("{:.0} MB", b / 1e6)
    }
}

/// The button's fill and text colour: none idle, red while recording well,
/// yellow (bright or dim, for the blink) with a problem.
fn look(status: &crate::record::RecordStatus, bright: bool) -> (Option<egui::Color32>, bool) {
    if !status.recording {
        (None, false)
    } else if status.is_healthy() {
        (Some(LIVE_RED), false)
    } else {
        (Some(if bright { WARN_YELLOW } else { WARN_DIM }), true)
    }
}

/// Whether NOT RECORDING blinks behind the take buttons: asked for, and
/// not recording.
pub(super) fn warns_not_recording(data: &UIData) -> bool {
    data.record.options.warn_when_off && !data.record.status.recording
}

/// How long one blink of NOT RECORDING takes, on and off.
const WARN_BLINK_SECS: f64 = 1.2;

/// Take buttons drawn by `add`, with NOT RECORDING blinking behind them while
/// [`warns_not_recording`]: its words over them — in a slot `label_height`
/// tall, in a column — and the space they stand in pulsing red. Without the
/// reminder, just `add`.
pub(super) fn with_record_warning<R>(
    ui: &mut egui::Ui,
    data: &UIData,
    label_height: Option<f32>,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    if !warns_not_recording(data) {
        return add(ui);
    }
    let behind = ui.painter().add(egui::Shape::Noop);
    let time = ui.input(|i| i.time);
    #[allow(clippy::cast_possible_truncation)]
    let pulse = ((time * std::f64::consts::TAU / WARN_BLINK_SECS).sin() * 0.5 + 0.5) as f32;
    let inner = ui.scope(|ui| {
        let words = egui::RichText::new("NOT RECORDING")
            .strong()
            .color(egui::Color32::WHITE);
        let hover = "Recording is off — press REC at the top. \
                     (This reminder: REC ▸ Reminder.)";
        match label_height {
            Some(height) => {
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), height),
                    egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
                    |ui| ui.label(words).on_hover_text(hover),
                );
            }
            None => {
                ui.label(words).on_hover_text(hover);
            }
        }
        add(ui)
    });
    let rect = inner.response.rect.expand(4.0);
    let alpha = 0.18 + 0.62 * pulse * pulse;
    ui.painter().set(
        behind,
        egui::epaint::RectShape::filled(rect, 6.0, LIVE_RED.gamma_multiply(alpha)),
    );
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(40));
    inner.inner
}

/// The top-bar button.
pub(super) fn render_record_button(ui: &mut egui::Ui, data: &UIData) {
    let status = &data.record.status;
    let bright = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
    let (fill, blinking) = look(status, bright);
    let text = button_text(status);
    let mut rich = egui::RichText::new(text).strong();
    if fill.is_some() {
        rich = rich.color(if blinking {
            egui::Color32::BLACK
        } else {
            egui::Color32::WHITE
        });
    }
    let mut button = egui::Button::new(rich);
    if let Some(fill) = fill {
        button = button.fill(fill);
    }
    let hover = match (&status.problem, &status.file) {
        (Some(problem), _) => problem.clone(),
        (None, Some(path)) => format!("Recording to {path}"),
        (None, None) => "Recording: set up and start".to_string(),
    };
    if ui.add(button).on_hover_text(hover).clicked() {
        let open = window_open(ui.ctx());
        ui.ctx()
            .memory_mut(|m| m.data.insert_temp(open_id(), !open));
    }
    if status.recording {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(if blinking {
                250
            } else {
                500
            }));
    }
}

/// "REC", or with its clock while recording.
fn button_text(status: &crate::record::RecordStatus) -> String {
    if status.recording {
        format!("REC {}", clock(status.secs))
    } else {
        "REC".to_string()
    }
}

/// How much of the top bar the REC button takes now, the row's gap
/// included (see [`super::stream::button_width`]).
pub(super) fn button_width(ui: &egui::Ui, data: &UIData) -> f32 {
    button_text_width(ui, button_text(&data.record.status))
}

/// The take's size and the room left beside the REC button while
/// recording, and the frames the encoder could not take, if any: yellow
/// below the space a recording is flagged at, red when frames went. Shorter
/// in a narrow top bar (`tier`: 1 the size alone, 2 nothing).
pub(super) fn render_record_stats(ui: &mut egui::Ui, data: &UIData, tier: u8) {
    let pieces = record_pieces(data, tier);
    if pieces.is_empty() {
        return;
    }
    let status = &data.record.status;
    let hover = status.file.as_ref().map_or_else(
        || "Recording".to_string(),
        |path| format!("Recording to {path}"),
    );
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = STATS_SPACING;
        for piece in pieces {
            if let Piece::Text(text, colour) = piece {
                ui.label(piece_text(text, colour)).on_hover_text(&hover);
            }
        }
    });
}

/// How wide [`render_record_stats`] takes in the top bar at `tier`.
pub(super) fn stats_width(ui: &egui::Ui, data: &UIData, tier: u8) -> f32 {
    pieces_width(ui, &record_pieces(data, tier))
}

fn record_pieces(data: &UIData, tier: u8) -> Vec<Piece> {
    let status = &data.record.status;
    if !status.recording || tier >= 2 {
        return Vec::new();
    }
    let (text, colour) = record_stats_text(status, tier == 0);
    vec![Piece::Text(text, colour.unwrap_or(egui::Color32::GRAY))]
}

/// "1.2 GB · 230.0 GB free" (`full`) or "1.2 GB", with " · dropped 12" when
/// the encoder missed frames; and its colour, if not the plain one. A count,
/// not a share: a take whose encoder started again at another frame rate
/// has no one rate to take a share of.
fn record_stats_text(
    status: &crate::record::RecordStatus,
    full: bool,
) -> (String, Option<egui::Color32>) {
    use std::fmt::Write as _;
    let mut text = size(status.bytes);
    if let (true, Some(free)) = (full, status.free_bytes) {
        let _ = write!(text, " · {} free", size(free));
    }
    let mut colour = status
        .free_bytes
        .filter(|&free| free < crate::record::LOW_SPACE_BYTES)
        .map(|_| WARN);
    if status.dropped > 0 {
        let _ = write!(text, " · dropped {}", status.dropped);
        colour = Some(BAD);
    }
    (text, colour)
}

fn window_open(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.data.get_temp::<bool>(open_id()))
        .unwrap_or(false)
}

/// The Record window, when open.
pub(super) fn render_record_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    if !window_open(ctx) {
        return;
    }
    let mut open = true;
    egui::Window::new("Record")
        .id(egui::Id::new("record_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_pos(egui::pos2(300.0, 60.0))
        .default_width(560.0)
        .show(ctx, |ui| {
            render_header(ui, data, actions);
            ui.separator();
            render_options(
                ui,
                &data.record.options,
                data.record.status.recording,
                data,
                actions,
            );
        });
    if !open {
        ctx.memory_mut(|m| m.data.insert_temp(open_id(), false));
    }
}

fn render_header(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let status = &data.record.status;
    ui.horizontal(|ui| {
        render_start_stop(ui, data, actions);
        if status.recording {
            let color = if status.is_healthy() {
                LIVE_RED
            } else {
                WARN_YELLOW
            };
            ui.label(
                egui::RichText::new(format!(" REC {} ", clock(status.secs)))
                    .strong()
                    .color(egui::Color32::WHITE)
                    .background_color(color),
            );
            ui.label(size(status.bytes));
        } else {
            ui.label(egui::RichText::new("Not recording").strong());
        }
    });
    if let Some(problem) = &status.problem {
        ui.colored_label(WARN_YELLOW, format!("⚠ {problem}"));
    }
    let file = status.file.as_ref().or(status.last_file.as_ref());
    if let Some(file) = file {
        ui.horizontal(|ui| {
            let name = std::path::Path::new(file)
                .file_name()
                .map_or_else(|| file.clone(), |n| n.to_string_lossy().into_owned());
            let verb = if status.recording { "Writing" } else { "Last" };
            ui.label(egui::RichText::new(format!("{verb}: {name}")).small())
                .on_hover_text(file);
            if ui.small_button("Show in Finder").clicked() {
                actions.session.reveal_file = Some(file.into());
            }
        });
    }
    if status.files.len() > 1 {
        ui.label(
            egui::RichText::new(format!(
                "This take is in {} files: the encoder had to start again",
                status.files.len()
            ))
            .small()
            .color(WARN_YELLOW),
        );
    }
    if let Some(free) = status.free_bytes {
        let low = free < crate::record::LOW_SPACE_BYTES;
        let text = egui::RichText::new(format!("{} free", size(free))).small();
        ui.label(if low {
            text.color(WARN_YELLOW)
        } else {
            text.weak()
        });
    }
    if status.dropped > 0 {
        ui.label(
            egui::RichText::new(format!(
                "{} frames the encoder missed (each covered by repeating the one before)",
                status.dropped
            ))
            .small()
            .weak(),
        );
    }
}

fn render_options(
    ui: &mut egui::Ui,
    options: &RecordOptions,
    recording: bool,
    data: &UIData,
    actions: &mut UIActions,
) {
    let set = |actions: &mut UIActions, options: RecordOptions| {
        actions
            .commands
            .push(EngineCommand::SetRecordOptions { options });
    };
    egui::Grid::new("record_options")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Picture");
            egui::ComboBox::from_id_salt("record_source")
                .selected_text(source_label(&options.source, data))
                .width(220.0)
                .show_ui(ui, |ui| {
                    let mut pick = |ui: &mut egui::Ui, source: OutputSource| {
                        let label = source_label(&source, data);
                        if ui
                            .selectable_label(options.source == source, label)
                            .clicked()
                            && options.source != source
                        {
                            set(
                                actions,
                                RecordOptions {
                                    source,
                                    ..options.clone()
                                },
                            );
                        }
                    };
                    pick(ui, OutputSource::Master);
                    for bus in &data.buses {
                        pick(ui, OutputSource::BusProgram(bus.bus.uuid.clone()));
                    }
                })
                .response
                .on_hover_text("What is recorded — can be changed while recording");
            ui.end_row();

            ui.label("Sound");
            ui.add_enabled_ui(!recording, |ui| {
                let buses = &data.audio_mix.buses;
                let current = options
                    .audio_bus
                    .as_deref()
                    .and_then(|u| buses.iter().find(|b| b.config.uuid == u))
                    .or(buses.first())
                    .map_or("Silence (no audio bus)".to_string(), |b| {
                        b.config.name.clone()
                    });
                egui::ComboBox::from_id_salt("record_audio")
                    .selected_text(current)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (i, bus) in buses.iter().enumerate() {
                            let chosen = options.audio_bus.as_deref() == Some(&bus.config.uuid)
                                || (options.audio_bus.is_none() && i == 0);
                            if ui.selectable_label(chosen, &bus.config.name).clicked() && !chosen {
                                set(
                                    actions,
                                    RecordOptions {
                                        audio_bus: Some(bus.config.uuid.clone()),
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Folder");
            ui.add_enabled_ui(!recording, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(options.folder.display().to_string()).small());
                    if ui.small_button("Choose…").clicked() {
                        actions.session.choose_record_folder = true;
                    }
                });
            });
            ui.end_row();

            ui.label("Codec");
            ui.add_enabled_ui(!recording, |ui| {
                egui::ComboBox::from_id_salt("record_codec")
                    .selected_text(options.codec.label())
                    .width(160.0)
                    .show_ui(ui, |ui| {
                        for codec in [
                            RecordCodec::H264,
                            RecordCodec::Hevc,
                            RecordCodec::ProRes,
                            RecordCodec::ProResHq,
                        ] {
                            let chosen = options.codec == codec;
                            if ui.selectable_label(chosen, codec.label()).clicked() && !chosen {
                                set(
                                    actions,
                                    RecordOptions {
                                        codec,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Format");
            ui.add_enabled_ui(!recording && options.codec.has_bit_rate(), |ui| {
                egui::ComboBox::from_id_salt("record_format")
                    .selected_text(options.format.label())
                    .width(260.0)
                    .show_ui(ui, |ui| {
                        for format in [RecordFormat::Mp4, RecordFormat::Mov, RecordFormat::Mkv] {
                            let chosen = options.format == format;
                            if ui.selectable_label(chosen, format.label()).clicked() && !chosen {
                                set(
                                    actions,
                                    RecordOptions {
                                        format,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    })
                    .response
                    .on_disabled_hover_text("ProRes is always written as MOV");
            });
            ui.end_row();

            if options.codec.has_bit_rate() {
                ui.label("Quality");
                ui.add_enabled_ui(!recording, |ui| {
                    ui.horizontal(|ui| {
                        let mut auto = options.video_kbps.is_none();
                        if ui
                            .checkbox(&mut auto, "Auto")
                            .on_hover_text("A bit rate for the picture's size and frame rate")
                            .changed()
                        {
                            let fps = options.fps.unwrap_or(data.target_fps.max(1));
                            let size = options
                                .size
                                .unwrap_or((data.render_width, data.render_height));
                            set(
                                actions,
                                RecordOptions {
                                    video_kbps: (!auto).then(|| options.effective_kbps(size, fps)),
                                    ..options.clone()
                                },
                            );
                        }
                        if let Some(kbps) = options.video_kbps {
                            let mut value = kbps;
                            let drag = ui.add(
                                egui::DragValue::new(&mut value)
                                    .range(1_000..=200_000)
                                    .speed(100)
                                    .suffix(" kbps"),
                            );
                            if drag.changed() && value != kbps {
                                set(
                                    actions,
                                    RecordOptions {
                                        video_kbps: Some(value),
                                        ..options.clone()
                                    },
                                );
                            }
                        } else {
                            let fps = options.fps.unwrap_or(data.target_fps.max(1));
                            let size = options
                                .size
                                .unwrap_or((data.render_width, data.render_height));
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} kbps",
                                    options.effective_kbps(size, fps)
                                ))
                                .weak(),
                            );
                        }
                        ui.menu_button("Presets", |ui| {
                            for (kbps, what) in BITRATE_PRESETS {
                                if ui.button(format!("{kbps} kbps — {what}")).clicked() {
                                    set(
                                        actions,
                                        RecordOptions {
                                            video_kbps: Some(kbps),
                                            ..options.clone()
                                        },
                                    );
                                    ui.close();
                                }
                            }
                        });
                    });
                });
                ui.end_row();
            }

            ui.label("Encoder");
            ui.add_enabled_ui(!recording, |ui| {
                egui::ComboBox::from_id_salt("record_encoder")
                    .selected_text(options.encoder.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for enc in [StreamEncoder::Hardware, StreamEncoder::Software] {
                            let chosen = options.encoder == enc;
                            let item = ui.add_enabled(
                                enc.available(),
                                egui::Button::selectable(chosen, enc.label()),
                            );
                            if item.clicked() && !chosen {
                                set(
                                    actions,
                                    RecordOptions {
                                        encoder: enc,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Size");
            ui.add_enabled_ui(!recording, |ui| {
                let label = |size: Option<(u32, u32)>| {
                    size.map_or_else(
                        || {
                            format!(
                                "Same as Nida ({}×{})",
                                data.render_width, data.render_height
                            )
                        },
                        |(w, h)| format!("{w}×{h}"),
                    )
                };
                egui::ComboBox::from_id_salt("record_size")
                    .selected_text(label(options.size))
                    .width(200.0)
                    .show_ui(ui, |ui| {
                        for size in [
                            None,
                            Some((1280, 720)),
                            Some((1920, 1080)),
                            Some((2560, 1440)),
                            Some((3840, 2160)),
                        ] {
                            let chosen = options.size == size;
                            if ui.selectable_label(chosen, label(size)).clicked() && !chosen {
                                set(
                                    actions,
                                    RecordOptions {
                                        size,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Frame rate");
            ui.add_enabled_ui(!recording, |ui| {
                let label = |fps: Option<u32>| {
                    fps.map_or_else(|| "Same as Nida".to_string(), |f| format!("{f} fps"))
                };
                egui::ComboBox::from_id_salt("record_fps")
                    .selected_text(label(options.fps))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for fps in [None, Some(24), Some(25), Some(30), Some(50), Some(60)] {
                            let chosen = options.fps == fps;
                            if ui.selectable_label(chosen, label(fps)).clicked() && !chosen {
                                set(
                                    actions,
                                    RecordOptions {
                                        fps,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Audio");
            ui.add_enabled_ui(!recording && options.codec.has_bit_rate(), |ui| {
                if options.codec.has_bit_rate() {
                    egui::ComboBox::from_id_salt("record_audio_kbps")
                        .selected_text(format!("AAC {} kbps", options.audio_kbps))
                        .width(130.0)
                        .show_ui(ui, |ui| {
                            for kbps in [128, 192, 256, 320] {
                                let chosen = options.audio_kbps == kbps;
                                if ui
                                    .selectable_label(chosen, format!("AAC {kbps} kbps"))
                                    .clicked()
                                    && !chosen
                                {
                                    set(
                                        actions,
                                        RecordOptions {
                                            audio_kbps: kbps,
                                            ..options.clone()
                                        },
                                    );
                                }
                            }
                        });
                } else {
                    ui.label("Uncompressed (PCM), as ProRes expects");
                }
            });
            ui.end_row();

            ui.label("Reminder");
            let mut warn = options.warn_when_off;
            if ui
                .checkbox(&mut warn, "Blink NOT RECORDING behind the take buttons")
                .on_hover_text(
                    "While not recording, NOT RECORDING blinks behind Cut and Fade — \
                     in the middle of a show, Record is easily forgotten",
                )
                .changed()
            {
                set(
                    actions,
                    RecordOptions {
                        warn_when_off: warn,
                        ..options.clone()
                    },
                );
            }
            ui.end_row();
        });
    if recording {
        ui.label(
            egui::RichText::new(
                "Stop recording to change the settings — the picture and the reminder can \
                 change now",
            )
            .small()
            .weak(),
        );
    }
}

fn render_start_stop(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let now = ui.input(|i| i.time);
    if !data.record.status.recording {
        if ui
            .add(
                egui::Button::new(egui::RichText::new("Start recording").strong())
                    .min_size(egui::vec2(160.0, 30.0)),
            )
            .on_hover_text("Record into a new file named by the date and time")
            .clicked()
        {
            actions.commands.push(EngineCommand::StartRecording);
        }
        return;
    }
    // Stopping a recording by accident splits the take: the first click
    // arms, the second within three seconds stops.
    let armed_at = ui.data(|d| d.get_temp::<f64>(stop_armed_id()));
    let armed = armed_at.is_some_and(|at| now - at < STOP_CONFIRM_SECS);
    let text = if armed {
        "Click again to stop"
    } else {
        "Stop recording"
    };
    let stop = ui.add(
        egui::Button::new(
            egui::RichText::new(text)
                .strong()
                .color(egui::Color32::WHITE),
        )
        .fill(LIVE_RED)
        .min_size(egui::vec2(160.0, 30.0)),
    );
    if stop.clicked() {
        if armed {
            ui.data_mut(|d| d.remove_temp::<f64>(stop_armed_id()));
            actions.commands.push(EngineCommand::StopRecording);
        } else {
            ui.data_mut(|d| d.insert_temp(stop_armed_id(), now));
        }
    }
    if armed {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Beside REC: the take's size and the room left — yellow below the
    /// space a recording is flagged at — and frames dropped, in red.
    #[test]
    fn the_stats_beside_rec_show_size_room_and_frames_dropped() {
        let mut status = crate::record::RecordStatus::idle(None);
        status.recording = true;
        status.secs = 400.0;
        status.bytes = 1_200_000_000;
        status.free_bytes = Some(230_000_000_000);
        assert_eq!(
            record_stats_text(&status, true),
            ("1.2 GB · 230.0 GB free".to_string(), None)
        );
        assert_eq!(record_stats_text(&status, false).0, "1.2 GB", "short");
        status.free_bytes = Some(2_000_000_000);
        assert_eq!(record_stats_text(&status, true).1, Some(WARN), "low on space");
        status.dropped = 12;
        assert_eq!(
            record_stats_text(&status, true),
            ("1.2 GB · 2.0 GB free · dropped 12".to_string(), Some(BAD))
        );
        assert_eq!(record_stats_text(&status, false).0, "1.2 GB · dropped 12");
    }

    /// Each form takes the room it says it does, the row's gap included
    /// (see `stream`'s own test).
    #[test]
    fn the_stats_beside_rec_take_the_room_they_say() {
        let mut data = UIData::test_fixture();
        let status = &mut data.record.status;
        status.recording = true;
        status.secs = 400.0;
        status.bytes = 1_200_000_000;
        status.free_bytes = Some(230_000_000_000);
        status.dropped = 12;
        let data = std::rc::Rc::new(data);
        for tier in 0..3u8 {
            let shown = data.clone();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1200.0, 100.0))
                .build_ui_state(
                    move |ui, measured: &mut (f32, f32)| {
                        ui.horizontal(|ui| {
                            let before = ui.cursor().left();
                            let said = stats_width(ui, &shown, tier);
                            render_record_stats(ui, &shown, tier);
                            *measured = (ui.cursor().left() - before, said);
                        });
                    },
                    (0.0, 0.0),
                );
            harness.run_steps(1);
            let (took, said) = *harness.state();
            assert!(
                (took - said).abs() < 0.5,
                "form {tier}: took {took}, said {said}"
            );
        }
    }

    /// The REC button takes the room it says it does, idle and recording.
    #[test]
    fn the_rec_button_takes_the_room_it_says() {
        for recording in [false, true] {
            let mut data = UIData::test_fixture();
            data.record.status.recording = recording;
            data.record.status.secs = 754.0;
            let shown = std::rc::Rc::new(data);
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1200.0, 100.0))
                .build_ui_state(
                    move |ui, measured: &mut (f32, f32)| {
                        ui.horizontal(|ui| {
                            let before = ui.cursor().left();
                            let said = button_width(ui, &shown);
                            render_record_button(ui, &shown);
                            *measured = (ui.cursor().left() - before, said);
                        });
                    },
                    (0.0, 0.0),
                );
            harness.run_steps(1);
            let (took, said) = *harness.state();
            assert!(
                (took - said).abs() < 0.5,
                "recording {recording}: took {took}, said {said}"
            );
        }
    }

    /// Narrowest, the stats beside REC go; not recording, there are none.
    #[test]
    fn the_stats_beside_rec_go_when_narrowest_or_idle() {
        let mut data = UIData::test_fixture();
        assert!(record_pieces(&data, 0).is_empty(), "idle: none");
        let status = &mut data.record.status;
        status.recording = true;
        status.bytes = 1_200_000_000;
        assert_eq!(record_pieces(&data, 1).len(), 1);
        assert!(record_pieces(&data, 2).is_empty(), "narrowest: none");
    }
    use crate::record::RecordStatus;
    use egui_kittest::kittest::Queryable;

    fn recording(problem: Option<&str>) -> RecordStatus {
        RecordStatus {
            recording: true,
            secs: 12.0,
            file: Some("/tmp/Nida 2026-09-26 10-00-00.mp4".into()),
            files: vec!["/tmp/Nida 2026-09-26 10-00-00.mp4".into()],
            bytes: 12_345_678,
            free_bytes: Some(100_000_000_000),
            problem: problem.map(str::to_string),
            dropped: 0,
            fps: 0,
            last_file: None,
            audio_bus: None,
        }
    }

    #[test]
    fn the_button_is_plain_idle_red_recording_and_blinks_yellow_on_trouble() {
        assert_eq!(look(&RecordStatus::idle(None), true).0, None);
        assert_eq!(look(&recording(None), true).0, Some(LIVE_RED));
        assert_eq!(look(&recording(None), false).0, Some(LIVE_RED), "steady");
        let trouble = recording(Some("The disk is nearly full"));
        assert_eq!(look(&trouble, true).0, Some(WARN_YELLOW));
        assert_eq!(look(&trouble, false).0, Some(WARN_DIM));
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, UIActions> {
        let data = std::rc::Rc::new(data);
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .build_ui_state(
                move |ui, actions: &mut UIActions| {
                    let mut frame = UIActions::new();
                    render_record_button(ui, &data);
                    render_record_window(ui.ctx(), &data, &mut frame);
                    actions.commands.extend(frame.commands);
                    actions.session.choose_record_folder |= frame.session.choose_record_folder;
                    if frame.session.reveal_file.is_some() {
                        actions.session.reveal_file = frame.session.reveal_file;
                    }
                },
                UIActions::new(),
            )
    }

    #[test]
    fn rec_opens_the_window_and_start_records() {
        let mut h = harness(UIData::test_fixture());
        h.run();
        h.get_by_label("REC").click();
        h.run();
        h.get_by_label("Start recording").click();
        h.run();
        assert!(h
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::StartRecording)));
        h.get_by_label("Choose…").click();
        h.run();
        assert!(
            h.state().session.choose_record_folder,
            "the folder picker is asked for"
        );
    }

    #[test]
    fn prores_greys_out_the_format_and_bit_rate() {
        let mut data = UIData::test_fixture();
        data.record.options.codec = RecordCodec::ProRes;
        data.record.options.format = RecordFormat::Mov;
        let mut h = harness(data);
        h.run();
        h.get_by_label("REC").click();
        h.run();
        assert!(h.query_by_label("Auto").is_none(), "no bit rate for ProRes");
        h.get_by_label("Uncompressed (PCM), as ProRes expects");
    }

    #[test]
    fn stopping_takes_two_clicks_and_the_file_can_be_shown() {
        let mut data = UIData::test_fixture();
        data.record.status = recording(None);
        let mut h = harness(data);
        h.get_by_label_contains("REC").click();
        h.run_steps(2);
        h.get_by_label("Show in Finder").click();
        h.run_steps(2);
        assert!(h.state().session.reveal_file.is_some());
        h.get_by_label("Stop recording").click();
        h.run_steps(2);
        assert!(!h
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::StopRecording)));
        h.get_by_label("Click again to stop").click();
        h.run_steps(2);
        assert!(h
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::StopRecording)));
    }
}
