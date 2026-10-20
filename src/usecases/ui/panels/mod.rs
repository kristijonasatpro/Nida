//! UI panel rendering
//!
//! Each sub-module renders a specific panel or UI section.
//! The `render_ui` function orchestrates the top-level layout.

mod add_input;
mod arrangement;
mod audio_equalizer;
mod audio_mixer;
mod bottom_bar;
mod channel_settings;
mod clipboard_menu;
mod cue_bank;
mod deck_adjust;
mod deck_detail;
mod dnd;
mod dock;
mod effects;
mod file_drop;
mod inputs;
mod insertion;
mod layers;
mod led;
mod library;
mod macros;
mod main_desk;
mod mask_edit;
mod midi;
mod mixer;
mod modulation;
mod monitoring;
mod monitors;
mod notifications_overlay;
mod outputs;
mod playlist;
mod popovers;
mod record;
mod rename;
mod right_panel;
mod rundown;
mod screens;
mod sequence;
mod show_menu;
mod stage;
mod stream;
mod switcher;
mod timer;
mod tonemap;
mod transport;
pub(crate) mod utils;

use super::{UIActions, UIData};
use crate::engine::EngineCommand;
use dnd::{
    begin_fx_surface_frame, handle_effect_dnd, handle_library_dnd, handle_sequence_step_dnd,
};
use monitoring::render_monitoring_section;
use notifications_overlay::render_notifications;
use popovers::{
    clock_is_live, followers_hint, handle_midi_learn_popup, record_button, render_clock_popover,
    render_resolution_popover, render_target_fps_popover, render_transport_popover, status_popover,
    transport_color,
};

/// Forget where each channel took drops last frame, before the panels say
/// where they are this frame. A channel view that is no longer drawn — the
/// layout changed, its tab went behind another — must not keep catching
/// drops in the place it used to be.
fn begin_drop_target_frame(ctx: &egui::Context, data: &UIData) {
    ctx.memory_mut(|m| {
        for ch_idx in 0..=data.channels.len() {
            m.data
                .remove::<egui::Rect>(egui::Id::new("ch_drop_rect").with(ch_idx));
            m.data
                .remove::<insertion::Cards>(egui::Id::new("deck_card_rects").with(ch_idx));
        }
        for side in 0..2 {
            m.data
                .remove::<egui::Rect>(egui::Id::new("new_ch_drop_rect").with(side));
        }
        m.data
            .remove::<(usize, egui::Rect)>(channel_settings::drop_window_id());
    });
}

/// The input settings window, if `pos` is over it: its channel and rect.
pub(super) fn drop_window_at(ctx: &egui::Context, pos: egui::Pos2) -> Option<(usize, egui::Rect)> {
    ctx.memory(|m| {
        m.data
            .get_temp::<(usize, egui::Rect)>(channel_settings::drop_window_id())
    })
    .filter(|(_, rect)| rect.contains(pos))
}

/// The channel under `pos` for a drop: the input settings window first, as
/// it is drawn over the channel views, then each channel's drop area.
pub(super) fn drop_channel_at(
    ctx: &egui::Context,
    pos: egui::Pos2,
    channels: usize,
) -> Option<usize> {
    if let Some((ch_idx, _)) = drop_window_at(ctx, pos).filter(|(i, _)| *i < channels) {
        return Some(ch_idx);
    }
    (0..channels).find(|&ch_idx| {
        ctx.memory(|m| {
            m.data
                .get_temp::<egui::Rect>(egui::Id::new("ch_drop_rect").with(ch_idx))
        })
        .is_some_and(|rect| rect.contains(pos))
    })
}

/// Whether a text field is being typed into: the only thing that keeps a
/// take key to itself. `wants_keyboard_input` is true for any focused widget,
/// and a box or button left focused by Tab must not swallow the take — it
/// must not be pressed by it either, which [`consume`] sees to.
fn typing(ctx: &egui::Context) -> bool {
    ctx.memory(egui::Memory::focused)
        .is_some_and(|id| egui::text_edit::TextEditState::load(ctx, id).is_some())
}

/// Take a key press for a take, so it does not also press a focused button.
fn consume(ctx: &egui::Context, combo: &crate::keymap::KeyCombo) {
    let Some(key) = egui::Key::from_name(&combo.key) else {
        return;
    };
    let modifiers = egui::Modifiers {
        alt: combo.alt,
        shift: combo.shift,
        command: combo.command,
        mac_cmd: combo.command && cfg!(target_os = "macos"),
        ctrl: combo.command && !cfg!(target_os = "macos"),
    };
    ctx.input_mut(|i| i.consume_key(modifiers, key));
}

/// Space cuts and Enter fades the selected channel to Main — or whatever keys
/// the keymap binds to those actions — in Main mode, when no text field is
/// being typed into.
fn take_to_main_shortcuts(ui: &egui::Ui, data: &UIData, actions: &mut UIActions) {
    use crate::keymap::{ActionId, KeyTarget};
    if !main_desk::in_main_mode(data)
        || data.keyboard_learn_active
        || data.stage_editor_open
        || typing(ui.ctx())
    {
        return;
    }
    for combo in crate::usecases::ui::keyboard::collect_pressed_keys(ui.ctx()) {
        let fade = match data.keymap_bindings.get(&combo) {
            Some(KeyTarget::Action(ActionId::CutToMain)) => false,
            Some(KeyTarget::Action(ActionId::FadeToMain)) => true,
            _ => continue,
        };
        consume(ui.ctx(), &combo);
        main_desk::take_next(data, actions, fade);
    }
}

/// Give text the glyphs egui's text font lacks — "→", which Nida's labels
/// and notices use ("System Settings → Displays", "ON MAIN A → B"), drew as
/// a box — from egui's own monospace font, Hack, which has them: the last
/// fallback. Once for each context; from the next frame on.
fn complete_the_fonts(ctx: &egui::Context) {
    let done = egui::Id::new("nida_fonts_completed");
    if ctx.data(|d| d.get_temp::<bool>(done)).unwrap_or(false) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(done, true));
    let mut fonts = egui::FontDefinitions::default();
    if let Some(text) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        text.push("Hack".to_owned());
    }
    ctx.set_fonts(fonts);
}

/// What the top bar's other items took last frame: from the STREAM button
/// to the end of the left half, less the live stats and the STREAM and REC
/// buttons (counted as they are now); and the right half.
#[derive(Clone, Copy)]
struct TopBarRoom {
    others: f32,
    right: f32,
}

fn top_bar_room_id() -> egui::Id {
    egui::Id::new("top_bar_room")
}

/// Room kept free between the top bar's halves.
const TOP_BAR_MARGIN: f32 = 12.0;

/// The fullest form of the live stats beside STREAM and REC that fits the
/// top bar — 0 everything, 1 short, 2 the bars alone — and how much of the
/// row each form takes. A narrow window had the stats run under the target
/// frame rate and the resolution. Judged by the room from the STREAM button
/// to the bar's end now, less what the other items took last frame, against
/// how much each form takes now; the first frame has them whole.
fn live_stats_tier(ui: &egui::Ui, data: &UIData) -> (u8, [f32; 3], f32) {
    let widths = [0u8, 1, 2].map(|tier| {
        stream::stats_width(ui, data, tier) + record::stats_width(ui, data, tier)
    });
    // The buttons as they are now: they widen by their clocks as a stream
    // or a recording starts, the very frame the stats appear.
    let buttons = stream::button_width(ui, data) + record::button_width(ui, data);
    let Some(last) = ui.ctx().data(|d| d.get_temp::<TopBarRoom>(top_bar_room_id())) else {
        return (0, widths, buttons);
    };
    let room = ui.available_width() - last.others - last.right - buttons;
    let tier = (0..3u8)
        .find(|&tier| room - widths[usize::from(tier)] >= TOP_BAR_MARGIN)
        .unwrap_or(2);
    (tier, widths, buttons)
}

pub fn render_ui(ui: &mut egui::Ui, data: &UIData) -> UIActions {
    let mut actions = UIActions::new();

    complete_the_fonts(ui.ctx());

    // Disable all egui animations — instant panel/widget transitions
    ui.global_style_mut(|style| {
        style.animation_time = 0.0;
    });

    // Before any panel publishes: drop surfaces are rebuilt every frame.
    begin_fx_surface_frame(ui.ctx());
    begin_drop_target_frame(ui.ctx(), data);

    // Main mode takes (Space / Enter), before any panel draws: the keys are
    // consumed here, so a button or checkbox that still has keyboard focus
    // does not also act on them.
    take_to_main_shortcuts(ui, data, &mut actions);
    rundown::go_shortcut(ui, data, &mut actions);

    // The panel arrangement this frame works on: the Layout menu and the
    // dock change it, and it goes back to the runner at the end.
    let mut screen = data.screen.clone();

    // === TOP BAR: Save button + FPS/BPM status ===
    egui::Panel::top("top_bar").exact_size(28.0).show(ui, |ui| {
        ui.horizontal_centered(|ui| {
            let any_learn = data.midi_learn_active || data.keyboard_learn_active;
            // Undo / Redo / Save — in learn mode: show glow + select target on click.
            // Outside learn mode: normal action on click.
            {
                let undo_enabled = if any_learn { true } else { data.can_undo };
                let undo_resp = ui
                    .add_enabled(undo_enabled, egui::Button::new("↩ Undo"))
                    .on_hover_text("Undo (⌘Z)");
                if any_learn {
                    if data.midi_learn_active {
                        let is_target = data.midi_learn_target.as_deref() == Some("action/undo");
                        if is_target {
                            super::widgets::draw_midi_learn_selected(ui, undo_resp.rect);
                        } else {
                            super::widgets::draw_midi_learn_glow(ui, undo_resp.rect);
                        }
                        if undo_resp.clicked() {
                            actions.session.midi_learn_select = Some("action/undo".to_string());
                        }
                    } else {
                        let is_target = data.keyboard_learn_target.as_deref() == Some("Undo");
                        if is_target {
                            super::widgets::draw_keyboard_learn_selected(ui, undo_resp.rect);
                        } else {
                            super::widgets::draw_keyboard_learn_glow(ui, undo_resp.rect);
                        }
                        if undo_resp.clicked() {
                            actions.session.keyboard_learn_select = Some(
                                crate::keymap::KeyTarget::Action(crate::keymap::ActionId::Undo),
                            );
                        }
                    }
                } else if undo_resp.clicked() {
                    actions.session.undo_requested = true;
                }
            }
            {
                let redo_enabled = if any_learn { true } else { data.can_redo };
                let redo_resp = ui
                    .add_enabled(redo_enabled, egui::Button::new("↪ Redo"))
                    .on_hover_text("Redo (⌘⇧Z)");
                if any_learn {
                    if data.midi_learn_active {
                        let is_target = data.midi_learn_target.as_deref() == Some("action/redo");
                        if is_target {
                            super::widgets::draw_midi_learn_selected(ui, redo_resp.rect);
                        } else {
                            super::widgets::draw_midi_learn_glow(ui, redo_resp.rect);
                        }
                        if redo_resp.clicked() {
                            actions.session.midi_learn_select = Some("action/redo".to_string());
                        }
                    } else {
                        let is_target = data.keyboard_learn_target.as_deref() == Some("Redo");
                        if is_target {
                            super::widgets::draw_keyboard_learn_selected(ui, redo_resp.rect);
                        } else {
                            super::widgets::draw_keyboard_learn_glow(ui, redo_resp.rect);
                        }
                        if redo_resp.clicked() {
                            actions.session.keyboard_learn_select = Some(
                                crate::keymap::KeyTarget::Action(crate::keymap::ActionId::Redo),
                            );
                        }
                    }
                } else if redo_resp.clicked() {
                    actions.session.redo_requested = true;
                }
            }
            {
                let save_resp = ui.button("💾 Save").on_hover_text("Save workspace (⌘S)");
                if any_learn {
                    if data.midi_learn_active {
                        let is_target = data.midi_learn_target.as_deref() == Some("action/save");
                        if is_target {
                            super::widgets::draw_midi_learn_selected(ui, save_resp.rect);
                        } else {
                            super::widgets::draw_midi_learn_glow(ui, save_resp.rect);
                        }
                        if save_resp.clicked() {
                            actions.session.midi_learn_select = Some("action/save".to_string());
                        }
                    } else {
                        let is_target = data.keyboard_learn_target.as_deref() == Some("Save");
                        if is_target {
                            super::widgets::draw_keyboard_learn_selected(ui, save_resp.rect);
                        } else {
                            super::widgets::draw_keyboard_learn_glow(ui, save_resp.rect);
                        }
                        if save_resp.clicked() {
                            actions.session.keyboard_learn_select = Some(
                                crate::keymap::KeyTarget::Action(crate::keymap::ActionId::Save),
                            );
                        }
                    }
                } else if save_resp.clicked() {
                    actions.session.save_requested = true;
                }
            }
            show_menu::render_show_menu(ui, data, &mut actions);

            ui.separator();

            // Performance and Arrangement are two views of one scene, so the
            // switch is a toggle rather than a mode people enter and leave.
            // See /spec/arrangement.md § UI.
            {
                let label = if data.arrangement_mode_open {
                    "▶ Perform"
                } else {
                    "▤ Arrange"
                };
                let hover = if data.arrangement_mode_open {
                    "Back to the mixer. The arrangement keeps driving decks either way."
                } else {
                    "Lay decks out against show time"
                };
                if ui.button(label).on_hover_text(hover).clicked() {
                    actions.session.toggle_arrangement_mode = true;
                }
            }

            dock::render_layout_menu(ui, &mut screen, data, &mut actions);

            ui.separator();
            let stats_start = ui.cursor().left();
            let (stats_tier, stats_widths, stats_buttons) = live_stats_tier(ui, data);
            stream::render_stream_button(ui, data);
            stream::render_stream_stats(ui, data, stats_tier);
            record::render_record_button(ui, data);
            record::render_record_stats(ui, data, stats_tier);

            ui.separator();
            render_monitoring_section(ui, data, &mut actions);
            if data.pending_deck_loads > 0 {
                ui.spinner();
                ui.label(
                    egui::RichText::new(if data.pending_deck_loads == 1 {
                        "Loading 1 deck…".to_string()
                    } else {
                        format!("Loading {} decks…", data.pending_deck_loads)
                    })
                    .small()
                    .color(egui::Color32::from_rgb(180, 180, 255)),
                );
                ui.ctx().request_repaint();
            }

            // Learn mode indicators
            if data.midi_learn_active {
                let text = egui::RichText::new("🎹 MIDI LEARN")
                    .color(egui::Color32::from_rgb(180, 100, 255))
                    .strong();
                if ui
                    .button(text)
                    .on_hover_text("Click to exit MIDI learn mode")
                    .clicked()
                {
                    actions.session.midi_learn_toggle = true;
                }
            }
            if data.keyboard_learn_active {
                let text = egui::RichText::new("⌨ KB LEARN")
                    .color(egui::Color32::from_rgb(255, 165, 0))
                    .strong();
                if ui
                    .button(text)
                    .on_hover_text("Click to exit keyboard learn mode")
                    .clicked()
                {
                    actions.session.keyboard_learn_toggle = true;
                }
            }

            let left_end = ui.cursor().left();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                // BPM from unified clock (MIDI > OSC > Audio > --)
                let bpm_text = if let Some(bpm) = data.clock_bpm {
                    format!("{bpm:.0} BPM")
                } else {
                    "-- BPM".to_string()
                };
                if let Some(dev) = &data.clock_device_name {
                    ui.label(egui::RichText::new(format!("({dev})")).weak().small());
                }
                // Clickable BPM label → opens clock source popover
                let bpm_rich = if clock_is_live(data) {
                    egui::RichText::new(&bpm_text).monospace()
                } else {
                    egui::RichText::new(&bpm_text).monospace().weak()
                };
                let bpm_response = ui
                    .add(egui::Label::new(bpm_rich).sense(egui::Sense::click()))
                    .on_hover_text(format!(
                        "{} — {}. Click to select clock source",
                        if data.clock_active {
                            data.clock_source.as_str()
                        } else {
                            "No clock source"
                        },
                        followers_hint(data.clock_beat_followers, "the beat")
                    ));
                status_popover(&bpm_response, |ui| {
                    render_clock_popover(ui, data, &mut actions);
                });

                ui.separator();

                // Show position sits beside the tempo it will eventually be
                // able to drive: both answer "where are we?", one in bars
                // and one in absolute time.
                let transport_response = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(&data.transport.timecode)
                                .monospace()
                                .color(transport_color(data)),
                        )
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text(format!(
                        "{} — {}. Click for transport controls",
                        data.transport.status_label,
                        followers_hint(data.transport.followers, "the transport")
                    ));
                status_popover(&transport_response, |ui| {
                    render_transport_popover(ui, data, &mut actions);
                });

                record_button(ui, data, &mut actions);

                ui.separator();

                // Resolution selector
                let res_label = if data.render_auto {
                    format!("📐 Auto {}×{}", data.render_width, data.render_height)
                } else {
                    format!("📐 {}×{}", data.render_width, data.render_height)
                };
                let res_response = ui
                    .add(
                        egui::Label::new(egui::RichText::new(&res_label).monospace())
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Click to change render resolution");
                status_popover(&res_response, |ui| {
                    render_resolution_popover(ui, data, &mut actions);
                });

                ui.separator();

                // FPS target selector
                let fps_target_label = if data.target_fps == 0 {
                    "🎯 Uncapped".to_string()
                } else {
                    format!("🎯 {}fps", data.target_fps)
                };
                let fps_target_response = ui
                    .add(
                        egui::Label::new(egui::RichText::new(&fps_target_label).monospace())
                            .sense(egui::Sense::click()),
                    )
                    .on_hover_text("Click to change target FPS");
                status_popover(&fps_target_response, |ui| {
                    render_target_fps_popover(ui, data, &mut actions);
                });
                // What the other items took, for the next frame's live stats
                // to fit (see `live_stats_tier`).
                let room = TopBarRoom {
                    others: left_end
                        - stats_start
                        - stats_widths[usize::from(stats_tier)]
                        - stats_buttons,
                    right: ui.max_rect().right() - ui.min_rect().left(),
                };
                ui.ctx()
                    .data_mut(|d| d.insert_temp(top_bar_room_id(), room));
            });
        });
    });

    // === PANELS: the dock, arranged by the screen layout ===
    let desk = egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(ui.style().visuals.panel_fill))
        .show(ui, |ui| {
            dock::render_dock(ui, &mut screen, data, &mut actions)
        })
        .inner;

    // === INPUT SETTINGS: double-click a channel or a deck card ===
    channel_settings::render_channel_settings(ui.ctx(), desk, data, &mut actions);
    // After the settings window has looked at the selection, which a take
    // changes without moving that window.
    main_desk::swap_preview_after_take(ui.ctx(), data, &mut actions);
    rename::render_rename(ui.ctx(), data, &mut actions);
    rundown::render_windows(ui.ctx(), data, &mut actions);
    led::render_led_window(ui.ctx(), data, &mut actions);
    screens::render_screens_window(ui.ctx(), data, &mut actions);
    add_input::render_add_input(ui.ctx(), data, &mut actions);
    playlist::render_playlist_window(ui.ctx(), data, &mut actions);
    timer::render_timer_window(ui.ctx(), data, &mut actions);

    // === STREAM: the top bar's STREAM button opens it ===
    stream::render_stream_window(ui.ctx(), data, &mut actions);
    record::render_record_window(ui.ctx(), data, &mut actions);

    // === LIBRARY DnD: deferred drop handler ===
    handle_library_dnd(ui, data, &mut actions);

    // === FILES FROM FINDER: highlight while hovering, add on drop ===
    file_drop::handle_file_drop(ui, data, &mut actions);

    // === EFFECT REORDER DnD: deferred drop handler ===
    handle_effect_dnd(ui, data, &mut actions);

    // === SEQUENCE STEP REORDER DnD: deferred drop handler ===
    handle_sequence_step_dnd(ui, data, &mut actions);

    // === NOTIFICATION OVERLAY ===
    render_notifications(ui, &data.notifications, &mut actions);

    // === GLOBAL RIGHT-CLICK: Toggle MIDI Learn Mode ===
    handle_midi_learn_popup(ui, data, &mut actions);

    // === KEYBOARD SHORTCUTS (data-driven via keymap) ===
    {
        use crate::keymap::{ActionId, KeyTarget};
        use crate::usecases::ui::keyboard::collect_pressed_keys;
        let pressed = collect_pressed_keys(ui.ctx());

        if data.keyboard_learn_active {
            // In learn mode: intercept key presses for binding, don't dispatch normally
            if let Some(combo) = pressed.first() {
                actions.session.keyboard_learn_bind = Some(combo.clone());
            }
        } else {
            // Normal dispatch: look up each pressed key in the keymap
            for combo in &pressed {
                if let Some(target) = data.keymap_bindings.get(combo) {
                    match target {
                        KeyTarget::Action(ActionId::Undo) => actions.session.undo_requested = true,
                        KeyTarget::Action(ActionId::Redo) => actions.session.redo_requested = true,
                        KeyTarget::Action(ActionId::Save) => actions.session.save_requested = true,
                        KeyTarget::Action(ActionId::ToggleLibrary) => {
                            if !ui.egui_wants_keyboard_input() {
                                actions.session.toggle_library_panel = true;
                            }
                        }
                        KeyTarget::Action(ActionId::ToggleStageEditor) => {
                            actions.session.toggle_stage_editor = true;
                        }
                        KeyTarget::Action(ActionId::ToggleMidiLearn) => {
                            actions.session.midi_learn_toggle = true;
                        }
                        KeyTarget::Action(ActionId::ToggleKeyboardLearn) => {
                            actions.session.keyboard_learn_toggle = true;
                        }
                        KeyTarget::Action(
                            id @ (ActionId::Copy | ActionId::Paste | ActionId::Duplicate),
                        ) => {
                            // Never into a text field, never over an automation
                            // lane (its own breakpoint clipboard), and never
                            // while an arrangement slice selection is armed (its
                            // slice clipboard wins). See
                            // /spec/arrangement-selection.md § Copy.
                            if !ui.egui_wants_keyboard_input()
                                && !arrangement::a_lane_is_selected(ui.ctx())
                                && !arrangement::selection_active(ui.ctx())
                            {
                                clipboard_menu::shortcut(*id, data, &mut actions);
                            }
                        }
                        KeyTarget::ParamPath(path) => {
                            actions
                                .commands
                                .push(EngineCommand::ToggleParam { path: path.clone() });
                        }
                        // Stage-context actions are handled in stage.rs
                        KeyTarget::Action(_) => {}
                    }
                }
            }
        }
    }

    // L (or its binding) opens and closes the Library panel, wherever the
    // layout keeps it.
    if actions.session.toggle_library_panel {
        crate::usecases::ui::screen_layout::toggle_pane(
            &mut screen.dock,
            crate::usecases::ui::screen_layout::Pane::Library,
        );
    }
    for (pane, open) in std::mem::take(&mut actions.session.panel_requests) {
        if open {
            crate::usecases::ui::screen_layout::add_pane(&mut screen.dock, pane);
        } else {
            crate::usecases::ui::screen_layout::remove_pane(&mut screen.dock, pane);
        }
    }
    actions.session.screen_layout = Some(screen);

    actions
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// Streaming to two destinations, one struggling, and recording.
    fn streaming_and_recording() -> UIData {
        use crate::streaming::{DestinationState, StreamDestination};
        let mut data = UIData::test_fixture();
        data.stream_settings.options.video_kbps = 2500;
        data.stream_settings.options.audio_kbps = 160;
        for (i, name) in ["YouTube", "Cloudflare"].into_iter().enumerate() {
            data.stream_settings.destinations[i] = StreamDestination {
                enabled: true,
                name: name.into(),
                url: "rtmps://live.example.com/live".into(),
                key: "key".into(),
                ..StreamDestination::default()
            };
        }
        let status = &mut data.stream_status;
        status.live = true;
        status.live_secs = 600.0;
        status.destinations[0].state = DestinationState::Live;
        status.destinations[0].kbps = 2900.0;
        status.destinations[1].state = DestinationState::Struggling;
        status.destinations[1].kbps = 1600.0;
        status.destinations[1].dropped_bytes = 2_660_000 / 8 * 30;
        status.destinations[1].dropped_secs = 30.0;
        let record = &mut data.record.status;
        record.recording = true;
        record.secs = 600.0;
        record.bytes = 1_200_000_000;
        record.free_bytes = Some(230_000_000_000);
        data
    }

    /// The whole UI `width` points wide, a few frames on.
    fn top_bar_at(width: f32) -> egui_kittest::Harness<'static, UIActions> {
        let data = std::rc::Rc::new(streaming_and_recording());
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(width, 900.0))
            .build_ui_state(
                move |ui, actions: &mut UIActions| *actions = render_ui(ui, &data),
                UIActions::new(),
            );
        // Live, it never settles; the stats take their form on the second.
        harness.run_steps(4);
        harness
    }

    /// The left half's last figure ends before the right half begins.
    fn clear_of_the_right(harness: &egui_kittest::Harness<'static, UIData>) -> bool {
        let last_left = harness.get_by_label_contains("decks ~").rect();
        let first_right = harness.get_by_label("🎯 60fps").rect();
        last_left.right() <= first_right.left()
    }

    /// The very frame a stream and a recording start — their buttons
    /// widening with their clocks, the stats appearing — nothing runs under
    /// the right half at any width: the buttons are counted as they are
    /// now. (Judged by last frame's narrower buttons, a form near its
    /// cut-off fitted on paper and overlapped on screen; laying the frame
    /// out again instead would have lost a key pressed in it.)
    #[test]
    fn the_frame_a_stream_starts_keeps_clear_of_the_right_hand_controls() {
        let live = streaming_and_recording();
        for width in (1380..=1900).step_by(6) {
            let mut idle = UIData::test_fixture();
            idle.stream_settings = live.stream_settings.clone();
            #[allow(clippy::cast_precision_loss)]
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width as f32, 900.0))
                .build_ui_state(
                    |ui, data: &mut UIData| {
                        let _ = render_ui(ui, data);
                    },
                    idle,
                );
            harness.run_steps(3);
            harness.state_mut().stream_status = live.stream_status.clone();
            harness.state_mut().record = live.record.clone();
            harness.step();
            assert!(clear_of_the_right(&harness), "{width} wide: the first live frame");
        }
    }

    /// The form of the live stats shown: 0 whole, 1 short, 2 the bars.
    fn shown_form(harness: &egui_kittest::Harness<'static, UIActions>) -> u8 {
        if harness.query_by_label("Dropped ~1800 (5.0%)").is_some() {
            0
        } else if harness.query_by_label("5.0%").is_some() {
            1
        } else {
            2
        }
    }

    /// Across window widths, frame after frame, the live stats keep one form
    /// and never run under the right-hand controls: counted with the row's
    /// gaps, a form that fits is not given up the next frame and taken back
    /// the one after. From 1380 points: narrower, the top bar's other items
    /// alone no longer fit beside the right-hand ones, stats or none.
    #[test]
    fn the_live_stats_keep_their_form_and_their_room_frame_after_frame() {
        for width in (1380..=1900).step_by(20) {
            #[allow(clippy::cast_precision_loss)]
            let mut harness = top_bar_at(width as f32);
            let mut forms = Vec::new();
            for _ in 0..4 {
                harness.run_steps(1);
                let last_left = harness.get_by_label_contains("decks ~").rect();
                let first_right = harness.get_by_label("🎯 60fps").rect();
                assert!(
                    last_left.right() <= first_right.left(),
                    "{width} wide: the left ends at {}, the right begins at {}",
                    last_left.right(),
                    first_right.left()
                );
                forms.push(shown_form(&harness));
            }
            assert!(
                forms.windows(2).all(|pair| pair[0] == pair[1]),
                "{width} wide: {forms:?}"
            );
        }
    }

    /// The live stats never run under the right-hand controls: in a window
    /// 1440 points wide, streaming and recording, they shorten until the
    /// left half's last figure ends before the target frame rate begins;
    /// at 1920 there is room for them whole.
    #[test]
    fn the_live_stats_shorten_to_keep_clear_of_the_right_hand_controls() {
        let narrow = top_bar_at(1440.0);
        let last_left = narrow.get_by_label_contains("decks ~").rect();
        let first_right = narrow.get_by_label("🎯 60fps").rect();
        assert!(
            last_left.right() + TOP_BAR_MARGIN / 2.0 <= first_right.left(),
            "left ends at {}, right begins at {}",
            last_left.right(),
            first_right.left()
        );
        assert!(
            narrow.query_by_label("Dropped ~1800 (5.0%)").is_none(),
            "shortened at 1440"
        );

        let wide = top_bar_at(1920.0);
        wide.get_by_label("Dropped ~1800 (5.0%)");
        wide.get_by_label("1.2 GB · 230.0 GB free");
    }

    /// Every character the UI writes has a glyph in the fonts it draws
    /// with. One without drew as a box — the Arrange button's icon, "→" in
    /// "System Settings → Privacy & Security", "✓" — in labels nobody had
    /// looked at closely. Scans the string literals of `src/usecases/ui`.
    #[test]
    fn every_character_the_ui_writes_can_be_drawn() {
        let ctx = egui::Context::default();
        // The fonts set in one pass are used from the next.
        for _ in 0..2 {
            let mut pass =
                ctx.run_ui(egui::RawInput::default(), |ui| complete_the_fonts(ui.ctx()));
            pass.textures_delta.clear();
        }
        let font = egui::FontId::proportional(12.0);
        let literal = |line: &str| -> Vec<String> {
            line.split('"').skip(1).step_by(2).map(str::to_string).collect()
        };
        let mut boxes = std::collections::BTreeSet::new();
        let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/usecases/ui")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).expect("the UI sources") {
                let path = entry.expect("an entry").path();
                if path.is_dir() {
                    dirs.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).expect("a source file");
                for (n, line) in text.lines().enumerate() {
                    if line.trim_start().starts_with("//") {
                        continue;
                    }
                    for piece in literal(line) {
                        for c in piece.chars() {
                            // Emoji joiners, presentation selectors and
                            // combining marks have no width of their own.
                            let marks = matches!(c, '\u{0300}'..='\u{036f}' | '\u{20d0}'..='\u{20ff}');
                            if c.is_ascii() || marks || c == '\u{fe0f}' || c == '\u{200d}' {
                                continue;
                            }
                            // Not `has_glyph`: it calls a glyph missing when it
                            // shares a face with the replacement box (▶, ⚠ do).
                            // A glyph in no face has no width.
                            if ctx.fonts_mut(|f| f.glyph_width(&font, c)) <= 0.0 {
                                let file = path.file_name().unwrap_or_default().to_string_lossy();
                                boxes.insert(format!("{c:?} {file}:{}", n + 1));
                            }
                        }
                    }
                }
            }
        }
        assert!(boxes.is_empty(), "drawn as a box: {boxes:#?}");
    }

    /// Toasts stop part way down the window, each at most six lines (the
    /// rest on its hover), and a click on a toast's body puts it away
    /// instead of reaching the control under it.
    #[test]
    fn toasts_stop_part_way_down_and_a_click_puts_one_away() {
        let mut data = UIData::test_fixture();
        data.notifications = (1..=5)
            .map(|id| crate::usecases::ui::NotificationUI {
                id,
                level: crate::notifications::NotificationLevel::Warning,
                message: "a long notice ".repeat(60),
                progress: 0.1,
            })
            .collect();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_ui_state(
                move |ui, dismissed: &mut Vec<u64>| {
                    let actions = render_ui(ui, &data);
                    dismissed.extend(actions.session.notifications_to_dismiss.iter().copied());
                },
                Vec::new(),
            );
        // Toasts repaint for their countdown: step, don't settle.
        harness.run_steps(3);
        let toast = |id: u64| {
            harness
                .ctx
                .memory(|m| m.area_rect(egui::Id::new(("notification", id))))
        };
        let first = toast(1).expect("the first toast");
        assert!(first.height() < 140.0, "six lines at most: {first:?}");
        assert!(toast(2).is_some(), "a second fits");
        assert!(toast(3).is_none(), "a third would run past 40 % of the window");

        let at = first.center();
        harness.event(egui::Event::PointerMoved(at));
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::default(),
            });
            harness.run_steps(1);
        }
        assert_eq!(harness.state().as_slice(), [1], "the toast clicked, put away");
    }

    /// A toast arriving between a press on another and its release — taking
    /// the place pressed — is not the one put away.
    #[test]
    fn a_toast_arriving_mid_click_is_not_put_away() {
        let notice = |id: u64| crate::usecases::ui::NotificationUI {
            id,
            level: crate::notifications::NotificationLevel::Info,
            message: format!("notice {id}"),
            progress: 0.1,
        };
        let data = std::rc::Rc::new(std::cell::RefCell::new(UIData::test_fixture()));
        data.borrow_mut().notifications = vec![notice(1)];
        let shown = std::rc::Rc::clone(&data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 720.0))
            .build_ui_state(
                move |ui, dismissed: &mut Vec<u64>| {
                    let actions = render_ui(ui, &shown.borrow());
                    dismissed.extend(actions.session.notifications_to_dismiss.iter().copied());
                },
                Vec::new(),
            );
        harness.run_steps(3);
        let at = harness
            .ctx
            .memory(|m| m.area_rect(egui::Id::new(("notification", 1_u64))))
            .expect("the toast")
            .center();
        harness.event(egui::Event::PointerMoved(at));
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        });
        harness.run_steps(1);
        // A new notice comes in on top, where the press was.
        data.borrow_mut().notifications.insert(0, notice(2));
        harness.run_steps(1);
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        harness.run_steps(1);
        assert!(
            !harness.state().contains(&2),
            "the notice that arrived was put away: {:?}",
            harness.state()
        );
    }

    /// Smoke test: `render_ui` doesn't panic with the test fixture.
    #[test]
    fn render_ui_smoke_default_fixture() {
        let data = UIData::test_fixture();
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        // Running the harness processes a frame — if render_ui panics, this test fails.
        let _ = harness;
    }

    /// Every preview and stage canvas now sizes itself from the render
    /// resolution, so a portrait or square project drives layout arithmetic
    /// that a 16:9 fixture never reaches.
    #[test]
    fn render_ui_smoke_non_landscape_resolutions() {
        for (w, h) in [(1080u32, 1920u32), (1080, 1080), (1080, 1350), (0, 0)] {
            let mut data = UIData::test_fixture();
            data.render_width = w;
            data.render_height = h;
            data.stage_editor_open = true;
            let harness = egui_kittest::Harness::new_ui(|ui| {
                let _ = render_ui(ui, &data);
            });
            let _ = harness;
        }
    }

    /// Smoke test: `render_ui` with empty channels doesn't panic.
    #[test]
    fn render_ui_smoke_empty_channels() {
        let mut data = UIData::test_fixture();
        data.channels.clear();
        data.channel_count = 0;
        data.selected_deck = None;
        data.selected_channel = None;
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        let _ = harness;
    }

    /// Smoke test: `render_ui` with library panel closed doesn't panic.
    #[test]
    fn render_ui_smoke_library_closed() {
        let mut data = UIData::test_fixture();
        data.library_panel_open = false;
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        let _ = harness;
    }

    /// Smoke test: `render_ui` with stage editor open doesn't panic.
    #[test]
    fn render_ui_smoke_stage_editor_open() {
        let mut data = UIData::test_fixture();
        data.stage_editor_open = true;
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        let _ = harness;
    }

    /// Smoke test: `render_ui` with master selected doesn't panic.
    #[test]
    fn render_ui_smoke_master_selected() {
        let mut data = UIData::test_fixture();
        data.selected_deck = None;
        data.selected_master = true;
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        let _ = harness;
    }

    /// Smoke test: `render_ui` with channel selected doesn't panic.
    #[test]
    fn render_ui_smoke_channel_selected() {
        let mut data = UIData::test_fixture();
        data.selected_deck = None;
        data.selected_channel = Some(0);
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        let _ = harness;
    }

    /// A channel view that is no longer on screen stops catching drops: after
    /// switching to a layout without one, no channel has a drop area left.
    #[test]
    fn a_channel_view_that_goes_away_stops_catching_drops() {
        use crate::usecases::ui::screen_layout::{Pane, ScreenLayout};
        let data = std::rc::Rc::new(std::cell::RefCell::new(UIData::test_fixture()));
        let shared = data.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1600.0, 900.0))
            .build_ui(move |ui| {
                let _ = render_ui(ui, &shared.borrow());
            });
        harness.run();
        let drop_area = |harness: &egui_kittest::Harness<'_>| -> Option<egui::Rect> {
            harness
                .ctx
                .memory(|m| m.data.get_temp(egui::Id::new("ch_drop_rect").with(0_usize)))
        };
        assert!(
            drop_area(&harness).is_some(),
            "the Channels panel takes drops"
        );
        data.borrow_mut().screen = ScreenLayout {
            name: "Only the library".into(),
            dock: egui_dock::DockState::new(vec![Pane::Library]),
            locked: false,
            naming: crate::usecases::ui::screen_layout::NAMING,
        };
        harness.run();
        assert!(drop_area(&harness).is_none(), "{:?}", drop_area(&harness));
    }

    /// The input settings window is drawn over the channel views, so a drop
    /// on it goes to its channel even where another channel lies beneath.
    #[test]
    fn a_drop_on_the_input_settings_window_goes_to_its_channel() {
        let ctx = egui::Context::default();
        let whole = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let window = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(300.0, 200.0));
        ctx.memory_mut(|m| {
            m.data
                .insert_temp(egui::Id::new("ch_drop_rect").with(0_usize), whole);
            m.data
                .insert_temp(channel_settings::drop_window_id(), (1_usize, window));
        });
        assert_eq!(drop_channel_at(&ctx, egui::pos2(200.0, 200.0), 2), Some(1));
        assert_eq!(drop_channel_at(&ctx, egui::pos2(600.0, 500.0), 2), Some(0));
        assert_eq!(
            drop_channel_at(&ctx, egui::pos2(200.0, 200.0), 1),
            Some(0),
            "a window for a channel since removed takes nothing"
        );
    }

    /// Smoke test: `render_ui` with MIDI learn active doesn't panic.
    #[test]
    fn render_ui_smoke_midi_learn() {
        let mut data = UIData::test_fixture();
        data.midi_learn_active = true;
        data.midi_learn_target = Some("crossfader".to_string());
        let harness = egui_kittest::Harness::new_ui(|ui| {
            let _ = render_ui(ui, &data);
        });
        let _ = harness;
    }
}
