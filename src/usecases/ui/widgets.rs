use super::{
    modulator_color, AudioUIData, ModAssignmentUI, ModSourceUI, ModSourceUIEntry, ParamUIInfo,
};
use crate::engine::EngineCommand;
use crate::params::ParamValue;

/// Callback that builds a modulation-assignment command from (`param_path`, `source_name`).
type MakeModAssign<'a> = &'a dyn Fn(&str, &str) -> EngineCommand;

/// Callback that builds a command from a `param_path` alone.
type MakeParamCommand<'a> = &'a dyn Fn(&str) -> EngineCommand;

/// The commands the 〰 dropdown can emit. Bundled because only the caller knows
/// how to build a parameter's full target path, and they always travel together.
struct ModMenu<'a> {
    assign: MakeModAssign<'a>,
    /// Detach one source, leaving any others on the parameter alone. Without it
    /// the checklist could only ever be ticked, and un-ticking would have to go
    /// through "Clear all" and lose the other sources with it.
    unassign: MakeModAssign<'a>,
    remove: MakeParamCommand<'a>,
    automate: Option<MakeParamCommand<'a>>,
}

/// One line of the 〰 checklist.
struct ModRow<'a> {
    /// Index in the *unfiltered* source list, which picks the modulator's colour
    /// and number. Those have to match the modulation panel's cards.
    idx: usize,
    entry: &'a ModSourceUIEntry,
    assigned: bool,
}

/// One entry of a 〰 checklist, in the modulator's own colour.
///
/// Shared with the mod-on-mod menu in the modulation panel so the two cannot
/// disagree about what a tick looks like.
pub fn mod_tick_label(assigned: bool, label: &str, color: egui::Color32) -> egui::RichText {
    egui::RichText::new(format!("{} {label}", if assigned { "☑" } else { "☐" })).color(color)
}

/// Which sources the checklist lists, and which of them are ticked.
///
/// An automation curve belongs to the one parameter it was drawn for, so an
/// unassigned one is not on offer: sharing a shape between parameters is copy
/// and paste between lanes, which leaves each parameter its own curve to edit.
/// See /spec/automation.md § One envelope per parameter. An *assigned* one is
/// still listed, because a parameter driven by a lane would otherwise show an
/// empty checklist under a coloured ghost, which is the confusion this list
/// exists to remove.
fn mod_rows<'a>(
    modulation_sources: &'a [ModSourceUIEntry],
    assignments: &[ModAssignmentUI],
) -> Vec<ModRow<'a>> {
    modulation_sources
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let assigned = assignments.iter().any(|a| a.source_id == entry.uuid);
            let offerable = !matches!(entry.source, ModSourceUI::Envelope { .. });
            (offerable || assigned).then_some(ModRow {
                idx,
                entry,
                assigned,
            })
        })
        .collect()
}

/// The 〰 dropdown on a modulatable parameter: a checklist of every source, with
/// the ones driving this parameter ticked, plus an automation lane and a clear.
///
/// A checklist rather than a list of assign actions because a parameter can have
/// several sources stacked on it, and the affordances that hint at that only
/// carry one: the ghost line and the coloured label both take the colour of the
/// *first* assignment, so two sources and one source look identical. Ticks are
/// the only place the whole set is visible.
///
/// Shared by the deck and effect param renderers so the two cannot drift.
fn modulation_dropdown(
    ui: &mut egui::Ui,
    id_salt: String,
    param_name: &str,
    modulation_sources: &[ModSourceUIEntry],
    assignments: &[ModAssignmentUI],
    menu: &ModMenu,
    commands: &mut Vec<EngineCommand>,
) {
    let ModMenu {
        assign: assign_fn,
        unassign: unassign_fn,
        remove: remove_fn,
        automate: automate_fn,
    } = *menu;
    let rows = mod_rows(modulation_sources, assignments);
    // Automation needs no existing source, so the menu is worth showing even
    // when nothing has been created yet.
    if rows.is_empty() && automate_fn.is_none() {
        return;
    }

    let active: Vec<String> = rows
        .iter()
        .filter(|r| r.assigned)
        .map(|r| r.entry.label(r.idx))
        .collect();

    let response = egui::ComboBox::from_id_salt(id_salt)
        .selected_text("〰")
        .width(30.0)
        // Ticking one source should not dismiss the list. Stacking two sources
        // is one decision, and closing after each would make it two visits.
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            if !rows.is_empty() {
                ui.label(egui::RichText::new("Modulation").small().strong());
                for ModRow {
                    idx,
                    entry,
                    assigned,
                } in &rows
                {
                    let text = mod_tick_label(*assigned, &entry.label(*idx), modulator_color(*idx));
                    if ui.selectable_label(*assigned, text).clicked() {
                        commands.push(if *assigned {
                            unassign_fn(param_name, &entry.uuid)
                        } else {
                            assign_fn(param_name, &entry.uuid)
                        });
                    }
                }
                ui.separator();
            }
            if let Some(automate) = automate_fn {
                if ui
                    .button("+ Automation lane")
                    .on_hover_text(
                        "Draw this parameter as a curve against the show position.\n\
                         The curve sets the value outright, so it plays back the same every run.",
                    )
                    .clicked()
                {
                    commands.push(automate(param_name));
                }
            }
            // Only worth offering once there is something to clear, and it says
            // "all" because a checklist makes single removal the obvious gesture.
            if !active.is_empty() {
                ui.separator();
                if ui.button("Clear all").clicked() {
                    commands.push(remove_fn(param_name));
                }
            }
        })
        .response;

    // Readable without opening anything, which is the case that matters on a
    // dark stage. The colours cannot say how many; the names can.
    response.on_hover_text(if active.is_empty() {
        "No modulation assigned".to_string()
    } else {
        format!("Modulated by {}", active.join(", "))
    });
}

/// The 〰 dropdown for a parameter that is a control of its own rather than one
/// row of a param list, addressed by its full modulation key.
///
/// The fader panels have no `ParamUIInfo` to hang a menu off, so they name the
/// key directly. See /spec/modulation.md § Parameter Addressing.
pub fn modulation_menu_for_key<S: std::hash::BuildHasher>(
    ui: &mut egui::Ui,
    id_salt: String,
    param_key: &str,
    modulation_sources: &[ModSourceUIEntry],
    mod_assignments: &std::collections::HashMap<String, Vec<ModAssignmentUI>, S>,
    commands: &mut Vec<EngineCommand>,
) {
    let assign = |target: &str, source_id: &str| EngineCommand::AssignModulation {
        target: target.to_string(),
        source_id: source_id.to_string(),
        amount: 1.0,
    };
    let unassign = |target: &str, source_id: &str| EngineCommand::ClearModulationSource {
        target: target.to_string(),
        source_id: source_id.to_string(),
    };
    let remove = |target: &str| EngineCommand::ClearModulation {
        target: target.to_string(),
    };
    let automate = |target: &str| EngineCommand::AddAutomationLane {
        target: target.to_string(),
        timebase: crate::timebase::Timebase::Transport,
    };
    modulation_dropdown(
        ui,
        id_salt,
        param_key,
        modulation_sources,
        mod_assignments.get(param_key).map_or(&[], Vec::as_slice),
        &ModMenu {
            assign: &assign,
            unassign: &unassign,
            remove: &remove,
            automate: Some(&automate),
        },
        commands,
    );
}

/// Build a set of prefixes whose params should be hidden.
/// Convention: a bool param named `<prefix>_mode` controls visibility of params
/// whose name starts with `<prefix>_`. When the bool is false, those params are hidden.
fn hidden_prefixes(params: &[ParamUIInfo]) -> Vec<String> {
    let mut prefixes = Vec::new();
    for p in params {
        if let Some(stem) = p.name.strip_suffix("_mode") {
            if let ParamValue::Bool(false) = p.value {
                prefixes.push(format!("{stem}_"));
            }
        }
    }
    prefixes
}

/// Check if a param name should be hidden based on `_mode` toggle conventions.
fn is_hidden(name: &str, hidden: &[String]) -> bool {
    // Don't hide the _mode toggle itself
    if name.ends_with("_mode") {
        return false;
    }
    hidden.iter().any(|prefix| name.starts_with(prefix))
}

/// How much finer a drag moves a value while Shift is held.
const FINE_DRAG: f32 = 0.1;

/// What a float parameter's row holds after its slider: the number field
/// and the modulation menu, with the spacing between them.
const SLIDER_ROW_TAIL: f32 = 115.0;

/// The shortest a parameter's slider is made to fit its row.
const MIN_SLIDER_WIDTH: f32 = 40.0;

/// The slider of the float parameter row being laid out, after its name, no
/// longer than the row has room for after its number and modulation menu: a
/// long name leaves it less, rather than pushing the row out of its column.
fn fit_slider_to_row(ui: &mut egui::Ui) {
    let room = ui.available_width() - SLIDER_ROW_TAIL;
    ui.spacing_mut().slider_width = ui.spacing().slider_width.min(room).max(MIN_SLIDER_WIDTH);
}

/// What a float parameter's control did this frame.
struct FloatControl {
    /// The slider's rect, for learn-mode overlays and the modulation ghost.
    rect: egui::Rect,
    dragged: bool,
    changed: bool,
}

/// A float parameter's control: a slider for coarse moves, and a number field
/// beside it to type an exact value into. Holding Shift while dragging either
/// one moves the value ten times more finely.
fn float_control(ui: &mut egui::Ui, v: &mut f32, min: f32, max: f32) -> FloatControl {
    let before = *v;
    let slider = ui.add(egui::Slider::new(v, min..=max).show_value(false));
    // A slider jumps to the pointer. In Shift mode follow the pointer's
    // movement instead, scaled down, so the value creeps rather than leaps —
    // including on release, where the slider would otherwise snap back to the
    // pointer one last time.
    let fine =
        ui.input(|i| i.modifiers.shift_only()) && (slider.dragged() || slider.drag_stopped());
    if fine {
        let dx = ui.input(|i| i.pointer.delta().x);
        let width = slider.rect.width().max(1.0);
        *v = (before + dx / width * (max - min) * FINE_DRAG).clamp(min, max);
    }
    // egui's number field already slows tenfold under Shift and takes typing.
    let field = ui.add(
        egui::DragValue::new(v)
            .range(min..=max)
            .speed(f64::from((max - min) / 200.0))
            .max_decimals(3),
    );
    FloatControl {
        rect: slider.rect,
        dragged: slider.dragged() || field.dragged(),
        changed: slider.changed() || field.changed() || (fine && (*v - before).abs() > 0.0),
    }
}

/// Render parameter controls (sliders, checkboxes, color pickers) for a list of params.
/// Returns any param updates generated by user interaction.
// UI render fn taking many independent egui state/handle args; no shared invariant to bundle.
#[allow(clippy::too_many_arguments)]
pub fn render_params<S: std::hash::BuildHasher>(
    ui: &mut egui::Ui,
    params: &[ParamUIInfo],
    modulation_sources: &[ModSourceUIEntry],
    make_update: &dyn Fn(&str, ParamValue) -> EngineCommand,
    make_mod_assign: Option<MakeModAssign>,
    make_mod_unassign: Option<MakeModAssign>,
    make_mod_remove: Option<MakeParamCommand>,
    make_automation: Option<MakeParamCommand>,
    commands: &mut Vec<EngineCommand>,
    gesture_active: &mut bool,
    id_prefix: &str,
    midi_learn_path_prefix: Option<&str>,
    midi_learn_active: bool,
    midi_learn_select: &mut Option<String>,
    midi_learn_target: Option<&str>,
    mod_assignments: &std::collections::HashMap<String, Vec<ModAssignmentUI>, S>,
    mod_current_values: &std::collections::HashMap<String, f32, S>,
    mod_param_prefix: &str,
    keyboard_learn_active: bool,
    keyboard_learn_select: &mut Option<crate::keymap::KeyTarget>,
    keyboard_learn_target: Option<&str>,
) {
    let hidden = hidden_prefixes(params);
    for param in params {
        if is_hidden(&param.name, &hidden) {
            continue;
        }
        let label = param.label.as_ref().unwrap_or(&param.name);
        // Check if this param is modulated and get color info
        let mod_key = format!("{}:{}", mod_param_prefix, param.name);
        let assignments = mod_assignments.get(&mod_key);
        let is_modulated = assignments.is_some_and(|a| !a.is_empty());
        // Pick the primary modulator color (first assignment)
        let mod_label_color = assignments.and_then(|a| a.first()).map(|a| {
            let color_idx = modulation_sources
                .iter()
                .position(|e| e.uuid == a.source_id)
                .unwrap_or(0);
            modulator_color(color_idx)
        });
        match param.value {
            ParamValue::Float(mut v) => {
                let min = param.min.unwrap_or(0.0);
                let max = param.max.unwrap_or(1.0);
                ui.horizontal(|ui| {
                    // Color-code label if modulated
                    if let Some(color) = mod_label_color {
                        ui.label(egui::RichText::new(label).small().color(color));
                    } else {
                        ui.label(egui::RichText::new(label).small());
                    }
                    fit_slider_to_row(ui);
                    // Render slider — in learn mode, disable mouse interaction via a scope
                    let any_learn_active = midi_learn_active || keyboard_learn_active;
                    let slider_rect = if any_learn_active {
                        let inner = ui.scope(|ui| {
                            ui.disable();
                            float_control(ui, &mut v, min, max)
                        });
                        inner.inner.rect
                    } else {
                        let control = float_control(ui, &mut v, min, max);
                        // A held drag is a single undo gesture (collapsed by
                        // the runner's `gesture_active` edge).
                        if control.dragged {
                            *gesture_active = true;
                        }
                        if control.changed {
                            commands.push(make_update(&param.name, ParamValue::Float(v)));
                        }
                        control.rect
                    };
                    // MIDI learn mode: glow + click overlay on the (now-enabled) outer ui
                    if midi_learn_active {
                        if let Some(prefix) = midi_learn_path_prefix {
                            let path = format!("{}/param/{}", prefix, param.name);
                            let is_target = midi_learn_target.is_some_and(|t| t == path);
                            if is_target {
                                draw_midi_learn_selected(ui, slider_rect);
                            } else {
                                draw_midi_learn_glow(ui, slider_rect);
                            }
                            let click_id = ui.id().with(("midi_learn_param", &param.name));
                            let click_resp =
                                ui.interact(slider_rect, click_id, egui::Sense::click());
                            if click_resp.clicked() {
                                *midi_learn_select = Some(path);
                            }
                        }
                    }
                    // Keyboard learn mode: orange glow + click overlay
                    if keyboard_learn_active {
                        if let Some(prefix) = midi_learn_path_prefix {
                            let path = format!("{}/param/{}", prefix, param.name);
                            let is_target = keyboard_learn_target.is_some_and(|t| t == path);
                            if is_target {
                                draw_keyboard_learn_selected(ui, slider_rect);
                            } else {
                                draw_keyboard_learn_glow(ui, slider_rect);
                            }
                            let click_id = ui.id().with(("kb_learn_param", &param.name));
                            let click_resp =
                                ui.interact(slider_rect, click_id, egui::Sense::click());
                            if click_resp.clicked() {
                                *keyboard_learn_select =
                                    Some(crate::keymap::KeyTarget::ParamPath(path));
                            }
                        }
                    }
                    // Draw modulation ghost indicator on top of slider
                    if is_modulated {
                        if let Some(assigns) = assignments {
                            let mut total_offset = 0.0f32;
                            for a in assigns {
                                total_offset +=
                                    mod_current_values.get(&a.source_id).copied().unwrap_or(0.0)
                                        * a.amount;
                            }
                            // Scale by param range to match GPU-side modulation
                            let range = max - min;
                            let modulated_val = (v + total_offset * range).clamp(min, max);
                            let frac = (modulated_val - min) / (max - min);
                            let x = slider_rect.left() + frac * slider_rect.width();
                            let color = mod_label_color.unwrap_or(egui::Color32::YELLOW);
                            let painter = ui.painter();
                            // Vertical line at modulated value position
                            painter.line_segment(
                                [
                                    egui::pos2(x, slider_rect.top()),
                                    egui::pos2(x, slider_rect.bottom()),
                                ],
                                egui::Stroke::new(2.0_f32, color),
                            );
                        }
                    }
                    if let (Some(assign_fn), Some(unassign_fn), Some(remove_fn)) =
                        (make_mod_assign, make_mod_unassign, make_mod_remove)
                    {
                        modulation_dropdown(
                            ui,
                            format!("mod_{}_{}", id_prefix, param.name),
                            &param.name,
                            modulation_sources,
                            assignments.map_or(&[], Vec::as_slice),
                            &ModMenu {
                                assign: assign_fn,
                                unassign: unassign_fn,
                                remove: remove_fn,
                                automate: make_automation,
                            },
                            commands,
                        );
                    }
                });
            }
            ParamValue::Bool(mut v) => {
                if ui
                    .checkbox(&mut v, egui::RichText::new(label).small())
                    .changed()
                {
                    commands.push(make_update(&param.name, ParamValue::Bool(v)));
                }
            }
            ParamValue::Color(c) => {
                let mut color = [c[0], c[1], c[2], c[3]];
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).small());
                    if colour_edit(ui, &param.name, &mut color) {
                        commands.push(make_update(&param.name, ParamValue::Color(color)));
                    }
                });
            }
            _ => {} // Long, Point2D — not yet handled
        }
    }
}

/// A colour: its button, opening egui's picker, and where the screen can be
/// sampled a **Pick** button — then a click anywhere on the screen, a
/// picture in Nida too, takes that colour (Esc leaves it). `salt` tells this
/// field from others beside it. True when the colour changed.
pub fn colour_edit(
    ui: &mut egui::Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    colour: &mut [f32; 4],
) -> bool {
    use super::colour_sampler;
    let field = ui.make_persistent_id(("colour_edit", salt));
    let mut changed = ui.color_edit_button_rgba_unmultiplied(colour).changed();
    if colour_sampler::available() {
        let picking = colour_sampler::picking(field);
        if ui
            .add_enabled(!picking, egui::Button::new("Pick").small())
            .on_hover_text(
                "Click anywhere on the screen — a picture in Nida too — to take its colour. Esc cancels.",
            )
            .clicked()
        {
            colour_sampler::start(field);
        }
        if picking {
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(rgb) = colour_sampler::take(field) {
            colour[..3].copy_from_slice(&rgb);
            changed = true;
        }
    }
    changed
}

/// Render effect parameter controls with optional modulation assignment
// UI render fn taking many independent egui state/handle args; no shared invariant to bundle.
#[allow(clippy::too_many_arguments)]
pub fn render_effect_params<S: std::hash::BuildHasher>(
    ui: &mut egui::Ui,
    params: &[ParamUIInfo],
    modulation_sources: &[ModSourceUIEntry],
    make_update: &dyn Fn(&str, ParamValue) -> EngineCommand,
    make_mod_assign: Option<MakeModAssign>,
    make_mod_unassign: Option<MakeModAssign>,
    make_mod_remove: Option<MakeParamCommand>,
    make_automation: Option<MakeParamCommand>,
    commands: &mut Vec<EngineCommand>,
    gesture_active: &mut bool,
    id_prefix: &str,
    midi_learn_path_prefix: Option<&str>,
    midi_learn_active: bool,
    midi_learn_select: &mut Option<String>,
    midi_learn_target: Option<&str>,
    mod_assignments: &std::collections::HashMap<String, Vec<ModAssignmentUI>, S>,
    mod_current_values: &std::collections::HashMap<String, f32, S>,
    mod_param_prefix: &str,
    keyboard_learn_active: bool,
    keyboard_learn_select: &mut Option<crate::keymap::KeyTarget>,
    keyboard_learn_target: Option<&str>,
) {
    let hidden = hidden_prefixes(params);
    for param in params {
        if is_hidden(&param.name, &hidden) {
            continue;
        }
        let label = param.label.as_ref().unwrap_or(&param.name);
        let mod_key = format!("{}:{}", mod_param_prefix, param.name);
        let assignments = mod_assignments.get(&mod_key);
        let is_modulated = assignments.is_some_and(|a| !a.is_empty());
        let mod_label_color = assignments.and_then(|a| a.first()).map(|a| {
            let color_idx = modulation_sources
                .iter()
                .position(|e| e.uuid == a.source_id)
                .unwrap_or(0);
            modulator_color(color_idx)
        });
        match param.value {
            ParamValue::Float(mut v) => {
                let min = param.min.unwrap_or(0.0);
                let max = param.max.unwrap_or(1.0);
                ui.horizontal(|ui| {
                    if let Some(color) = mod_label_color {
                        ui.label(egui::RichText::new(label).small().weak().color(color));
                    } else {
                        ui.label(egui::RichText::new(label).small().weak());
                    }
                    fit_slider_to_row(ui);
                    // Render slider — in learn mode, disable mouse interaction via a scope
                    let any_learn_active = midi_learn_active || keyboard_learn_active;
                    let slider_rect = if any_learn_active {
                        let inner = ui.scope(|ui| {
                            ui.disable();
                            float_control(ui, &mut v, min, max)
                        });
                        inner.inner.rect
                    } else {
                        let control = float_control(ui, &mut v, min, max);
                        // A held drag is a single undo gesture (collapsed by
                        // the runner's `gesture_active` edge).
                        if control.dragged {
                            *gesture_active = true;
                        }
                        if control.changed {
                            commands.push(make_update(&param.name, ParamValue::Float(v)));
                        }
                        control.rect
                    };
                    // MIDI learn mode: glow + click overlay
                    if midi_learn_active {
                        if let Some(prefix) = midi_learn_path_prefix {
                            let path = format!("{}/param/{}", prefix, param.name);
                            let is_target = midi_learn_target.is_some_and(|t| t == path);
                            if is_target {
                                draw_midi_learn_selected(ui, slider_rect);
                            } else {
                                draw_midi_learn_glow(ui, slider_rect);
                            }
                            let click_id = ui.id().with(("midi_learn_fx_param", &param.name));
                            let click_resp =
                                ui.interact(slider_rect, click_id, egui::Sense::click());
                            if click_resp.clicked() {
                                *midi_learn_select = Some(path);
                            }
                        }
                    }
                    // Keyboard learn mode: orange glow + click overlay
                    if keyboard_learn_active {
                        if let Some(prefix) = midi_learn_path_prefix {
                            let path = format!("{}/param/{}", prefix, param.name);
                            let is_target = keyboard_learn_target.is_some_and(|t| t == path);
                            if is_target {
                                draw_keyboard_learn_selected(ui, slider_rect);
                            } else {
                                draw_keyboard_learn_glow(ui, slider_rect);
                            }
                            let click_id = ui.id().with(("kb_learn_fx_param", &param.name));
                            let click_resp =
                                ui.interact(slider_rect, click_id, egui::Sense::click());
                            if click_resp.clicked() {
                                *keyboard_learn_select =
                                    Some(crate::keymap::KeyTarget::ParamPath(path));
                            }
                        }
                    }
                    // Draw modulation ghost indicator
                    if is_modulated {
                        if let Some(assigns) = assignments {
                            let mut total_offset = 0.0f32;
                            for a in assigns {
                                total_offset +=
                                    mod_current_values.get(&a.source_id).copied().unwrap_or(0.0)
                                        * a.amount;
                            }
                            // Scale by param range to match GPU-side modulation
                            let range = max - min;
                            let modulated_val = (v + total_offset * range).clamp(min, max);
                            let frac = (modulated_val - min) / (max - min);
                            let x = slider_rect.left() + frac * slider_rect.width();
                            let color = mod_label_color.unwrap_or(egui::Color32::YELLOW);
                            let painter = ui.painter();
                            painter.line_segment(
                                [
                                    egui::pos2(x, slider_rect.top()),
                                    egui::pos2(x, slider_rect.bottom()),
                                ],
                                egui::Stroke::new(2.0_f32, color),
                            );
                        }
                    }
                    if let (Some(assign_fn), Some(unassign_fn), Some(remove_fn)) =
                        (make_mod_assign, make_mod_unassign, make_mod_remove)
                    {
                        modulation_dropdown(
                            ui,
                            format!("mod_{}_{}", id_prefix, param.name),
                            &param.name,
                            modulation_sources,
                            assignments.map_or(&[], Vec::as_slice),
                            &ModMenu {
                                assign: assign_fn,
                                unassign: unassign_fn,
                                remove: remove_fn,
                                automate: make_automation,
                            },
                            commands,
                        );
                    }
                });
            }
            ParamValue::Bool(mut v) => {
                if ui
                    .checkbox(&mut v, egui::RichText::new(label).small().weak())
                    .changed()
                {
                    commands.push(make_update(&param.name, ParamValue::Bool(v)));
                }
            }
            ParamValue::Color(c) => {
                let mut color = [c[0], c[1], c[2], c[3]];
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(label).small().weak());
                    if colour_edit(ui, &param.name, &mut color) {
                        commands.push(make_update(&param.name, ParamValue::Color(color)));
                    }
                });
            }
            _ => {}
        }
    }
}

/// Render audio level bars
pub fn render_audio_levels(ui: &mut egui::Ui, audio: &AudioUIData) {
    if audio.enabled {
        ui.horizontal(|ui| {
            ui.label("Vol:");
            ui.add(egui::ProgressBar::new(audio.level).desired_width(100.0));
        });
        ui.horizontal(|ui| {
            ui.label("Bass:");
            ui.add(
                egui::ProgressBar::new(audio.bass)
                    .desired_width(100.0)
                    .fill(egui::Color32::from_rgb(220, 60, 60)),
            );
        });
        ui.horizontal(|ui| {
            ui.label("Mid:");
            ui.add(
                egui::ProgressBar::new(audio.mid)
                    .desired_width(100.0)
                    .fill(egui::Color32::from_rgb(60, 220, 60)),
            );
        });
        ui.horizontal(|ui| {
            ui.label("High:");
            ui.add(
                egui::ProgressBar::new(audio.treble)
                    .desired_width(100.0)
                    .fill(egui::Color32::from_rgb(60, 60, 220)),
            );
        });
        if let Some(bpm) = audio.bpm {
            ui.horizontal(|ui| {
                ui.label(format!("BPM: {bpm:.0}"));
                ui.add(
                    egui::ProgressBar::new(audio.beat_phase)
                        .desired_width(50.0)
                        .fill(egui::Color32::from_rgb(255, 165, 0)),
                );
            });
        }
    } else {
        ui.label("⚠ No audio input");
    }
}

/// Draw a pulsing purple glow around a rect to indicate it's a MIDI-learnable target.
pub fn draw_midi_learn_glow(ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    let glow_color = egui::Color32::from_rgba_unmultiplied(180, 80, 220, 80);
    let expanded = rect.expand(2.0);
    painter.rect_stroke(
        expanded,
        3.0,
        egui::Stroke::new(2.0_f32, glow_color),
        egui::StrokeKind::Outside,
    );
}

/// Draw a brighter glow for the currently selected MIDI learn target.
pub fn draw_midi_learn_selected(ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    let glow_color = egui::Color32::from_rgba_unmultiplied(255, 100, 50, 120);
    let expanded = rect.expand(3.0);
    painter.rect_stroke(
        expanded,
        3.0,
        egui::Stroke::new(3.0_f32, glow_color),
        egui::StrokeKind::Outside,
    );
}

/// Draw an orange glow around a rect for keyboard-learnable target.
pub fn draw_keyboard_learn_glow(ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    let glow_color = egui::Color32::from_rgba_unmultiplied(255, 165, 0, 80);
    let expanded = rect.expand(2.0);
    painter.rect_stroke(
        expanded,
        3.0,
        egui::Stroke::new(2.0_f32, glow_color),
        egui::StrokeKind::Outside,
    );
}

/// Draw a brighter orange glow for the currently selected keyboard learn target.
pub fn draw_keyboard_learn_selected(ui: &egui::Ui, rect: egui::Rect) {
    let painter = ui.painter();
    let glow_color = egui::Color32::from_rgba_unmultiplied(255, 120, 0, 120);
    let expanded = rect.expand(3.0);
    painter.rect_stroke(
        expanded,
        3.0,
        egui::Stroke::new(3.0_f32, glow_color),
        egui::StrokeKind::Outside,
    );
}

/// A rotary knob for a normalized `0.0..=1.0` value. Vertical drag adjusts it
/// (drag **up** = increase, down = decrease); the arc and pointer fill in the
/// `accent` color. Returns the response with `.changed()` set on movement, so
/// callers can emit a live value action and attach a MIDI-learn overlay.
pub fn render_knob(
    ui: &mut egui::Ui,
    value: &mut f32,
    diameter: f32,
    accent: egui::Color32,
    ghost: Option<(f32, egui::Color32)>,
) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(
        egui::vec2(diameter, diameter),
        egui::Sense::click_and_drag(),
    );

    if response.dragged() {
        let dy = response.drag_delta().y;
        if dy != 0.0 {
            // 200px of vertical travel spans the full range — precise but reachable.
            *value = (*value - dy / 200.0).clamp(0.0, 1.0);
            response.mark_changed();
        }
    }

    if ui.is_rect_visible(rect) {
        // The knob sweeps 270° clockwise from lower-left (min) to lower-right (max),
        // passing through the top. Screen space is y-down, so angles grow clockwise.
        const START_DEG: f32 = 135.0;
        const SWEEP_DEG: f32 = 270.0;

        let painter = ui.painter();
        let center = rect.center();
        let radius = diameter * 0.5 - 2.0;

        // Body
        painter.circle_filled(center, radius, egui::Color32::from_rgb(28, 28, 38));
        painter.circle_stroke(
            center,
            radius,
            egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(70, 70, 90)),
        );

        let v = value.clamp(0.0, 1.0);
        let start = START_DEG.to_radians();
        let end = (START_DEG + SWEEP_DEG * v).to_radians();
        let on_arc = |a: f32, r: f32| center + egui::vec2(a.cos(), a.sin()) * r;

        // Full track (faint) then the filled value arc on top.
        let arc_r = radius - 2.0;
        let track_end = (START_DEG + SWEEP_DEG).to_radians();
        let track: Vec<egui::Pos2> = (0..=48)
            .map(|i| {
                let a = start + (track_end - start) * (i as f32 / 48.0);
                on_arc(a, arc_r)
            })
            .collect();
        painter.add(egui::Shape::line(
            track,
            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(55, 55, 70)),
        ));
        let fill: Vec<egui::Pos2> = (0..=48)
            .map(|i| {
                let a = start + (end - start) * (i as f32 / 48.0);
                on_arc(a, arc_r)
            })
            .collect();
        painter.add(egui::Shape::line(fill, egui::Stroke::new(2.5_f32, accent)));

        // Modulation ghost: a marker at the effective (base + offset) value in
        // the modulator's color, so a modulated knob visibly tracks the source
        // (mirrors the ghost line on modulated param sliders).
        if let Some((gv, gcolor)) = ghost {
            let gend = (START_DEG + SWEEP_DEG * gv.clamp(0.0, 1.0)).to_radians();
            painter.line_segment(
                [center, on_arc(gend, radius - 3.0)],
                egui::Stroke::new(1.5_f32, gcolor),
            );
            painter.circle_filled(on_arc(gend, arc_r), 2.5_f32, gcolor);
        }

        // Pointer from center to the current angle.
        painter.line_segment(
            [center, on_arc(end, radius - 3.0)],
            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(220, 220, 230)),
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::ModSourceUI;
    use egui_kittest::kittest::Queryable;

    fn lfo_entry(uuid: &str) -> ModSourceUIEntry {
        ModSourceUIEntry {
            uuid: uuid.to_string(),
            source: ModSourceUI::LFO {
                waveform: crate::modulation::LFOWaveform::Sine,
                frequency: 1.0,
                phase: 0.0,
                amplitude: 1.0,
                bipolar: false,
            },
            timebase: crate::timebase::Timebase::FreeRun,
        }
    }

    fn envelope_entry(uuid: &str) -> ModSourceUIEntry {
        ModSourceUIEntry {
            uuid: uuid.to_string(),
            source: ModSourceUI::Envelope {
                breakpoints: Vec::new(),
            },
            timebase: crate::timebase::Timebase::Transport,
        }
    }

    fn assigned_to(source_id: &str) -> ModAssignmentUI {
        ModAssignmentUI {
            source_id: source_id.to_string(),
            amount: 1.0,
        }
    }

    /// Open the dropdown and hand it to `act`, which drives the harness.
    fn in_dropdown(
        sources: &[ModSourceUIEntry],
        assignments: &[ModAssignmentUI],
        automate: bool,
        act: &dyn Fn(&mut egui_kittest::Harness<'_>),
    ) -> Vec<EngineCommand> {
        let mut commands = Vec::new();
        {
            let assign = |name: &str, uuid: &str| EngineCommand::AssignModulation {
                target: name.to_string(),
                source_id: uuid.to_string(),
                amount: 1.0,
            };
            let unassign = |name: &str, uuid: &str| EngineCommand::ClearModulationSource {
                target: name.to_string(),
                source_id: uuid.to_string(),
            };
            let remove = |name: &str| EngineCommand::ClearModulation {
                target: name.to_string(),
            };
            let add_lane = |name: &str| EngineCommand::AddAutomationLane {
                target: name.to_string(),
                timebase: crate::timebase::Timebase::Transport,
            };
            let mut harness = egui_kittest::Harness::new_ui(|ui| {
                modulation_dropdown(
                    ui,
                    "test".to_string(),
                    "opacity",
                    sources,
                    assignments,
                    &ModMenu {
                        assign: &assign,
                        unassign: &unassign,
                        remove: &remove,
                        automate: if automate {
                            Some(&add_lane as MakeParamCommand)
                        } else {
                            None
                        },
                    },
                    &mut commands,
                );
            });
            // A ComboBox exposes its selected text as AccessKit `value`, not `label`.
            harness.get_by_value("〰").click();
            harness.run();
            act(&mut harness);
        }
        commands
    }

    /// Drive the dropdown open and click `label`, returning what it emitted.
    fn click_in_dropdown(
        sources: &[ModSourceUIEntry],
        assignments: &[ModAssignmentUI],
        automate: bool,
        label: &str,
    ) -> Vec<EngineCommand> {
        in_dropdown(sources, assignments, automate, &|harness| {
            harness.get_by_label(label).click();
            harness.run();
        })
    }

    #[test]
    fn the_automation_entry_creates_a_lane_for_this_parameter() {
        let commands = click_in_dropdown(&[lfo_entry("lfo1")], &[], true, "+ Automation lane");
        assert!(
            commands.iter().any(|c| matches!(
                c,
                EngineCommand::AddAutomationLane { target, timebase }
                    if target == "opacity" && *timebase == crate::timebase::Timebase::Transport
            )),
            "expected a transport-locked lane on this parameter, got {commands:?}"
        );
    }

    /// Automation needs no existing source, so the menu has to be reachable on a
    /// scene that has never created a modulator.
    #[test]
    fn the_menu_opens_with_no_modulation_sources_at_all() {
        let commands = click_in_dropdown(&[], &[], true, "+ Automation lane");
        assert!(commands
            .iter()
            .any(|c| matches!(c, EngineCommand::AddAutomationLane { .. })));
    }

    #[test]
    fn ticking_a_source_assigns_it() {
        let commands = click_in_dropdown(&[lfo_entry("lfo1")], &[], true, "☐ LFO 1");
        assert!(commands.iter().any(|c| matches!(
            c,
            EngineCommand::AssignModulation { target, source_id, .. }
                if target == "opacity" && source_id == "lfo1"
        )));
    }

    /// The point of the checklist: a source already on the parameter reads as
    /// ticked, so the set driving it is legible without opening anything else.
    #[test]
    fn an_assigned_source_reads_as_ticked() {
        in_dropdown(
            &[lfo_entry("lfo1"), lfo_entry("lfo2")],
            &[assigned_to("lfo2")],
            true,
            &|harness| {
                assert!(
                    harness.query_by_label("☐ LFO 1").is_some(),
                    "an unassigned source is offered unticked"
                );
                assert!(
                    harness.query_by_label("☑ LFO 2").is_some(),
                    "an assigned source is ticked"
                );
            },
        );
    }

    /// Un-ticking detaches only that source. Two stacked modulators, and
    /// removing one has to leave the other driving the parameter.
    #[test]
    fn unticking_a_source_detaches_only_that_one() {
        let commands = click_in_dropdown(
            &[lfo_entry("lfo1"), lfo_entry("lfo2")],
            &[assigned_to("lfo1"), assigned_to("lfo2")],
            true,
            "☑ LFO 1",
        );
        assert!(
            commands.iter().any(|c| matches!(
                c,
                EngineCommand::ClearModulationSource { target, source_id }
                    if target == "opacity" && source_id == "lfo1"
            )),
            "expected only lfo1 detached, got {commands:?}"
        );
        assert!(
            !commands
                .iter()
                .any(|c| matches!(c, EngineCommand::ClearModulation { .. })),
            "un-ticking one source must not clear the whole parameter"
        );
    }

    #[test]
    fn clearing_removes_every_assignment() {
        let commands = click_in_dropdown(
            &[lfo_entry("lfo1")],
            &[assigned_to("lfo1")],
            true,
            "Clear all",
        );
        assert!(commands.iter().any(|c| matches!(
            c,
            EngineCommand::ClearModulation { target } if target == "opacity"
        )));
    }

    /// Nothing to clear, so the entry stays out of the way rather than sitting
    /// there as a no-op under a list of empty boxes.
    #[test]
    fn an_unmodulated_parameter_offers_no_clear() {
        in_dropdown(&[lfo_entry("lfo1")], &[], true, &|harness| {
            assert!(harness.query_by_label("Clear all").is_none());
        });
    }

    /// Envelopes are still named for the lanes and cards that list them, even
    /// though this menu does not offer them.
    #[test]
    fn an_envelope_is_labelled_as_automation() {
        assert_eq!(envelope_entry("e1").label(2), "Automation 3");
    }

    /// A curve drives the one parameter it was drawn for. Offering it here would
    /// let two parameters share a source, and then editing either lane would
    /// silently rewrite the other.
    #[test]
    fn an_automation_curve_is_not_offered_as_a_source() {
        in_dropdown(
            &[lfo_entry("lfo1"), envelope_entry("env1")],
            &[],
            false,
            &|harness| {
                assert!(
                    harness.query_by_label("☐ LFO 1").is_some(),
                    "an ordinary modulator is still on offer"
                );
                assert!(
                    harness.query_by_label("☐ Automation 2").is_none(),
                    "a curve must not be assignable to a second parameter"
                );
            },
        );
    }

    /// A lane already drives this parameter, so it is listed even though an
    /// unassigned one would not be. Leaving it out would show an empty checklist
    /// under the coloured ghost the lane is drawing.
    #[test]
    fn an_assigned_curve_is_listed_so_the_ghost_has_an_owner() {
        in_dropdown(
            &[lfo_entry("lfo1"), envelope_entry("env1")],
            &[assigned_to("env1")],
            false,
            &|harness| {
                assert!(harness.query_by_label("☑ Automation 2").is_some());
            },
        );
    }

    /// The menu still opens for a parameter in a scene whose only sources are
    /// curves, because drawing a new one has to stay reachable.
    #[test]
    fn a_scene_of_only_curves_still_offers_a_new_lane() {
        let commands =
            click_in_dropdown(&[envelope_entry("env1")], &[], true, "+ Automation lane");
        assert!(commands
            .iter()
            .any(|c| matches!(c, EngineCommand::AddAutomationLane { .. })));
    }
}

#[cfg(test)]
mod float_control_tests {
    use super::float_control;
    use egui::{Event, Modifiers, PointerButton};
    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;

    fn harness(start: f32) -> Harness<'static, f32> {
        Harness::builder()
            .with_size(egui::vec2(320.0, 60.0))
            .build_ui_state(
                |ui, v: &mut f32| {
                    ui.horizontal(|ui| {
                        float_control(ui, v, 0.0, 1.0);
                    });
                },
                start,
            )
    }

    /// Press on the slider's handle, move right by `dx`, release.
    fn drag_handle(harness: &mut Harness<'static, f32>, dx: f32, modifiers: Modifiers) {
        let slider = harness.get_by_role(egui::accesskit::Role::Slider).rect();
        let start = egui::pos2(slider.center().x, slider.center().y);
        let end = start + egui::vec2(dx, 0.0);
        harness.event(Event::ModifiersChanged(modifiers));
        harness.event(Event::PointerMoved(start));
        harness.event(Event::PointerButton {
            pos: start,
            button: PointerButton::Primary,
            pressed: true,
            modifiers,
        });
        harness.step();
        harness.event(Event::PointerMoved(end));
        harness.step();
        harness.event(Event::PointerButton {
            pos: end,
            button: PointerButton::Primary,
            pressed: false,
            modifiers,
        });
        harness.event(Event::ModifiersChanged(Modifiers::default()));
        harness.run();
    }

    #[test]
    fn shift_makes_a_slider_drag_ten_times_finer() {
        let mut coarse = harness(0.5);
        drag_handle(&mut coarse, 20.0, Modifiers::default());
        let coarse_move = *coarse.state() - 0.5;

        let mut fine = harness(0.5);
        drag_handle(&mut fine, 20.0, Modifiers::SHIFT);
        let fine_move = *fine.state() - 0.5;

        assert!(
            coarse_move > 0.1,
            "a plain drag follows the pointer, moved {coarse_move}"
        );
        assert!(
            fine_move > 0.0,
            "a Shift drag still moves, moved {fine_move}"
        );
        assert!(
            fine_move < coarse_move / 5.0,
            "Shift should move far less: {fine_move} vs {coarse_move}"
        );
    }

    #[test]
    fn an_exact_value_can_be_typed() {
        let mut h = harness(0.5);
        h.get_by_role(egui::accesskit::Role::SpinButton).focus();
        h.run();
        h.key_press_modifiers(Modifiers::COMMAND, egui::Key::A);
        h.get_by_role(egui::accesskit::Role::SpinButton)
            .type_text("0.25");
        h.key_press(egui::Key::Enter);
        h.run();
        assert!(
            (*h.state() - 0.25).abs() < 1e-6,
            "typed 0.25, got {}",
            h.state()
        );
    }
}
