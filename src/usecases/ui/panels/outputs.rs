//! Unified output management and warp calibration.

use std::fmt::Write as _;

use super::super::{UIActions, UIData};
use crate::engine::EngineCommand;
use crate::renderer::context::{CalibrationMode, OutputTarget};

pub(super) fn render_output_section(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    // New output buttons
    ui.horizontal_wrapped(|ui| {
        if ui.button("+ Windowed").clicked() {
            actions.commands.push(EngineCommand::CreateOutput);
        }
        if ui.button("+ Recording").clicked() {
            use crate::renderer::context::RecordingCodec;
            actions.commands.push(EngineCommand::CreateHeadlessOutput {
                target: OutputTarget::Recording {
                    path: "output.mp4".to_string(),
                    codec: RecordingCodec::H264,
                    audio_device: None,
                },
            });
        }
        if ui.button("+ Stream").clicked() {
            actions.commands.push(EngineCommand::CreateHeadlessOutput {
                target: OutputTarget::NdiSend {
                    sender_name: "Nida NDI".to_string(),
                },
            });
        }
        // A Blackmagic device's video out — HDMI and SDI — in the format
        // nearest the show's rate (see `crate::decklink`).
        for device in &data.decklink_devices {
            let Some(mode) = device.mode_for(data.target_fps) else {
                continue;
            };
            if ui
                .button(format!("+ {}", device.name))
                .on_hover_text(format!(
                    "A Blackmagic output: its HDMI and SDI, {}; Start puts the show on it",
                    mode.name
                ))
                .clicked()
            {
                actions.commands.push(EngineCommand::CreateHeadlessOutput {
                    target: OutputTarget::DeckLink {
                        device: device.name.clone(),
                        mode: mode.name.clone(),
                        full_range: false,
                    },
                });
            }
        }
        if ui
            .button("Screens…")
            .on_hover_text(
                "The screens connected: name each one (kept on this computer) and \
                 Identify them",
            )
            .clicked()
        {
            super::screens::open(ui.ctx());
        }
        if ui
            .button("LED screens…")
            .on_hover_text(
                "Several LED screens on one signal to the LED processor, each at its \
                 own pixels, showing Main or a bus",
            )
            .clicked()
        {
            super::led::open(ui.ctx());
        }
    });

    ui.add_space(4.0);

    // List all outputs (unified)
    if data.outputs.is_empty() {
        ui.label(
            egui::RichText::new("No outputs")
                .small()
                .color(egui::Color32::GRAY),
        );
    } else {
        for (idx, output) in data.outputs.iter().enumerate() {
            egui::Frame::default()
                .inner_margin(6.0)
                .corner_radius(4.0)
                .fill(egui::Color32::from_rgb(30, 30, 45))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        // Status indicator
                        let status_color = if output.is_active {
                            egui::Color32::from_rgb(80, 255, 80)
                        } else {
                            egui::Color32::from_rgb(128, 128, 128)
                        };
                        ui.colored_label(status_color, "●");
                        ui.label(egui::RichText::new(&output.name).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("x").on_hover_text("Close output").clicked() {
                                actions.commands.push(EngineCommand::CloseOutput {
                                    output_uuid: output.uuid.clone(),
                                });
                            }
                        });
                    });

                    // Target label
                    ui.label(egui::RichText::new(&output.target_label).small().weak());

                    // Audio passthrough health (active outputs with audio only)
                    if let Some(audio) = &output.audio_passthrough {
                        let color = if audio.frames_dropped > 0 {
                            egui::Color32::from_rgb(255, 200, 80)
                        } else {
                            egui::Color32::from_rgb(120, 200, 255)
                        };
                        // Spliced silence is the audible part of a drop, so it
                        // is what the warning reports; the chunk count alone
                        // says nothing about how long the interruption was.
                        let mut text = format!(
                            "♪ {} — {} sent, {} dropped",
                            audio.device, audio.frames_written, audio.frames_dropped
                        );
                        if audio.silence_spliced > 0 {
                            let _ = write!(text, ", {} samples muted", audio.silence_spliced);
                        }
                        ui.label(egui::RichText::new(text).small().color(color));
                    }

                    if let Some(delivery) = &output.delivery {
                        let color = if delivery.frames_dropped > 0 {
                            egui::Color32::from_rgb(255, 200, 80)
                        } else {
                            egui::Color32::from_rgb(120, 200, 255)
                        };
                        let mut text = format!(
                            "▸ {} sent, {} dropped",
                            delivery.frames_written, delivery.frames_dropped
                        );
                        if delivery.frames_padded > 0 {
                            let _ = write!(text, ", {} padded", delivery.frames_padded);
                        }
                        ui.label(egui::RichText::new(text).small().color(color));
                    }

                    // Preview toggle + image
                    {
                        let preview_id = egui::Id::new("output_preview_toggle").with(&output.uuid);
                        let show_preview: bool =
                            ui.data(|d| d.get_temp(preview_id)).unwrap_or(false);
                        let toggle_label = if show_preview {
                            "▼ Hide Preview"
                        } else {
                            "▶ Show Preview"
                        };
                        if ui
                            .small_button(egui::RichText::new(toggle_label).small())
                            .clicked()
                        {
                            ui.data_mut(|d| d.insert_temp(preview_id, !show_preview));
                        }
                        if show_preview {
                            if let Some(&tex_id) = data.output_preview_textures.get(&idx) {
                                let width = ui.available_width().min(320.0);
                                // The output's own texture size, not the render
                                // resolution: a windowed output previews its
                                // window, which can be a different shape.
                                let size = super::utils::preview_size(
                                    egui::vec2(width, width),
                                    output.preview_width,
                                    output.preview_height,
                                );
                                ui.image(egui::load::SizedTexture::new(tex_id, size));
                            } else {
                                ui.label(
                                    egui::RichText::new("No preview available").small().weak(),
                                );
                            }
                        }
                    }

                    if output.is_windowed {
                        // Windowed output controls
                        render_windowed_controls(ui, &output.uuid, output, data, actions);
                    } else {
                        // Headless output controls (recording/SRT/NDI/Syphon)
                        render_headless_controls(ui, &output.uuid, output, data, actions);
                    }
                    // A Blackmagic output is 8-bit YUV whatever is asked:
                    // its card has its own Levels instead.
                    if !matches!(output.target, OutputTarget::DeckLink { .. }) {
                        render_presentation_controls(ui, &output.uuid, output, actions);
                    }
                });
            ui.add_space(4.0);
        }
    }
}

fn render_presentation_controls(
    ui: &mut egui::Ui,
    output_uuid: &str,
    output: &super::super::OutputUI,
    actions: &mut UIActions,
) {
    use crate::engine::value::render::{PresentationDepth, PresentationRequest};

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("SDR precision:").small());
        egui::ComboBox::from_id_salt(format!("presentation_depth_{output_uuid}"))
            .selected_text(egui::RichText::new(output.presentation_request.depth.label()).small())
            .width(100.0)
            .show_ui(ui, |ui| {
                for depth in PresentationDepth::ALL {
                    if ui
                        .selectable_label(output.presentation_request.depth == depth, depth.label())
                        .clicked()
                    {
                        actions.commands.push(EngineCommand::SetOutputPresentation {
                            output_uuid: output_uuid.to_string(),
                            request: PresentationRequest {
                                depth,
                                ..output.presentation_request
                            },
                        });
                    }
                }
            });

        let mut dither = output.presentation_request.dither;
        if ui
            .checkbox(&mut dither, egui::RichText::new("Dither").small())
            .changed()
        {
            actions.commands.push(EngineCommand::SetOutputPresentation {
                output_uuid: output_uuid.to_string(),
                request: PresentationRequest {
                    dither,
                    ..output.presentation_request
                },
            });
        }
    });
    // A screen's levels: only a screen's — a recording or a stream is video
    // levels already, and a Blackmagic output too.
    if output.is_windowed {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Levels:").small());
            let current = output.presentation_request.video_levels;
            let label = |video: bool| if video { "Video (16–235)" } else { "Full (0–255)" };
            egui::ComboBox::from_id_salt(format!("presentation_levels_{output_uuid}"))
                .selected_text(egui::RichText::new(label(current)).small())
                .width(110.0)
                .show_ui(ui, |ui| {
                    for video in [false, true] {
                        if ui.selectable_label(current == video, label(video)).clicked()
                            && current != video
                        {
                            actions.commands.push(EngineCommand::SetOutputPresentation {
                                output_uuid: output_uuid.to_string(),
                                request: PresentationRequest {
                                    video_levels: video,
                                    ..output.presentation_request
                                },
                            });
                        }
                    }
                })
                .response
                .on_hover_text(
                    "Full: black at 0, white at 255, as a computer monitor takes it. Video: \
                     black at 16, white at 235 — for gear that reads video levels (some LED \
                     processors, a monitor set to limited range), which otherwise crushes the \
                     blacks and clips the whites. Check with the test card's 0/16/235/255 \
                     patches.",
                );
        });
    }

    let resolved = &output.resolved_presentation;
    let status = format!(
        "Delivering {} · {}",
        resolved.resolved.label(),
        resolved.pixel_format
    );
    let color = if resolved.fallback_reason.is_some() {
        egui::Color32::from_rgb(255, 190, 80)
    } else {
        egui::Color32::from_rgb(120, 200, 255)
    };
    ui.label(egui::RichText::new(status).small().color(color));
    if let Some(reason) = &resolved.fallback_reason {
        ui.label(
            egui::RichText::new(format!("10-bit fallback: {reason}"))
                .small()
                .color(color),
        );
    }
}

/// Controls specific to windowed outputs (display selector, calibration, surfaces).
fn render_windowed_controls(
    ui: &mut egui::Ui,
    output_uuid: &str,
    output: &super::super::OutputUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    // Display target selector
    // A screen shown as it is called (see `app::screens`).
    let shown = match &output.target {
        OutputTarget::Display { name, .. } => data
            .available_monitors
            .iter()
            .find(|m| &m.name == name)
            .map_or_else(|| output.target_label.clone(), |m| m.label.clone()),
        _ => output.target_label.clone(),
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Display:").small());
        egui::ComboBox::from_id_salt(format!("output_target_{output_uuid}"))
            .selected_text(egui::RichText::new(shown).small())
            .width(160.0)
            .show_ui(ui, |ui| {
                let is_windowed = matches!(output.target, OutputTarget::Windowed);
                if ui.selectable_label(is_windowed, "Windowed").clicked() {
                    actions.commands.push(EngineCommand::SetOutputTarget {
                        output_uuid: output_uuid.to_string(),
                        target: OutputTarget::Windowed,
                    });
                }
                for monitor in &data.available_monitors {
                    let label = super::switcher::screen_label(monitor);
                    if ui.selectable_label(false, &label).clicked() {
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: OutputTarget::Display {
                                name: monitor.name.clone(),
                                monitor_index: monitor.index,
                            },
                        });
                    }
                }
            });
    });

    // Rotation selector
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Rotation:").small());
        egui::ComboBox::from_id_salt(format!("output_rotation_{output_uuid}"))
            .selected_text(egui::RichText::new(output.rotation.label()).small())
            .width(80.0)
            .show_ui(ui, |ui| {
                for rot in crate::renderer::context::OutputRotation::ALL {
                    if ui
                        .selectable_label(output.rotation == rot, rot.label())
                        .clicked()
                    {
                        actions.commands.push(EngineCommand::SetOutputRotation {
                            output_uuid: output_uuid.to_string(),
                            rotation: rot,
                        });
                    }
                }
            });
    });

    // Calibration mode selector (Off / Projector test card / per-Surface cards).
    // Warp editing itself now lives in the stage editor's bottom detail bar.
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("🔧 Calibrate:").small());
        for (label, mode) in [
            ("Off", CalibrationMode::Off),
            ("Projector", CalibrationMode::Projector),
            ("Surfaces", CalibrationMode::Surfaces),
        ] {
            if ui
                .selectable_label(output.calibration_mode == mode, label)
                .clicked()
            {
                actions.commands.push(EngineCommand::SetCalibrationMode {
                    output_uuid: output_uuid.to_string(),
                    mode,
                });
            }
        }
    });

    // Surface assignments
    ui.add_space(2.0);
    ui.label(egui::RichText::new("Surfaces:").small().strong());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(format!("assign_surf_{output_uuid}"))
            .selected_text("+ Assign Surface")
            .width(140.0)
            .show_ui(ui, |ui| {
                for surface in &data.surfaces {
                    let already_assigned = output
                        .surface_assignments
                        .iter()
                        .any(|a| a.surface_uuid == surface.uuid);
                    if !already_assigned && ui.selectable_label(false, &surface.name).clicked() {
                        actions.commands.push(EngineCommand::AssignSurfaceToOutput {
                            output_uuid: output_uuid.to_string(),
                            surface_uuid: surface.uuid.clone(),
                        });
                    }
                }
            });
    });

    for assignment in &output.surface_assignments {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&assignment.surface_name).small());
            if ui.small_button("x").on_hover_text("Unassign").clicked() {
                actions
                    .commands
                    .push(EngineCommand::UnassignSurfaceFromOutput {
                        output_uuid: output_uuid.to_string(),
                        surface_uuid: assignment.surface_uuid.clone(),
                    });
            }
        });
    }

    // Edge blending
    render_edge_blend_controls(ui, output_uuid, output, actions);
}

/// Controls specific to headless outputs (start/stop, duration, inline config).
fn render_headless_controls(
    ui: &mut egui::Ui,
    output_uuid: &str,
    output: &super::super::OutputUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    use crate::renderer::context::RecordingCodec;

    render_source_picker(ui, output_uuid, output, data, actions);

    // Inline config for Recording outputs
    if let OutputTarget::Recording {
        ref path,
        ref codec,
        ref audio_device,
    } = output.target
    {
        if !output.is_active {
            // Codec selector
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Codec:").small());
                let codec_id = egui::Id::new(format!("rec_codec_{output_uuid}"));
                egui::ComboBox::from_id_salt(codec_id)
                    .selected_text(egui::RichText::new(codec.to_string()).small())
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for c in &[
                            RecordingCodec::H264,
                            RecordingCodec::H265,
                            RecordingCodec::AV1,
                            RecordingCodec::ProRes,
                            RecordingCodec::ProRes4444,
                            RecordingCodec::Hap,
                            RecordingCodec::HapAlpha,
                            RecordingCodec::HapQ,
                        ] {
                            if ui.selectable_label(*codec == *c, c.to_string()).clicked() {
                                actions.commands.push(EngineCommand::SetOutputTarget {
                                    output_uuid: output_uuid.to_string(),
                                    target: OutputTarget::Recording {
                                        path: path.clone(),
                                        codec: c.clone(),
                                        audio_device: audio_device.clone(),
                                    },
                                });
                            }
                        }
                    });
            });
            // File path input
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("File:").small());
                let path_id = egui::Id::new(format!("rec_path_{output_uuid}"));
                let mut current_path: String = ui
                    .data(|d| d.get_temp(path_id))
                    .unwrap_or_else(|| path.clone());
                let response = ui.add(
                    egui::TextEdit::singleline(&mut current_path)
                        .desired_width(160.0)
                        .font(egui::TextStyle::Small),
                );
                if response.lost_focus() || response.changed() {
                    ui.data_mut(|d| d.insert_temp(path_id, current_path.clone()));
                    if response.lost_focus() {
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: OutputTarget::Recording {
                                path: current_path,
                                codec: codec.clone(),
                                audio_device: audio_device.clone(),
                            },
                        });
                    }
                }
            });
        }
    }

    // Unified stream config (SRT, HLS, DASH, RTMP, NDI, Syphon)
    let is_stream = matches!(
        output.target,
        OutputTarget::SrtStream { .. }
            | OutputTarget::HlsStream { .. }
            | OutputTarget::DashStream { .. }
            | OutputTarget::RtmpStream { .. }
            | OutputTarget::NdiSend { .. }
            | OutputTarget::SyphonServer { .. }
    );
    if is_stream {
        render_stream_config(ui, output_uuid, output, actions);
    }
    if let OutputTarget::DeckLink {
        device,
        mode,
        full_range,
    } = &output.target
    {
        render_blackmagic_config(
            ui,
            output_uuid,
            output.is_active,
            (device, mode, *full_range),
            data,
            actions,
        );
    }

    // Audio passthrough device selector (ffmpeg targets only; locked while active)
    let is_ffmpeg = matches!(
        output.target,
        OutputTarget::Recording { .. }
            | OutputTarget::SrtStream { .. }
            | OutputTarget::HlsStream { .. }
            | OutputTarget::DashStream { .. }
            | OutputTarget::RtmpStream { .. }
    );
    if is_ffmpeg && !output.is_active {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Audio:").small());
            let current = output.target.audio_device();
            let bus_name = output.audio_bus.as_ref().map(|uuid| {
                data.audio_mix
                    .buses
                    .iter()
                    .find(|b| &b.config.uuid == uuid)
                    .map_or_else(
                        || "⚠ removed bus".to_string(),
                        |b| format!("Bus: {}", b.config.name),
                    )
            });
            let selected_text = bus_name
                .clone()
                .unwrap_or_else(|| current.unwrap_or("None (silent)").to_string());
            // A bus and a direct device are alternatives: picking one clears
            // the other.
            let clear_bus = |actions: &mut UIActions| {
                if output.audio_bus.is_some() {
                    actions.commands.push(EngineCommand::SetOutputAudioBus {
                        output_uuid: output_uuid.to_string(),
                        bus_uuid: None,
                    });
                }
            };
            egui::ComboBox::from_id_salt(format!("out_audio_{output_uuid}"))
                .selected_text(egui::RichText::new(selected_text).small())
                .width(160.0)
                .show_ui(ui, |ui| {
                    if ui
                        .selectable_label(
                            current.is_none() && output.audio_bus.is_none(),
                            "None (silent)",
                        )
                        .clicked()
                    {
                        clear_bus(actions);
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: output.target.with_audio_device(None),
                        });
                    }
                    for bus in &data.audio_mix.buses {
                        let on = output.audio_bus.as_deref() == Some(bus.config.uuid.as_str());
                        if ui
                            .selectable_label(on, format!("Bus: {}", bus.config.name))
                            .on_hover_text("The audio mixer's bus, as mixed")
                            .clicked()
                            && !on
                        {
                            actions.commands.push(EngineCommand::SetOutputAudioBus {
                                output_uuid: output_uuid.to_string(),
                                bus_uuid: Some(bus.config.uuid.clone()),
                            });
                            if current.is_some() {
                                actions.commands.push(EngineCommand::SetOutputTarget {
                                    output_uuid: output_uuid.to_string(),
                                    target: output.target.with_audio_device(None),
                                });
                            }
                        }
                    }
                    for dev in &data.audio.devices {
                        let selected =
                            output.audio_bus.is_none() && current == Some(dev.name.as_str());
                        if ui.selectable_label(selected, &dev.name).clicked() {
                            clear_bus(actions);
                            actions.commands.push(EngineCommand::SetOutputTarget {
                                output_uuid: output_uuid.to_string(),
                                target: output.target.with_audio_device(Some(dev.name.clone())),
                            });
                        }
                    }
                });
        });
    }

    // An NDI sender carries an audio bus, changed over at once, on air too.
    if matches!(output.target, OutputTarget::NdiSend { .. }) {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Audio:").small());
            let bus_name = |uuid: &str| {
                data.audio_mix
                    .buses
                    .iter()
                    .find(|b| b.config.uuid == uuid)
                    .map_or_else(
                        || "⚠ removed bus".to_string(),
                        |b| format!("Bus: {}", b.config.name),
                    )
            };
            let selected_text = output
                .audio_bus
                .as_deref()
                .map_or_else(|| "None (silent)".to_string(), bus_name);
            egui::ComboBox::from_id_salt(format!("ndi_audio_{output_uuid}"))
                .selected_text(egui::RichText::new(selected_text).small())
                .width(160.0)
                .show_ui(ui, |ui| {
                    let mut choose = |bus_uuid: Option<String>| {
                        actions.commands.push(EngineCommand::SetOutputAudioBus {
                            output_uuid: output_uuid.to_string(),
                            bus_uuid,
                        });
                    };
                    if ui
                        .selectable_label(output.audio_bus.is_none(), "None (silent)")
                        .clicked()
                    {
                        choose(None);
                    }
                    for bus in &data.audio_mix.buses {
                        let on = output.audio_bus.as_deref() == Some(bus.config.uuid.as_str());
                        if ui
                            .selectable_label(on, format!("Bus: {}", bus.config.name))
                            .on_hover_text("Sent with the picture, as the audio mixer mixes it")
                            .clicked()
                            && !on
                        {
                            choose(Some(bus.config.uuid.clone()));
                        }
                    }
                });
        });
    }

    // Rotation selector
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Rotation:").small());
        egui::ComboBox::from_id_salt(format!("headless_rotation_{output_uuid}"))
            .selected_text(egui::RichText::new(output.rotation.label()).small())
            .width(80.0)
            .show_ui(ui, |ui| {
                for rot in crate::renderer::context::OutputRotation::ALL {
                    if ui
                        .selectable_label(output.rotation == rot, rot.label())
                        .clicked()
                    {
                        actions.commands.push(EngineCommand::SetOutputRotation {
                            output_uuid: output_uuid.to_string(),
                            rotation: rot,
                        });
                    }
                }
            });
    });

    // Surface assignments
    ui.add_space(2.0);
    ui.label(egui::RichText::new("Surfaces:").small().strong());
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(format!("assign_surf_{output_uuid}"))
            .selected_text("+ Assign Surface")
            .width(140.0)
            .show_ui(ui, |ui| {
                for surface in &data.surfaces {
                    let already_assigned = output
                        .surface_assignments
                        .iter()
                        .any(|a| a.surface_uuid == surface.uuid);
                    if !already_assigned && ui.selectable_label(false, &surface.name).clicked() {
                        actions.commands.push(EngineCommand::AssignSurfaceToOutput {
                            output_uuid: output_uuid.to_string(),
                            surface_uuid: surface.uuid.clone(),
                        });
                    }
                }
            });
    });

    for assignment in &output.surface_assignments {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(&assignment.surface_name).small());
            if ui.small_button("x").on_hover_text("Unassign").clicked() {
                actions
                    .commands
                    .push(EngineCommand::UnassignSurfaceFromOutput {
                        output_uuid: output_uuid.to_string(),
                        surface_uuid: assignment.surface_uuid.clone(),
                    });
            }
        });
    }

    // Start/Stop + duration
    ui.horizontal(|ui| {
        if output.is_active {
            let dur = output.active_duration.as_secs_f32();
            ui.label(
                egui::RichText::new(format!("{dur:.1}s"))
                    .monospace()
                    .color(egui::Color32::from_rgb(255, 80, 80)),
            );
            if ui.button("⏹ Stop").clicked() {
                actions.commands.push(EngineCommand::StopOutput {
                    output_uuid: output_uuid.to_string(),
                });
            }
        } else if ui.button("▶ Start").clicked() {
            actions.commands.push(EngineCommand::StartOutput {
                output_uuid: output_uuid.to_string(),
            });
        }
    });

    // Edge blending
    render_edge_blend_controls(ui, output_uuid, output, actions);
}

/// Unified stream output config with protocol dropdown (SRT, HLS, DASH, RTMP, NDI, Syphon).
/// What a recording, stream or NDI output sends: the master, or one bus's
/// program — vMix's per-output source. Changing it mid-recording is fine; it
/// only changes which picture is copied in.
fn render_source_picker(
    ui: &mut egui::Ui,
    output_uuid: &str,
    output: &super::super::OutputUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    use crate::renderer::context::OutputSource;

    let Some(source) = &output.source else {
        return;
    };
    let current = match source {
        OutputSource::BusProgram(uuid) => {
            data.buses.iter().find(|b| &b.bus.uuid == uuid).map_or_else(
                || "⚠ removed bus — black".to_string(),
                |b| b.bus.name.clone(),
            )
        }
        _ => "Master".to_string(),
    };
    let mut pick = |source: OutputSource| {
        actions.commands.push(EngineCommand::SetOutputSource {
            output_uuid: output_uuid.to_string(),
            source,
        });
    };
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Source:").small());
        egui::ComboBox::from_id_salt(format!("out_source_{output_uuid}"))
            .selected_text(egui::RichText::new(current).small())
            .width(140.0)
            .show_ui(ui, |ui| {
                let on_master = *source == OutputSource::Master;
                if ui
                    .selectable_label(on_master, "Master")
                    .on_hover_text("The main output, every layer")
                    .clicked()
                    && !on_master
                {
                    pick(OutputSource::Master);
                }
                for bus_ui in &data.buses {
                    let bus = &bus_ui.bus;
                    let on_it = matches!(source, OutputSource::BusProgram(u) if *u == bus.uuid);
                    if ui
                        .selectable_label(on_it, &bus.name)
                        .on_hover_text(format!("Whatever {} has on Program", bus.name))
                        .clicked()
                        && !on_it
                    {
                        pick(OutputSource::BusProgram(bus.uuid.clone()));
                    }
                }
            });
    });
    if !output.surface_assignments.is_empty() {
        ui.label(
            egui::RichText::new("Sends its assigned surfaces instead")
                .small()
                .weak(),
        );
    }
}

/// A Blackmagic output's format, picked from its device's, while it is
/// stopped; whether the device is there at all.
fn render_blackmagic_config(
    ui: &mut egui::Ui,
    output_uuid: &str,
    active: bool,
    (device, mode, full_range): (&str, &str, bool),
    data: &UIData,
    actions: &mut UIActions,
) {
    let target = |mode: String, full_range: bool| OutputTarget::DeckLink {
        device: device.to_string(),
        mode,
        full_range,
    };
    // Levels: chosen per output, running or not — taken up at once.
    ui.horizontal(|ui| {
        let label = ui.label(egui::RichText::new("Levels:").small());
        let shown = |full: bool| if full { "Full (0–255)" } else { "Video (16–235)" };
        egui::ComboBox::from_id_salt(format!("blackmagic_levels_{output_uuid}"))
            .selected_text(egui::RichText::new(shown(full_range)).small())
            .width(110.0)
            .show_ui(ui, |ui| {
                for full in [false, true] {
                    if ui.selectable_label(full_range == full, shown(full)).clicked()
                        && full != full_range
                    {
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: target(mode.to_string(), full),
                        });
                    }
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(
                "Video: black at 16, white at 235, as video gear takes it. Full: black at 0, \
                 white at 255, for a display set to take full levels (its HDMI black level \
                 at Full or High).",
            );
    });
    // The test card on the device — its levels strip shows them there.
    let mut card = data.outputs.iter().any(|o| {
        o.uuid == output_uuid && o.calibration_mode == CalibrationMode::Projector
    });
    if ui
        .checkbox(&mut card, egui::RichText::new("Test card").small())
        .on_hover_text(
            "The calibration card on the device in place of the picture: its strip of \
             codes 0, 16, 235 and 255 shows how the Levels come out there",
        )
        .changed()
    {
        actions.commands.push(EngineCommand::SetCalibrationMode {
            output_uuid: output_uuid.to_string(),
            mode: if card {
                CalibrationMode::Projector
            } else {
                CalibrationMode::Off
            },
        });
    }
    let Some(found) = data.decklink_devices.iter().find(|d| d.name == device) else {
        ui.label(
            egui::RichText::new(format!("⚠ {device} is not connected"))
                .small()
                .color(egui::Color32::from_rgb(255, 180, 80)),
        );
        return;
    };
    if active {
        ui.label(egui::RichText::new(format!("Format: {mode}")).small().weak());
        return;
    }
    ui.horizontal(|ui| {
        let label = ui.label(egui::RichText::new("Format:").small());
        let mut chosen = mode.to_string();
        egui::ComboBox::from_id_salt(format!("blackmagic_mode_{output_uuid}"))
            .selected_text(egui::RichText::new(mode).small())
            .width(110.0)
            .show_ui(ui, |ui| {
                for m in &found.modes {
                    ui.selectable_value(&mut chosen, m.name.clone(), &m.name);
                }
            })
            .response
            .labelled_by(label.id);
        if chosen != mode {
            actions.commands.push(EngineCommand::SetOutputTarget {
                output_uuid: output_uuid.to_string(),
                target: target(chosen, full_range),
            });
        }
    });
}

fn render_stream_config(
    ui: &mut egui::Ui,
    output_uuid: &str,
    output: &super::super::OutputUI,
    actions: &mut UIActions,
) {
    use crate::renderer::context::{RtmpCodecContract, SrtCodec, StreamingCodec};

    // Determine current protocol label
    let current_proto = match &output.target {
        OutputTarget::SrtStream { .. } => "SRT",
        OutputTarget::HlsStream { .. } => "HLS",
        OutputTarget::DashStream { .. } => "DASH",
        OutputTarget::RtmpStream { .. } => "RTMP",
        OutputTarget::NdiSend { .. } => "NDI",
        OutputTarget::SyphonServer { .. } => "Syphon",
        _ => return,
    };

    // Protocol dropdown (disabled while active)
    if output.is_active {
        ui.label(
            egui::RichText::new(format!("Protocol: {current_proto}"))
                .small()
                .weak(),
        );
    } else {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Protocol:").small());
            egui::ComboBox::from_id_salt(format!("stream_proto_{output_uuid}"))
                .selected_text(egui::RichText::new(current_proto).small())
                .width(80.0)
                .show_ui(ui, |ui| {
                    // `mut` is required on macOS for the Syphon push below; on other
                    // platforms that push is compiled out, leaving the binding unused-mut.
                    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
                    let mut protocols: Vec<(&str, OutputTarget)> = vec![
                        (
                            "SRT",
                            OutputTarget::SrtStream {
                                url: "srt://0.0.0.0:9001".to_string(),
                                codec: SrtCodec::default(),
                                audio_device: None,
                            },
                        ),
                        (
                            "HLS",
                            OutputTarget::HlsStream {
                                name: "live".to_string(),
                                codec: StreamingCodec::default(),
                                low_latency: false,
                                audio_device: None,
                            },
                        ),
                        (
                            "DASH",
                            OutputTarget::DashStream {
                                name: "live".to_string(),
                                codec: StreamingCodec::default(),
                                audio_device: None,
                            },
                        ),
                        (
                            "RTMP",
                            OutputTarget::RtmpStream {
                                url: "rtmp://".to_string(),
                                codec: StreamingCodec::default(),
                                codec_contract: RtmpCodecContract::default(),
                                audio_device: None,
                            },
                        ),
                        (
                            "NDI",
                            OutputTarget::NdiSend {
                                sender_name: "Nida NDI".to_string(),
                            },
                        ),
                    ];
                    #[cfg(target_os = "macos")]
                    protocols.push((
                        "Syphon",
                        OutputTarget::SyphonServer {
                            server_name: "Nida".to_string(),
                        },
                    ));
                    for (label, default_target) in &protocols {
                        if ui
                            .selectable_label(current_proto == *label, *label)
                            .clicked()
                            && current_proto != *label
                        {
                            actions.commands.push(EngineCommand::SetOutputTarget {
                                output_uuid: output_uuid.to_string(),
                                target: default_target.clone(),
                            });
                        }
                    }
                });
        });
    }

    // Protocol-specific config
    match &output.target {
        OutputTarget::SrtStream {
            ref url,
            ref codec,
            ref audio_device,
        } => {
            if !output.is_active {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Codec:").small());
                    egui::ComboBox::from_id_salt(format!("srt_codec_{output_uuid}"))
                        .selected_text(egui::RichText::new(codec.to_string()).small())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for c in &[SrtCodec::H264, SrtCodec::H265] {
                                if ui.selectable_label(*codec == *c, c.to_string()).clicked() {
                                    actions.commands.push(EngineCommand::SetOutputTarget {
                                        output_uuid: output_uuid.to_string(),
                                        target: OutputTarget::SrtStream {
                                            url: url.clone(),
                                            codec: c.clone(),
                                            audio_device: audio_device.clone(),
                                        },
                                    });
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("URL:").small());
                    let url_id = egui::Id::new(format!("srt_url_{output_uuid}"));
                    let mut current_url: String = ui
                        .data(|d| d.get_temp(url_id))
                        .unwrap_or_else(|| url.clone());
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut current_url)
                            .desired_width(180.0)
                            .font(egui::TextStyle::Small),
                    );
                    if response.lost_focus() || response.changed() {
                        ui.data_mut(|d| d.insert_temp(url_id, current_url.clone()));
                        if response.lost_focus() {
                            actions.commands.push(EngineCommand::SetOutputTarget {
                                output_uuid: output_uuid.to_string(),
                                target: OutputTarget::SrtStream {
                                    url: current_url,
                                    codec: codec.clone(),
                                    audio_device: audio_device.clone(),
                                },
                            });
                        }
                    }
                });
            }
        }
        OutputTarget::HlsStream {
            ref name,
            ref codec,
            low_latency,
            ref audio_device,
        } => {
            render_hls_dash_name_codec(
                ui,
                output_uuid,
                "hls",
                name,
                codec,
                output.is_active,
                actions,
                |n, c| OutputTarget::HlsStream {
                    name: n,
                    codec: c,
                    low_latency: *low_latency,
                    audio_device: audio_device.clone(),
                },
            );
            if !output.is_active {
                ui.horizontal(|ui| {
                    let mut ll = *low_latency;
                    if ui
                        .checkbox(&mut ll, egui::RichText::new("LL-HLS (Low Latency)").small())
                        .changed()
                    {
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: OutputTarget::HlsStream {
                                name: name.clone(),
                                codec: codec.clone(),
                                low_latency: ll,
                                audio_device: audio_device.clone(),
                            },
                        });
                    }
                });
            }
            let player_url = format!("http://localhost:8080/streams/{name}/player.html");
            let manifest_url = format!("http://localhost:8080/streams/{name}/index.m3u8");
            render_copyable_url(ui, "▶", &player_url, 10.0, actions);
            render_copyable_url(ui, "🌐", &manifest_url, 9.0, actions);
        }
        OutputTarget::DashStream {
            ref name,
            ref codec,
            ref audio_device,
        } => {
            render_hls_dash_name_codec(
                ui,
                output_uuid,
                "dash",
                name,
                codec,
                output.is_active,
                actions,
                |n, c| OutputTarget::DashStream {
                    name: n,
                    codec: c,
                    audio_device: audio_device.clone(),
                },
            );
            let player_url = format!("http://localhost:8080/streams/{name}/player.html");
            let manifest_url = format!("http://localhost:8080/streams/{name}/manifest.mpd");
            render_copyable_url(ui, "▶", &player_url, 10.0, actions);
            render_copyable_url(ui, "🌐", &manifest_url, 9.0, actions);
        }
        OutputTarget::RtmpStream {
            ref url,
            ref codec,
            codec_contract,
            ref audio_device,
        } => {
            if !output.is_active {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Codec:").small());
                    egui::ComboBox::from_id_salt(format!("rtmp_codec_{output_uuid}"))
                        .selected_text(egui::RichText::new(codec.to_string()).small())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for c in &[
                                StreamingCodec::H264,
                                StreamingCodec::H265,
                                StreamingCodec::AV1,
                            ] {
                                if ui.selectable_label(*codec == *c, c.to_string()).clicked() {
                                    actions.commands.push(EngineCommand::SetOutputTarget {
                                        output_uuid: output_uuid.to_string(),
                                        target: OutputTarget::RtmpStream {
                                            url: url.clone(),
                                            codec: c.clone(),
                                            codec_contract: *codec_contract,
                                            audio_device: audio_device.clone(),
                                        },
                                    });
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Endpoint:").small());
                    egui::ComboBox::from_id_salt(format!("rtmp_contract_{output_uuid}"))
                        .selected_text(match codec_contract {
                            RtmpCodecContract::Legacy => "Legacy RTMP",
                            RtmpCodecContract::Enhanced => "Enhanced RTMP",
                        })
                        .show_ui(ui, |ui| {
                            for (contract, label) in [
                                (RtmpCodecContract::Legacy, "Legacy RTMP"),
                                (RtmpCodecContract::Enhanced, "Enhanced RTMP"),
                            ] {
                                if ui
                                    .selectable_label(*codec_contract == contract, label)
                                    .clicked()
                                {
                                    actions.commands.push(EngineCommand::SetOutputTarget {
                                        output_uuid: output_uuid.to_string(),
                                        target: OutputTarget::RtmpStream {
                                            url: url.clone(),
                                            codec: codec.clone(),
                                            codec_contract: contract,
                                            audio_device: audio_device.clone(),
                                        },
                                    });
                                }
                            }
                        });
                });
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("URL:").small());
                    let url_id = egui::Id::new(format!("rtmp_url_{output_uuid}"));
                    let mut current_url: String = ui
                        .data(|d| d.get_temp(url_id))
                        .unwrap_or_else(|| url.clone());
                    let response = ui.add(
                        egui::TextEdit::singleline(&mut current_url)
                            .desired_width(180.0)
                            .font(egui::TextStyle::Small),
                    );
                    if response.lost_focus() || response.changed() {
                        ui.data_mut(|d| d.insert_temp(url_id, current_url.clone()));
                        if response.lost_focus() {
                            actions.commands.push(EngineCommand::SetOutputTarget {
                                output_uuid: output_uuid.to_string(),
                                target: OutputTarget::RtmpStream {
                                    url: current_url,
                                    codec: codec.clone(),
                                    codec_contract: *codec_contract,
                                    audio_device: audio_device.clone(),
                                },
                            });
                        }
                    }
                });
            }
        }
        OutputTarget::NdiSend { ref sender_name } if !output.is_active => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Name:").small());
                let name_id = egui::Id::new(format!("ndi_name_{output_uuid}"));
                let mut current_name: String = ui
                    .data(|d| d.get_temp(name_id))
                    .unwrap_or_else(|| sender_name.clone());
                let response = ui.add(
                    egui::TextEdit::singleline(&mut current_name)
                        .desired_width(140.0)
                        .font(egui::TextStyle::Small),
                );
                if response.lost_focus() || response.changed() {
                    ui.data_mut(|d| d.insert_temp(name_id, current_name.clone()));
                    if response.lost_focus() {
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: OutputTarget::NdiSend {
                                sender_name: current_name,
                            },
                        });
                    }
                }
            });
        }
        OutputTarget::SyphonServer { ref server_name } if !output.is_active => {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Name:").small());
                let name_id = egui::Id::new(format!("syphon_name_{output_uuid}"));
                let mut current_name: String = ui
                    .data(|d| d.get_temp(name_id))
                    .unwrap_or_else(|| server_name.clone());
                let response = ui.add(
                    egui::TextEdit::singleline(&mut current_name)
                        .desired_width(140.0)
                        .font(egui::TextStyle::Small),
                );
                if response.lost_focus() || response.changed() {
                    ui.data_mut(|d| d.insert_temp(name_id, current_name.clone()));
                    if response.lost_focus() {
                        actions.commands.push(EngineCommand::SetOutputTarget {
                            output_uuid: output_uuid.to_string(),
                            target: OutputTarget::SyphonServer {
                                server_name: current_name,
                            },
                        });
                    }
                }
            });
        }
        _ => {}
    }
}

/// Render a clickable URL label that copies to clipboard on click.
fn render_copyable_url(
    ui: &mut egui::Ui,
    icon: &str,
    url: &str,
    font_size: f32,
    actions: &mut UIActions,
) {
    let text = format!("{icon} {url}");
    let response = ui.add(
        egui::Label::new(
            egui::RichText::new(&text)
                .size(font_size)
                .color(egui::Color32::from_rgb(130, 160, 200)),
        )
        .sense(egui::Sense::click()),
    );
    if response.clicked() {
        ui.ctx().copy_text(url.to_string());
        actions
            .session
            .info_notifications
            .push(format!("📋 Copied to clipboard: {url}"));
    }
    response.on_hover_text("Click to copy URL");
}

/// Shared codec + name config for HLS and DASH stream outputs.
// UI render fn taking many independent egui state/handle args; no shared invariant to bundle.
#[allow(clippy::too_many_arguments)]
fn render_hls_dash_name_codec(
    ui: &mut egui::Ui,
    output_uuid: &str,
    prefix: &str,
    name: &str,
    codec: &crate::renderer::context::StreamingCodec,
    is_active: bool,
    actions: &mut UIActions,
    make_target: impl Fn(String, crate::renderer::context::StreamingCodec) -> OutputTarget,
) {
    use crate::renderer::context::StreamingCodec;
    if !is_active {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Codec:").small());
            egui::ComboBox::from_id_salt(format!("{prefix}_codec_{output_uuid}"))
                .selected_text(egui::RichText::new(codec.to_string()).small())
                .width(120.0)
                .show_ui(ui, |ui| {
                    for c in &[
                        StreamingCodec::H264,
                        StreamingCodec::H265,
                        StreamingCodec::AV1,
                    ] {
                        if ui.selectable_label(*codec == *c, c.to_string()).clicked() {
                            actions.commands.push(EngineCommand::SetOutputTarget {
                                output_uuid: output_uuid.to_string(),
                                target: make_target(name.to_string(), c.clone()),
                            });
                        }
                    }
                });
        });
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Name:").small());
            let name_id = egui::Id::new(format!("{prefix}_name_{output_uuid}"));
            let mut current_name: String = ui
                .data(|d| d.get_temp(name_id))
                .unwrap_or_else(|| name.to_string());
            let response = ui.add(
                egui::TextEdit::singleline(&mut current_name)
                    .desired_width(140.0)
                    .font(egui::TextStyle::Small),
            );
            if response.lost_focus() || response.changed() {
                ui.data_mut(|d| d.insert_temp(name_id, current_name.clone()));
                if response.lost_focus() {
                    actions.commands.push(EngineCommand::SetOutputTarget {
                        output_uuid: output_uuid.to_string(),
                        target: make_target(current_name, codec.clone()),
                    });
                }
            }
        });
    }
}

/// Render edge blending controls for an output (shared by windowed and headless).
fn render_edge_blend_controls(
    ui: &mut egui::Ui,
    output_uuid: &str,
    output: &super::super::OutputUI,
    actions: &mut UIActions,
) {
    use crate::renderer::edge_blend::EdgeBlendMode;

    let collapse_id = egui::Id::new("edge_blend_section").with(output_uuid);
    egui::CollapsingHeader::new(egui::RichText::new("Edge Blending").small().strong())
        .id_salt(collapse_id)
        .default_open(false)
        .show(ui, |ui| {
            // Mode toggle: Auto / Manual
            let is_auto = output.edge_blend_mode == EdgeBlendMode::Auto;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Mode:").small());
                if ui
                    .selectable_label(!is_auto, egui::RichText::new("Manual").small())
                    .clicked()
                    && is_auto
                {
                    actions.commands.push(EngineCommand::SetEdgeBlendMode {
                        output_uuid: output_uuid.to_string(),
                        mode: EdgeBlendMode::Manual,
                    });
                }
                if ui
                    .selectable_label(is_auto, egui::RichText::new("Auto").small())
                    .clicked()
                    && !is_auto
                {
                    actions.commands.push(EngineCommand::SetEdgeBlendMode {
                        output_uuid: output_uuid.to_string(),
                        mode: EdgeBlendMode::Auto,
                    });
                }
            });

            let mut cfg = output.edge_blend;
            let mut changed = false;

            if is_auto {
                // Auto mode: show per-surface overlap zones (read-only)
                let mut any_zones = false;
                for sa in &output.surface_assignments {
                    if sa.overlap_zones.any_enabled() {
                        any_zones = true;
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(format!("{}:", sa.surface_name)).small());
                            ui.label(
                                egui::RichText::new(format!(
                                    "{} zone(s)",
                                    sa.overlap_zones.zones.len()
                                ))
                                .small()
                                .weak(),
                            );
                        });
                        for (zi, zone) in sa.overlap_zones.zones.iter().enumerate() {
                            let dir = match (zone.ramp_x as i32, zone.ramp_y as i32) {
                                (1, 0) => "→",
                                (-1, 0) => "←",
                                (0, 1) => "↓",
                                (0, -1) => "↑",
                                (1, 1) => "↘",
                                (-1, 1) => "↙",
                                (1, -1) => "↗",
                                (-1, -1) => "↖",
                                _ => "·",
                            };
                            ui.horizontal(|ui| {
                                ui.add_space(12.0);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Zone {}: UV [{:.2},{:.2}]→[{:.2},{:.2}] {} γ:{:.1}",
                                        zi + 1,
                                        zone.uv_rect[0],
                                        zone.uv_rect[1],
                                        zone.uv_rect[2],
                                        zone.uv_rect[3],
                                        dir,
                                        zone.gamma,
                                    ))
                                    .small()
                                    .weak(),
                                );
                            });
                        }
                    }
                }
                if !any_zones {
                    ui.label(
                        egui::RichText::new("No overlapping surfaces detected")
                            .small()
                            .weak(),
                    );
                }
            } else {
                // Manual mode: full per-edge controls (existing behavior)
                for (label, edge) in [
                    ("Left", &mut cfg.left),
                    ("Right", &mut cfg.right),
                    ("Top", &mut cfg.top),
                    ("Bottom", &mut cfg.bottom),
                ] {
                    ui.horizontal(|ui| {
                        if ui
                            .checkbox(&mut edge.enabled, egui::RichText::new(label).small())
                            .changed()
                        {
                            changed = true;
                        }
                        if edge.enabled {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new("W:").small());
                            if ui
                                .add(
                                    egui::Slider::new(&mut edge.width, 0.01..=0.5)
                                        .step_by(0.01)
                                        .max_decimals(2),
                                )
                                .on_hover_text("Blend zone width (fraction of output)")
                                .changed()
                            {
                                changed = true;
                            }
                            ui.label(egui::RichText::new("γ:").small());
                            if ui
                                .add(
                                    egui::Slider::new(&mut edge.gamma, 0.5..=4.0)
                                        .step_by(0.1)
                                        .max_decimals(1),
                                )
                                .on_hover_text("Gamma curve exponent")
                                .changed()
                            {
                                changed = true;
                            }
                        }
                    });
                }
            }

            if changed {
                actions.commands.push(EngineCommand::SetEdgeBlend {
                    output_uuid: output_uuid.to_string(),
                    config: cfg,
                });
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn sample_output(
        resolved: crate::engine::value::render::ResolvedPresentation,
    ) -> crate::usecases::ui::OutputUI {
        crate::usecases::ui::OutputUI {
            uuid: "out00001".to_string(),
            name: "Main".to_string(),
            target: OutputTarget::Windowed,
            target_label: "Windowed".to_string(),
            is_windowed: true,
            is_active: true,
            active_duration: std::time::Duration::ZERO,
            surface_assignments: vec![],
            calibration_mode: crate::renderer::context::CalibrationMode::Off,
            edge_blend_mode: crate::renderer::edge_blend::EdgeBlendMode::default(),
            edge_blend: crate::renderer::edge_blend::EdgeBlendConfig::default(),
            rotation: crate::renderer::context::OutputRotation::default(),
            presentation_request: crate::engine::value::render::PresentationRequest::default(),
            resolved_presentation: resolved,
            audio_passthrough: None,
            delivery: None,
            preview_width: 1920,
            preview_height: 1080,
            source: None,
            audio_bus: None,
            led: None,
        }
    }

    #[test]
    fn render_output_section_smoke() {
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        let _harness = egui_kittest::Harness::new_ui(|ui| {
            render_output_section(ui, &data, &mut actions);
        });
    }

    #[test]
    fn render_output_section_smoke_with_outputs() {
        let mut data = UIData::test_fixture();
        data.outputs.push(sample_output(
            crate::engine::value::render::ResolvedPresentation::default(),
        ));
        let mut actions = UIActions::new();
        let _harness = egui_kittest::Harness::new_ui(|ui| {
            render_output_section(ui, &data, &mut actions);
        });
    }

    #[test]
    fn dither_checkbox_emits_set_output_presentation() {
        let mut data = UIData::test_fixture();
        data.outputs.push(sample_output(
            crate::engine::value::render::ResolvedPresentation::default(),
        ));
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 640.0))
                .build_ui(|ui| {
                    render_output_section(ui, &data, &mut actions);
                });
            harness.get_by_label("Dither").click();
            harness.run();
        }
        assert!(actions.commands.iter().any(|command| matches!(
            command,
            EngineCommand::SetOutputPresentation {
                output_uuid,
                request
            } if output_uuid == "out00001" && !request.dither
        )));
    }

    #[test]
    fn fallback_reason_is_visible_on_the_output_card() {
        use crate::engine::value::render::{
            AlphaMode, PresentationColorProfile, PresentationDepth, PresentationPixelFormat,
            ResolvedPresentation,
        };
        let mut data = UIData::test_fixture();
        data.outputs.push(sample_output(ResolvedPresentation {
            requested: PresentationDepth::Sdr10,
            resolved: PresentationDepth::Sdr8,
            pixel_format: PresentationPixelFormat::Bgra8,
            color_profile: PresentationColorProfile::SrgbFull,
            alpha_mode: AlphaMode::Premultiplied,
            dither: true,
            fallback_reason: Some("Syphon interoperability is limited to BGRA8".into()),
        }));
        let mut actions = UIActions::new();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 640.0))
            .build_ui(|ui| {
                render_output_section(ui, &data, &mut actions);
            });
        harness.run();
        harness.get_by_label("Delivering 8-bit SDR · BGRA8");
        harness.get_by_label("10-bit fallback: Syphon interoperability is limited to BGRA8");
    }

    #[test]
    fn a_recording_can_follow_a_bus() {
        let mut data = UIData::test_fixture();
        let mut output =
            sample_output(crate::engine::value::render::ResolvedPresentation::default());
        output.is_windowed = false;
        output.target = OutputTarget::NdiSend {
            sender_name: "Nida NDI".into(),
        };
        output.source = Some(crate::renderer::context::OutputSource::Master);
        data.outputs = vec![output];
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 900.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            harness.get_by_value("Master").click();
            harness.run_steps(2);
            harness.get_by_label("TV").click();
            harness.run_steps(2);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetOutputSource {
                    output_uuid,
                    source: crate::renderer::context::OutputSource::BusProgram(bus),
                } if output_uuid == "out00001" && bus == "bus00001"
            )),
            "{:?}",
            actions.commands
        );
    }

    /// A screen's Levels are picked on its card — Video for video gear —
    /// keeping its precision and dither; a recording or stream has none.
    #[test]
    fn a_screens_levels_are_picked_on_its_card() {
        let mut data = UIData::test_fixture();
        data.outputs.push(sample_output(
            crate::engine::value::render::ResolvedPresentation::default(),
        ));
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 900.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            harness.get_by_value("Full (0–255)").click();
            harness.run_steps(2);
            harness.get_by_label("Video (16–235)").click();
            harness.run_steps(2);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetOutputPresentation { output_uuid, request }
                    if output_uuid == "out00001" && request.video_levels && request.dither
            )),
            "{:?}",
            actions.commands
        );
        // A Blackmagic output, say: no Levels.
        data.outputs = vec![blackmagic_output(false)];
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 900.0))
            .build_ui(|ui| render_output_section(ui, &data, &mut actions));
        assert!(harness.query_by_value("Full (0–255)").is_none());
    }

    fn ultrastudio() -> crate::decklink::DeckLinkDevice {
        let mode = |name: &str, fps| crate::decklink::DeckLinkMode {
            name: name.into(),
            width: 1920,
            height: 1080,
            fps,
            code: 0,
        };
        crate::decklink::DeckLinkDevice {
            name: "UltraStudio Monitor 3G".into(),
            arrival: 0,
            modes: vec![mode("1080i50", 25.0), mode("1080p50", 50.0), mode("1080p60", 60.0)],
        }
    }

    /// A Blackmagic device that is connected is offered as an output, in
    /// 1080p at the show's rate; with none connected, nothing is offered.
    #[test]
    fn a_connected_blackmagic_device_is_offered_as_an_output() {
        let mut data = UIData::test_fixture();
        let mut actions = UIActions::new();
        {
            let harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(640.0, 400.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            assert!(harness.query_by_label("+ UltraStudio Monitor 3G").is_none());
        }
        data.decklink_devices = vec![ultrastudio()];
        data.target_fps = 60;
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(640.0, 400.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            harness.get_by_label("+ UltraStudio Monitor 3G").click();
            harness.run();
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::CreateHeadlessOutput {
                    target: OutputTarget::DeckLink { device, mode, .. },
                } if device == "UltraStudio Monitor 3G" && mode == "1080p60"
            )),
            "{:?}",
            actions.commands
        );
    }

    fn blackmagic_output(active: bool) -> crate::usecases::ui::OutputUI {
        let mut output =
            sample_output(crate::engine::value::render::ResolvedPresentation::default());
        output.is_windowed = false;
        output.is_active = active;
        output.target = OutputTarget::DeckLink {
            device: "UltraStudio Monitor 3G".into(),
            mode: "1080p60".into(),
            full_range: false,
        };
        output.source = Some(crate::renderer::context::OutputSource::Master);
        output
    }

    /// A stopped Blackmagic output's format is picked from its device's.
    #[test]
    fn a_blackmagic_outputs_format_is_picked_while_it_is_stopped() {
        let mut data = UIData::test_fixture();
        data.decklink_devices = vec![ultrastudio()];
        data.outputs = vec![blackmagic_output(false)];
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 900.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            harness.get_by_value("1080p60").click();
            harness.run_steps(2);
            harness.get_by_label("1080p50").click();
            harness.run_steps(2);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetOutputTarget {
                    output_uuid,
                    target: OutputTarget::DeckLink { device, mode, .. },
                } if output_uuid == "out00001" && device == "UltraStudio Monitor 3G" && mode == "1080p50"
            )),
            "{:?}",
            actions.commands
        );
    }

    /// A Blackmagic output's Levels are picked on its card, running or not —
    /// Video by default, Full for a display set to take it; its test card
    /// put on the device from there too.
    #[test]
    fn a_blackmagic_outputs_levels_and_test_card_are_picked_on_its_card() {
        let mut data = UIData::test_fixture();
        data.decklink_devices = vec![ultrastudio()];
        data.outputs = vec![blackmagic_output(true)];
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 900.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            harness.get_by_value("Video (16–235)").click();
            harness.run_steps(2);
            harness.get_by_label("Full (0–255)").click();
            harness.run_steps(2);
            harness.get_by_label("Test card").click();
            harness.run_steps(2);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetOutputTarget {
                    output_uuid,
                    target: OutputTarget::DeckLink { mode, full_range: true, .. },
                } if output_uuid == "out00001" && mode == "1080p60"
            )),
            "{:?}",
            actions.commands
        );
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetCalibrationMode { output_uuid, mode: CalibrationMode::Projector }
                    if output_uuid == "out00001"
            )),
            "{:?}",
            actions.commands
        );
    }

    /// Running, its format is shown, not offered; its device unplugged, that
    /// is said.
    #[test]
    fn a_running_or_unplugged_blackmagic_output_says_so() {
        let mut data = UIData::test_fixture();
        data.decklink_devices = vec![ultrastudio()];
        data.outputs = vec![blackmagic_output(true)];
        let mut actions = UIActions::new();
        {
            let harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(420.0, 900.0))
                .build_ui(|ui| render_output_section(ui, &data, &mut actions));
            harness.get_by_label("Format: 1080p60");
            assert!(harness.query_by_value("1080p60").is_none(), "not offered while running");
        }
        data.decklink_devices.clear();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(420.0, 900.0))
            .build_ui(|ui| render_output_section(ui, &data, &mut actions));
        harness.get_by_label("⚠ UltraStudio Monitor 3G is not connected");
    }
}
