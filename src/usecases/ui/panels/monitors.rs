//! Preview / Program, as vMix's top half: what goes next on the left, what is
//! on Main on the right, and the take buttons between them.
//!
//! Preview is the channel selected (or holding the selected deck) — the one
//! Space cuts and Enter fades to Main — with its clip's transport under it.
//! Program is the Main output.

use super::super::{ChannelUIInfo, UIActions, UIData};
use super::main_desk::{self, NEXT_GREEN, ON_MAIN_RED};
use crate::engine::EngineCommand;
use crate::mixer::MixMode;

/// The room a clip's whole transport row takes — buttons, times, In, Out,
/// Loop and its name. A picture's column narrower than this keeps to the
/// bar, the buttons and the times.
const FULL_TRANSPORT: f32 = 540.0;
/// Width of the column of take buttons between the pictures.
const TAKE_WIDTH: f32 = 132.0;
const GAP: f32 = 10.0;
/// Room for the bar naming what each picture shows.
const LABEL_HEIGHT: f32 = 28.0;
const FADE_TIMES: [f32; 7] = [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 5.0];

pub(super) fn render_monitors(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let area = ui.available_rect_before_wrap();
    // Beside Program, what the recording, the stream and the speakers hear.
    let meters = super::out_meters::width(data);
    let monitor_width = ((area.width() - TAKE_WIDTH - 3.0 * GAP - meters) / 2.0).max(80.0);
    // Both pictures keep the same size, the transport under Preview's.
    let picture = super::utils::preview_size(
        egui::vec2(
            monitor_width,
            (area.height() - LABEL_HEIGHT - super::transport::HEIGHT - 12.0).max(40.0),
        ),
        data.render_width,
        data.render_height,
    );
    let next = main_desk::next_channel(data);
    // The pictures' columns as wide as what is in them — the picture, or a
    // clip's transport under it if that is wider, as measured last frame —
    // and the row centred: the meters stand against Program, inside the
    // panel, not at its edge.
    let seen = ui.id().with("monitors_column_need");
    let need = ui
        .ctx()
        .memory(|m| m.data.get_temp::<f32>(seen))
        .unwrap_or(0.0);
    // Room for a clip's whole transport row, or it keeps to the essentials.
    let full = monitor_width >= FULL_TRANSPORT;
    let column = picture.x.max(need).min(monitor_width);
    let row = 2.0 * column + TAKE_WIDTH + meters + 3.0 * GAP;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        let pad = (area.width() - row) / 2.0 - GAP;
        if pad > 0.0 {
            ui.add_space(pad);
        }
        let preview = ui.allocate_ui_with_layout(
            egui::vec2(column, area.height()),
            egui::Layout::top_down(egui::Align::Center),
            |ui| render_preview(ui, next, picture, column, !full, data, actions),
        );
        ui.allocate_ui_with_layout(
            egui::vec2(TAKE_WIDTH, area.height()),
            egui::Layout::top_down(egui::Align::Center),
            |ui| render_take_column(ui, next, data, actions),
        );
        let program = ui.allocate_ui_with_layout(
            egui::vec2(column, area.height()),
            egui::Layout::top_down(egui::Align::Center),
            |ui| render_program(ui, picture, column, !full, data, actions),
        );
        // What the transports need, not the room they were given: it
        // shrinks again when a clip goes.
        let used = preview.inner.max(program.inner);
        if (used - need).abs() > 0.5 {
            ui.ctx().memory_mut(|m| m.data.insert_temp(seen, used));
            ui.ctx().request_repaint();
        }
        ui.allocate_ui_with_layout(
            egui::vec2(meters, area.height()),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.add_space(LABEL_HEIGHT);
                super::out_meters::render(ui, picture.y, data);
            },
        );
    });
}

/// The bar over a picture: which it is, and what it shows.
fn monitor_label(ui: &mut egui::Ui, which: &str, what: &str, color: egui::Color32, width: f32) {
    let text = format!("{which}   {what}");
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, LABEL_HEIGHT - 4.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, 3.0, color);
    let painter = ui.painter().with_clip_rect(rect.shrink(2.0));
    painter.text(
        rect.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        &text,
        egui::FontId::proportional(15.0),
        egui::Color32::WHITE,
    );
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &text));
}

/// A monitor picture with a coloured frame, or a dark box saying why there
/// is none.
fn picture(
    ui: &mut egui::Ui,
    texture: Option<egui::TextureId>,
    size: egui::Vec2,
    frame: egui::Color32,
    empty: &str,
    label: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    if let Some(id) = texture {
        ui.painter().image(
            id,
            rect,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    } else {
        ui.painter()
            .rect_filled(rect, 2.0, egui::Color32::from_rgb(18, 18, 26));
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            empty,
            egui::FontId::proportional(14.0),
            egui::Color32::GRAY,
        );
    }
    ui.painter().rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(3.0, frame),
        egui::StrokeKind::Outside,
    );
    let label = label.to_string();
    response.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &label));
    response
}

fn render_preview(
    ui: &mut egui::Ui,
    next: Option<&ChannelUIInfo>,
    size: egui::Vec2,
    column: f32,
    compact: bool,
    data: &UIData,
    actions: &mut UIActions,
) -> f32 {
    let what = next.map_or_else(
        || "select a channel".to_string(),
        |ch| {
            if ch.overlay {
                format!("{} (overlay)", ch.name)
            } else {
                ch.name.clone()
            }
        },
    );
    monitor_label(ui, "PREVIEW", &what, NEXT_GREEN, size.x);
    let texture = next.and_then(|ch| data.channel_preview_textures.get(&ch.ch_idx).copied());
    let empty = if next.is_some() {
        "No picture yet"
    } else {
        "Click a channel to preview it"
    };
    let response = picture(ui, texture, size, NEXT_GREEN, empty, "Preview picture");
    if let Some(ch) = next {
        // What a click would show anyway: the channel's effects below.
        if response.clicked() {
            actions.session.select_channel = Some(ch.ch_idx);
        }
        if response.double_clicked() {
            super::channel_settings::open_channel(ui.ctx(), ch);
        }
        response.context_menu(|ui| super::mixer::channel_context_menu(ui, data, actions, ch));
        ui.add_space(6.0);
        let clip = super::transport::clip_of(data, ch);
        return super::transport::render_transport(
            ui,
            clip,
            ch.decks.len() > 1,
            column,
            compact,
            actions,
        );
    }
    0.0
}

/// Program: Main's picture, framed red, and the transport of the clip on air
/// under it, `column` wide.
fn render_program(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    column: f32,
    compact: bool,
    data: &UIData,
    actions: &mut UIActions,
) -> f32 {
    let mut what = main_desk::source_name(data, &data.main.program);
    if let Some(fade) = &data.main.fade {
        what = format!("{} → {what}", main_desk::source_name(data, &fade.from));
    }
    let overlays: Vec<&str> = data
        .channels
        .iter()
        .filter(|c| main_desk::overlay_on(data, c))
        .map(|c| c.name.as_str())
        .collect();
    if !overlays.is_empty() {
        what = format!("{what} + {}", overlays.join(", "));
    }
    if !main_desk::in_main_mode(data) {
        what = "layered mix".into();
    }
    monitor_label(ui, "PROGRAM", &what, ON_MAIN_RED, size.x);
    let response = picture(
        ui,
        data.main_output_texture,
        size,
        ON_MAIN_RED,
        "No Output",
        "Program picture",
    );
    super::dnd::publish_master_surface_fx(ui.ctx(), response.rect);
    if response.clicked() {
        actions.session.select_master = true;
    }
    response.context_menu(|ui| {
        super::switcher::take_menu_items(ui, &crate::bus::BusInputRef::Master, data, actions);
    });
    // The transport of the clip on air, as Preview has its own: paused
    // there, and how much of it is left seen.
    ui.add_space(6.0);
    let (clip, several_layers) = on_air_clip(data);
    super::transport::render_transport(ui, clip, several_layers, column, compact, actions)
}

/// The clip Program's transport drives, and whether to name it: Main's
/// input's clip that shows (see [`super::transport::top_clip`]) or the layer
/// on Main on its own; else the top overlay's showing; else, while a fade
/// runs, the one fading out. None in a layered mix.
fn on_air_clip(
    data: &UIData,
) -> (
    Option<(&super::super::DeckUIInfo, &super::super::VideoPlaybackUI)>,
    bool,
) {
    if !main_desk::in_main_mode(data) {
        return (None, false);
    }
    let channel = |uuid: &str| data.channels.iter().find(|c| c.uuid == uuid);
    let clip_of = |source: &crate::mixer::MainSource| match source {
        crate::mixer::MainSource::Channel(uuid) => channel(uuid)
            .and_then(|ch| super::transport::top_clip(ch).map(|clip| (clip, ch.decks.len() > 1))),
        crate::mixer::MainSource::Deck(uuid) => data
            .channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.uuid == *uuid)
            .and_then(|d| d.video_playback.as_ref().map(|vp| ((d, vp), false))),
        crate::mixer::MainSource::Black => None,
    };
    // The top overlay showing — composited in channel order, the last
    // over the rest — still while it fades out.
    let overlay = || {
        data.channels
            .iter()
            .rev()
            .filter(|c| data.main.overlays_showing.contains(&c.uuid))
            .find_map(super::transport::top_clip)
            .map(|clip| (clip, true))
    };
    let fading = || {
        data.main
            .fade
            .as_ref()
            .and_then(|fade| clip_of(&fade.from))
            .map(|(clip, _)| (clip, true))
    };
    match clip_of(&data.main.program).or_else(overlay).or_else(fading) {
        Some((clip, named)) => (Some(clip), named),
        None => (None, false),
    }
}

/// The take buttons, NOT RECORDING blinking behind them when asked for
/// (REC ▸ Reminder) and not recording.
fn render_take_column(
    ui: &mut egui::Ui,
    next: Option<&ChannelUIInfo>,
    data: &UIData,
    actions: &mut UIActions,
) {
    if super::record::warns_not_recording(data) {
        super::record::with_record_warning(ui, data, Some(LABEL_HEIGHT), |ui| {
            render_takes(ui, next, data, actions);
        });
    } else {
        ui.add_space(LABEL_HEIGHT);
        render_takes(ui, next, data, actions);
    }
}

fn render_takes(
    ui: &mut egui::Ui,
    next: Option<&ChannelUIInfo>,
    data: &UIData,
    actions: &mut UIActions,
) {
    if !main_desk::in_main_mode(data) {
        ui.label(
            egui::RichText::new("The layered mixer is on: every channel mixes to the output.")
                .small()
                .weak(),
        );
        if ui
            .button("Main mode")
            .on_hover_text("One channel on Main at a time, taken with Cut or Fade")
            .clicked()
        {
            actions.commands.push(EngineCommand::SetMixMode {
                mode: MixMode::Program,
            });
        }
        return;
    }
    let big = |text: &str| egui::RichText::new(text).size(18.0).strong();
    let size = egui::vec2(TAKE_WIDTH, 48.0);
    let overlay = next.is_some_and(|ch| ch.overlay);
    let can_take = next.is_some_and(|ch| ch.overlay || !main_desk::channel_on_main(data, ch));
    let (cut, fade) = if overlay {
        let on = next.is_some_and(|ch| main_desk::overlay_on(data, ch));
        if on {
            ("OFF", "FADE OFF")
        } else {
            ("ON", "FADE ON")
        }
    } else {
        ("CUT", "")
    };
    let take_name = if overlay {
        fade.to_string()
    } else {
        data.main.transition.name().to_uppercase()
    };
    if ui
        .add_enabled(can_take, egui::Button::new(big(cut)).min_size(size))
        .on_hover_text("Put Preview on Main now (Space)")
        .clicked()
    {
        main_desk::take_next(data, actions, false);
    }
    ui.add_space(6.0);
    if ui
        .add_enabled(
            can_take && data.main.fade.is_none(),
            egui::Button::new(big(&take_name)).min_size(size),
        )
        .on_hover_text(format!(
            "Take Preview to Main — {} — over {} (Enter)",
            data.main.transition.name().to_lowercase(),
            seconds(data.main.fade_secs)
        ))
        .clicked()
    {
        main_desk::take_next(data, actions, true);
    }
    ui.add_space(6.0);
    main_desk::transition_picker(ui, "monitors_transition", data, actions);
    ui.add_space(6.0);
    egui::ComboBox::from_id_salt("monitors_fade_time")
        .selected_text(seconds(data.main.fade_secs))
        .width(TAKE_WIDTH - 8.0)
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
    if let Some(fade) = &data.main.fade {
        ui.add_space(6.0);
        ui.add(egui::ProgressBar::new(fade.progress.clamp(0.0, 1.0)).desired_width(TAKE_WIDTH));
        ui.ctx().request_repaint();
    }
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
    use crate::mixer::MainSource;
    use egui_kittest::kittest::Queryable;

    fn main_mode_fixture() -> UIData {
        let mut data = UIData::test_fixture();
        data.main.mode = MixMode::Program;
        data.main.program = MainSource::Channel(data.channels[0].uuid.clone());
        data.selected_deck = None;
        data.selected_channel = Some(1);
        data
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        let mut harness = unrun(data);
        harness.run();
        harness
    }

    /// The monitors, not yet drawn: a fade running repaints every frame.
    fn unrun(data: UIData) -> egui_kittest::Harness<'static, Vec<EngineCommand>> {
        let data = std::rc::Rc::new(data);
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(1400.0, 420.0))
            .build_ui_state(
                move |ui, commands: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_monitors(ui, &data, &mut actions);
                    commands.extend(actions.commands);
                },
                Vec::new(),
            )
    }

    /// Preview's channel with a 60-second clip on top, 20 s in, paused.
    fn with_clip() -> (UIData, String) {
        let mut data = main_mode_fixture();
        let deck = data.channels[1].decks.last_mut().unwrap();
        deck.video_playback = Some(crate::usecases::ui::VideoPlaybackUI {
            playing: false,
            position: 20.0,
            duration: 60.0,
            speed: 1.0,
            effective_speed: 1.0,
            position_offset: 0.0,
            loop_mode: crate::video::LoopMode::Loop,
            in_point: 0.0,
            out_point: 0.0,
            frame_rate: 25.0,
            transport_sync: crate::video::DeckTransportSync::default(),
            hardware_decode: false,
            auto: crate::engine::value::video::AutoPlayback::default(),
            sound_only: false,
        });
        let uuid = deck.uuid.clone();
        (data, uuid)
    }

    fn seeks(harness: &egui_kittest::Harness<'static, Vec<EngineCommand>>) -> Vec<f64> {
        harness
            .state()
            .iter()
            .filter_map(|c| match c {
                EngineCommand::VideoSeek { position_secs, .. } => Some(*position_secs),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn previews_clip_plays_and_jumps_from_under_the_picture() {
        let (data, deck) = with_clip();
        let mut harness = harness(data);
        harness.get_by_label("Play").click();
        harness.run();
        assert!(
            harness.state().iter().any(
                |c| matches!(c, EngineCommand::VideoTogglePlay { deck_uuid } if *deck_uuid == deck)
            ),
            "{:?}",
            harness.state()
        );
        harness.get_by_label("Forward 5 seconds").click();
        harness.run();
        harness.get_by_label("Back 5 seconds").click();
        harness.run();
        harness.get_by_label("To the in-point").click();
        harness.run();
        assert_eq!(seeks(&harness), vec![25.0, 15.0, 0.0]);
    }

    /// Program has the transport of the clip on air, as Preview has its own:
    /// paused there, and how much of it is left seen.
    #[test]
    fn programs_clip_is_paused_from_under_its_picture() {
        let (mut data, _) = with_clip();
        // The clip on Main instead, playing; Preview's input has none.
        let mut vp = data.channels[1]
            .decks
            .last_mut()
            .unwrap()
            .video_playback
            .take();
        if let Some(vp) = vp.as_mut() {
            vp.playing = true;
        }
        let deck = data.channels[0].decks.last_mut().unwrap();
        deck.video_playback = vp;
        let uuid = deck.uuid.clone();
        let mut harness = harness(data);
        harness.get_by_label("-00:40.0");
        harness.get_by_label("Pause").click();
        harness.run();
        assert!(
            harness.state().iter().any(
                |c| matches!(c, EngineCommand::VideoTogglePlay { deck_uuid } if *deck_uuid == uuid)
            ),
            "{:?}",
            harness.state()
        );
    }

    /// Program's transport is the clip that shows on Main, whatever layer
    /// is selected; with none on Main, an overlay's switched on, named; and
    /// while a fade runs, the one fading out.
    #[test]
    fn programs_transport_follows_what_shows_on_main() {
        // Two clips in Main's input, the lower one selected.
        let make = || {
            let (mut data, _) = with_clip();
            let vp = data.channels[1]
                .decks
                .last_mut()
                .unwrap()
                .video_playback
                .take();
            data.channels[0].decks[0].video_playback.clone_from(&vp);
            data.channels[0].decks[1].video_playback.clone_from(&vp);
            data.selected_deck = Some((0, 0));
            data.selected_channel = Some(1);
            data
        };
        let data = make();
        let upper = data.channels[0].decks[1].uuid.clone();
        let name = data.channels[0].decks[1].name.clone();
        let mut on_main = harness(data);
        on_main.get_by_label("Play").click();
        on_main.run();
        assert!(on_main.state().iter().any(
            |c| matches!(c, EngineCommand::VideoTogglePlay { deck_uuid } if *deck_uuid == upper)
        ));

        // Main black; Main's input showing as an overlay instead — still
        // while it fades out.
        let mut data = make();
        data.main.program = MainSource::Black;
        data.main.overlays_showing = vec![data.channels[0].uuid.clone()];
        let overlay = harness(data);
        assert!(overlay.query_by_label(&name).is_some(), "named");
        assert!(overlay.query_by_label("Play").is_some());

        // Two overlays with clips: the later channel's is over the other.
        let mut data = make();
        data.main.program = MainSource::Black;
        let later = data.channels[1].decks[0].uuid.clone();
        let vp = data.channels[0].decks[0].video_playback.clone();
        data.channels[1].decks[0].video_playback = vp;
        // Nothing in Preview, whose transport would be another Play.
        data.selected_deck = None;
        data.selected_channel = None;
        data.main.overlays_showing =
            vec![data.channels[0].uuid.clone(), data.channels[1].uuid.clone()];
        let mut two = harness(data);
        two.get_by_label("Play").click();
        two.run();
        assert!(two.state().iter().any(
            |c| matches!(c, EngineCommand::VideoTogglePlay { deck_uuid } if *deck_uuid == later)
        ));

        // Fading from Main's input to black.
        let mut data = make();
        let from = data.main.program.clone();
        data.main.program = MainSource::Black;
        data.main.fade = Some(crate::mixer::MainFade {
            from,
            progress: 0.5,
            duration: 1.0,
            transition: crate::mixer::MainTransition::default(),
        });
        let mut fade = unrun(data);
        fade.run_steps(2);
        assert!(fade.query_by_label("Play").is_some());
    }

    #[test]
    fn clicking_the_bar_moves_the_playhead_there() {
        let (data, _) = with_clip();
        let mut harness = harness(data);
        let at = harness.get_by_label("Playhead").rect().center();
        harness.hover_at(at);
        harness.run();
        harness.drag_at(at);
        harness.run();
        harness.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        let seeks = seeks(&harness);
        assert_eq!(seeks.len(), 1, "{seeks:?}");
        assert!((seeks[0] - 30.0).abs() < 0.5, "half way along: {seeks:?}");
    }

    #[test]
    fn in_and_out_are_set_at_the_playhead() {
        let (data, deck) = with_clip();
        let mut harness = harness(data);
        harness.get_by_label("[ In").click();
        harness.run();
        harness.get_by_label("Out ]").click();
        harness.run();
        let marks: Vec<_> = harness
            .state()
            .iter()
            .filter_map(|c| match c {
                EngineCommand::VideoSetInPoint { deck_uuid, secs } if *deck_uuid == deck => {
                    Some(("in", *secs))
                }
                EngineCommand::VideoSetOutPoint { deck_uuid, secs } if *deck_uuid == deck => {
                    Some(("out", *secs))
                }
                _ => None,
            })
            .collect();
        assert_eq!(marks, vec![("in", 20.0), ("out", 20.0)]);
    }

    #[test]
    fn an_input_with_nothing_to_play_says_so() {
        // Under Preview and under Program alike.
        let harness = harness(main_mode_fixture());
        assert_eq!(
            harness
                .query_all_by_label_contains("No video or sound in this input")
                .count(),
            2
        );
        assert!(harness.query_by_label("Play").is_none());
    }

    #[test]
    fn preview_names_the_next_channel_and_program_what_is_on_main() {
        let data = main_mode_fixture();
        let next = data.channels[1].name.clone();
        let on_main = data.channels[0].name.clone();
        let harness = harness(data);
        assert!(harness
            .query_by_label_contains(&format!("PREVIEW   {next}"))
            .is_some());
        assert!(harness
            .query_by_label_contains(&format!("PROGRAM   {on_main}"))
            .is_some());
    }

    #[test]
    fn cut_takes_preview_to_main() {
        let data = main_mode_fixture();
        let uuid = data.channels[1].uuid.clone();
        let mut harness = harness(data);
        harness.get_by_label("CUT").click();
        harness.run();
        assert!(
            harness.state().iter().any(|c| matches!(
                c,
                EngineCommand::TakeToMain { source: MainSource::Channel(u), fade: false } if *u == uuid
            )),
            "{:?}",
            harness.state()
        );
    }

    #[test]
    fn fade_fades_preview_to_main() {
        let data = main_mode_fixture();
        let mut harness = harness(data);
        harness.get_by_label("FADE").click();
        harness.run();
        assert!(harness
            .state()
            .iter()
            .any(|c| matches!(c, EngineCommand::TakeToMain { fade: true, .. })));
    }

    #[test]
    fn nothing_to_take_when_preview_is_already_on_main() {
        let mut data = main_mode_fixture();
        data.selected_channel = Some(0);
        let mut harness = harness(data);
        harness.get_by_label("CUT").click();
        harness.run();
        assert!(harness.state().is_empty(), "{:?}", harness.state());
    }

    #[test]
    fn the_layered_mixer_offers_main_mode_instead() {
        let data = UIData::test_fixture();
        let mut harness = harness(data);
        assert!(harness.query_by_label("CUT").is_none());
        harness.get_by_label("Main mode").click();
        harness.run();
        assert!(harness.state().iter().any(|c| matches!(
            c,
            EngineCommand::SetMixMode {
                mode: MixMode::Program
            }
        )));
    }

    /// Beside Program, a meter for the recording, the stream and each bus on
    /// a sound device; right of the Program picture.
    #[test]
    fn meters_stand_beside_program() {
        let mut data = main_mode_fixture();
        let mut speakers = crate::audio_mix::AudioBusConfig::new("Bus B");
        speakers.device = Some("MacBook Pro Speakers".into());
        data.audio_mix.buses = vec![
            crate::audio_mix::AudioBusSnapshot {
                peak_db: [-96.0; 2],
                dynamics: crate::audio_mix::DynamicsSnapshot::default(),
                config: crate::audio_mix::AudioBusConfig::new("Master"),
                device_ok: None,
                meter_db: [-12.0; 2],
            },
            crate::audio_mix::AudioBusSnapshot {
                peak_db: [-96.0; 2],
                dynamics: crate::audio_mix::DynamicsSnapshot::default(),
                config: speakers,
                device_ok: Some(true),
                meter_db: [-6.0; 2],
            },
        ];
        // Wide, the pictures held back by the height: room to spare.
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(2200.0, 420.0))
            .build_ui(move |ui| render_monitors(ui, &data, &mut UIActions::new()));
        harness.run_steps(2);
        let program = harness.get_by_label("Program picture").rect();
        for meter in ["REC meter", "STREAM meter", "Bus B meter"] {
            let rect = harness.get_by_label(meter).rect();
            assert!(rect.left() >= program.right(), "{meter} at {rect:?}, Program {program:?}");
            assert!(
                rect.left() - program.right() < 3.0 * GAP + 52.0 * 3.0,
                "{meter} beside Program, not at the edge: {rect:?}, Program {program:?}"
            );
        }
    }

    /// With the reminder on and nothing recording, NOT RECORDING stands over
    /// the take buttons; recording, it goes.
    #[test]
    fn not_recording_blinks_over_the_takes() {
        let reminded = || {
            let mut data = main_mode_fixture();
            data.record.options.warn_when_off = true;
            data
        };
        let mut harness = unrun(reminded());
        harness.run_steps(2);
        let words = harness.get_by_label("NOT RECORDING").rect();
        let cut = harness.get_by_label("CUT").rect();
        assert!(words.bottom() <= cut.top(), "{words:?} over {cut:?}");
        let mut data = reminded();
        data.record.status.recording = true;
        let mut harness = unrun(data);
        harness.run_steps(2);
        assert!(harness.query_by_label("NOT RECORDING").is_none());
    }

    /// A short pane makes narrow pictures; with clips under both, their
    /// transports keep their room, and the meters still stand against
    /// Program, inside the pane.
    #[test]
    fn meters_stay_by_program_over_narrow_pictures() {
        let (mut data, _) = with_clip();
        let vp = data.channels[1].decks.last().unwrap().video_playback.clone();
        data.channels[0].decks.last_mut().unwrap().video_playback = vp;
        // Room for both transports, not much more: a row worked out from
        // the pictures alone would end past the pane.
        let width = 1300.0;
        let data = std::rc::Rc::new(data);
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(width, 260.0))
            .build_ui(move |ui| render_monitors(ui, &data, &mut UIActions::new()));
        harness.run_steps(2);
        let program = harness.get_by_label("Program picture").rect();
        let rec = harness.get_by_label("REC meter").rect();
        let last = harness.get_by_label("STREAM meter").rect();
        assert!(last.right() <= width, "inside the pane: {last:?}");
        assert!(rec.left() >= program.right(), "right of Program: {rec:?} {program:?}");
        assert!(
            rec.left() - program.right() < 200.0,
            "against Program, not past its transport: {rec:?} {program:?}"
        );
    }

    /// Both pictures with a clip under them, in a panel as narrow as a
    /// detached window opens: the transports keep to the essentials and
    /// every meter stays inside, against Program. Wide, the whole row.
    #[test]
    fn narrow_transports_keep_to_the_essentials() {
        let clips = || {
            let (mut data, _) = with_clip();
            let vp = data.channels[1].decks.last().unwrap().video_playback.clone();
            data.channels[0].decks.last_mut().unwrap().video_playback = vp;
            std::rc::Rc::new(data)
        };
        for (width, full) in [(960.0, false), (1800.0, true)] {
            let data = clips();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, 420.0))
                .build_ui(move |ui| render_monitors(ui, &data, &mut UIActions::new()));
            harness.run_steps(3);
            let program = harness.get_by_label("Program picture").rect();
            let last = harness.get_by_label("STREAM meter").rect();
            assert!(last.right() <= width, "inside {width}: {last:?}");
            assert!(last.left() >= program.right(), "right of Program: {last:?}");
            assert_eq!(
                harness.query_all_by_label("[ In").count() > 0,
                full,
                "the whole row at {width}"
            );
        }
    }

    /// Clips gone, the columns shrink back to the pictures: the meters come
    /// back against Program.
    #[test]
    fn the_columns_shrink_when_the_clips_go() {
        let (mut data, _) = with_clip();
        let vp = data.channels[1].decks.last().unwrap().video_playback.clone();
        data.channels[0].decks.last_mut().unwrap().video_playback = vp;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1800.0, 260.0))
            .build_ui_state(
                |ui, data: &mut UIData| render_monitors(ui, data, &mut UIActions::new()),
                data,
            );
        harness.run_steps(3);
        let gap = |h: &egui_kittest::Harness<'_, UIData>| {
            h.get_by_label("REC meter").rect().left() - h.get_by_label("Program picture").rect().right()
        };
        let with_clips = gap(&harness);
        for ch in &mut harness.state_mut().channels {
            for deck in &mut ch.decks {
                deck.video_playback = None;
            }
        }
        harness.run_steps(3);
        let without = gap(&harness);
        assert!(without < with_clips, "shrunk: {without} after {with_clips}");
        assert!(without <= 2.0 * GAP, "against Program: {without}");
    }
}
