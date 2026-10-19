//! Deck detail: the bottom-bar mode shown when a deck is selected.

use super::super::{
    widgets, DeckUIInfo, DepthPreproUI, EffectDrag, EffectInfo, LibraryDrag, ParamUIInfo,
    PointCloudUI, ScreenCaptureUI, ShaderParamsUI, TapUI, UIActions, UIData,
};
use super::utils::{
    channel_color, format_time, render_collapsed_column, render_effect_drag_ghost,
    render_effect_drag_handle, render_effect_drop_zone_row,
};
use crate::channel::DeckRenderFps;
use crate::engine::{EffectTarget, EngineCommand};
use crate::modulation::DEFAULT_ASSIGNMENT_AMOUNT;
use crate::params::ParamValue;
use crate::{BlendMode, ScalingMode};

/// The `〰` dropdown for one of a video deck's playback controls.
///
/// Playback targets are keyed by reserved names on the deck's own modulation
/// prefix rather than by a `ParamUIInfo` row, so they are addressed the way a
/// channel fader is. See /spec/video-playback-modulation.md § Key naming.
fn playback_mod_menu(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    name: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    widgets::modulation_menu_for_key(
        ui,
        format!("vidmod-{deck_uuid}-{name}"),
        &format!("deck_{deck_uuid}:{name}"),
        &data.modulation_sources,
        &data.modulation_assignments,
        &mut actions.commands,
    );
}

/// The colour of the first modulator driving `name` on this deck, or `None` when
/// nothing is assigned to it.
///
/// The colour has to match the card in the modulation panel, so the index comes
/// from the unfiltered source list.
fn playback_mod_color(deck_uuid: &str, name: &str, data: &UIData) -> Option<egui::Color32> {
    let key = format!("deck_{deck_uuid}:{name}");
    let first = data.modulation_assignments.get(&key)?.first()?;
    let idx = data
        .modulation_sources
        .iter()
        .position(|s| s.uuid == first.source_id)?;
    Some(super::super::modulator_color(idx))
}

/// The track of a slider, excluding any value box drawn beside it.
///
/// egui returns one rect for the whole widget, so measuring a ghost against it
/// stretches the scale across the number as well and pushes every reading
/// right. A slider built with `show_value(false)` is already all track.
fn slider_track(ui: &egui::Ui, rect: egui::Rect, shows_value: bool) -> egui::Rect {
    if !shows_value {
        return rect;
    }
    let width = ui.spacing().slider_width.min(rect.width());
    egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()))
}

/// Draw a ghost line at `value` on a slider whose track spans `range`.
///
/// Which of the two positions the ghost carries depends on the control: where
/// the handle shows the performer's set point the ghost shows the live value,
/// and where the handle already rides the live value it shows the set point the
/// modulator is working from. Either way the pair is set point and actual.
fn draw_slider_ghost(
    ui: &egui::Ui,
    rect: egui::Rect,
    value: f32,
    range: std::ops::RangeInclusive<f32>,
    color: egui::Color32,
) {
    let span = range.end() - range.start();
    if span <= 0.0 {
        return;
    }
    let t = ((value - range.start()) / span).clamp(0.0, 1.0);
    let x = rect.left() + t * rect.width();
    ui.painter().line_segment(
        [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
        egui::Stroke::new(2.0_f32, color),
    );
}

/// Apply MIDI + keyboard learn affordances (glow + click-to-select) to a just-drawn
/// control. `path` is the parameter-router path the control binds to. The two learn
/// modes are mutually exclusive, so at most one overlay is active at a time.
fn learn_overlay(
    ui: &egui::Ui,
    rect: egui::Rect,
    path: String,
    data: &UIData,
    actions: &mut UIActions,
) {
    if data.midi_learn_active {
        if data.midi_learn_target.as_deref() == Some(path.as_str()) {
            widgets::draw_midi_learn_selected(ui, rect);
        } else {
            widgets::draw_midi_learn_glow(ui, rect);
        }
        let id = ui.id().with(("midi_learn", path.as_str()));
        if ui.interact(rect, id, egui::Sense::click()).clicked() {
            actions.session.midi_learn_select = Some(path);
        }
    } else if data.keyboard_learn_active {
        if data.keyboard_learn_target.as_deref() == Some(path.as_str()) {
            widgets::draw_keyboard_learn_selected(ui, rect);
        } else {
            widgets::draw_keyboard_learn_glow(ui, rect);
        }
        let id = ui.id().with(("kb_learn", path.as_str()));
        if ui.interact(rect, id, egui::Sense::click()).clicked() {
            actions.session.keyboard_learn_select = Some(crate::keymap::KeyTarget::ParamPath(path));
        }
    }
}

/// Render point-cloud controls for a depth-sensor deck. Values are sent
/// normalized (0.0–1.0) through the generic `deck/<uuid>/depth/<name>` param
/// path, matching the router in `src/internal/param_router.rs`. See
/// spec/depth-sensors.md.
fn render_depth_controls(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    pc: &PointCloudUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.separator();
    ui.label(egui::RichText::new("📡 Point Cloud").strong().size(12.0));

    // (label, param name, current normalized value)
    let sliders: [(&str, &str, f32); 9] = [
        ("Yaw", "orbit_yaw", pc.orbit_yaw),
        ("Pitch", "orbit_pitch", pc.orbit_pitch),
        ("Zoom", "zoom", pc.zoom),
        ("Points", "point_size", pc.point_size),
        ("Near", "depth_min", pc.depth_min),
        ("Far", "depth_max", pc.depth_max),
        ("Seed", "seed", pc.seed),
        ("Drift", "drift", pc.drift),
        ("Disruption", "disruption", pc.disruption),
    ];
    for (label, name, current) in sliders {
        let mut v = current;
        ui.horizontal(|ui| {
            ui.label(label);
            let resp = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false));
            if resp.changed() {
                actions.commands.push(EngineCommand::SetParam {
                    path: format!("deck/{}/depth/{}", deck.uuid, name),
                    value: ParamValue::Float(v),
                });
            }
            learn_overlay(
                ui,
                resp.rect,
                format!("deck/{}/depth/{}", deck.uuid, name),
                data,
                actions,
            );
        });
    }

    // Color mode: 3 buckets mapped into the normalized 0..1 range.
    ui.horizontal(|ui| {
        ui.label("Color:");
        let modes = ["RGB", "Depth", "Solid"];
        let mut idx = usize::from(pc.color_mode).min(2);
        let before = idx;
        egui::ComboBox::from_id_salt("depth_color_combo")
            .selected_text(modes[idx])
            .width(70.0)
            .show_ui(ui, |ui| {
                for (i, m) in modes.iter().enumerate() {
                    ui.selectable_value(&mut idx, i, *m);
                }
            });
        if idx != before {
            // Map bucket index to a normalized value that lands in that bucket.
            let norm = (idx as f32 + 0.5) / 3.0;
            actions.commands.push(EngineCommand::SetParam {
                path: format!("deck/{}/depth/color_mode", deck.uuid),
                value: ParamValue::Float(norm),
            });
        }
    });
}

/// Render depth-preprocessor controls for a deck whose shader declared a
/// `depth_sensor` PREPROCESSOR. Values are sent normalized (0.0–1.0) through the
/// generic `deck/<uuid>/depth_prepro/<name>` param path, matching the router in
/// `src/internal/param_router.rs`. See spec/depth-sensor-preprocessor.md.
fn render_depth_prepro_controls(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    prepro: &DepthPreproUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.separator();
    ui.label(
        egui::RichText::new(format!("📡 Depth Sensor — {}", prepro.sensor_name))
            .strong()
            .size(12.0),
    );

    // (label, param name, current normalized value)
    let sliders: [(&str, &str, f32); 6] = [
        ("Near", "near", prepro.near),
        ("Far", "far", prepro.far),
        ("Smoothing", "smoothing", prepro.smoothing),
        ("Hole Fill", "hole_fill", prepro.hole_fill),
        ("Mask Feather", "mask_feather", prepro.mask_feather),
        ("Motion Gain", "motion_gain", prepro.motion_gain),
    ];
    for (label, name, current) in sliders {
        let mut v = current;
        ui.horizontal(|ui| {
            ui.label(label);
            let resp = ui.add(egui::Slider::new(&mut v, 0.0..=1.0).show_value(false));
            if resp.changed() {
                actions.commands.push(EngineCommand::SetParam {
                    path: format!("deck/{}/depth_prepro/{}", deck.uuid, name),
                    value: ParamValue::Float(v),
                });
            }
            learn_overlay(
                ui,
                resp.rect,
                format!("deck/{}/depth_prepro/{}", deck.uuid, name),
                data,
                actions,
            );
        });
    }

    // Mirror is a fader-bucketed bool on the router; send the bucket centre.
    ui.horizontal(|ui| {
        let mut mirror = prepro.mirror;
        let resp = ui.checkbox(&mut mirror, "Mirror");
        if resp.changed() {
            actions.commands.push(EngineCommand::SetParam {
                path: format!("deck/{}/depth_prepro/mirror", deck.uuid),
                value: ParamValue::Float(f32::from(u8::from(mirror))),
            });
        }
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/depth_prepro/mirror", deck.uuid),
            data,
            actions,
        );
    });
}

/// Tap controls for the selected deck: which internal output it re-enters,
/// whether it keeps that picture's transparency, plus the two things a
/// performer has to know about feedback. See spec/program-tap.md.
fn render_tap_controls(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    tap: &TapUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.separator();
    ui.label(egui::RichText::new("🔁 Tap").strong().size(12.0));

    ui.horizontal(|ui| {
        ui.label("Source:");
        let selected = if tap.bound {
            tap.label.clone()
        } else {
            format!("{} (missing)", tap.label)
        };
        let mut chosen: Option<crate::scene::TapSourceConfig> = None;
        egui::ComboBox::from_id_salt("sel_deck_tap_source")
            .selected_text(selected)
            .width(160.0)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_label(tap.kind == "master_program", "Master Program")
                    .clicked()
                {
                    chosen = Some(crate::scene::TapSourceConfig::MasterProgram);
                }
                for ch in &data.channels {
                    let is_current = tap.channel_uuid.as_deref() == Some(ch.uuid.as_str());
                    if ui
                        .selectable_label(is_current, format!("{} ({})", ch.name, ch.uuid))
                        .clicked()
                    {
                        chosen = Some(crate::scene::TapSourceConfig::Channel {
                            uuid: ch.uuid.clone(),
                        });
                    }
                }
            });
        if let Some(source) = chosen {
            actions.commands.push(EngineCommand::SetTapSource {
                deck_uuid: deck.uuid.clone(),
                source,
            });
        }
    });

    let mut transparent = deck.transparent;
    let resp = ui.checkbox(&mut transparent, "Transparent").on_hover_text(
        "On: what is under this layer shows wherever its input is clear. Off: the input \
         flattened over black — black where it is clear.",
    );
    if resp.changed() {
        actions.commands.push(EngineCommand::SetDeckTransparent {
            deck_uuid: deck.uuid.clone(),
            transparent,
        });
    }
    learn_overlay(
        ui,
        resp.rect,
        format!("deck/{}/transparent", deck.uuid),
        data,
        actions,
    );

    if !tap.bound {
        ui.colored_label(
            egui::Color32::from_rgb(220, 160, 60),
            "Tapped channel no longer exists — showing black.",
        );
    }
    ui.label(
        egui::RichText::new(
            "Shows the previous frame. Feedback above 1.0 opacity on an additive \
             blend grows without limit — the tonemap rolls it off, it is not clamped.",
        )
        .small()
        .weak(),
    );
}

/// Screen-capture controls for the selected deck: rate, crop, cursor, and
/// (for display targets) Varda-window exclusion. See spec/screen-capture.md.
fn render_capture_controls(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    capture: &ScreenCaptureUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    let send = |actions: &mut UIActions, name: &str, value: f32| {
        actions.commands.push(EngineCommand::SetParam {
            path: format!("deck/{}/capture/{}", deck.uuid, name),
            value: ParamValue::Float(value),
        });
    };

    ui.separator();
    ui.label(
        egui::RichText::new(format!("🖥 Screen Capture — {}", capture.target_label))
            .strong()
            .size(12.0),
    );
    if !capture.bound {
        ui.colored_label(
            egui::Color32::from_rgb(220, 160, 60),
            "Target not found — showing black. Reopen the target and rescan.",
        );
    } else if !capture.connected {
        ui.colored_label(egui::Color32::GRAY, "Waiting for frames…");
    }

    ui.horizontal(|ui| {
        ui.label("Rate:");
        let mut rate = capture.rate;
        let resp = ui.add(egui::Slider::new(&mut rate, 0.0..=1.0).show_value(false));
        if resp.changed() {
            send(actions, "rate", rate);
        }
        ui.label(format!("{:.0} fps", capture.rate_fps));
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/capture/rate", deck.uuid),
            data,
            actions,
        );
    });

    // Crop is a sub-section of its own: the params column is only 200–280px, too
    // narrow to hold four sliders side by side, so they get a row each. Labels stay
    // single-character so every slider starts at the same x.
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Crop").strong());
        if ui.small_button("Reset").clicked() {
            send(actions, "crop_x", 0.0);
            send(actions, "crop_y", 0.0);
            send(actions, "crop_w", 1.0);
            send(actions, "crop_h", 1.0);
        }
    });

    let crop_sliders: [(&str, &str, f32); 4] = [
        ("X", "crop_x", capture.crop[0]),
        ("Y", "crop_y", capture.crop[1]),
        ("W", "crop_w", capture.crop[2]),
        ("H", "crop_h", capture.crop[3]),
    ];
    for (label, name, current) in crop_sliders {
        let mut v = current;
        ui.horizontal(|ui| {
            ui.label(label);
            let resp = ui.add(
                egui::Slider::new(&mut v, 0.0..=1.0)
                    .show_value(false)
                    .fixed_decimals(2),
            );
            if resp.changed() {
                send(actions, name, v);
            }
            learn_overlay(
                ui,
                resp.rect,
                format!("deck/{}/capture/{}", deck.uuid, name),
                data,
                actions,
            );
        });
    }

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let mut cursor = capture.show_cursor;
        let resp = ui.checkbox(&mut cursor, "Cursor");
        if resp.changed() {
            send(actions, "cursor", f32::from(u8::from(cursor)));
        }
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/capture/cursor", deck.uuid),
            data,
            actions,
        );

        // Only displays can contain Varda's own windows; for a window target
        // the toggle would be a no-op, so it is not offered.
        if capture.is_display {
            let mut exclude = capture.exclude_varda;
            let resp = ui
                .checkbox(&mut exclude, "Exclude Nida")
                .on_hover_text("Omit Nida's own windows from this display capture");
            if resp.changed() {
                send(actions, "exclude_varda", f32::from(u8::from(exclude)));
            }
            learn_overlay(
                ui,
                resp.rect,
                format!("deck/{}/capture/exclude_varda", deck.uuid),
                data,
                actions,
            );
        }
    });
}

/// What to do when a camera's picture differs from its own output.
const CAMERA_COLOUR_HINT: &str = "If blacks look crushed and highlights clipped next to the \
     camera's own output, choose Full; if washed out, Limited.";

/// A camera layer's colour range and matrix: as the camera says, or set by
/// hand for one that says one thing and sends another. Every layer on the
/// camera takes it (see `SetCameraColour`).
fn render_camera_colour(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    colour: crate::engine::value::camera::CameraColour,
    actions: &mut UIActions,
) {
    use crate::engine::value::camera::{CameraMatrix, CameraRange};
    ui.separator();
    ui.label(egui::RichText::new("📹 Camera picture").strong().size(12.0));
    let mut chosen = colour;
    ui.horizontal(|ui| {
        let label = ui.label("Colour range:");
        egui::ComboBox::from_id_salt(("camera_range", &deck.uuid))
            .selected_text(colour.range.label())
            .show_ui(ui, |ui| {
                for range in CameraRange::ALL {
                    ui.selectable_value(&mut chosen.range, range, range.label());
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(CAMERA_COLOUR_HINT);
    });
    ui.horizontal(|ui| {
        let label = ui.label("Colour matrix:");
        egui::ComboBox::from_id_salt(("camera_matrix", &deck.uuid))
            .selected_text(colour.matrix.label())
            .show_ui(ui, |ui| {
                for matrix in CameraMatrix::ALL {
                    ui.selectable_value(&mut chosen.matrix, matrix, matrix.label());
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text("The camera's own tag unless set; HD is usually BT.709.");
    });
    if chosen != colour {
        actions.commands.push(EngineCommand::SetCameraColour {
            deck_uuid: deck.uuid.clone(),
            colour: chosen,
        });
    }
}

/// What the lowest latency costs, for a camera layer's switch.
const LOWEST_LATENCY_HINT: &str = "Shows the camera's newest picture every frame: about a frame \
     sooner, but motion judders whenever a picture comes late — about twice a second with a \
     camera over USB. Off: every picture in turn, motion even.";

/// A camera layer's latency: its newest picture every frame, or each in turn
/// at the delay that keeps motion even. Every layer on the camera takes it
/// (see `SetCameraLowestLatency`).
fn render_camera_latency(ui: &mut egui::Ui, deck: &DeckUIInfo, on: bool, actions: &mut UIActions) {
    let mut chosen = on;
    ui.checkbox(&mut chosen, "Lowest latency")
        .on_hover_text(LOWEST_LATENCY_HINT);
    if chosen != on {
        actions.commands.push(EngineCommand::SetCameraLowestLatency {
            deck_uuid: deck.uuid.clone(),
            on: chosen,
        });
    }
}

/// What a camera's picture is coded as, to be shown as Rec.709 video: shown
/// as it comes, a log or HLG profile, or a LUT file — one in the show's LUT
/// folder, or one chosen from anywhere. Every layer on the camera takes it
/// (see `SetCameraProfile`).
fn render_camera_profile(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    profile: &crate::engine::value::camera::CameraProfile,
    data: &UIData,
    actions: &mut UIActions,
) {
    use crate::engine::value::camera::CameraProfile;
    let mut chosen = profile.clone();
    ui.horizontal(|ui| {
        let label = ui.label("Picture profile:");
        egui::ComboBox::from_id_salt(("camera_profile", &deck.uuid))
            .selected_text(profile.label())
            .show_ui(ui, |ui| {
                for built_in in CameraProfile::BUILT_IN {
                    let text = built_in.label();
                    ui.selectable_value(&mut chosen, built_in, text);
                }
                let mut luts: Vec<CameraProfile> = data
                    .available_luts
                    .iter()
                    .map(|file| CameraProfile::Lut { file: file.clone() })
                    .collect();
                // One chosen from elsewhere stays offered.
                if matches!(profile, CameraProfile::Lut { .. }) && !luts.contains(profile) {
                    luts.push(profile.clone());
                }
                if !luts.is_empty() {
                    ui.separator();
                }
                for lut in luts {
                    let text = lut.label();
                    ui.selectable_value(&mut chosen, lut, text);
                }
            })
            .response
            .labelled_by(label.id)
            .on_hover_text(CAMERA_PROFILE_HINT);
        if ui
            .button("Load LUT…")
            .on_hover_text("A .cube or .3dl file made for this camera's picture")
            .clicked()
        {
            choose_camera_lut(ui.ctx(), &deck.uuid);
        }
    });
    if let Some(file) = chosen_camera_lut(ui.ctx(), &deck.uuid) {
        chosen = CameraProfile::Lut { file };
    }
    if chosen != *profile {
        actions.commands.push(EngineCommand::SetCameraProfile {
            deck_uuid: deck.uuid.clone(),
            profile: chosen,
        });
    }
}

/// What a camera's picture profile is for.
const CAMERA_PROFILE_HINT: &str = "Set as the camera is: a log or HLG picture is turned into \
     Rec.709 video for the show. Blackmagic Film Gen 5 is Film on current URSA Broadcast G2, \
     Pocket and Cinema cameras; for the first URSA Broadcast, Gen 4 software, or a \
     manufacturer's own look, load its LUT file.";

/// A LUT file being chosen for a camera layer: the dialog runs on a thread,
/// and its answer is taken as the UI renders.
type ChosenLut = std::sync::Arc<std::sync::Mutex<Option<std::sync::mpsc::Receiver<std::path::PathBuf>>>>;

fn camera_lut_id(deck_uuid: &str) -> egui::Id {
    egui::Id::new(("camera_lut_chosen", deck_uuid))
}

/// Ask for a LUT file for camera layer `deck_uuid`.
fn choose_camera_lut(ctx: &egui::Context, deck_uuid: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("camera-lut".into())
        .spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("A LUT for the camera's picture")
                .add_filter("LUT", &["cube", "3dl"])
                .pick_file();
            if let Some(path) = picked {
                let _ = tx.send(path);
            }
        });
    if spawned.is_ok() {
        let chosen: ChosenLut = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
        ctx.data_mut(|d| d.insert_temp(camera_lut_id(deck_uuid), chosen));
    }
}

/// The LUT file the dialog came back with for `deck_uuid`, once.
fn chosen_camera_lut(ctx: &egui::Context, deck_uuid: &str) -> Option<String> {
    let chosen: ChosenLut = ctx.data(|d| d.get_temp(camera_lut_id(deck_uuid)))?;
    let mut waiting = chosen.lock().ok()?;
    let path = waiting.as_ref()?.try_recv().ok()?;
    *waiting = None;
    Some(path.to_string_lossy().into_owned())
}

/// Render the selected deck's full details (params, effects, blend, scaling) in the bottom bar
pub(super) fn render_selected_deck_detail(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.heading("Selected Deck");

    let Some((ch_idx, deck_idx)) = data.selected_deck else {
        ui.label(
            egui::RichText::new("Click a deck thumbnail to see its controls here")
                .weak()
                .small(),
        );
        return;
    };

    // Find the deck data
    let Some(ch) = data.channels.get(ch_idx) else {
        ui.label(egui::RichText::new("Channel not found").weak());
        return;
    };
    let Some(deck) = ch.decks.iter().find(|d| d.deck_idx == deck_idx) else {
        ui.label(egui::RichText::new("Deck not found").weak());
        return;
    };

    let accent = channel_color(ch_idx);
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(format!(
                "{} / Deck {} — {}",
                ch.name,
                deck_idx + 1,
                deck.name
            ))
            .strong()
            .color(accent),
        );

        if ui
            .small_button("Adjust…")
            .on_hover_text("Open this deck's picture, framing and effects in a window (or double-click its card)")
            .clicked()
        {
            super::deck_adjust::open(ui.ctx(), &deck.uuid);
        }

        // Save as preset — inline name prompt
        let prompt_id = egui::Id::new("deck_preset_name_prompt");
        let name_id = egui::Id::new("deck_preset_name_input");
        let is_prompting: bool = ui.data(|d| d.get_temp(prompt_id)).unwrap_or(false);

        if is_prompting {
            let cleared_id = egui::Id::new("deck_preset_name_cleared");
            let was_cleared: bool = ui.data(|d| d.get_temp(cleared_id)).unwrap_or(false);
            let mut name: String = ui
                .data(|d| d.get_temp(name_id))
                .unwrap_or_else(|| deck.name.clone());
            let response = ui.text_edit_singleline(&mut name);
            if response.gained_focus() && !was_cleared {
                name.clear();
                ui.data_mut(|d| d.insert_temp(cleared_id, true));
            }
            if ui.small_button("✔ Save").clicked() && !name.is_empty() {
                actions.commands.push(EngineCommand::SaveDeckPreset {
                    deck_uuid: deck.uuid.clone(),
                    name: name.clone(),
                });
                ui.data_mut(|d| d.insert_temp(prompt_id, false));
            }
            if ui.small_button("×").clicked() {
                ui.data_mut(|d| d.insert_temp(prompt_id, false));
            }
            ui.data_mut(|d| d.insert_temp(name_id, name));
        } else if ui.small_button("💾 Save Preset").clicked() {
            ui.data_mut(|d| {
                d.insert_temp(prompt_id, true);
                d.remove_temp::<String>(name_id);
                d.insert_temp(egui::Id::new("deck_preset_name_cleared"), false);
            });
        }
    });

    // Horizontal columns: Preview | Generator | Effect 1 | Effect 2 | ... | Add Effect
    egui::ScrollArea::horizontal().id_salt("selected_deck_hscroll").show(ui, |ui| {
        ui.horizontal_top(|ui| {
            // Column 0: the channel the deck is in, every layer composited —
            // not the deck alone, so a change to any deck is judged against
            // the result. The deck alone is in its Adjust window. Scales with
            // the bottom bar's height, with the visible panel width as the
            // other bound so an ultra-wide project cannot produce a column
            // wider than the bar it sits in.
            if let Some(&tex_id) = data.channel_preview_textures.get(&ch_idx) {
                let available_height = ui.available_height() - 12.0; // margin
                let preview = super::utils::preview_size(
                    egui::vec2(
                        (ui.available_width() - columns_after_preview(ui, deck, ch_idx, deck_idx))
                            .max(160.0),
                        available_height.max(60.0),
                    ),
                    data.render_width,
                    data.render_height,
                );
                channel_result_preview(ui, tex_id, preview, &ch.name, accent);
                ui.separator();
            }
            render_take_frame(ui, &ch.uuid, data, actions);
            ui.separator();

            // Column: HTML source controls (only for HTML decks)
            if deck.is_html {
                egui::Frame::default()
                    .inner_margin(6.0)
                    .corner_radius(4.0)
                    .fill(ui.visuals().faint_bg_color)
                    .show(ui, |ui| {
                        ui.set_min_width(140.0);
                        ui.set_max_width(200.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                            ui.label(egui::RichText::new("🌐 HTML").strong());
                            let reload_resp = ui.button("⟳ Reload");
                            if reload_resp.clicked() {
                                actions.commands.push(EngineCommand::ReloadHtmlDeck {
                                    deck_uuid: deck.uuid.clone(),
                                });
                            }
                            learn_overlay(
                                ui,
                                reload_resp.rect,
                                format!("deck/{}/html/reload", deck.uuid),
                                data,
                                actions,
                            );
                            let interactive_label = if deck.is_html_interactive {
                                "🖱 Exit Interactive"
                            } else {
                                "🖱 Interactive"
                            };
                            let interactive_resp = ui.button(interactive_label);
                            if interactive_resp.clicked() {
                                let cmd = if deck.is_html_interactive {
                                    EngineCommand::CloseHtmlInteractive
                                } else {
                                    EngineCommand::OpenHtmlInteractive {
                                        deck_uuid: deck.uuid.clone(),
                                    }
                                };
                                actions.commands.push(cmd);
                            }
                            learn_overlay(
                                ui,
                                interactive_resp.rect,
                                format!("deck/{}/html/interactive", deck.uuid),
                                data,
                                actions,
                            );
                            let mut transparent = deck.transparent;
                            let transparent_resp =
                                ui.checkbox(&mut transparent, "Transparent BG");
                            if transparent_resp.changed() {
                                actions.commands.push(EngineCommand::SetDeckTransparent {
                                    deck_uuid: deck.uuid.clone(),
                                    transparent,
                                });
                            }
                            learn_overlay(
                                ui,
                                transparent_resp.rect,
                                format!("deck/{}/transparent", deck.uuid),
                                data,
                                actions,
                            );
                        });
                    });
                ui.separator();
            }

            // Column: Video playback controls (only for video decks)
            if let Some(ref vp) = deck.video_playback {
                egui::Frame::default()
                    .inner_margin(6.0)
                    .corner_radius(4.0)
                    .fill(ui.visuals().faint_bg_color)
                    .show(ui, |ui| {
                        ui.set_min_width(220.0);
                        ui.set_max_width(280.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                            render_playback_controls(ui, deck, vp, data, actions);
                        });
                    });
                ui.separator();
            }

            // Column: a playlist's transport and list
            if let Some(ref list) = deck.playlist {
                egui::Frame::default()
                    .inner_margin(6.0)
                    .corner_radius(4.0)
                    .fill(ui.visuals().faint_bg_color)
                    .show(ui, |ui| {
                        ui.set_min_width(160.0);
                        ui.set_max_width(240.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                            super::playlist::render_deck_column(ui, deck, list, actions);
                        });
                    });
                ui.separator();
            }

            // Column: a timer's reading, start and pause, window and panel
            if let Some(ref timer) = deck.timer {
                egui::Frame::default()
                    .inner_margin(6.0)
                    .corner_radius(4.0)
                    .fill(ui.visuals().faint_bg_color)
                    .show(ui, |ui| {
                        ui.set_min_width(160.0);
                        ui.set_max_width(240.0);
                        ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                            super::timer::render_deck_column(ui, data, deck, timer, actions);
                        });
                    });
                ui.separator();
            }

            // Column: Auto-Transition controls (collapsible column, default closed)
            {
                let at_open_id = egui::Id::new("at_col_open").with((ch_idx, deck_idx));
                let at_open = ui.ctx().memory(|mem| mem.data.get_temp::<bool>(at_open_id).unwrap_or(false));
                if at_open {
                    egui::Frame::default()
                        .inner_margin(6.0)
                        .corner_radius(4.0)
                        .fill(ui.visuals().faint_bg_color)
                        .show(ui, |ui| {
                            ui.set_min_width(200.0);
                            ui.set_max_width(260.0);
                            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                // Clickable full-width header to collapse
                                let header_rect = ui.available_rect_before_wrap();
                                let header_rect = egui::Rect::from_min_size(header_rect.min, egui::vec2(ui.available_width(), 20.0));
                                let header_resp = ui.allocate_rect(header_rect, egui::Sense::click());
                                ui.painter().text(header_rect.left_center(), egui::Align2::LEFT_CENTER, "Auto Transition", egui::FontId::proportional(13.0), ui.visuals().strong_text_color());
                                if header_resp.clicked() {
                                    ui.ctx().memory_mut(|mem| mem.data.insert_temp(at_open_id, false));
                                }
                                if header_resp.hovered() {
                                    ui.painter().rect_filled(header_rect, 2.0, ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.3));
                                }
                                ui.separator();
                                // Enable toggle
                                ui.horizontal(|ui| {
                                    let enabled = deck.auto_transition.as_ref().is_some_and(|at| at.enabled);
                                    let mut en = enabled;
                                    ui.checkbox(&mut en, "Enabled");
                                    if en != enabled {
                                        actions.commands.push(EngineCommand::SetAutoTransitionEnabled { deck_uuid: deck.uuid.clone(), enabled: en });
                                    }
                                });

                                if let Some(ref at) = deck.auto_transition {
                                    if at.enabled {
                                        ui.horizontal(|ui| {
                                            ui.label("Trigger:");
                                            let mut clip_end = at.trigger_is_clip_end;
                                            if ui.selectable_label(!clip_end, "Timer").clicked() && clip_end {
                                                clip_end = false;
                                                actions.commands.push(EngineCommand::SetAutoTransitionTrigger { deck_uuid: deck.uuid.clone(), clip_end: false });
                                            }
                                            if ui.selectable_label(clip_end, "Clip End").clicked() && !clip_end {
                                                actions.commands.push(EngineCommand::SetAutoTransitionTrigger { deck_uuid: deck.uuid.clone(), clip_end: true });
                                            }
                                        });
                                        let any_learn = data.midi_learn_active || data.keyboard_learn_active;
                                        ui.horizontal(|ui| {
                                            ui.label("Play:");
                                            let mut val = at.play_duration_value as f32;
                                            let max = if at.play_duration_is_beats { 128.0 } else { 300.0 };
                                            let play_path = format!("deck/{}/at/play_duration", deck.uuid);
                                            let slider_rect = if any_learn {
                                                let inner = ui.scope(|ui| {
                                                    ui.disable();
                                                    ui.add(egui::Slider::new(&mut val, 0.5..=max)
                                                        .logarithmic(true)
                                                        .suffix(if at.play_duration_is_beats { " beats" } else { " sec" }))
                                                });
                                                inner.inner.rect
                                            } else {
                                                let resp = ui.add(egui::Slider::new(&mut val, 0.5..=max)
                                                    .logarithmic(true)
                                                    .suffix(if at.play_duration_is_beats { " beats" } else { " sec" }));
                                                if resp.changed() {
                                                    actions.commands.push(EngineCommand::SetAutoTransitionPlayDurationValue { deck_uuid: deck.uuid.clone(), value: f64::from(val) });
                                                }
                                                resp.rect
                                            };
                                            if data.midi_learn_active {
                                                let is_target = data.midi_learn_target.as_deref() == Some(play_path.as_str());
                                                if is_target { widgets::draw_midi_learn_selected(ui, slider_rect); }
                                                else { widgets::draw_midi_learn_glow(ui, slider_rect); }
                                                let click_id = ui.id().with(("midi_learn_at_play", ch_idx, deck_idx));
                                                if ui.interact(slider_rect, click_id, egui::Sense::click()).clicked() {
                                                    actions.session.midi_learn_select = Some(play_path.clone());
                                                }
                                            }
                                            if data.keyboard_learn_active {
                                                let is_target = data.keyboard_learn_target.as_deref() == Some(play_path.as_str());
                                                if is_target { widgets::draw_keyboard_learn_selected(ui, slider_rect); }
                                                else { widgets::draw_keyboard_learn_glow(ui, slider_rect); }
                                                let click_id = ui.id().with(("kb_learn_at_play", ch_idx, deck_idx));
                                                if ui.interact(slider_rect, click_id, egui::Sense::click()).clicked() {
                                                    actions.session.keyboard_learn_select = Some(crate::keymap::KeyTarget::ParamPath(play_path));
                                                }
                                            }
                                            if !any_learn
                                                && ui.small_button(if at.play_duration_is_beats { "♩" } else { "⏱" })
                                                    .on_hover_text("Toggle beats/seconds").clicked()
                                                {
                                                    actions.commands.push(EngineCommand::ToggleAutoTransitionPlayDurationUnit { deck_uuid: deck.uuid.clone() });
                                                }
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label("Trans:");
                                            let mut val = at.transition_duration_value as f32;
                                            let max = if at.transition_duration_is_beats { 32.0 } else { 30.0 };
                                            let trans_path = format!("deck/{}/at/trans_duration", deck.uuid);
                                            let slider_rect = if any_learn {
                                                let inner = ui.scope(|ui| {
                                                    ui.disable();
                                                    ui.add(egui::Slider::new(&mut val, 0.1..=max)
                                                        .logarithmic(true)
                                                        .suffix(if at.transition_duration_is_beats { " beats" } else { " sec" }))
                                                });
                                                inner.inner.rect
                                            } else {
                                                let resp = ui.add(egui::Slider::new(&mut val, 0.1..=max)
                                                    .logarithmic(true)
                                                    .suffix(if at.transition_duration_is_beats { " beats" } else { " sec" }));
                                                if resp.changed() {
                                                    actions.commands.push(EngineCommand::SetAutoTransitionDurationValue { deck_uuid: deck.uuid.clone(), value: f64::from(val) });
                                                }
                                                resp.rect
                                            };
                                            if data.midi_learn_active {
                                                let is_target = data.midi_learn_target.as_deref() == Some(trans_path.as_str());
                                                if is_target { widgets::draw_midi_learn_selected(ui, slider_rect); }
                                                else { widgets::draw_midi_learn_glow(ui, slider_rect); }
                                                let click_id = ui.id().with(("midi_learn_at_trans", ch_idx, deck_idx));
                                                if ui.interact(slider_rect, click_id, egui::Sense::click()).clicked() {
                                                    actions.session.midi_learn_select = Some(trans_path.clone());
                                                }
                                            }
                                            if data.keyboard_learn_active {
                                                let is_target = data.keyboard_learn_target.as_deref() == Some(trans_path.as_str());
                                                if is_target { widgets::draw_keyboard_learn_selected(ui, slider_rect); }
                                                else { widgets::draw_keyboard_learn_glow(ui, slider_rect); }
                                                let click_id = ui.id().with(("kb_learn_at_trans", ch_idx, deck_idx));
                                                if ui.interact(slider_rect, click_id, egui::Sense::click()).clicked() {
                                                    actions.session.keyboard_learn_select = Some(crate::keymap::KeyTarget::ParamPath(trans_path));
                                                }
                                            }
                                            if !any_learn
                                                && ui.small_button(if at.transition_duration_is_beats { "♩" } else { "⏱" })
                                                    .on_hover_text("Toggle beats/seconds").clicked()
                                                {
                                                    actions.commands.push(EngineCommand::ToggleAutoTransitionDurationUnit { deck_uuid: deck.uuid.clone() });
                                                }
                                        });
                                        ui.horizontal(|ui| {
                                            ui.label("Shader:");
                                            let current = at.transition_shader_name.as_deref().unwrap_or("(fade)");
                                            egui::ComboBox::from_id_salt(format!("at_shader_{ch_idx}_{deck_idx}"))
                                                .selected_text(current)
                                                .width(120.0)
                                                .show_ui(ui, |ui| {
                                                    if ui.selectable_label(at.transition_shader_name.is_none(), "(fade)").clicked() {
                                                        actions.commands.push(EngineCommand::SetAutoTransitionShader { deck_uuid: deck.uuid.clone(), shader_name: None });
                                                    }
                                                    for name in &data.transition_names {
                                                        let selected = at.transition_shader_name.as_deref() == Some(name.as_str());
                                                        if ui.selectable_label(selected, name).clicked() && !selected {
                                                            actions.commands.push(EngineCommand::SetAutoTransitionShader { deck_uuid: deck.uuid.clone(), shader_name: Some(name.clone()) });
                                                        }
                                                    }
                                                });
                                        });
                                    }
                                }
                            });
                        });
                } else {
                    // Collapsed: narrow vertical strip with vertical text
                    render_collapsed_column(ui, "Auto Transition", at_open_id);
                }
                ui.separator();
            }

            // Column: Generator parameters + blend/scale (collapsible column, default open)
            {
                let params_open_id = egui::Id::new("params_col_open").with((ch_idx, deck_idx));
                let params_open = ui.ctx().memory(|mem| mem.data.get_temp::<bool>(params_open_id).unwrap_or(true));
                if params_open {
                    egui::Frame::default()
                        .inner_margin(6.0)
                        .corner_radius(4.0)
                        .fill(ui.visuals().faint_bg_color)
                        .show(ui, |ui| {
                            ui.set_min_width(200.0);
                            ui.set_max_width(280.0);
                            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                // Clickable full-width header to collapse
                                let header_rect = ui.available_rect_before_wrap();
                                let header_rect = egui::Rect::from_min_size(header_rect.min, egui::vec2(ui.available_width(), 20.0));
                                let header_resp = ui.allocate_rect(header_rect, egui::Sense::click());
                                let params_label = format!("Params: {}", deck.generator.shader_name);
                                ui.painter().text(header_rect.left_center(), egui::Align2::LEFT_CENTER, &params_label, egui::FontId::proportional(13.0), ui.visuals().strong_text_color());
                                if header_resp.clicked() {
                                    ui.ctx().memory_mut(|mem| mem.data.insert_temp(params_open_id, false));
                                }
                                if header_resp.hovered() {
                                    ui.painter().rect_filled(header_rect, 2.0, ui.visuals().widgets.hovered.bg_fill.linear_multiply(0.3));
                                }
                                ui.separator();
                            let max_h = (ui.available_height() - 8.0).max(100.0);
                            egui::ScrollArea::vertical().id_salt("deck_gen_scroll").max_height(max_h).show(ui, |ui| {
                                // Blend mode
                                let all_modes = BlendMode::all();
                                let current_blend = all_modes.iter().position(|m| *m == deck.blend_mode).unwrap_or(0);
                                let mut selected = current_blend;
                                ui.horizontal(|ui| {
                                    ui.label("Blend:");
                                    egui::ComboBox::from_id_salt("sel_deck_blend")
                                        .selected_text(all_modes[selected].short_name())
                                        .width(60.0)
                                        .show_ui(ui, |ui| {
                                            for (i, mode) in all_modes.iter().enumerate() {
                                                ui.selectable_value(&mut selected, i, mode.short_name());
                                            }
                                        });
                                });
                                if selected != current_blend {
                                    actions.commands.push(EngineCommand::SetDeckBlendMode {
                                        deck_uuid: deck.uuid.clone(),
                                        mode: all_modes[selected],
                                    });
                                }

                                render_source_settings(ui, deck, data, actions);

                                render_framing(ui, deck, data, actions);

                                render_generator_params(ui, deck, &format!("sel_{ch_idx}_{deck_idx}"), data, actions);
                            });
                            });
                        });
                } else {
                    // Collapsed: narrow vertical strip with vertical text
                    render_collapsed_column(ui, &format!("Params: {}", deck.generator.shader_name), params_open_id);
                }
            }

            ui.separator();

            // Effect chain: a list beside the selected effect's controls, so the
            // panel stays the same width however many effects a deck carries.
            {
                let chain_key = format!("deck_{}", deck.uuid);
                let selected_id = egui::Id::new(("deck_fx_selected", &deck.uuid));
                let remembered: Option<String> = ui.ctx().data(|d| d.get_temp(selected_id));
                let framing = framing_indices(deck);
                let listed = |i: usize| !framing.contains(&i);
                let selected = remembered
                    .filter(|u| {
                        deck.effects
                            .iter()
                            .enumerate()
                            .any(|(i, (e, ..))| listed(i) && e == u)
                    })
                    .or_else(|| {
                        deck.effects
                            .iter()
                            .enumerate()
                            .find(|(i, _)| listed(*i))
                            .map(|(_, (u, ..))| u.clone())
                    });
                let max_h = (ui.available_height() - 28.0).max(80.0);

                ui.vertical(|ui| {
                    ui.set_width(EFFECT_LIST_WIDTH);
                    ui.label(egui::RichText::new("Effects").strong());
                    egui::ScrollArea::vertical()
                        .id_salt(("deck_fx_list", &deck.uuid))
                        .max_height(max_h)
                        .show(ui, |ui| {
                            for (eff_idx, (eff_uuid, eff_name, eff_enabled, _)) in
                                deck.effects.iter().enumerate()
                            {
                                if !listed(eff_idx) {
                                    continue;
                                }
                                render_effect_drop_zone_row(ui, &chain_key, eff_idx);
                                ui.horizontal(|ui| {
                                    render_effect_drag_handle(
                                        ui,
                                        EffectDrag::Deck(deck.uuid.clone(), eff_idx),
                                    );
                                    let mut enabled = *eff_enabled;
                                    if ui
                                        .checkbox(&mut enabled, "")
                                        .on_hover_text("On / bypassed")
                                        .changed()
                                    {
                                        actions.commands.push(EngineCommand::ToggleEffect {
                                            effect_uuid: eff_uuid.clone(),
                                        });
                                    }
                                    let is_selected = selected.as_deref() == Some(eff_uuid.as_str());
                                    let name = ui.selectable_label(is_selected, eff_name);
                                    if name.clicked() {
                                        ui.ctx()
                                            .data_mut(|d| d.insert_temp(selected_id, eff_uuid.clone()));
                                    }
                                    super::effects::effect_context_menu(
                                        &name, data, actions, eff_uuid, eff_name,
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if ui
                                                .small_button("x")
                                                .on_hover_text("Remove effect")
                                                .clicked()
                                            {
                                                actions.commands.push(EngineCommand::RemoveEffect {
                                                    effect_uuid: eff_uuid.clone(),
                                                });
                                            }
                                        },
                                    );
                                });
                                render_effect_drag_ghost(
                                    ui,
                                    egui::Id::new(("eff_ghost", &deck.uuid, eff_uuid)),
                                    EffectDrag::Deck(deck.uuid.clone(), eff_idx),
                                    eff_name,
                                );
                            }
                            render_effect_drop_zone_row(ui, &chain_key, deck.effects.len());

                            let has_fx_drag = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx())
                                .is_some_and(|p| matches!(&*p, LibraryDrag::Effect(_)));
                            let hint = egui::RichText::new("🔮 Drag effects here").small();
                            ui.label(if has_fx_drag {
                                hint.color(egui::Color32::from_rgb(100, 200, 255))
                            } else {
                                hint.weak()
                            });
                        });
                });

                ui.separator();

                // The selected effect's controls, one effect at a time.
                if let Some(effect) = selected
                    .as_ref()
                    .and_then(|u| deck.effects.iter().find(|(e, ..)| e == u))
                {
                    ui.vertical(|ui| {
                        ui.set_width(EFFECT_CONTROLS_WIDTH);
                        ui.label(egui::RichText::new(&effect.1).strong());
                        egui::ScrollArea::vertical()
                            .id_salt(("deck_fx_detail", &deck.uuid, &effect.0))
                            .max_height(max_h)
                            .scroll_source(egui::scroll_area::ScrollSource {
                                drag: egui::scroll_area::DragScroll::Never,
                                scroll_bar: true,
                                mouse_wheel: true,
                            })
                            .show(ui, |ui| {
                                render_deck_effect_controls(ui, &deck.uuid, effect, data, actions);
                            });
                    });
                }
            }

            // The entire horizontal_top area takes deferred library effect drops
            let chain_rect = ui.min_rect();
            super::dnd::publish_deck_surface_fx(ui.ctx(), &deck.uuid, ch_idx, deck_idx, chain_rect);
            let deck_chain_key = format!("deck_{}", deck.uuid);
            ui.ctx().memory_mut(|mem| {
                mem.data.insert_temp(egui::Id::new("eff_dz_count").with(deck_chain_key), deck.effects.len() + 1);
            });
        });
    });
}

/// Effects shown in the Params column instead of the effect list, in order:
/// framing every input needs, so it sits beside blend and scale.
const FRAMING_EFFECTS: [(&str, &str); 2] = [("crop", "Crop"), ("transform", "Transform")];

const EFFECT_LIST_WIDTH: f32 = 180.0;
const EFFECT_CONTROLS_WIDTH: f32 = 260.0;

/// A deck's source settings: how its picture is scaled into the frame, the
/// controls a depth sensor, capture or tap has, how often and how large it
/// is drawn. In the Properties panel's Params column and the input settings'
/// Source tab.
pub(super) fn render_source_settings(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    // A colour layer's colour.
    if let Some(mut colour) = deck.solid_color {
        ui.horizontal(|ui| {
            ui.label("Colour:");
            if crate::usecases::ui::widgets::colour_edit(ui, &deck.uuid, &mut colour) {
                actions.commands.push(EngineCommand::SetDeckColor {
                    deck_uuid: deck.uuid.clone(),
                    color: colour,
                });
            }
        });
    }
    // Scaling mode
    if let Some(current_scaling) = deck.scaling_mode {
        let scaling_modes = ["Fill", "Fit", "Stretch", "Center"];
        let current_idx = match current_scaling {
            ScalingMode::Fill => 0,
            ScalingMode::Fit => 1,
            ScalingMode::Stretch => 2,
            ScalingMode::Center => 3,
        };
        let mut selected_scaling = current_idx;
        ui.horizontal(|ui| {
            ui.label("Scale:");
            let combo = egui::ComboBox::from_id_salt("sel_deck_scale")
                .selected_text(scaling_modes[selected_scaling])
                .width(60.0)
                .show_ui(ui, |ui| {
                    for (i, mode_name) in scaling_modes.iter().enumerate() {
                        ui.selectable_value(&mut selected_scaling, i, *mode_name);
                    }
                });
            learn_overlay(
                ui,
                combo.response.rect,
                format!("deck/{}/scaling_mode", deck.uuid),
                data,
                actions,
            );
            playback_mod_menu(
                ui,
                &deck.uuid,
                crate::video::modulation::SCALING_MODE,
                data,
                actions,
            );
        });
        if selected_scaling != current_idx {
            let new_scaling = match selected_scaling {
                1 => ScalingMode::Fit,
                2 => ScalingMode::Stretch,
                3 => ScalingMode::Center,
                _ => ScalingMode::Fill,
            };
            actions.commands.push(EngineCommand::SetDeckScalingMode {
                deck_uuid: deck.uuid.clone(),
                mode: new_scaling,
            });
        }
    }

    // Depth-sensor point-cloud controls
    if let Some(pc) = &deck.point_cloud {
        render_depth_controls(ui, deck, pc, data, actions);
    }

    // Depth-sensor shader preprocessor controls
    if let Some(prepro) = &deck.depth_prepro {
        render_depth_prepro_controls(ui, deck, prepro, data, actions);
    }

    // Screen-capture controls
    if let Some(capture) = &deck.screen_capture {
        render_capture_controls(ui, deck, capture, data, actions);
    }

    // A camera's colour range and matrix, and what its picture is coded as
    if let Some(colour) = deck.camera_colour {
        render_camera_colour(ui, deck, colour, actions);
    }
    if let Some(on) = deck.camera_lowest_latency {
        render_camera_latency(ui, deck, on, actions);
    }
    if let Some(profile) = &deck.camera_profile {
        render_camera_profile(ui, deck, profile, data, actions);
    }

    // Tap controls
    if let Some(tap) = &deck.tap {
        render_tap_controls(ui, deck, tap, data, actions);
    }

    // Render FPS
    ui.horizontal(|ui| {
        ui.label("Render:");
        let options = ["Auto", "60", "30", "15"];
        let current_idx = match deck.render_fps {
            DeckRenderFps::Fixed(60) => 1,
            DeckRenderFps::Fixed(30) => 2,
            DeckRenderFps::Fixed(15) => 3,
            // Auto and any other fixed rate fall back to "Auto"
            DeckRenderFps::Auto | DeckRenderFps::Fixed(_) => 0,
        };
        let mut selected = current_idx;
        egui::ComboBox::from_id_salt("sel_deck_render_fps")
            .selected_text(options[selected])
            .width(50.0)
            .show_ui(ui, |ui| {
                for (i, opt) in options.iter().enumerate() {
                    ui.selectable_value(&mut selected, i, *opt);
                }
            });
        if selected != current_idx {
            let new_fps = match selected {
                1 => DeckRenderFps::Fixed(60),
                2 => DeckRenderFps::Fixed(30),
                3 => DeckRenderFps::Fixed(15),
                _ => DeckRenderFps::Auto,
            };
            actions.commands.push(EngineCommand::SetDeckRenderFps {
                deck_uuid: deck.uuid.clone(),
                render_fps: new_fps,
            });
        }
        // Show render cost
        if deck.gpu_render_cost_us > 0.0 {
            let ms = deck.gpu_render_cost_us / 1000.0;
            ui.label(
                egui::RichText::new(format!("⚡{ms:.1}ms GPU"))
                    .small()
                    .weak(),
            );
        } else if deck.render_cost_us > 0.0 {
            let ms = deck.render_cost_us / 1000.0;
            ui.label(egui::RichText::new(format!("⚡{ms:.1}ms")).small().weak());
        }
    });

    // Render resolution: a fraction of the master, the way
    // to afford a heavy generator at 4K.
    ui.horizontal(|ui| {
        ui.label("Resolution:");
        let current = deck.render_scale;
        egui::ComboBox::from_id_salt("sel_deck_render_scale")
            .selected_text(current.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for scale in crate::deck::DeckRenderScale::ALL {
                    if ui
                        .selectable_label(scale == current, scale.label())
                        .clicked()
                        && scale != current
                    {
                        actions.commands.push(EngineCommand::SetDeckRenderScale {
                            deck_uuid: deck.uuid.clone(),
                            scale,
                        });
                    }
                }
            })
            .response
            .on_hover_text(
                "Resolution this deck is drawn at. Half draws a \
                 quarter of the pixels and is scaled up in the mix — \
                 generators look almost the same for a quarter of \
                 the work.",
            );
    });
}

/// A generator deck's own parameters, and a Reset. `salt` keeps the widgets'
/// ids apart where the same deck is drawn twice.
pub(super) fn render_generator_params(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    salt: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    // Generator parameters
    let gen_params = &deck.generator;
    if !gen_params.params.is_empty() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(&gen_params.shader_name).strong());
        let deck_uuid = deck.uuid.clone();
        let midi_path_prefix = format!("deck/{deck_uuid}");
        let deck_uuid_assign = deck_uuid.clone();
        let deck_uuid_unassign = deck_uuid.clone();
        let deck_uuid_remove = deck_uuid.clone();
        let deck_uuid_automate = deck_uuid.clone();
        widgets::render_params(
            ui,
            &gen_params.params,
            &data.modulation_sources,
            &|name: &str, val: ParamValue| EngineCommand::SetGeneratorParam {
                deck_uuid: deck.uuid.clone(),
                name: name.to_string(),
                value: val,
            },
            Some(
                &|name: &str, source_uuid: &str| EngineCommand::AssignModulation {
                    target: format!("deck_{deck_uuid_assign}:{name}"),
                    source_id: source_uuid.to_string(),
                    amount: DEFAULT_ASSIGNMENT_AMOUNT,
                },
            ),
            Some(
                &|name: &str, source_uuid: &str| EngineCommand::ClearModulationSource {
                    target: format!("deck_{deck_uuid_unassign}:{name}"),
                    source_id: source_uuid.to_string(),
                },
            ),
            Some(&|name: &str| EngineCommand::ClearModulation {
                target: format!("deck_{deck_uuid_remove}:{name}"),
            }),
            Some(&|name: &str| EngineCommand::AddAutomationLane {
                target: format!("deck_{deck_uuid_automate}:{name}"),
                timebase: crate::timebase::Timebase::Transport,
            }),
            &mut actions.commands,
            &mut actions.session.gesture_active,
            salt,
            Some(&midi_path_prefix),
            data.midi_learn_active,
            &mut actions.session.midi_learn_select,
            data.midi_learn_target.as_deref(),
            &data.modulation_assignments,
            &data.modulation_current_values,
            &format!("deck_{deck_uuid}"),
            data.keyboard_learn_active,
            &mut actions.session.keyboard_learn_select,
            data.keyboard_learn_target.as_deref(),
        );
        ui.add_space(4.0);
        if ui.button("Reset").clicked() {
            actions
                .commands
                .push(EngineCommand::ResetGeneratorParamsToDefaults {
                    deck_uuid: deck.uuid.clone(),
                });
        }
    }
}

/// A video deck's playback: play/pause, the scrub bar, speed, loop, in and
/// out points, and what decodes it — the Properties panel's Playback column
/// and the input settings window's.
pub(super) fn render_playback_controls(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    vp: &super::super::VideoPlaybackUI,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("▶ Playback").strong());

    let chasing_now = vp.transport_sync.mode.is_chasing(data.transport.running);

    // Play/Pause button
    ui.horizontal(|ui| {
        let play_label = if vp.playing { "⏸ Pause" } else { "▶ Play" };
        let play_resp = ui.button(play_label);
        if play_resp.clicked() {
            actions.commands.push(EngineCommand::VideoTogglePlay {
                deck_uuid: deck.uuid.clone(),
            });
        }
        learn_overlay(
            ui,
            play_resp.rect,
            format!("deck/{}/video/play", deck.uuid),
            data,
            actions,
        );
        playback_mod_menu(
            ui,
            &deck.uuid,
            crate::video::modulation::PLAY,
            data,
            actions,
        );
    });

    // Position scrub bar. The handle rides the live
    // playhead, so the ghost marks the opposite thing:
    // the point the modulator is swinging around.
    let duration = vp.duration.max(0.001);
    let mut pos = vp.position as f32;
    ui.horizontal(|ui| {
        ui.label(format_time(vp.position));
        let slider = egui::Slider::new(&mut pos, 0.0..=duration as f32)
            .show_value(false)
            .trailing_fill(true);
        let resp = ui.add(slider);
        if resp.changed() {
            actions.commands.push(EngineCommand::VideoSeek {
                deck_uuid: deck.uuid.clone(),
                position_secs: f64::from(pos),
            });
        }
        if let Some(color) =
            playback_mod_color(&deck.uuid, crate::video::modulation::POSITION, data)
        {
            let anchor = vp.position - vp.position_offset;
            let track = slider_track(ui, resp.rect, false);
            draw_slider_ghost(ui, track, anchor as f32, 0.0..=duration as f32, color);
        }
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/video/seek", deck.uuid),
            data,
            actions,
        );
        ui.label(format_time(duration));
        playback_mod_menu(
            ui,
            &deck.uuid,
            crate::video::modulation::POSITION,
            data,
            actions,
        );
    });

    // Speed control
    let mut speed = vp.speed as f32;
    ui.horizontal(|ui| {
        ui.label("Speed:");
        let resp = ui.add(
            egui::Slider::new(&mut speed, 0.1..=4.0)
                .step_by(0.05)
                .suffix("x"),
        );
        if resp.changed() {
            actions.commands.push(EngineCommand::VideoSetSpeed {
                deck_uuid: deck.uuid.clone(),
                speed: f64::from(speed),
            });
        }
        // The slider stays on the set point, so the
        // ghost is the only thing showing the live rate.
        if let Some(color) = playback_mod_color(&deck.uuid, crate::video::modulation::SPEED, data) {
            let track = slider_track(ui, resp.rect, true);
            draw_slider_ghost(ui, track, vp.effective_speed as f32, 0.1..=4.0, color);
        }
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/video/speed", deck.uuid),
            data,
            actions,
        );
        playback_mod_menu(
            ui,
            &deck.uuid,
            crate::video::modulation::SPEED,
            data,
            actions,
        );
    });

    // Loop mode
    let loop_resp = ui.horizontal(|ui| {
        ui.label("Loop:");
        let modes = [
            ("🔁", crate::video::LoopMode::Loop, "Loop"),
            ("🔄", crate::video::LoopMode::PingPong, "Ping-Pong"),
            ("1️⃣", crate::video::LoopMode::OneShot, "One Shot"),
            ("⏹", crate::video::LoopMode::HoldLast, "Hold Last"),
        ];
        for (icon, mode, tooltip) in &modes {
            let selected = vp.loop_mode == *mode;
            let btn = egui::Button::new(*icon).selected(selected);
            if ui.add(btn).on_hover_text(*tooltip).clicked() && !selected {
                actions.commands.push(EngineCommand::VideoSetLoopMode {
                    deck_uuid: deck.uuid.clone(),
                    mode: *mode,
                });
            }
        }
        playback_mod_menu(
            ui,
            &deck.uuid,
            crate::video::modulation::LOOP_MODE,
            data,
            actions,
        );
    });
    learn_overlay(
        ui,
        loop_resp.response.rect,
        format!("deck/{}/video/loop_mode", deck.uuid),
        data,
        actions,
    );

    if chasing_now {
        ui.label(
            egui::RichText::new("Loop is ignored while chasing the transport")
                .small()
                .weak(),
        );
        // Two authorities on one value is the seek storm
        // this design avoids, so say which one wins
        // rather than letting it look broken.
        let held = [
            ("Playhead", crate::video::modulation::POSITION),
            ("Speed", crate::video::modulation::SPEED),
        ]
        .into_iter()
        .filter(|(_, name)| playback_mod_color(&deck.uuid, name, data).is_some())
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
        if !held.is_empty() {
            ui.label(
                egui::RichText::new(format!(
                    "{} modulation is ignored while chasing — \
                             set Chase to Never to use it",
                    held.join(" and ")
                ))
                .small()
                .weak(),
            );
        }
    }

    ui.add_space(4.0);
    ui.label(egui::RichText::new("⏱ Transport").strong());
    let mut sync = vp.transport_sync;
    ui.horizontal(|ui| {
        ui.label("Chase:");
        egui::ComboBox::from_id_salt(format!("deck-chase-{}", deck.uuid))
            .selected_text(sync.mode.label())
            .show_ui(ui, |ui| {
                for mode in [
                    crate::video::TransportSyncMode::Auto,
                    crate::video::TransportSyncMode::Always,
                    crate::video::TransportSyncMode::Never,
                ] {
                    if ui
                        .selectable_label(sync.mode == mode, mode.label())
                        .clicked()
                    {
                        sync.mode = mode;
                        actions.commands.push(EngineCommand::VideoSetTransportSync {
                            deck_uuid: deck.uuid.clone(),
                            sync,
                        });
                    }
                }
            });
    });
    ui.horizontal(|ui| {
        ui.label("Offset:");
        let resp = ui.add(
            egui::DragValue::new(&mut sync.offset)
                .speed(0.01)
                .suffix(" s"),
        );
        if resp.changed() {
            actions.commands.push(EngineCommand::VideoSetTransportSync {
                deck_uuid: deck.uuid.clone(),
                sync,
            });
        }
    });
    ui.horizontal(|ui| {
        ui.label("Delay:");
        let resp = ui.add(
            egui::DragValue::new(&mut sync.delay_frames)
                .speed(1.0)
                .suffix(" f"),
        );
        if resp.changed() {
            actions.commands.push(EngineCommand::VideoSetTransportSync {
                deck_uuid: deck.uuid.clone(),
                sync,
            });
        }
    });

    // In/Out points (bookshelf)
    ui.add_space(4.0);
    ui.label(egui::RichText::new("📐 In/Out Points").strong());
    let effective_out = if vp.out_point > 0.0 {
        vp.out_point
    } else {
        duration
    };
    let has_range = vp.in_point > 0.0 || vp.out_point > 0.0;

    // In-point
    let mut in_pt = vp.in_point as f32;
    ui.horizontal(|ui| {
        ui.label("In:");
        let resp = ui.add(
            egui::Slider::new(&mut in_pt, 0.0..=duration as f32)
                .show_value(false)
                .trailing_fill(true),
        );
        if resp.changed() {
            actions.commands.push(EngineCommand::VideoSetInPoint {
                deck_uuid: deck.uuid.clone(),
                secs: f64::from(in_pt),
            });
        }
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/video/in_point", deck.uuid),
            data,
            actions,
        );
        ui.label(format_time(f64::from(in_pt)));
    });

    // Out-point
    let mut out_pt = effective_out as f32;
    ui.horizontal(|ui| {
        ui.label("Out:");
        let resp = ui.add(
            egui::Slider::new(&mut out_pt, 0.0..=duration as f32)
                .show_value(false)
                .trailing_fill(true),
        );
        if resp.changed() {
            actions.commands.push(EngineCommand::VideoSetOutPoint {
                deck_uuid: deck.uuid.clone(),
                secs: f64::from(out_pt),
            });
        }
        learn_overlay(
            ui,
            resp.rect,
            format!("deck/{}/video/out_point", deck.uuid),
            data,
            actions,
        );
        ui.label(format_time(f64::from(out_pt)));
    });

    // Set from current / clear buttons
    ui.horizontal(|ui| {
        if ui
            .small_button("[ Set In")
            .on_hover_text("Set in-point to current position")
            .clicked()
        {
            actions.commands.push(EngineCommand::VideoSetInPoint {
                deck_uuid: deck.uuid.clone(),
                secs: vp.position,
            });
        }
        if ui
            .small_button("Set Out ]")
            .on_hover_text("Set out-point to current position")
            .clicked()
        {
            actions.commands.push(EngineCommand::VideoSetOutPoint {
                deck_uuid: deck.uuid.clone(),
                secs: vp.position,
            });
        }
        // Clear is always shown (disabled when no range) so it stays MIDI/keyboard-mappable.
        let clear_resp = ui
            .add_enabled(has_range, egui::Button::new("x Clear").small())
            .on_hover_text("Reset to full clip");
        if clear_resp.clicked() {
            actions.commands.push(EngineCommand::VideoClearInOutPoints {
                deck_uuid: deck.uuid.clone(),
            });
        }
        learn_overlay(
            ui,
            clear_resp.rect,
            format!("deck/{}/video/clear", deck.uuid),
            data,
            actions,
        );
    });

    if has_range {
        ui.label(
            egui::RichText::new(format!(
                "Range: {} → {} ({})",
                format_time(vp.in_point),
                format_time(effective_out),
                format_time(effective_out - vp.in_point),
            ))
            .small()
            .weak(),
        );
    }

    // Info line
    let decode = if vp.hardware_decode {
        "hardware decode"
    } else {
        "software decode"
    };
    ui.label(
        egui::RichText::new(format!(
            "{:.0} fps • {} • {decode}",
            vp.frame_rate,
            format_time(duration)
        ))
        .small()
        .weak(),
    )
    .on_hover_text(if vp.hardware_decode {
        "Decoded on the GPU/media engine. Colour conversion and upload may still use the CPU"
    } else {
        "Decoded on the CPU: hardware decoding is unavailable for this codec, profile or device, or is disabled"
    });
}

/// Chain positions of the deck's framing effects — its first crop and first
/// transform — and of its own mask, all shown in the framing controls. Any
/// further ones are ordinary effects in the list.
pub(super) fn framing_indices(deck: &DeckUIInfo) -> Vec<usize> {
    use crate::engine::value::mask::LAYER_MASK_EFFECT;
    FRAMING_EFFECTS
        .iter()
        .map(|(name, _)| *name)
        .chain([LAYER_MASK_EFFECT])
        .filter_map(|name| deck.effects.iter().position(|(_, n, ..)| n == name))
        .collect()
}

/// Width the columns after the preview take, so the preview can have the rest
/// and the deck fits the panel without scrolling sideways.
/// A channel's composited picture in a bottom-bar card, captioned with its
/// name. Labelled for screen readers and tests as "{name} result".
pub(super) fn channel_result_preview(
    ui: &mut egui::Ui,
    tex_id: egui::TextureId,
    size: egui::Vec2,
    name: &str,
    accent: egui::Color32,
) {
    egui::Frame::default()
        .inner_margin(6.0)
        .corner_radius(4.0)
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.set_min_width(size.x + 12.0);
            ui.set_max_width(size.x + 12.0);
            ui.with_layout(egui::Layout::top_down(egui::Align::Center), |ui| {
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(tex_id, size))
                        .alt_text(format!("{name} result")),
                );
                ui.label(egui::RichText::new(name).small().color(accent));
            });
        });
}

/// Main's row in the Send to column: the channel's Main tally with Cut and
/// Fade — or, for an overlay, On / Off.
fn render_main_take_row(
    ui: &mut egui::Ui,
    ch: &super::super::ChannelUIInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    let on = if ch.overlay {
        super::main_desk::overlay_on(data, ch)
    } else {
        super::main_desk::channel_on_main(data, ch)
    };
    ui.horizontal(|ui| {
        let mut name = egui::RichText::new(" Main ").strong();
        if on {
            name = name
                .color(egui::Color32::WHITE)
                .background_color(super::main_desk::ON_MAIN_RED);
        }
        ui.label(name);
        let button = |text: &str| {
            egui::Button::new(egui::RichText::new(text).small()).min_size(egui::vec2(40.0, 20.0))
        };
        let (cut, fade) = if ch.overlay {
            if on {
                ("Off", "Fade off")
            } else {
                ("On", "Fade on")
            }
        } else {
            ("Cut", "Fade")
        };
        let takes = ch.overlay || !on;
        if ui
            .add_enabled(takes, button(cut))
            .on_hover_text("Put it on Main now (Space)")
            .clicked()
        {
            super::main_desk::take_channel(ch, data, actions, false);
        }
        if ui
            .add_enabled(takes && data.main.fade.is_none(), button(fade))
            .on_hover_text("Fade it onto Main (Enter)")
            .clicked()
        {
            super::main_desk::take_channel(ch, data, actions, true);
        }
    });
}

/// The bus Cut/Fade column for a channel, in a bottom-bar card.
pub(super) fn render_take_frame(
    ui: &mut egui::Ui,
    channel_uuid: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    egui::Frame::default()
        .inner_margin(6.0)
        .corner_radius(4.0)
        .fill(ui.visuals().faint_bg_color)
        .show(ui, |ui| {
            ui.set_width(super::switcher::TAKE_COLUMN_WIDTH - 12.0);
            ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                ui.label(egui::RichText::new("Send to").strong());
                if let Some(ch) = data
                    .channels
                    .iter()
                    .find(|c| c.uuid == channel_uuid)
                    .filter(|_| super::main_desk::in_main_mode(data))
                {
                    render_main_take_row(ui, ch, data, actions);
                    ui.add_space(4.0);
                }
                super::switcher::render_take_column(
                    ui,
                    &crate::bus::BusInputRef::Channel(channel_uuid.to_string()),
                    data,
                    actions,
                );
            });
        });
}

fn columns_after_preview(ui: &egui::Ui, deck: &DeckUIInfo, ch_idx: usize, deck_idx: usize) -> f32 {
    const SEPARATOR: f32 = 16.0;
    const COLLAPSED: f32 = 24.0;
    let open = |key: &str, default: bool| {
        let id = egui::Id::new(key).with((ch_idx, deck_idx));
        ui.ctx()
            .memory(|m| m.data.get_temp::<bool>(id).unwrap_or(default))
    };
    let mut width = 40.0; // frame margins
    width += super::switcher::TAKE_COLUMN_WIDTH + SEPARATOR;
    if deck.video_playback.is_some() {
        width += 280.0 + SEPARATOR;
    }
    if deck.is_html {
        width += 200.0 + SEPARATOR;
    }
    width += if open("at_col_open", false) {
        260.0
    } else {
        COLLAPSED
    } + SEPARATOR;
    width += if open("params_col_open", true) {
        280.0
    } else {
        COLLAPSED
    } + SEPARATOR;
    width += EFFECT_LIST_WIDTH + SEPARATOR;
    if deck.effects.len() > framing_indices(deck).len() {
        width += EFFECT_CONTROLS_WIDTH + SEPARATOR;
    }
    width
}

/// Crop, border, transform and the layer's own mask in the Params column
/// and in the input settings' Position tab, each with a Reset. The border
/// lives in the crop effect — it runs round what the crop keeps — but has a
/// section of its own. A deck made before every deck got crop and transform
/// offers to add whichever it lacks.
pub(super) fn render_framing(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    let mut missing = Vec::new();
    for (name, title) in FRAMING_EFFECTS {
        let Some(effect) = deck.effects.iter().find(|(_, n, ..)| n == name) else {
            missing.push(name);
            continue;
        };
        let (border, own): (Vec<ParamUIInfo>, Vec<ParamUIInfo>) = effect
            .3
            .params
            .iter()
            .cloned()
            .partition(|p| p.name.starts_with(BORDER_PREFIX));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let mut on = effect.2;
            if ui
                .checkbox(&mut on, section_title(ui, title))
                .on_hover_text("On / bypassed")
                .changed()
            {
                actions.commands.push(EngineCommand::ToggleEffect {
                    effect_uuid: effect.0.clone(),
                });
            }
            let part = if name == "crop" {
                FramingPart::Crop
            } else {
                FramingPart::Transform
            };
            copy_paste_buttons(ui, deck, part, actions);
            let names: Vec<String> = own.iter().map(|p| p.name.clone()).collect();
            reset_button(
                ui,
                &effect.0,
                names,
                &format!("Put the {} back as it was", title.to_lowercase()),
                actions,
            );
        });
        render_deck_effect_controls(ui, &deck.uuid, &with_params(effect, own), data, actions);
        if !border.is_empty() {
            render_border(ui, deck, effect, border, data, actions);
        }
    }
    ui.add_space(4.0);
    ui.label(section_title(ui, "Mask")).on_hover_text(
        "Cut a shape out of the layer — or keep only the shape — so what is under it shows",
    );
    super::mask_edit::render_layer_mask_row(ui, &deck.uuid, data, actions);
    if !missing.is_empty() {
        ui.add_space(4.0);
        if ui
            .small_button("+ Crop & Transform")
            .on_hover_text("Add the framing controls every new deck starts with")
            .clicked()
        {
            for name in missing {
                actions.commands.push(EngineCommand::AddEffect {
                    target: EffectTarget::Deck(deck.uuid.clone()),
                    shader_name: name.to_string(),
                });
            }
        }
    }
}

/// The crop effect's border parameters: `border_mode` (on / off), colour,
/// thickness and roundness.
const BORDER_PREFIX: &str = "border_";

/// The text style a window gives its sections' names — Crop, Border,
/// Transform, Mask — where it sets one (see `channel_settings::readable`).
pub(super) fn section_style() -> egui::TextStyle {
    egui::TextStyle::Name("section".into())
}

/// A section's name: strong, in the section style where the window has one.
pub(super) fn section_title(ui: &egui::Ui, title: &str) -> egui::RichText {
    let text = egui::RichText::new(title).strong();
    match ui.style().text_styles.get(&section_style()) {
        Some(font) => text.font(font.clone()),
        None => text,
    }
}

/// A copy of `effect` holding only `params`, to draw some of its controls.
fn with_params(effect: &EffectInfo, params: Vec<ParamUIInfo>) -> EffectInfo {
    (
        effect.0.clone(),
        effect.1.clone(),
        effect.2,
        ShaderParamsUI {
            shader_name: effect.3.shader_name.clone(),
            params,
        },
    )
}

/// The Border section: a switch, and when on its colour, thickness and
/// roundness, as vMix's layer border — or a circle, the podcast look.
fn render_border(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    crop: &EffectInfo,
    params: Vec<ParamUIInfo>,
    data: &UIData,
    actions: &mut UIActions,
) {
    let on = params
        .iter()
        .any(|p| p.name == "border_mode" && matches!(p.value, ParamValue::Bool(true)));
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let mut shown = on;
        if ui
            .checkbox(&mut shown, section_title(ui, "Border"))
            .on_hover_text("A coloured edge round the picture, with rounded corners if wanted")
            .changed()
        {
            actions.commands.push(EngineCommand::SetEffectParam {
                effect_uuid: crop.0.clone(),
                name: "border_mode".into(),
                value: ParamValue::Bool(shown),
            });
        }
        copy_paste_buttons(ui, deck, FramingPart::Border, actions);
        let names: Vec<String> = params.iter().map(|p| p.name.clone()).collect();
        reset_button(
            ui,
            &crop.0,
            names,
            "No border, and its colour, thickness and roundness back as they were",
            actions,
        );
    });
    if on {
        // A circle is as round as can be: Roundness has nothing to do.
        let circle = params
            .iter()
            .any(|p| p.name == "border_circle" && matches!(p.value, ParamValue::Bool(true)));
        let settings: Vec<ParamUIInfo> = params
            .into_iter()
            .filter(|p| p.name != "border_mode" && !(circle && p.name == "border_radius"))
            .collect();
        render_deck_effect_controls(ui, &deck.uuid, &with_params(crop, settings), data, actions);
    }
}

/// The three parts of a deck's framing, copied and pasted apart or together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FramingPart {
    Crop,
    Border,
    Transform,
}

impl FramingPart {
    pub(super) const ALL: [Self; 3] = [Self::Crop, Self::Border, Self::Transform];

    /// The part a parameter of an effect belongs to, if any.
    fn of(effect: &str, param: &str) -> Option<Self> {
        match effect {
            "crop" if param.starts_with(BORDER_PREFIX) => Some(Self::Border),
            "crop" => Some(Self::Crop),
            "transform" => Some(Self::Transform),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Crop => "crop",
            Self::Border => "border",
            Self::Transform => "transform",
        }
    }
}

/// What was copied: each (effect, parameter, value) of the parts copied.
type FramingClip = Vec<(String, String, ParamValue)>;

fn framing_clip_id() -> egui::Id {
    egui::Id::new("layer_framing_clipboard")
}

/// Copy `parts` of a deck's framing, replacing whatever was copied before.
pub(super) fn copy_framing(ctx: &egui::Context, deck: &DeckUIInfo, parts: &[FramingPart]) {
    let clip: FramingClip = deck
        .effects
        .iter()
        .flat_map(|(_, effect, _, params)| {
            params
                .params
                .iter()
                .filter(|p| {
                    FramingPart::of(effect, &p.name).is_some_and(|part| parts.contains(&part))
                })
                .map(|p| (effect.clone(), p.name.clone(), p.value))
        })
        .collect();
    ctx.data_mut(|d| d.insert_temp(framing_clip_id(), clip));
}

/// Whether what was copied holds `part`.
pub(super) fn clip_holds(ctx: &egui::Context, part: FramingPart) -> bool {
    ctx.data(|d| d.get_temp::<FramingClip>(framing_clip_id()))
        .is_some_and(|clip| {
            clip.iter()
                .any(|(e, p, _)| FramingPart::of(e, p) == Some(part))
        })
}

/// Paste the copied `parts` onto a deck — of this channel or any other. A
/// part not copied, or an effect the deck lacks, is passed over.
pub(super) fn paste_framing(
    ctx: &egui::Context,
    deck: &DeckUIInfo,
    parts: &[FramingPart],
    actions: &mut UIActions,
) {
    let clip: FramingClip = ctx
        .data(|d| d.get_temp(framing_clip_id()))
        .unwrap_or_default();
    for (effect, name, value) in clip {
        if !FramingPart::of(&effect, &name).is_some_and(|part| parts.contains(&part)) {
            continue;
        }
        if let Some((uuid, ..)) = deck.effects.iter().find(|(_, n, ..)| *n == effect) {
            actions.commands.push(EngineCommand::SetEffectParam {
                effect_uuid: uuid.clone(),
                name,
                value,
            });
        }
    }
}

/// Copy and Paste for one part, beside its title.
fn copy_paste_buttons(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    part: FramingPart,
    actions: &mut UIActions,
) {
    let name = part.name();
    if ui
        .small_button("Copy")
        .on_hover_text(format!(
            "Copy this layer's {name}, to paste on other layers"
        ))
        .clicked()
    {
        copy_framing(ui.ctx(), deck, &[part]);
    }
    if ui
        .add_enabled(
            clip_holds(ui.ctx(), part),
            egui::Button::new("Paste").small(),
        )
        .on_hover_text(format!("Give this layer the {name} copied"))
        .on_disabled_hover_text(format!("Copy a layer's {name} first"))
        .clicked()
    {
        paste_framing(ui.ctx(), deck, &[part], actions);
    }
}

/// Copy / Paste position — crop, border and transform together — for a
/// deck's right-click menu.
pub(super) fn framing_menu_items(ui: &mut egui::Ui, deck: &DeckUIInfo, actions: &mut UIActions) {
    if ui
        .button("Copy position")
        .on_hover_text("Copy this layer's crop, border and transform")
        .clicked()
    {
        copy_framing(ui.ctx(), deck, &FramingPart::ALL);
        ui.close();
    }
    let held = FramingPart::ALL.iter().any(|p| clip_holds(ui.ctx(), *p));
    if ui
        .add_enabled(held, egui::Button::new("Paste position"))
        .on_hover_text("Give this layer the crop, border and transform copied")
        .clicked()
    {
        paste_framing(ui.ctx(), deck, &FramingPart::ALL, actions);
        ui.close();
    }
}

/// A small Reset that puts `names` of an effect back to their defaults —
/// all of its parameters when `names` is empty.
pub(super) fn reset_button(
    ui: &mut egui::Ui,
    effect_uuid: &str,
    names: Vec<String>,
    hover: &str,
    actions: &mut UIActions,
) {
    if ui.small_button("Reset").on_hover_text(hover).clicked() {
        actions.commands.push(EngineCommand::ResetEffectParams {
            effect_uuid: effect_uuid.to_string(),
            names,
        });
    }
}

/// One deck effect's parameter controls — for crop and transform in the
/// Params column, and for whichever effect is selected in the list.
pub(super) fn render_deck_effect_controls(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    effect: &EffectInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    render_effect_controls(ui, &format!("deck/{deck_uuid}"), effect, data, actions);
}

/// Any effect's parameter controls. `owner` is its chain's MIDI address —
/// `deck/<uuid>`, `ch/<uuid>` or `master` — which also keeps each widget's
/// id apart from the same effect drawn elsewhere.
pub(super) fn render_effect_controls(
    ui: &mut egui::Ui,
    owner: &str,
    effect: &EffectInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    let (eff_uuid, eff_name, _, eff_params) = effect;
    if eff_params.params.is_empty() {
        ui.label(egui::RichText::new("No parameters").weak());
    } else {
        render_effect_param_widgets(ui, owner, effect, data, actions);
    }
    // Framing places the whole picture; a mask on it would make no sense.
    if !FRAMING_EFFECTS.iter().any(|(name, _)| name == eff_name) {
        super::mask_edit::render_mask_row(ui, eff_uuid, data, actions);
    }
}

fn render_effect_param_widgets(
    ui: &mut egui::Ui,
    owner: &str,
    effect: &EffectInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    let (eff_uuid, _, _, eff_params) = effect;
    let eff_midi_prefix = format!("{owner}/effect/{eff_uuid}");
    widgets::render_effect_params(
        ui,
        &eff_params.params,
        &data.modulation_sources,
        &|name: &str, val: ParamValue| EngineCommand::SetEffectParam {
            effect_uuid: eff_uuid.clone(),
            name: name.to_string(),
            value: val,
        },
        Some(
            &|name: &str, source_uuid: &str| EngineCommand::AssignModulation {
                target: format!("fx_{eff_uuid}:{name}"),
                source_id: source_uuid.to_string(),
                amount: DEFAULT_ASSIGNMENT_AMOUNT,
            },
        ),
        Some(
            &|name: &str, source_uuid: &str| EngineCommand::ClearModulationSource {
                target: format!("fx_{eff_uuid}:{name}"),
                source_id: source_uuid.to_string(),
            },
        ),
        Some(&|name: &str| EngineCommand::ClearModulation {
            target: format!("fx_{eff_uuid}:{name}"),
        }),
        Some(&|name: &str| EngineCommand::AddAutomationLane {
            target: format!("fx_{eff_uuid}:{name}"),
            timebase: crate::timebase::Timebase::Transport,
        }),
        &mut actions.commands,
        &mut actions.session.gesture_active,
        &format!("fx_{owner}_{eff_uuid}"),
        Some(&eff_midi_prefix),
        data.midi_learn_active,
        &mut actions.session.midi_learn_select,
        data.midi_learn_target.as_deref(),
        &data.modulation_assignments,
        &data.modulation_current_values,
        &format!("fx_{eff_uuid}"),
        data.keyboard_learn_active,
        &mut actions.session.keyboard_learn_select,
        data.keyboard_learn_target.as_deref(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::{ParamUIInfo, ShaderParamsUI};
    use egui_kittest::kittest::Queryable;

    fn effect(uuid: &str, name: &str) -> EffectInfo {
        (
            uuid.to_string(),
            name.to_string(),
            true,
            ShaderParamsUI {
                shader_name: name.to_string(),
                params: vec![ParamUIInfo {
                    name: "amount".to_string(),
                    label: Some(format!("{name} amount")),
                    value: ParamValue::Float(0.5),
                    min: Some(0.0),
                    max: Some(1.0),
                }],
            },
        )
    }

    fn harness<'a>(data: &'a UIData, actions: &'a mut UIActions) -> egui_kittest::Harness<'a> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(1700.0, 520.0))
            .build_ui(move |ui| render_selected_deck_detail(ui, data, actions))
    }

    /// The deck in Ch B selected — on the fixture's TV bus Preview, not on
    /// air — with preview pictures for it and its channel.
    fn ch_b_deck_selected() -> UIData {
        let mut data = UIData::test_fixture();
        data.selected_deck = Some((1, 0));
        data.channel_preview_textures
            .insert(1, egui::TextureId::User(7));
        data.deck_preview_textures
            .insert("b0000001".into(), egui::TextureId::User(8));
        data
    }

    fn takes(actions: &UIActions) -> Vec<String> {
        actions
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::SetBusPreview { uuid, input } => {
                    Some(format!("preview {uuid} {input:?}"))
                }
                EngineCommand::BusCut { uuid } => Some(format!("cut {uuid}")),
                EngineCommand::BusAuto { uuid, .. } => Some(format!("auto {uuid}")),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_selected_deck_shows_its_channel_result_not_itself() {
        let data = ch_b_deck_selected();
        let mut actions = UIActions::new();
        let h = harness(&data, &mut actions);
        h.get_by_label("Ch B result");
        assert!(
            h.query_by_label("test_generator_c result").is_none(),
            "the bottom preview stays on the channel whichever deck is picked"
        );
    }

    #[test]
    fn cut_and_fade_beside_the_preview_send_its_channel() {
        let data = ch_b_deck_selected();
        for (button, take) in [("Cut", "cut bus00001"), ("Fade", "auto bus00001")] {
            let mut actions = UIActions::new();
            {
                let mut h = harness(&data, &mut actions);
                h.get_by_label(button).click();
                h.run();
            }
            assert_eq!(
                takes(&actions),
                [
                    r#"preview bus00001 Channel("cb000001")"#.to_string(),
                    take.to_string()
                ],
                "{button} puts the channel, not the deck, on the bus"
            );
        }
    }

    #[test]
    fn a_bus_already_showing_the_channel_cannot_take_it_again() {
        use egui_kittest::kittest::NodeT;
        // Ch A is what the fixture's TV bus has on air.
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        let h = harness(&data, &mut actions);
        for button in ["Cut", "Fade"] {
            assert!(
                h.get_by_label(button).accesskit_node().is_disabled(),
                "{button} on the bus that has it on air"
            );
        }
    }

    /// The framing controls end with the layer's own Mask: picking Ellipse
    /// puts one in the middle of the layer, cut out of it; the effect that
    /// carries it is not listed among the layer's effects.
    #[test]
    fn the_mask_section_cuts_a_shape_out_of_the_layer() {
        use crate::engine::value::mask::{MaskShape, LAYER_MASK_EFFECT};
        let mut data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let mut actions = UIActions::new();
        {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(700.0, 900.0))
                .build_ui(|ui| render_framing(ui, &deck, &data, &mut actions));
            h.get_by_label("Ellipse").click();
            h.run();
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetLayerMask {
                    deck_uuid,
                    mask: Some(mask),
                } if *deck_uuid == deck.uuid
                    && matches!(mask.shape, MaskShape::Ellipse { .. })
                    && !mask.invert
            )),
            "{:?}",
            actions.commands
        );

        let with_mask = &mut data.channels[ch].decks[dk];
        with_mask.effects.push((
            "fx00mask".into(),
            LAYER_MASK_EFFECT.into(),
            true,
            ShaderParamsUI {
                shader_name: LAYER_MASK_EFFECT.into(),
                params: Vec::new(),
            },
        ));
        let at = with_mask.effects.len() - 1;
        assert!(
            framing_indices(with_mask).contains(&at),
            "not an ordinary effect in the list"
        );
    }

    /// Border on: Roundness offered — but not with Circle on, a circle
    /// being as round as can be.
    #[test]
    fn a_circle_border_offers_no_roundness() {
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let param = |name: &str, label: &str, value: ParamValue| ParamUIInfo {
            name: name.to_string(),
            label: Some(label.to_string()),
            value,
            min: Some(0.0),
            max: Some(540.0),
        };
        for (circle, offered) in [(false, true), (true, false)] {
            let params = vec![
                param("border_mode", "Border", ParamValue::Bool(true)),
                param("border_radius", "Roundness", ParamValue::Float(20.0)),
                param("border_circle", "Circle", ParamValue::Bool(circle)),
            ];
            let crop: EffectInfo = (
                "fx000001".into(),
                "crop".into(),
                true,
                ShaderParamsUI {
                    shader_name: "crop".into(),
                    params: params.clone(),
                },
            );
            let mut actions = UIActions::new();
            let h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(600.0, 300.0))
                .build_ui(|ui| render_border(ui, &deck, &crop, params.clone(), &data, &mut actions));
            h.get_by_label_contains("Circle");
            assert_eq!(
                h.query_by_label_contains("Roundness").is_some(),
                offered,
                "circle {circle}"
            );
        }
    }

    /// A camera layer offers its picture's colour range: choosing Full sends
    /// it for the layer, keeping the matrix as it was.
    #[test]
    fn choosing_full_range_for_a_camera_sends_it() {
        use crate::engine::value::camera::{CameraColour, CameraMatrix, CameraRange};
        let mut data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = &mut data.channels[ch].decks[dk];
        deck.camera_colour = Some(CameraColour {
            range: CameraRange::Auto,
            matrix: CameraMatrix::Bt709,
        });
        let deck_uuid = deck.uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = harness(&data, &mut actions);
            h.get_by_label("📹 Camera picture");
            h.get_by_label("Colour range:").click();
            h.run();
            h.get_by_label(CameraRange::Full.label()).click();
            h.run();
        }
        let sent: Vec<CameraColour> = actions
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::SetCameraColour {
                    deck_uuid: d,
                    colour,
                } if *d == deck_uuid => Some(*colour),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            [CameraColour {
                range: CameraRange::Full,
                matrix: CameraMatrix::Bt709,
            }]
        );
    }

    /// A camera layer offers its lowest latency: ticking it sends it for the
    /// layer, its newest picture shown every frame from then on.
    #[test]
    fn ticking_lowest_latency_for_a_camera_sends_it() {
        use crate::engine::value::camera::CameraColour;
        let mut data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = &mut data.channels[ch].decks[dk];
        deck.camera_colour = Some(CameraColour::default());
        deck.camera_lowest_latency = Some(false);
        let deck_uuid = deck.uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = harness(&data, &mut actions);
            h.get_by_label("Lowest latency").click();
            h.run();
        }
        let sent: Vec<bool> = actions
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::SetCameraLowestLatency { deck_uuid: d, on } if *d == deck_uuid => {
                    Some(*on)
                }
                _ => None,
            })
            .collect();
        assert_eq!(sent, [true]);
    }

    /// A camera layer offers picture profiles — the built-in ones and the
    /// show's LUT files: choosing S-Log3 or a LUT sends it for the layer.
    #[test]
    fn choosing_a_picture_profile_for_a_camera_sends_it() {
        use crate::engine::value::camera::{CameraColour, CameraProfile};
        let mut data = UIData::test_fixture();
        data.available_luts = vec!["URSA Broadcast Film to Video.cube".into()];
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = &mut data.channels[ch].decks[dk];
        deck.camera_colour = Some(CameraColour::default());
        deck.camera_profile = Some(CameraProfile::Normal);
        let deck_uuid = deck.uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = harness(&data, &mut actions);
            h.get_by_label("Picture profile:").click();
            h.run();
            h.get_by_label(&CameraProfile::SonySLog3Cine.label()).click();
            h.run();
            h.get_by_label("Picture profile:").click();
            h.run();
            h.get_by_label("LUT: URSA Broadcast Film to Video.cube").click();
            h.run();
        }
        let sent: Vec<CameraProfile> = actions
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::SetCameraProfile {
                    deck_uuid: d,
                    profile,
                } if *d == deck_uuid => Some(profile.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            [
                CameraProfile::SonySLog3Cine,
                CameraProfile::Lut {
                    file: "URSA Broadcast Film to Video.cube".into()
                }
            ]
        );
    }

    /// Only a camera layer offers a camera's picture settings.
    #[test]
    fn a_layer_that_is_no_camera_offers_no_camera_picture() {
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        let h = harness(&data, &mut actions);
        assert!(h.query_by_label("📹 Camera picture").is_none());
    }

    #[test]
    fn crop_and_transform_sit_in_params_not_in_the_effect_list() {
        let mut data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = &mut data.channels[ch].decks[dk];
        deck.effects.insert(0, effect("fxcrop01", "crop"));
        deck.effects.insert(1, effect("fxxfrm01", "transform"));
        let mut actions = UIActions::new();
        let h = harness(&data, &mut actions);

        h.get_by_label("Crop");
        h.get_by_label("Transform");
        h.get_by_label("crop amount");
        // List rows are labelled "<name> effect card" by their menu.
        h.get_by_label("test_effect effect card");
        assert!(
            h.query_by_label("crop effect card").is_none()
                && h.query_by_label("transform effect card").is_none(),
            "framing effects must not also appear as rows in the effect list"
        );
    }

    #[test]
    fn a_deck_without_framing_offers_to_add_it() {
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck_uuid = data.channels[ch].decks[dk].uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = harness(&data, &mut actions);
            h.get_by_label("+ Crop & Transform").click();
            h.run();
        }
        let added: Vec<&str> = actions
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::AddEffect {
                    target: EffectTarget::Deck(d),
                    shader_name,
                } if *d == deck_uuid => Some(shader_name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(added, ["crop", "transform"]);
    }
}
