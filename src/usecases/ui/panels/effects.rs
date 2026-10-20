//! Master and channel effect detail panels.

use super::super::{widgets, EffectDrag, LibraryDrag, UIActions, UIData};
use super::clipboard_menu;
use super::utils::{
    channel_color, render_effect_drag_ghost, render_effect_drag_handle, render_effect_drop_zone,
};
use crate::engine::EngineCommand;
use crate::modulation::DEFAULT_ASSIGNMENT_AMOUNT;
use crate::params::ParamValue;

/// Copy, duplicate, paste, and remove, on any effect card in any chain.
///
/// `card` must be the response of a *scope* wrapping the card rather than the
/// card's own `Frame` response. A `Ui` registers itself before its contents, so
/// its rect loses hit-test ties to the checkboxes and sliders inside it; a
/// `Frame` registers after, so making that one sense clicks would swallow every
/// click meant for a parameter.
pub(super) fn effect_context_menu(
    card: &egui::Response,
    data: &UIData,
    actions: &mut UIActions,
    uuid: &str,
    name: &str,
) {
    card.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            true,
            format!("{name} effect card"),
        )
    });
    card.context_menu(|ui| {
        clipboard_menu::items(
            ui,
            data,
            actions,
            &clipboard_menu::Subject::effect(uuid, name),
        );
        ui.separator();
        if ui.button("Remove effect").clicked() {
            actions.commands.push(EngineCommand::RemoveEffect {
                effect_uuid: uuid.to_string(),
            });
            ui.close();
        }
    });
}

pub(super) fn render_master_effect_detail(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.heading("🎬 Master Effects");

    egui::ScrollArea::horizontal()
        .id_salt("master_fx_hscroll")
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                {
                    for (eff_idx, (eff_uuid, eff_name, eff_enabled, eff_params)) in
                        data.master_effect_info.iter().enumerate()
                    {
                        let eff_uuid_master = eff_uuid.clone();
                        let eff_uuid_master_unassign = eff_uuid.clone();
                        let eff_uuid_master_remove = eff_uuid.clone();
                        let eff_uuid_master_automate = eff_uuid.clone();
                        render_effect_drop_zone(ui, "master", eff_idx);

                        // A scope, not the Frame, because a `Ui` registers itself
                        // before its contents and so loses hit-test ties to the
                        // parameter widgets inside the card.
                        let card = egui::UiBuilder::new().sense(egui::Sense::click());
                        let card_scope = ui.scope_builder(card, |ui| {
                            egui::Frame::default()
                            .inner_margin(6.0)
                            .corner_radius(4.0)
                            .fill(ui.visuals().faint_bg_color)
                            .show(ui, |ui| {
                                ui.set_min_width(180.0);
                                ui.set_max_width(250.0);
                                ui.with_layout(egui::Layout::top_down(egui::Align::LEFT), |ui| {
                                    let max_h = (ui.available_height() - 8.0).max(100.0);
                                    egui::ScrollArea::vertical()
                                        .id_salt(format!("master_fx_scroll_{eff_idx}"))
                                        .max_height(max_h)
                                        .scroll_source(egui::scroll_area::ScrollSource {
                                            drag: egui::scroll_area::DragScroll::Never,
                                            scroll_bar: true,
                                            mouse_wheel: true,
                                        })
                                        .show(ui, |ui| {
                                            ui.horizontal(|ui| {
                                                // Drag handle
                                                render_effect_drag_handle(
                                                    ui,
                                                    EffectDrag::Master(eff_idx),
                                                );
                                                let mut enabled = *eff_enabled;
                                                if ui.checkbox(&mut enabled, "").changed() {
                                                    actions.commands.push(
                                                        EngineCommand::ToggleEffect {
                                                            effect_uuid: eff_uuid.clone(),
                                                        },
                                                    );
                                                }
                                                ui.label(egui::RichText::new(eff_name).strong());
                                            });

                                            super::deck_detail::render_effect_load_state(ui, eff_uuid, data);
                                            if !eff_params.params.is_empty() {
                                                let eff_idx_copy = eff_idx;
                                                let eff_uuid_param = eff_uuid.clone();
                                                let midi_prefix =
                                                    format!("master/effect/{eff_uuid}");
                                                widgets::render_effect_params(
                                                    ui,
                                                    &eff_params.params,
                                                    &data.modulation_sources,
                                                    &|name: &str, val: ParamValue| {
                                                        EngineCommand::SetEffectParam {
                                                            effect_uuid: eff_uuid_param.clone(),
                                                            name: name.to_string(),
                                                            value: val,
                                                        }
                                                    },
                                                    Some(&|name: &str, source_uuid: &str| {
                                                        EngineCommand::AssignModulation {
                                                            target: format!(
                                                                "fx_{eff_uuid_master}:{name}"
                                                            ),
                                                            source_id: source_uuid.to_string(),
                                                            amount: DEFAULT_ASSIGNMENT_AMOUNT,
                                                        }
                                                    }),
                                                    Some(&|name: &str, source_uuid: &str| {
                                                        EngineCommand::ClearModulationSource {
                                                            target: format!(
                                                                "fx_{eff_uuid_master_unassign}:{name}"
                                                            ),
                                                            source_id: source_uuid.to_string(),
                                                        }
                                                    }),
                                                    Some(&|name: &str| {
                                                        EngineCommand::ClearModulation {
                                                            target: format!(
                                                                "fx_{eff_uuid_master_remove}:{name}"
                                                            ),
                                                        }
                                                    }),
                                                    Some(&|name: &str| {
                                                        EngineCommand::AddAutomationLane {
                                                            target: format!(
                                                                "fx_{eff_uuid_master_automate}:{name}"
                                                            ),
                                                            timebase:
                                                                crate::timebase::Timebase::Transport,
                                                        }
                                                    }),
                                                    &mut actions.commands,
                                                    &mut actions.session.gesture_active,
                                                    &format!("master_fx_{eff_idx_copy}"),
                                                    Some(&midi_prefix),
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
                                        });
                                });
                            })
                        });
                        let card_resp = card_scope.inner;
                        effect_context_menu(&card_scope.response, data, actions, eff_uuid, eff_name);
                        {
                            let card_rect = card_resp.response.rect;
                            let btn_size = egui::vec2(16.0, 16.0);
                            let btn_pos = egui::pos2(
                                card_rect.right() - btn_size.x - 4.0,
                                card_rect.top() + 4.0,
                            );
                            let btn_rect = egui::Rect::from_min_size(btn_pos, btn_size);
                            let btn_resp = ui.allocate_rect(btn_rect, egui::Sense::click());
                            let color = if btn_resp.hovered() {
                                ui.visuals().strong_text_color()
                            } else {
                                ui.visuals().text_color()
                            };
                            ui.painter().text(
                                btn_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "x",
                                egui::FontId::proportional(12.0),
                                color,
                            );
                            if btn_resp.clicked() {
                                actions.commands.push(EngineCommand::RemoveEffect {
                                    effect_uuid: eff_uuid.clone(),
                                });
                            }
                        }
                        render_effect_drag_ghost(
                            ui,
                            egui::Id::new(("eff_ghost_master", eff_idx)),
                            EffectDrag::Master(eff_idx),
                            eff_name,
                        );
                        ui.separator();
                    }

                    // Drop zone after last effect (for reordering)
                    if !data.master_effect_info.is_empty() {
                        let num_effects = data.master_effect_info.len();
                        render_effect_drop_zone(ui, "master", num_effects);
                    }

                    // Remaining space: always present drop target
                    let has_fx_drag = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx())
                        .is_some_and(|p| matches!(&*p, LibraryDrag::Effect(_)));
                    let remaining_w = ui.available_width().max(80.0);
                    let remaining_h = ui.available_height().max(40.0);
                    let stroke = if has_fx_drag {
                        egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(100, 200, 255))
                    } else {
                        egui::Stroke::NONE
                    };
                    let fill = if has_fx_drag {
                        egui::Color32::from_rgba_unmultiplied(100, 200, 255, 20)
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    egui::Frame::default()
                        .inner_margin(8.0)
                        .corner_radius(4.0)
                        .fill(fill)
                        .stroke(stroke)
                        .show(ui, |ui| {
                            ui.set_min_size(egui::vec2(remaining_w - 16.0, remaining_h - 16.0));
                            ui.centered_and_justified(|ui| {
                                ui.label(egui::RichText::new("🔮 Drag effects here").weak());
                            });
                        });
                }

                // Master effect chain takes deferred library drops
                let chain_rect = ui.min_rect();
                super::dnd::publish_master_surface_fx(ui.ctx(), chain_rect);
                ui.ctx().memory_mut(|mem| {
                    mem.data.insert_temp(
                        egui::Id::new("eff_dz_count").with("master".to_string()),
                        data.master_effect_info.len() + 1,
                    );
                });
            });
        });
}

/// Render channel effect chain detail in the bottom bar
pub(super) fn render_channel_effect_detail(
    ui: &mut egui::Ui,
    ch_idx: usize,
    data: &UIData,
    actions: &mut UIActions,
) {
    let Some(ch) = data.channels.get(ch_idx) else {
        ui.label(egui::RichText::new("Channel not found").weak());
        return;
    };

    let accent = channel_color(ch_idx);
    ui.horizontal(|ui| {
        ui.heading(egui::RichText::new(format!("🔮 {} Effects", ch.name)).color(accent));

        // Save channel as preset — inline name prompt
        let prompt_id = egui::Id::new("ch_preset_name_prompt");
        let name_id = egui::Id::new("ch_preset_name_input");
        let is_prompting: bool = ui.data(|d| d.get_temp(prompt_id)).unwrap_or(false);

        if is_prompting {
            let cleared_id = egui::Id::new("ch_preset_name_cleared");
            let was_cleared: bool = ui.data(|d| d.get_temp(cleared_id)).unwrap_or(false);
            let mut name: String = ui
                .data(|d| d.get_temp(name_id))
                .unwrap_or_else(|| ch.name.clone());
            let response = ui.text_edit_singleline(&mut name);
            if response.gained_focus() && !was_cleared {
                name.clear();
                ui.data_mut(|d| d.insert_temp(cleared_id, true));
            }
            if ui.small_button("✔ Save").clicked() && !name.is_empty() {
                actions.commands.push(EngineCommand::SaveChannelPreset {
                    channel_uuid: ch.uuid.clone(),
                    name: name.clone(),
                });
                ui.data_mut(|d| d.insert_temp(prompt_id, false));
            }
            if ui.small_button("×").clicked() {
                ui.data_mut(|d| d.insert_temp(prompt_id, false));
            }
            ui.data_mut(|d| d.insert_temp(name_id, name));
        } else if ui.small_button("💾 Save Channel Preset").clicked() {
            ui.data_mut(|d| {
                d.insert_temp(prompt_id, true);
                d.remove_temp::<String>(name_id);
                d.insert_temp(egui::Id::new("ch_preset_name_cleared"), false);
            });
        }
    });

    let reveal = channel_effect_reveal(ui.ctx(), &ch.uuid, &ch.effects);
    if ui.available_width() >= 580.0 {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(super::switcher::TAKE_COLUMN_WIDTH);
                egui::ScrollArea::vertical()
                    .id_salt(("channel_fx_sidebar", &ch.uuid))
                    .show(ui, |ui| render_channel_sidebar(ui, ch_idx, data, actions));
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                render_channel_effect_list(ui, ch_idx, data, actions, reveal.as_deref());
            });
        });
    } else {
        egui::CollapsingHeader::new("Preview and routing")
            .id_salt(("channel_fx_sidebar_compact", &ch.uuid))
            .default_open(false)
            .show(ui, |ui| render_channel_sidebar(ui, ch_idx, data, actions));
        render_channel_effect_list(ui, ch_idx, data, actions, reveal.as_deref());
    }
    render_channel_preview_window(ui.ctx(), ch_idx, data);
}

/// Keep the identities present when a library drop was requested, including
/// when that drop is what first opens this Properties view.
pub(super) fn reveal_added_channel_effect(
    ctx: &egui::Context,
    channel: &str,
    effects: &[super::super::EffectInfo],
) {
    let ids: Vec<String> = effects.iter().map(|e| e.0.clone()).collect();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("channel_fx_seen").with(channel), ids));
}

fn channel_effect_reveal(
    ctx: &egui::Context,
    channel: &str,
    effects: &[super::super::EffectInfo],
) -> Option<String> {
    let id = egui::Id::new("channel_fx_seen").with(channel);
    let previous = ctx.data(|d| d.get_temp::<Vec<String>>(id));
    let added = previous.and_then(|previous| {
        effects
            .iter()
            .find(|e| !previous.contains(&e.0))
            .map(|e| e.0.clone())
    });
    reveal_added_channel_effect(ctx, channel, effects);
    if let Some(uuid) = &added {
        let now = ctx.input(|i| i.time);
        ctx.data_mut(|d| {
            d.insert_temp(
                egui::Id::new("channel_fx_highlight").with(channel),
                (uuid.clone(), now),
            )
        });
    }
    added
}

fn render_channel_sidebar(
    ui: &mut egui::Ui,
    ch_idx: usize,
    data: &UIData,
    actions: &mut UIActions,
) {
    let ch = &data.channels[ch_idx];
    if let Some(&texture) = data.channel_preview_textures.get(&ch_idx) {
        let size = super::utils::preview_size(
            egui::vec2(ui.available_width().min(190.0), 140.0),
            data.render_width,
            data.render_height,
        );
        if ui
            .add(
                egui::Image::new(egui::load::SizedTexture::new(texture, size))
                    .alt_text(format!("{} result", ch.name))
                    .sense(egui::Sense::click()),
            )
            .on_hover_text("Click to enlarge preview")
            .clicked()
        {
            ui.data_mut(|d| {
                d.insert_temp(
                    egui::Id::new("channel_fx_preview_open").with(&ch.uuid),
                    true,
                )
            });
        }
    }
    egui::CollapsingHeader::new("Routing")
        .id_salt(("channel_fx_routing", &ch.uuid))
        .default_open(false)
        .show(ui, |ui| {
            super::deck_detail::render_take_frame(ui, &ch.uuid, data, actions);
        });
}

fn render_channel_preview_window(ctx: &egui::Context, ch_idx: usize, data: &UIData) {
    let ch = &data.channels[ch_idx];
    let id = egui::Id::new("channel_fx_preview_open").with(&ch.uuid);
    let mut open = ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if !open {
        return;
    }
    if let Some(&texture) = data.channel_preview_textures.get(&ch_idx) {
        egui::Window::new(format!("{} preview", ch.name))
            .id(id.with("window"))
            .open(&mut open)
            .default_size(egui::vec2(640.0, 400.0))
            .show(ctx, |ui| {
                let size = super::utils::preview_size(
                    ui.available_size(),
                    data.render_width,
                    data.render_height,
                );
                ui.image(egui::load::SizedTexture::new(texture, size));
            });
    }
    ctx.data_mut(|d| d.insert_temp(id, open));
}

fn render_channel_effect_list(
    ui: &mut egui::Ui,
    ch_idx: usize,
    data: &UIData,
    actions: &mut UIActions,
    reveal: Option<&str>,
) {
    let ch = &data.channels[ch_idx];
    let chain_key = format!("ch_{}", ch.uuid);
    let accent = channel_color(ch_idx);
    let highlight = ui.data(|d| {
        d.get_temp::<(String, f64)>(egui::Id::new("channel_fx_highlight").with(&ch.uuid))
    });
    ui.label(
        egui::RichText::new(format!(
            "Effects ({}) · run top to bottom",
            ch.effects.len()
        ))
        .small()
        .weak(),
    );
    let scroll = egui::ScrollArea::vertical()
        .id_salt(("channel_fx_list", &ch.uuid))
        .auto_shrink([false, false])
        .scroll_source(egui::scroll_area::ScrollSource {
            drag: egui::scroll_area::DragScroll::Never,
            scroll_bar: true,
            mouse_wheel: true,
        })
        .show(ui, |ui| {
            for (index, effect) in ch.effects.iter().enumerate() {
                let (uuid, name, enabled, _) = effect;
                super::utils::render_effect_drop_zone_row(ui, &chain_key, index);
                let highlighted = highlight.as_ref().is_some_and(|(id, time)| id == uuid && ui.input(|i| i.time) - time < 1.5);
                let card = ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                    egui::Frame::default()
                        .inner_margin(6.0)
                        .corner_radius(4.0)
                        .fill(ui.visuals().faint_bg_color)
                        .stroke(if highlighted { egui::Stroke::new(1.5, accent) } else { egui::Stroke::NONE })
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            let id = egui::Id::new(("channel_fx_card", &ch.uuid, uuid));
                            let mut state = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
                            if reveal == Some(uuid.as_str()) { state.set_open(true); }
                            state.show_header(ui, |ui| {
                                render_effect_drag_handle(ui, EffectDrag::Channel(ch.uuid.clone(), index));
                                let mut enabled = *enabled;
                                if ui.checkbox(&mut enabled, egui::RichText::new(name).strong().color(accent)).changed() {
                                    actions.commands.push(EngineCommand::ToggleEffect { effect_uuid: uuid.clone() });
                                }
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    if ui.small_button("Remove").on_hover_text("Remove effect").clicked() {
                                        actions.commands.push(EngineCommand::RemoveEffect { effect_uuid: uuid.clone() });
                                    }
                                });
                            }).body(|ui| {
                                super::deck_detail::render_effect_controls(ui, &format!("ch/{}", ch.uuid), effect, data, actions);
                            });
                        })
                });
                effect_context_menu(&card.response, data, actions, uuid, name);
                if reveal == Some(uuid.as_str()) { card.response.scroll_to_me(Some(egui::Align::Min)); }
                if highlighted { ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)); }
                render_effect_drag_ghost(ui, egui::Id::new(("eff_ghost_ch", &ch.uuid, uuid)), EffectDrag::Channel(ch.uuid.clone(), index), name);
            }
            super::utils::render_effect_drop_zone_row(ui, &chain_key, ch.effects.len());
            let has_drag = egui::DragAndDrop::payload::<LibraryDrag>(ui.ctx()).is_some_and(|p| matches!(&*p, LibraryDrag::Effect(_)));
            egui::Frame::default().inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let text = if ch.effects.is_empty() { "No effects yet — drag an effect here or onto this channel" } else { "Drag effects here to add to the chain" };
                ui.label(if has_drag { egui::RichText::new(text).color(accent) } else { egui::RichText::new(text).weak() });
            });
        });
    super::dnd::publish_channel_surface_fx(
        ui.ctx(),
        &ch.uuid,
        ch_idx,
        scroll.inner_rect.intersect(ui.clip_rect()),
    );
    ui.data_mut(|d| {
        d.insert_temp(
            egui::Id::new("eff_dz_count").with(chain_key),
            ch.effects.len() + 1,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::{EffectInfo, ParamUIInfo, ShaderParamsUI};
    use egui_kittest::kittest::Queryable;

    struct Fixture {
        data: UIData,
        actions: UIActions,
    }

    fn effect(id: &str, name: &str) -> EffectInfo {
        (
            id.into(),
            name.into(),
            true,
            ShaderParamsUI {
                shader_name: name.into(),
                params: vec![ParamUIInfo {
                    name: "amount".into(),
                    label: Some(format!("{name} amount")),
                    value: ParamValue::Float(0.5),
                    min: Some(0.0),
                    max: Some(1.0),
                }],
            },
        )
    }

    fn harness(width: f32, effects: Vec<EffectInfo>) -> egui_kittest::Harness<'static, Fixture> {
        let mut data = UIData::test_fixture();
        data.selected_channel = Some(0);
        data.selected_deck = None;
        data.render_width = 1080;
        data.render_height = 1920;
        data.channels[0].effects = effects;
        data.channel_preview_textures
            .insert(0, egui::TextureId::User(7));
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(width, 600.0))
            .with_step_dt(0.25)
            .build_ui_state(
                |ui, fixture| {
                    let mut actions = UIActions::new();
                    render_channel_effect_detail(ui, 0, &fixture.data, &mut actions);
                    super::super::dnd::handle_effect_dnd(ui, &fixture.data, &mut actions);
                    fixture.actions.commands.extend(actions.commands);
                },
                Fixture {
                    data,
                    actions: UIActions::new(),
                },
            )
    }

    #[test]
    fn properties_channel_effects_stack_and_portrait_preview_stays_small() {
        for width in [440.0, 800.0] {
            let mut h = harness(
                width,
                vec![effect("blur", "Blur"), effect("colour", "Colour")],
            );
            h.run();
            let first = h.get_by_label("Blur").rect();
            let second = h.get_by_label("Colour").rect();
            assert!(second.top() > first.bottom());
            assert!((first.left() - second.left()).abs() < 1.0);
            assert!(h.get_by_label("Blur amount").rect().right() < width);
            if width >= 580.0 {
                let preview = h.get_by_label("Ch A result").rect();
                assert!(preview.height() <= 141.0 && preview.width() <= 191.0);
                assert!(preview.right() < first.left());
            } else {
                assert!(first.left() < 60.0, "controls get the narrow panel's width");
            }
        }
    }

    #[test]
    fn properties_channel_effects_keep_background_loading_and_error_status() {
        use crate::engine::types::EffectLoadState;

        let mut h = harness(800.0, vec![effect("blur-id", "Blur")]);
        h.state_mut()
            .data
            .effect_load_states
            .insert("blur-id".into(), EffectLoadState::Building);
        h.run_steps(3);
        h.get_by_label("Loading — bypassed until ready");
        h.get_by_label("Blur amount");

        h.state_mut().data.effect_load_states.insert(
            "blur-id".into(),
            EffectLoadState::Failed {
                message: "Shader compilation failed".into(),
            },
        );
        h.run();
        h.get_by_label("Failed to load — bypassed");
        h.get_by_label("Remove").click();
        h.run();
        assert!(h
            .state()
            .actions
            .commands
            .iter()
            .any(|command| matches!(command,
                EngineCommand::RemoveEffect { effect_uuid } if effect_uuid == "blur-id"
            )));
    }

    #[test]
    fn properties_channel_effect_controls_keep_their_targets() {
        let mut h = harness(800.0, vec![effect("blur-id", "Blur")]);
        h.run();
        h.get_by_label("Blur").click();
        h.run();
        assert!(h.state().actions.commands.iter().any(|c| matches!(c,
            EngineCommand::ToggleEffect { effect_uuid } if effect_uuid == "blur-id"
        )));
        h.get_by_label("Remove").click();
        h.run();
        assert!(h.state().actions.commands.iter().any(|c| matches!(c,
            EngineCommand::RemoveEffect { effect_uuid } if effect_uuid == "blur-id"
        )));
    }

    #[test]
    fn properties_channel_effect_addition_reveals_the_new_card_once() {
        let effects = (0..20)
            .map(|i| effect(&format!("fx-{i}"), &format!("Effect {i}")))
            .collect();
        let mut h = harness(800.0, effects);
        h.run();
        h.state_mut().data.channels[0]
            .effects
            .push(effect("new", "New effect"));
        h.run_steps(8); // Let the brief highlight and scroll animation finish.
        let added = h.get_by_label("New effect").rect();
        assert!(
            added.top() >= 0.0 && added.bottom() < 600.0,
            "new card revealed: {added:?}"
        );
    }

    #[test]
    fn properties_channel_drop_reveal_handles_first_open_and_ignores_reordering() {
        let ctx = egui::Context::default();
        let before = vec![effect("first", "Blur")];
        reveal_added_channel_effect(&ctx, "channel", &before);
        let after = vec![before[0].clone(), effect("second", "Colour")];
        assert_eq!(
            channel_effect_reveal(&ctx, "channel", &after).as_deref(),
            Some("second")
        );
        assert_eq!(channel_effect_reveal(&ctx, "channel", &after), None);
        assert_eq!(
            channel_effect_reveal(&ctx, "channel", &[after[1].clone(), after[0].clone()]),
            None
        );
    }

    #[test]
    fn properties_channel_effect_rows_keep_drag_reordering() {
        let mut h = harness(
            800.0,
            vec![effect("first", "Blur"), effect("second", "Colour")],
        );
        h.run();
        let channel = h.state().data.channels[0].uuid.clone();
        let key = egui::Id::new("eff_dz_rect").with((format!("ch_{channel}"), 2_usize));
        let target = h
            .ctx
            .data(|d| d.get_temp::<egui::Rect>(key))
            .unwrap()
            .center();
        let start = h.get_by_label("Blur").rect().center();
        h.event(egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        egui::DragAndDrop::set_payload(&h.ctx, EffectDrag::Channel(channel.clone(), 0));
        h.event(egui::Event::PointerMoved(target));
        h.run();
        egui::DragAndDrop::take_payload::<EffectDrag>(&h.ctx);
        h.event(egui::Event::PointerButton {
            pos: target,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        h.run();
        assert!(h.state().actions.commands.iter().any(|c| matches!(c,
            EngineCommand::MoveEffect { target: crate::engine::EffectTarget::Channel(id), from_idx: 0, to_idx: 1 } if id == &channel
        )));
    }
}
