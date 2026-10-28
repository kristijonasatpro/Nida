//! Renaming a channel, a bus or an audio bus: a small window with its name
//! ready to type over, opened by double-clicking a channel tile's name bar
//! or a bus's name, or from Rename… in their right-click menus. Enter keeps
//! the new name, Esc leaves it.

use super::super::{ChannelUIInfo, UIActions, UIData};
use crate::engine::EngineCommand;

/// What is being renamed, by UUID.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Named {
    Channel(String),
    Bus(String),
    AudioBus(String),
}

#[derive(Clone)]
struct Renaming {
    named: Named,
    text: String,
    /// A channel never named, shown "Input 8" by where it is: left so,
    /// nothing is renamed — it is not given that as its name, even moved
    /// meanwhile and another number now.
    unnamed: Option<String>,
    /// The window it was asked in — the main window, or a panel's own (the
    /// Audio Mixer on another screen): it opens there, by the pointer.
    viewport: egui::ViewportId,
    at: egui::Pos2,
    /// Not yet drawn: the name is selected, to be typed over.
    fresh: bool,
}

fn state_id() -> egui::Id {
    egui::Id::new("rename_channel")
}

fn field_id() -> egui::Id {
    egui::Id::new("rename_channel_field")
}

/// Ask for a new name for `ch`, next to the pointer.
pub(super) fn open(ctx: &egui::Context, ch: &ChannelUIInfo) {
    ask(ctx, Named::Channel(ch.uuid.clone()), &ch.name);
    if ch.unnamed {
        if let Some(mut r) = ctx.data(|d| d.get_temp::<Renaming>(state_id())) {
            r.unnamed = Some(ch.name.clone());
            ctx.data_mut(|d| d.insert_temp(state_id(), r));
        }
    }
}

/// Ask for a new name for the bus `uuid`, now `name`, next to the pointer.
pub(super) fn open_bus(ctx: &egui::Context, uuid: &str, name: &str) {
    ask(ctx, Named::Bus(uuid.to_string()), name);
}

/// Ask for a new name for the audio bus `uuid`, now `name`, next to the
/// pointer.
pub(super) fn open_audio_bus(ctx: &egui::Context, uuid: &str, name: &str) {
    ask(ctx, Named::AudioBus(uuid.to_string()), name);
}

fn ask(ctx: &egui::Context, named: Named, name: &str) {
    let at = ctx
        .pointer_latest_pos()
        .unwrap_or_else(|| ctx.content_rect().center());
    let viewport = ctx.viewport_id();
    ctx.data_mut(|d| {
        d.insert_temp(
            state_id(),
            Renaming {
                named,
                text: name.to_string(),
                unnamed: None,
                viewport,
                at,
                fresh: true,
            },
        );
    });
}

/// The channel or bus being renamed, by UUID, for tests.
#[cfg(test)]
pub(super) fn renaming(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<Renaming>(state_id()))
        .map(|r| match r.named {
            Named::Channel(uuid) | Named::Bus(uuid) | Named::AudioBus(uuid) => uuid,
        })
}

/// The rename window, in the window it was asked in: called in each
/// window's pass, it draws only in that one's. `open` are the panels'
/// windows of their own still open (the main window's pass says): one asked
/// in a window closed since is let go.
pub(super) fn render_rename_in(
    ctx: &egui::Context,
    data: &UIData,
    actions: &mut UIActions,
    open: &[egui::ViewportId],
) {
    let Some(state) = ctx.data(|d| d.get_temp::<Renaming>(state_id())) else {
        return;
    };
    if ctx.viewport_id() == egui::ViewportId::ROOT
        && state.viewport != egui::ViewportId::ROOT
        && !open.contains(&state.viewport)
    {
        ctx.data_mut(|d| d.remove::<Renaming>(state_id()));
        return;
    }
    render_rename(ctx, data, actions);
}

pub(super) fn render_rename(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let Some(mut state) = ctx.data(|d| d.get_temp::<Renaming>(state_id())) else {
        return;
    };
    if state.viewport != ctx.viewport_id() {
        return;
    }
    let current = match &state.named {
        Named::Channel(uuid) => data
            .channels
            .iter()
            .find(|c| c.uuid == *uuid)
            .map(|c| c.name.clone()),
        Named::Bus(uuid) => data
            .buses
            .iter()
            .find(|b| b.bus.uuid == *uuid)
            .map(|b| b.bus.name.clone()),
        Named::AudioBus(uuid) => data
            .audio_mix
            .buses
            .iter()
            .find(|b| b.config.uuid == *uuid)
            .map(|b| b.config.name.clone()),
    };
    // Gone meanwhile: nothing to rename.
    let Some(current) = current else {
        ctx.data_mut(|d| d.remove::<Renaming>(state_id()));
        return;
    };
    let mut done = false;
    egui::Window::new(format!("Rename {current}"))
        .id(egui::Id::new("rename_channel_window"))
        .fixed_pos(state.at)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            let edit = ui.add(
                egui::TextEdit::singleline(&mut state.text)
                    .id(field_id())
                    .desired_width(220.0),
            );
            if state.fresh {
                edit.request_focus();
                select_all(ui.ctx(), &state.text);
                state.fresh = false;
            }
            let entered = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
            ui.horizontal(|ui| {
                let name = state.text.trim();
                let ok = ui
                    .add_enabled(!name.is_empty(), egui::Button::new("Rename"))
                    .clicked();
                if (ok || entered) && !name.is_empty() {
                    if name != current && state.unnamed.as_deref() != Some(name) {
                        actions.commands.push(match &state.named {
                            Named::Channel(uuid) => EngineCommand::RenameChannel {
                                channel_uuid: uuid.clone(),
                                name: name.to_string(),
                            },
                            Named::Bus(uuid) => EngineCommand::RenameBus {
                                uuid: uuid.clone(),
                                name: name.to_string(),
                            },
                            Named::AudioBus(uuid) => EngineCommand::RenameAudioBus {
                                uuid: uuid.clone(),
                                name: name.to_string(),
                            },
                        });
                    }
                    done = true;
                }
                if ui.button("Cancel").clicked() || escaped {
                    done = true;
                }
            });
        });
    ctx.data_mut(|d| {
        if done {
            d.remove::<Renaming>(state_id());
        } else {
            d.insert_temp(state_id(), state);
        }
    });
}

/// Select the whole name in the field, so typing replaces it.
fn select_all(ctx: &egui::Context, text: &str) {
    select_all_in(ctx, field_id(), text);
}

/// Select the whole of `text` in the text field `field`.
pub(super) fn select_all_in(ctx: &egui::Context, field: egui::Id, text: &str) {
    use egui::text::{CCursor, CCursorRange};
    let mut edit = egui::text_edit::TextEditState::load(ctx, field).unwrap_or_default();
    edit.cursor.set_char_range(Some(CCursorRange::two(
        CCursor::new(0),
        CCursor::new(text.chars().count()),
    )));
    edit.store(ctx, field);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn harness(data: UIData) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        let ch = data.channels[0].clone();
        let data = std::rc::Rc::new(data);
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open(ui.ctx(), &ch);
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_rename(ui.ctx(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        harness
    }

    /// A channel never named, shown by its number, is not given that name
    /// when the window is left as it was — even moved meanwhile, its number
    /// another now.
    #[test]
    fn a_name_left_as_it_was_renames_nothing_even_moved_meanwhile() {
        let data = std::rc::Rc::new(std::cell::RefCell::new(UIData::test_fixture()));
        data.borrow_mut().channels[0].name = "Input 8".into();
        data.borrow_mut().channels[0].ch_idx = 7;
        data.borrow_mut().channels[0].unnamed = true;
        let ch = data.borrow().channels[0].clone();
        let shown = data.clone();
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open(ui.ctx(), &ch);
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_rename(ui.ctx(), &shown.borrow(), &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        data.borrow_mut().channels[0].name = "Input 7".into();
        data.borrow_mut().channels[0].ch_idx = 6;
        harness.run();
        harness.get_by_label("Rename").click();
        harness.run();
        assert!(harness.state().is_empty(), "{:?}", harness.state());
        assert_eq!(renaming(&harness.ctx), None, "closed");
    }

    /// A channel named "Input 8" by hand, renamed meanwhile from outside,
    /// is given that name back when Rename is pressed.
    #[test]
    fn a_channel_named_like_its_number_is_given_its_name_back() {
        let data = std::rc::Rc::new(std::cell::RefCell::new(UIData::test_fixture()));
        data.borrow_mut().channels[0].name = "Input 8".into();
        data.borrow_mut().channels[0].ch_idx = 7;
        let ch = data.borrow().channels[0].clone();
        let shown = data.clone();
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open(ui.ctx(), &ch);
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_rename(ui.ctx(), &shown.borrow(), &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        data.borrow_mut().channels[0].name = "Slides".into();
        harness.run();
        harness.get_by_label("Rename").click();
        harness.run();
        assert!(
            format!("{:?}", harness.state()).contains(r#"name: "Input 8""#),
            "{:?}",
            harness.state()
        );
    }

    /// A bus renamed meanwhile from outside is given back the name the
    /// window opened with when Rename is pressed.
    #[test]
    fn a_bus_renamed_meanwhile_is_given_its_name_back() {
        let data = UIData::test_fixture();
        let uuid = data.buses[0].bus.uuid.clone();
        let now = data.buses[0].bus.name.clone();
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open_bus(ui.ctx(), &uuid, "LED");
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_rename(ui.ctx(), &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        assert_ne!(now, "LED");
        harness.get_by_label("Rename").click();
        harness.run();
        assert!(
            format!("{:?}", harness.state()).contains(r#"name: "LED""#),
            "{:?}",
            harness.state()
        );
    }

    /// Asked in a panel's own window — the Audio Mixer on another screen —
    /// it opens there, not in the main window; that window closed, it is
    /// let go.
    #[test]
    fn a_rename_opens_in_the_window_it_was_asked_in() {
        let data = std::rc::Rc::new(UIData::test_fixture());
        let audio = egui::ViewportId::from_hash_of("audio mixer window");
        let open = std::rc::Rc::new(std::cell::Cell::new(true));
        let (shown, still) = (data.clone(), open.clone());
        let mut asked = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !asked {
                        ui.ctx().data_mut(|d| {
                            d.insert_temp(
                                state_id(),
                                Renaming {
                                    named: Named::AudioBus("amaster1".into()),
                                    text: "Master".into(),
                                    unnamed: None,
                                    viewport: audio,
                                    at: egui::pos2(100.0, 100.0),
                                    fresh: true,
                                },
                            );
                        });
                        asked = true;
                    }
                    let mut actions = UIActions::new();
                    let windows: Vec<egui::ViewportId> =
                        if still.get() { vec![audio] } else { vec![] };
                    render_rename_in(ui.ctx(), &shown, &mut actions, &windows);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        assert!(harness.query_by_label("Rename").is_none(), "not in the main window");
        assert!(harness.ctx.data(|d| d.get_temp::<Renaming>(state_id())).is_some(), "kept");
        open.set(false);
        harness.run();
        assert!(harness.ctx.data(|d| d.get_temp::<Renaming>(state_id())).is_none(), "let go");
    }

    #[test]
    fn typing_over_the_name_and_pressing_enter_renames_the_channel() {
        let data = UIData::test_fixture();
        let uuid = data.channels[0].uuid.clone();
        let mut harness = harness(data);
        // The old name is selected: typing replaces it.
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Slides");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(harness.state().iter().any(|c| matches!(
            c,
            EngineCommand::RenameChannel { channel_uuid, name } if *channel_uuid == uuid && name == "Slides"
        )), "{:?}", harness.state());
        assert!(renaming(&harness.ctx).is_none(), "closed");
    }

    /// A bus is renamed the same way: its name selected, typed over, kept
    /// with Enter.
    #[test]
    fn typing_over_a_bus_name_and_pressing_enter_renames_the_bus() {
        let data = std::rc::Rc::new(UIData::test_fixture());
        let bus = data.buses[0].bus.clone();
        let mut opened = false;
        let shown = std::rc::Rc::clone(&data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open_bus(ui.ctx(), &bus.uuid, &bus.name);
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_rename(ui.ctx(), &shown, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Stage TV");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        let uuid = data.buses[0].bus.uuid.clone();
        assert!(harness.state().iter().any(|c| matches!(
            c,
            EngineCommand::RenameBus { uuid: u, name } if *u == uuid && name == "Stage TV"
        )), "{:?}", harness.state());
        assert!(renaming(&harness.ctx).is_none(), "closed");
    }

    /// An audio bus is renamed the same way, by its own command.
    #[test]
    fn renaming_an_audio_bus_sends_its_own_command() {
        let mut data = UIData::test_fixture();
        let mut bus = crate::audio_mix::AudioBusConfig::new("Bus A");
        bus.uuid = "astream1".into();
        data.audio_mix.buses = vec![crate::audio_mix::AudioBusSnapshot {
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            config: bus,
            device_ok: None,
            meter_db: [f32::NEG_INFINITY; 2],
            peak_db: [-96.0;2],
        }];
        let data = std::rc::Rc::new(data);
        let mut opened = false;
        let shown = std::rc::Rc::clone(&data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    if !opened {
                        open_audio_bus(ui.ctx(), "astream1", "Bus A");
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_rename(ui.ctx(), &shown, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            );
        harness.run();
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Stream");
        harness.run();
        harness.key_press(egui::Key::Enter);
        harness.run();
        assert!(harness.state().iter().any(|c| matches!(
            c,
            EngineCommand::RenameAudioBus { uuid, name } if uuid == "astream1" && name == "Stream"
        )), "{:?}", harness.state());
    }

    #[test]
    fn escape_leaves_the_name_as_it_was() {
        let mut harness = harness(UIData::test_fixture());
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Nope");
        harness.run();
        harness.key_press(egui::Key::Escape);
        harness.run();
        assert!(harness.state().is_empty());
        assert!(renaming(&harness.ctx).is_none());
    }
}
