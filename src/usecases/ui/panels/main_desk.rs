//! The central area in Main mode, laid out as a vMix operator works: a
//! toolbar with **+ Channel** and the Main controls, then the channels in one
//! row, wide, with large pictures.
//!
//! The channel selected (or holding the selected deck) is what goes to Main
//! next — the same picture the bottom bar previews — with **Cut** (Space) or
//! **Fade** (Enter). An overlay channel is switched on and off instead.

use super::super::{ChannelUIInfo, UIActions, UIData};
use crate::engine::EngineCommand;
use crate::mixer::{MainSource, MixMode};

pub(super) const ON_MAIN_RED: egui::Color32 = egui::Color32::from_rgb(200, 30, 30);
pub(super) const NEXT_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 150, 60);

/// Channel columns are never narrower than this; more channels than fit
/// scroll sideways.
const MIN_CHANNEL_WIDTH: f32 = 240.0;
/// Nor wider: past this a picture only gets taller, pushing decks out of view.
const MAX_CHANNEL_WIDTH: f32 = 340.0;
const CHANNEL_GAP: f32 = 8.0;

/// Fade times offered, in seconds.
const FADE_TIMES: [f32; 7] = [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 5.0];

/// Whether the master is Main with overlays.
pub(super) fn in_main_mode(data: &UIData) -> bool {
    data.main.mode == MixMode::Program
}

/// The channel the next take sends: the one selected, or the one holding the
/// selected deck.
pub(super) fn next_channel(data: &UIData) -> Option<&ChannelUIInfo> {
    let idx = data
        .selected_channel
        .or(data.selected_deck.map(|(ch, _)| ch))?;
    data.channels.get(idx)
}

/// Whether a channel is on Main, or leaving it in a fade.
pub(super) fn channel_on_main(data: &UIData, ch: &ChannelUIInfo) -> bool {
    let source = MainSource::Channel(ch.uuid.clone());
    data.main.program == source || data.main.fade.as_ref().is_some_and(|f| f.from == source)
}

/// Whether a deck is on Main on its own.
pub(super) fn deck_on_main(data: &UIData, deck_uuid: &str) -> bool {
    let source = MainSource::Deck(deck_uuid.to_string());
    data.main.program == source || data.main.fade.as_ref().is_some_and(|f| f.from == source)
}

/// Whether an overlay channel is switched on.
pub(super) fn overlay_on(data: &UIData, ch: &ChannelUIInfo) -> bool {
    data.main.overlays_on.contains(&ch.uuid)
}

/// What a Main source is called on screen.
pub(super) fn source_name(data: &UIData, source: &MainSource) -> String {
    match source {
        MainSource::Black => "Black".into(),
        MainSource::Channel(uuid) => data
            .channels
            .iter()
            .find(|c| &c.uuid == uuid)
            .map_or_else(|| "missing channel".into(), |c| c.name.clone()),
        MainSource::Deck(uuid) => data
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| &d.uuid == uuid)
            .map_or_else(|| "missing input".into(), |d| d.name.clone()),
    }
}

/// Send a channel to Main — or, for an overlay, switch it on or off.
pub(super) fn take_channel(ch: &ChannelUIInfo, data: &UIData, actions: &mut UIActions, fade: bool) {
    if ch.overlay {
        actions.commands.push(EngineCommand::SetOverlayOn {
            channel_uuid: ch.uuid.clone(),
            on: !overlay_on(data, ch),
            fade,
        });
    } else {
        actions.commands.push(EngineCommand::TakeToMain {
            source: MainSource::Channel(ch.uuid.clone()),
            fade,
        });
    }
}

/// Preview and Program swap after a take, as in vMix: what was on Main
/// becomes Preview — the selection — so the next take puts it back. Main is
/// watched rather than the buttons, so a take from anywhere swaps: a tile, a
/// menu, a key, the API. An open Input Settings window stays where it is.
pub(super) fn swap_preview_after_take(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let id = egui::Id::new("main_program_seen");
    let now = data.main.program.clone();
    let seen = ctx.memory(|m| m.data.get_temp::<MainSource>(id));
    ctx.memory_mut(|m| m.data.insert_temp(id, now.clone()));
    if !in_main_mode(data) {
        return;
    }
    let Some(before) = seen.filter(|before| *before != now) else {
        return;
    };
    let channel = match &before {
        MainSource::Black => None,
        MainSource::Channel(uuid) => data.channels.iter().find(|c| &c.uuid == uuid),
        MainSource::Deck(uuid) => data
            .channels
            .iter()
            .find(|c| c.decks.iter().any(|d| &d.uuid == uuid)),
    };
    let Some(ch) = channel else {
        return;
    };
    actions.session.select_channel = Some(ch.ch_idx);
    super::channel_settings::stay_on_selection_change(ctx, &ch.uuid);
}

/// Take whatever is next (Space = cut, Enter = fade). Nothing selected,
/// nothing happens.
pub(super) fn take_next(data: &UIData, actions: &mut UIActions, fade: bool) {
    if let Some(ch) = next_channel(data) {
        take_channel(ch, data, actions, fade);
    }
}

/// "Cut to Main" / "Fade to Main" for a right-click menu, or overlay on/off.
pub(super) fn main_menu_items(
    ui: &mut egui::Ui,
    source: &MainSource,
    overlay: Option<&ChannelUIInfo>,
    data: &UIData,
    actions: &mut UIActions,
) {
    if let Some(ch) = overlay {
        let on = overlay_on(data, ch);
        let verb = if on { "off" } else { "on" };
        for (fade, label) in [
            (false, format!("Overlay {verb} (cut)")),
            (true, format!("Overlay {verb} (fade)")),
        ] {
            if ui.button(label).clicked() {
                take_channel(ch, data, actions, fade);
                ui.close();
            }
        }
        return;
    }
    let on_air = data.main.program == *source;
    for (fade, verb) in [(false, "Cut to Main"), (true, "Fade to Main")] {
        let label = if on_air {
            format!("{verb} (on Main)")
        } else {
            verb.to_string()
        };
        if ui.add_enabled(!on_air, egui::Button::new(label)).clicked() {
            actions.commands.push(EngineCommand::TakeToMain {
                source: source.clone(),
                fade,
            });
            ui.close();
        }
    }
}

/// The central area in Main mode.
pub(super) fn render_main_desk(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    render_toolbar(ui, data, actions);
    ui.add_space(4.0);
    let n = data.channels.len().max(1);
    #[allow(clippy::cast_precision_loss)]
    let width = ((ui.available_width() - CHANNEL_GAP * (n as f32 - 1.0)) / n as f32)
        .clamp(MIN_CHANNEL_WIDTH, MAX_CHANNEL_WIDTH);
    let height = ui.available_height();
    egui::ScrollArea::horizontal()
        .id_salt("main_desk_channels")
        .show(ui, |ui| {
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = CHANNEL_GAP;
                for ch in &data.channels {
                    ui.vertical(|ui| {
                        ui.set_width(width);
                        egui::ScrollArea::vertical()
                            .id_salt(("desk_col_scroll", ch.ch_idx))
                            .max_height(height)
                            .auto_shrink([false, true])
                            .show(ui, |ui| {
                                super::mixer::render_channel_column(ui, ch, data, actions);
                            });
                    });
                }
                if data.channels.is_empty() {
                    ui.label(
                        egui::RichText::new("No channels — click + Channel, then drop files in it")
                            .weak(),
                    );
                }
            });
        });
}

pub(super) fn render_toolbar(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        let big = |text: &str| egui::RichText::new(text).size(15.0).strong();
        if ui
            .add(egui::Button::new(big("+ Channel")).min_size(egui::vec2(0.0, 30.0)))
            .on_hover_text("Add a channel, then drop files or Library items into it")
            .clicked()
        {
            actions.commands.push(EngineCommand::AddChannel);
        }
        ui.separator();

        // What is on Main.
        let program = source_name(data, &data.main.program);
        let program_text = match &data.main.fade {
            Some(fade) => format!(" ON MAIN  {} → {program} ", source_name(data, &fade.from)),
            None => format!(" ON MAIN  {program} "),
        };
        ui.label(
            egui::RichText::new(program_text)
                .size(15.0)
                .strong()
                .color(egui::Color32::WHITE)
                .background_color(ON_MAIN_RED),
        );
        if data.main.fade.is_some() {
            ui.ctx().request_repaint();
        }

        // What goes next, and the takes.
        let next = next_channel(data);
        let next_text = next.map_or_else(
            || " NEXT  select a channel ".to_string(),
            |ch| {
                if ch.overlay {
                    let verb = if overlay_on(data, ch) { "off" } else { "on" };
                    format!(" NEXT  {} overlay {verb} ", ch.name)
                } else {
                    format!(" NEXT  {} ", ch.name)
                }
            },
        );
        ui.label(
            egui::RichText::new(next_text)
                .size(15.0)
                .strong()
                .color(egui::Color32::WHITE)
                .background_color(NEXT_GREEN),
        );
        let can_take = next.is_some_and(|ch| ch.overlay || !channel_on_main(data, ch));
        super::record::with_record_warning(ui, data, None, |ui| {
            if ui
                .add_enabled(
                    can_take,
                    egui::Button::new(big("CUT (Space)")).min_size(egui::vec2(70.0, 30.0)),
                )
                .on_hover_text("Put the next channel on Main now (Space)")
                .clicked()
            {
                take_next(data, actions, false);
            }
            let transition = data.main.transition;
            if ui
                .add_enabled(
                    can_take && data.main.fade.is_none(),
                    egui::Button::new(big(&format!("{} (Enter)", transition.name().to_uppercase())))
                        .min_size(egui::vec2(70.0, 30.0)),
                )
                .on_hover_text(format!(
                    "Take the next channel to Main — {} — over {} (Enter)",
                    transition.name().to_lowercase(),
                    seconds(data.main.fade_secs)
                ))
                .clicked()
            {
                take_next(data, actions, true);
            }
            transition_picker(ui, "main_transition", data, actions);
        });
        egui::ComboBox::from_id_salt("main_fade_time")
            .selected_text(seconds(data.main.fade_secs))
            .width(64.0)
            .show_ui(ui, |ui| {
                for secs in FADE_TIMES {
                    let chosen = (data.main.fade_secs - secs).abs() < 1e-3;
                    if ui.selectable_label(chosen, seconds(secs)).clicked() && !chosen {
                        actions
                            .commands
                            .push(EngineCommand::SetMainFadeTime { secs });
                    }
                }
            })
            .response
            .on_hover_text("How long a fade takes");

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Layered mix…")
                .on_hover_text(
                    "Switch to the layered VJ mixer: every channel at its fader, a crossfader",
                )
                .clicked()
            {
                actions.commands.push(EngineCommand::SetMixMode {
                    mode: MixMode::Layered,
                });
            }
            if let Some(fade) = &data.main.fade {
                ui.add(egui::ProgressBar::new(fade.progress.clamp(0.0, 1.0)).desired_width(80.0));
            }
        });
    });
}

/// What each transition does, said as a picker offers it.
pub(super) fn transition_hint(transition: crate::mixer::MainTransition) -> &'static str {
    use crate::mixer::MainTransition as T;
    match transition {
        T::Fade => "The next picture dissolves in over this one",
        T::Merge => {
            "Layers both have — the same camera, clip or input — move and resize into \
             their new places; the rest fade"
        }
        T::Wipe => "The next picture wipes across from the left",
        T::Slide => "The next picture slides in, pushing this one out",
        T::Zoom => "This picture zooms in and away, the next behind it",
        T::Iris => "A circle opens from the middle onto the next picture",
    }
}

/// The transition a take that is not a cut uses: Fade, Merge, Wipe…
pub(super) fn transition_picker(
    ui: &mut egui::Ui,
    id: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    let current = data.main.transition;
    egui::ComboBox::from_id_salt(id)
        .selected_text(current.name())
        .width(72.0)
        .show_ui(ui, |ui| {
            for transition in crate::mixer::MainTransition::ALL {
                let chosen = transition == current;
                if ui
                    .selectable_label(chosen, transition.name())
                    .on_hover_text(transition_hint(transition))
                    .clicked()
                    && !chosen
                {
                    actions
                        .commands
                        .push(EngineCommand::SetMainTransition { transition });
                }
            }
        })
        .response
        .on_hover_text("How the take button changes Main");
}

fn seconds(secs: f32) -> String {
    if (secs - secs.round()).abs() < 1e-3 {
        format!("{secs:.0} s")
    } else {
        format!("{secs} s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    /// The fixture in Main mode, Ch A's first deck selected.
    fn main_mode() -> UIData {
        let mut data = UIData::test_fixture();
        data.main.mode = MixMode::Program;
        data.main.fade_secs = 1.0;
        data.selected_deck = Some((0, 0));
        data.keymap_bindings = crate::keymap::KeymapStore::defaults();
        data
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, UIActions> {
        let data = std::rc::Rc::new(data);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                move |ui, actions: &mut UIActions| {
                    let frame = super::super::render_ui(ui, &data);
                    actions.commands.extend(frame.commands);
                },
                UIActions::new(),
            );
        h.run();
        h.state_mut().commands.clear();
        h
    }

    fn takes(h: &egui_kittest::Harness<'static, UIActions>) -> Vec<String> {
        h.state()
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::TakeToMain { source, fade } => {
                    Some(format!("take {source:?} fade={fade}"))
                }
                EngineCommand::SetOverlayOn {
                    channel_uuid,
                    on,
                    fade,
                } => Some(format!("overlay {channel_uuid} on={on} fade={fade}")),
                _ => None,
            })
            .collect()
    }

    /// The take button is named after the transition it takes with.
    #[test]
    fn the_take_button_says_its_transition() {
        let mut data = main_mode();
        data.main.transition = crate::mixer::MainTransition::Merge;
        let h = harness(data);
        h.get_by_label("MERGE (Enter)");
        assert!(h.query_by_label("FADE (Enter)").is_none());
    }

    /// Picking a transition in its menu sends it.
    #[test]
    fn picking_a_transition_sends_it() {
        let data = main_mode();
        let mut actions = UIActions::new();
        {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(300.0, 300.0))
                .build_ui(|ui| transition_picker(ui, "test_transition", &data, &mut actions));
            h.get_by_value("Fade").click();
            h.run();
            h.get_by_label("Merge").click();
            h.run();
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetMainTransition {
                    transition: crate::mixer::MainTransition::Merge
                }
            )),
            "{:?}",
            actions.commands
        );
    }

    #[test]
    fn the_desk_has_an_add_button_and_no_drop_zones() {
        let h = harness(main_mode());
        h.get_by_label("+ Channel");
        assert!(h
            .query_by_label_contains("Drop here to add channel")
            .is_none());
        assert_eq!(
            h.get_all_by_label("Overlay").count(),
            2,
            "one switch per channel"
        );
    }

    #[test]
    fn space_cuts_and_enter_fades_the_selected_channel_to_main() {
        let mut h = harness(main_mode());
        h.key_press(egui::Key::Space);
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        assert_eq!(
            takes(&h),
            [
                r#"take Channel("ca000001") fade=false"#,
                r#"take Channel("ca000001") fade=true"#
            ],
            "the channel holding the selected deck: what the bottom bar previews"
        );
    }

    #[test]
    fn an_overlay_channel_is_switched_on_instead_of_taken() {
        let mut data = main_mode();
        data.channels[0].overlay = true;
        let mut h = harness(data);
        h.key_press(egui::Key::Space);
        h.run();
        assert_eq!(takes(&h), ["overlay ca000001 on=true fade=false"]);
    }

    #[test]
    fn space_takes_even_with_a_box_in_keyboard_focus() {
        let mut h = harness(main_mode());
        // Tabbing through the desk leaves a box focused; Space would press it.
        h.get_all_by_label("Overlay").next().unwrap().focus();
        h.run();
        h.key_press(egui::Key::Space);
        h.run();
        let overlay_edits = h
            .state()
            .commands
            .iter()
            .filter(|c| matches!(c, EngineCommand::SetChannelOverlay { .. }))
            .count();
        assert_eq!(overlay_edits, 0, "Space did not press the focused box");
        assert_eq!(takes(&h).len(), 1, "it took the next channel");
    }

    #[test]
    fn a_space_typed_into_a_text_field_takes_nothing() {
        let data = std::rc::Rc::new(main_mode());
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                move |ui, state: &mut (UIActions, String)| {
                    let frame = super::super::render_ui(ui, &data);
                    state.0.commands.extend(frame.commands);
                    egui::Area::new(egui::Id::new("typing_test"))
                        .fixed_pos(egui::pos2(900.0, 500.0))
                        .order(egui::Order::Foreground)
                        .show(ui.ctx(), |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.1).hint_text("typing here"),
                            );
                        });
                },
                (UIActions::new(), String::new()),
            );
        h.run();
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .last()
            .expect("the test's text field, drawn last")
            .click();
        h.run();
        h.key_press(egui::Key::Space);
        h.run();
        let took = h
            .state()
            .0
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::TakeToMain { .. }));
        assert!(!took, "a space in a name is a space");
    }

    /// A key learned for a parameter — a layer's opacity, a mute — acts on
    /// its own, but typed into a text field it is a letter, nothing more.
    #[test]
    fn a_learned_key_typed_into_a_text_field_changes_nothing() {
        use crate::keymap::{KeyCombo, KeyTarget};
        let mut data = main_mode();
        let m = KeyCombo {
            key: "M".into(),
            command: false,
            shift: false,
            alt: false,
        };
        data.keymap_bindings
            .insert(m, KeyTarget::ParamPath("ch/0/opacity".into()));
        let data = std::rc::Rc::new(data);
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                move |ui, state: &mut (UIActions, String)| {
                    let frame = super::super::render_ui(ui, &data);
                    state.0.commands.extend(frame.commands);
                    egui::Area::new(egui::Id::new("typing_test"))
                        .fixed_pos(egui::pos2(900.0, 500.0))
                        .order(egui::Order::Foreground)
                        .show(ui.ctx(), |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut state.1).hint_text("typing here"),
                            );
                        });
                },
                (UIActions::new(), String::new()),
            );
        let toggles = |h: &egui_kittest::Harness<'_, (UIActions, String)>| {
            let commands = &h.state().0.commands;
            let toggle = |c: &&EngineCommand| matches!(c, EngineCommand::ToggleParam { .. });
            commands.iter().filter(toggle).count()
        };
        h.run();
        h.key_press(egui::Key::M);
        h.run();
        assert_eq!(toggles(&h), 1, "the learned key acts");
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .last()
            .expect("the test's text field, drawn last")
            .click();
        h.run();
        h.key_press(egui::Key::M);
        h.run();
        assert_eq!(toggles(&h), 1, "an M in a name is an M");
    }

    /// Enter finishing a name is the name's, even when Enter is learned for a
    /// parameter: the field lets go of the keyboard as the Enter arrives,
    /// while the panels draw — before the keymap is looked at.
    #[test]
    fn the_enter_that_finishes_a_name_changes_nothing() {
        use crate::keymap::{KeyCombo, KeyTarget};
        let mut data = main_mode();
        let enter = KeyCombo {
            key: "Enter".into(),
            command: false,
            shift: false,
            alt: false,
        };
        data.keymap_bindings
            .insert(enter, KeyTarget::ParamPath("ch/0/opacity".into()));
        let ch = data.channels[0].clone();
        let name = ch.name.clone();
        let data = std::rc::Rc::new(data);
        let mut opened = false;
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1920.0, 1080.0))
            .build_ui_state(
                move |ui, actions: &mut UIActions| {
                    if !opened {
                        super::super::rename::open(ui.ctx(), &ch);
                        opened = true;
                    }
                    let frame = super::super::render_ui(ui, &data);
                    actions.commands.extend(frame.commands);
                },
                UIActions::new(),
            );
        h.run();
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .find(|field| field.value().as_deref() == Some(name.as_str()))
            .expect("the name being changed")
            .type_text("Slides");
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
        let commands = &h.state().commands;
        let renamed = |c: &EngineCommand| matches!(c, EngineCommand::RenameChannel { .. });
        assert!(commands.iter().any(renamed), "the Enter reached the name");
        let toggled = |c: &EngineCommand| matches!(c, EngineCommand::ToggleParam { .. });
        assert!(!commands.iter().any(toggled), "and was the name's alone");
    }

    #[test]
    fn the_keys_do_nothing_in_the_layered_mix() {
        let mut data = main_mode();
        data.main.mode = MixMode::Layered;
        let mut h = harness(data);
        h.key_press(egui::Key::Space);
        h.run();
        assert!(takes(&h).is_empty());
    }

    #[test]
    fn the_cut_button_takes_the_next_channel() {
        let mut h = harness(main_mode());
        h.get_by_label("CUT (Space)").click();
        h.run();
        assert_eq!(takes(&h), [r#"take Channel("ca000001") fade=false"#]);
    }

    #[test]
    fn preview_and_program_swap_after_a_take() {
        let ctx = egui::Context::default();
        let mut data = main_mode();
        let (a, b) = (data.channels[0].clone(), data.channels[1].clone());
        data.main.program = MainSource::Channel(a.uuid.clone());
        let mut actions = UIActions::new();
        swap_preview_after_take(&ctx, &data, &mut actions);
        assert_eq!(actions.session.select_channel, None, "nothing taken yet");

        data.main.program = MainSource::Channel(b.uuid.clone());
        let mut actions = UIActions::new();
        swap_preview_after_take(&ctx, &data, &mut actions);
        assert_eq!(
            actions.session.select_channel,
            Some(a.ch_idx),
            "what was on Main is on Preview"
        );

        let mut actions = UIActions::new();
        swap_preview_after_take(&ctx, &data, &mut actions);
        assert_eq!(actions.session.select_channel, None, "once per take");
    }

    #[test]
    fn a_take_from_black_or_in_the_layered_mix_leaves_preview_alone() {
        let ctx = egui::Context::default();
        let mut data = main_mode();
        let mut actions = UIActions::new();
        swap_preview_after_take(&ctx, &data, &mut actions);
        data.main.program = MainSource::Channel(data.channels[0].uuid.clone());
        swap_preview_after_take(&ctx, &data, &mut actions);
        assert_eq!(
            actions.session.select_channel, None,
            "black is not a preview"
        );

        data.main.mode = MixMode::Layered;
        data.main.program = MainSource::Channel(data.channels[1].uuid.clone());
        swap_preview_after_take(&ctx, &data, &mut actions);
        assert_eq!(actions.session.select_channel, None);
    }
}
