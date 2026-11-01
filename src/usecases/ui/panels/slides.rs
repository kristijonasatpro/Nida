//! The Presentations panel: a Slides input's running order — talks added,
//! put in order, named, and each started with one press — with the talk on
//! and the one coming next, and what moves the talk on. See
//! docs/26-presentations.md.
//!
//! Files come from a file dialog, or dropped from Finder on the panel (see
//! [`take_drop`]); every change to the list is one undo step, starting a
//! talk is live.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use super::super::{ChannelUIInfo, DeckUIInfo, UIActions, UIData};
use crate::engine::value::playlist::PlaylistTransition;
use crate::engine::value::presentation::{Go, Screen};
use crate::engine::value::slides::{
    SlidesSettings, SlidesSnapshot, TalkKind, TalkRef, TalkSnapshot, TalkState,
    MAX_TALK_TRANSITION_SECS, POWERPOINT_EXTENSIONS,
};
use crate::engine::{EngineCommand, InputSource};

const ON_AIR_RED: egui::Color32 = egui::Color32::from_rgb(200, 40, 40);
const READY_GREEN: egui::Color32 = egui::Color32::from_rgb(60, 170, 80);
const WARN: egui::Color32 = egui::Color32::from_rgb(230, 160, 40);
const FAILED_RED: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);

/// A Slides input as the panel shows it.
#[derive(Clone, Copy)]
struct Input<'a> {
    channel: &'a ChannelUIInfo,
    deck: &'a DeckUIInfo,
    slides: &'a SlidesSnapshot,
}

/// The show's Slides inputs, in the order of the inputs.
fn inputs(data: &UIData) -> Vec<Input<'_>> {
    data.channels
        .iter()
        .flat_map(|channel| {
            channel.decks.iter().filter_map(move |deck| {
                deck.slides.as_ref().map(|slides| Input {
                    channel,
                    deck,
                    slides,
                })
            })
        })
        .collect()
}

/// A channel's Slides layer, if it has one.
pub(super) fn of_channel(ch: &ChannelUIInfo) -> Option<(&DeckUIInfo, &SlidesSnapshot)> {
    ch.decks
        .iter()
        .find_map(|d| d.slides.as_ref().map(|s| (d, s)))
}

/// ◀ ▶, ⏭ and where the Slides input is — the talk among the talks, the
/// slide among the slides — over the foot of a channel's picture, above
/// the rows already there (`rows_below`).
pub(super) fn overlay_on_picture(
    ui: &mut egui::Ui,
    picture: egui::Rect,
    ch: &ChannelUIInfo,
    rows_below: usize,
    actions: &mut UIActions,
) {
    let Some((deck, s)) = of_channel(ch) else {
        return;
    };
    let size = egui::vec2(30.0, 24.0);
    #[allow(clippy::cast_precision_loss)]
    let raised = rows_below as f32 * (size.y + 4.0);
    let row = egui::Rect::from_min_size(
        picture.left_bottom() + egui::vec2(4.0, -size.y - 4.0 - raised),
        egui::vec2(picture.width() - 8.0, size.y),
    );
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row_ui.spacing_mut().item_spacing.x = 3.0;
    // The talk on as it plays — in the list or not.
    let live = s.on.as_ref();
    let steps = live.and_then(|t| t.kind).is_some_and(TalkKind::steps);
    let button = |text: &str| {
        egui::Button::new(
            egui::RichText::new(text)
                .size(size.y * 0.55)
                .color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_black_alpha(170))
        .min_size(size)
    };
    for (text, to, hover) in [
        ("◀", Go::Previous, "Back a step"),
        ("▶", Go::Next, "On a step"),
    ] {
        if row_ui
            .add_enabled(steps, button(text))
            .on_hover_text(hover)
            .clicked()
        {
            actions.commands.push(EngineCommand::PresentationGo {
                deck_uuid: Some(deck.uuid.clone()),
                to,
            });
        }
    }
    let next = s.next.and_then(|i| s.talks.get(i));
    if row_ui
        .add_enabled(next.is_some(), button("⏭"))
        .on_hover_text(next.map_or_else(
            || "No talk comes next".to_string(),
            |t| format!("Start next talk: {}", t.name),
        ))
        .clicked()
    {
        actions.commands.push(EngineCommand::StartTalk {
            deck_uuid: Some(deck.uuid.clone()),
            to: TalkRef::Next,
        });
    }
    let text = match (s.live, live) {
        (Some(i), Some(talk)) => {
            let at = position_text(talk);
            if at.is_empty() {
                format!(" {}/{} ", i + 1, s.talks.len())
            } else {
                format!(" {}/{} · {at} ", i + 1, s.talks.len())
            }
        }
        // On, though taken out of the list.
        (None, Some(talk)) => format!(" on · {} ", position_text(talk)),
        _ if s.starting.is_some() => " starting… ".to_string(),
        _ => format!(" {} talks ", s.talks.len()),
    };
    let label = row_ui.label(
        egui::RichText::new(text)
            .small()
            .color(egui::Color32::WHITE)
            .background_color(egui::Color32::from_black_alpha(170)),
    );
    if let Some(talk) = live {
        label.on_hover_text(&talk.name);
    }
}

/// The Slides layer's settings column: the talk on and what comes next,
/// Start next talk, PowerPoint's sound, a presenter view — the list itself
/// is in the Presentations panel.
pub(super) fn render_deck_column(
    ui: &mut egui::Ui,
    data: &UIData,
    deck: &DeckUIInfo,
    s: &SlidesSnapshot,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("Slides").strong());
    // The talk on as it plays — in the list or not.
    let live = s.on.as_ref();
    let next = s.next.and_then(|i| s.talks.get(i));
    ui.label(match live {
        Some(talk) => format!("On: {}", talk.name),
        None => "No talk on".to_string(),
    });
    ui.label(match next {
        Some(talk) => format!("Next: {}", talk.name),
        None => "Nothing comes next".to_string(),
    });
    if ui
        .add_enabled(next.is_some(), egui::Button::new("▶ Start next talk"))
        .clicked()
    {
        actions.commands.push(EngineCommand::StartTalk {
            deck_uuid: Some(deck.uuid.clone()),
            to: TalkRef::Next,
        });
    }
    ui.label(
        egui::RichText::new(format!(
            "{} talks — put in order, named and started in the Presentations panel",
            s.talks.len()
        ))
        .small()
        .weak(),
    );
    if s.talks.iter().any(|t| t.kind == Some(TalkKind::PowerPoint)) {
        super::presentation::powerpoint_sound(ui, data, deck, actions);
    }
    if s.talks.iter().any(|t| t.kind == Some(TalkKind::Web)) {
        web_sound(ui, data, deck, actions);
    }
    if ui
        .button("+ Presenter view")
        .on_hover_text("An input for the speaker's screen: the talk on, its notes and clocks")
        .clicked()
    {
        actions.commands.push(EngineCommand::AddInput {
            source: InputSource::PresenterView {
                deck_uuid: deck.uuid.clone(),
            },
            name: None,
            audio: None,
        });
    }
}

/// Web talks' own sound — a page's video or music, which `WebKit` plays —
/// taken into the audio mixer in one click: one strip for every Slides
/// input, as PowerPoint's is, heard while one is on air (Audio Auto); once it
/// is there, said so.
fn web_sound(ui: &mut egui::Ui, data: &UIData, deck: &DeckUIInfo, actions: &mut UIActions) {
    use crate::audio::app_sound::WEB_TALKS;
    use crate::audio_mix::AudioSourceKind;
    let there = data.audio_mix.sources.iter().any(|s| {
        matches!(&s.kind, AudioSourceKind::Input { app: Some(app), .. } if app == WEB_TALKS)
    });
    if there {
        ui.label(
            egui::RichText::new("Web talks' sound: in the audio mixer")
                .small()
                .weak(),
        );
    } else if ui
        .button("Web talks' sound to the mixer")
        .on_hover_text(
            "A web talk's video or music: taken into the audio mixer, heard while a Slides input \
             is on air (Audio Auto) — one strip for every Slides input; a Slides input off air is \
             not heard in it. This Mac's own speakers then stay quiet. A page plays sound only \
             after its first step on (a key), as browsers allow it.",
        )
        .clicked()
    {
        actions.commands.push(EngineCommand::AddAppSound {
            app: WEB_TALKS.to_string(),
            name: "Web talks".into(),
            deck_uuid: Some(deck.uuid.clone()),
        });
    }
}

/// Whether a talk of `kind` plays on this computer: clips and pictures
/// anywhere; PowerPoint, PDF and web talks on a Mac.
pub(super) fn plays_here(kind: Option<TalkKind>) -> bool {
    match kind {
        Some(TalkKind::Clip | TalkKind::Picture) => true,
        Some(_) => cfg!(target_os = "macos"),
        None => false,
    }
}

fn chosen_id() -> egui::Id {
    egui::Id::new("slides_panel_input")
}

/// Where the panel is, in the window it is drawn in (`ctx`'s viewport): a
/// panel in a window of its own takes no drops meant for this one.
fn panel_rect_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::new("slides_panel_rect").with(ctx.viewport_id())
}

/// Where each talk's row is, this frame, in the window the panel is drawn
/// in: a drop between rows lands there.
fn rows_id(ctx: &egui::Context) -> egui::Id {
    egui::Id::new("slides_panel_rows").with(ctx.viewport_id())
}

fn rename_id() -> egui::Id {
    egui::Id::new("slides_panel_rename")
}

/// The panel.
pub(super) fn render_slides_panel(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    // What of the panel shows — it scrolls — this frame, and in which
    // layer: a drop lands on it only while it is drawn there, on top.
    let panel = ui.clip_rect().intersect(ui.max_rect());
    let frame = ui.ctx().cumulative_frame_nr();
    let layer = ui.layer_id();
    let all = inputs(data);
    let chosen = ui
        .ctx()
        .data(|d| d.get_temp::<String>(chosen_id()))
        .and_then(|uuid| all.iter().position(|i| i.deck.uuid == uuid))
        .unwrap_or(0);
    let rect_id = panel_rect_id(ui.ctx());
    ui.ctx().data_mut(|d| {
        d.insert_temp(
            rect_id,
            (panel, all.get(chosen).map(|i| i.deck.uuid.clone()), frame, layer),
        );
    });
    let Some(input) = all.get(chosen).copied() else {
        render_empty(ui);
        return;
    };
    if all.len() > 1 {
        ui.horizontal(|ui| {
            ui.label("Slides input");
            egui::ComboBox::from_id_salt("slides_panel_pick")
                .selected_text(&input.channel.name)
                .show_ui(ui, |ui| {
                    for (i, other) in all.iter().enumerate() {
                        if ui.selectable_label(i == chosen, &other.channel.name).clicked() {
                            ui.ctx()
                                .data_mut(|d| d.insert_temp(chosen_id(), other.deck.uuid.clone()));
                        }
                    }
                });
        });
    }
    render_now_and_next(ui, data, input, actions);
    ui.add_space(6.0);
    render_settings(ui, input, actions);
    render_pictures(ui, input, actions);
    render_clicker(ui, &data.clicker, actions);
    ui.separator();
    render_list(ui, input, actions);
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui
            .button("+ Add files…")
            .on_hover_text("PowerPoint decks, PDFs, web pages (.html), clips and pictures, as talks")
            .clicked()
        {
            choose_files(ui.ctx(), Some(input.deck.uuid.clone()), None);
        }
        // Check again at the right end first, the hint cut short to what is
        // left: a narrow panel hides neither button.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("Check again")
                .on_hover_text(
                    "Look the talks' files over again — after copying a video beside its deck, \
                     or installing a font",
                )
                .clicked()
            {
                actions.commands.push(EngineCommand::CheckTalks {
                    deck_uuid: Some(input.deck.uuid.clone()),
                });
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(
                            "or drop files here — between two talks to put them there",
                        )
                        .small()
                        .weak(),
                    )
                    .truncate(),
                );
            });
        });
    });
    let s = input.slides;
    if s.live.is_some() || s.starting.is_some() {
        // The talk's clock, and a talk starting.
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

/// No Slides input yet: what the panel is for, and how to start.
fn render_empty(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.label(egui::RichText::new("Presentations").heading());
    ui.label(
        "The day's talks in running order, in one input: PowerPoint decks, PDFs, web \
         presentations, clips and pictures. Add them before the show, start each with one \
         press. Whatever shows the Slides input — a picture-in-picture, a presenter view — \
         follows the talk on.",
    );
    ui.add_space(8.0);
    if ui.button("+ Add files…").clicked() {
        choose_files(ui.ctx(), None, None);
    }
    ui.label(
        egui::RichText::new("or drop them here: a Slides input is made for them")
            .small()
            .weak(),
    );
}

/// "4 / 31", or what the talk waits on.
fn position_text(talk: &TalkSnapshot) -> String {
    let at = match (talk.position, talk.shown) {
        (Some(at), Some(of)) => format!("{at} / {of}"),
        (Some(at), None) => at.to_string(),
        _ => String::new(),
    };
    if let Some(clip) = talk.clip {
        return format!("{} / {}", clock(clip.at), clock(clip.length));
    }
    at
}

fn clock(secs: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = secs.max(0.0).floor() as u64;
    if whole >= 3600 {
        format!("{}:{:02}:{:02}", whole / 3600, whole / 60 % 60, whole % 60)
    } else {
        format!("{}:{:02}", whole / 60, whole % 60)
    }
}

fn kind_label(kind: Option<TalkKind>) -> &'static str {
    kind.map_or("—", TalkKind::label)
}

/// The talk on, with its picture and what moves it on; the talk coming
/// next, and Start next talk.
fn render_now_and_next(ui: &mut egui::Ui, data: &UIData, input: Input<'_>, actions: &mut UIActions) {
    let wide = ui.available_width() > 560.0;
    if wide {
        ui.columns(2, |cols| {
            now_box(&mut cols[0], data, input, actions);
            next_box(&mut cols[1], data, input, actions);
        });
    } else {
        now_box(ui, data, input, actions);
        next_box(ui, data, input, actions);
    }
}

/// The talk on: its picture, where it is, what moves it on, its notes.
fn now_box(ui: &mut egui::Ui, data: &UIData, input: Input<'_>, actions: &mut UIActions) {
    let s = input.slides;
    let deck = &input.deck.uuid;
    // The talk on as it plays — in the list or not.
    let live = s.on.as_ref();
    let starting = s.starting.and_then(|i| s.talks.get(i));
    {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (text, colour) = match &input.channel.on_air {
                    Some(place) => (format!("● ON AIR · {place}"), ON_AIR_RED),
                    None => ("ON · off air".to_string(), ui.visuals().weak_text_color()),
                };
                ui.label(egui::RichText::new(text).strong().color(colour));
                if let Some(secs) = s.talk_secs {
                    ui.label(egui::RichText::new(clock(secs)).monospace());
                }
            });
            ui.horizontal_top(|ui| {
                // Beside what is said of the talk: the list stays in view
                // in a narrow panel.
                let width = (ui.available_width() * 0.42).clamp(140.0, 360.0);
                // The show's own shape, in a box as wide as it is high.
                let size = super::utils::preview_size(
                    egui::vec2(width, width),
                    data.render_width,
                    data.render_height,
                );
                let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
                if let Some(&tex) = data.deck_preview_textures.get(deck) {
                    ui.painter().image(
                        tex,
                        rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    );
                } else {
                    ui.painter()
                        .rect_filled(rect, 2.0, egui::Color32::from_rgb(20, 20, 26));
                }
                ui.vertical(|ui| {
                    match live {
                        Some(talk) => {
                            ui.label(
                                egui::RichText::new(match s.live {
                                    Some(i) => format!("{} · {}", i + 1, talk.name),
                                    None => format!("{} · out of the list", talk.name),
                                })
                                .strong(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(kind_label(talk.kind)).small().weak());
                                let at = position_text(talk);
                                if !at.is_empty() {
                                    ui.label(egui::RichText::new(at).monospace());
                                }
                                if talk
                                    .presentation
                                    .as_ref()
                                    .is_some_and(|p| p.screen == Screen::Ended)
                                {
                                    ui.label(egui::RichText::new("end").color(WARN));
                                }
                            });
                        }
                        None => {
                            ui.label(egui::RichText::new("No talk on").weak());
                        }
                    }
                    if let Some(talk) = starting {
                        ui.label(
                            egui::RichText::new(format!("Starting {}…", talk.name)).color(WARN),
                        )
                        .on_hover_text(
                            "What is on stays on until the talk's picture is there: PowerPoint \
                             starts its show in about half a second",
                        );
                    }
                    render_moves(ui, input, live, actions);
                    if let Some(p) = live.and_then(|t| t.presentation.as_ref()) {
                        if let Some(title) = &p.title {
                            ui.label(egui::RichText::new(title).small().strong());
                        }
                        if !p.notes.trim().is_empty() {
                            egui::ScrollArea::vertical()
                                .id_salt("slides_panel_notes")
                                .max_height(60.0)
                                .show(ui, |ui| {
                                    ui.label(egui::RichText::new(&p.notes).small());
                                });
                        }
                    }
                });
            });
        });
    }
}

/// The talk coming next, Start next talk, and back to the one before.
fn next_box(ui: &mut egui::Ui, data: &UIData, input: Input<'_>, actions: &mut UIActions) {
    let s = input.slides;
    let deck = &input.deck.uuid;
    let next = s.next.and_then(|i| s.talks.get(i));
    {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("NEXT").strong());
                if s.next_picked {
                    ui.label(egui::RichText::new("picked").small().weak())
                        .on_hover_text("Picked to come next by hand");
                }
            });
            match next {
                Some(talk) => {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} · {}",
                            s.next.map_or(0, |i| i + 1),
                            talk.name
                        ))
                        .strong()
                        .size(15.0),
                    );
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(kind_label(talk.kind)).small().weak());
                        state_label(ui, talk);
                    });
                }
                None => {
                    ui.label(egui::RichText::new("Nothing comes next").weak());
                }
            }
            let start = ui
                .add_enabled(
                    next.is_some() && !data.keyboard_learn_active,
                    egui::Button::new(
                        egui::RichText::new("▶  Start next talk")
                            .size(16.0)
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(READY_GREEN.gamma_multiply(0.8))
                    .min_size(egui::vec2(ui.available_width(), 36.0)),
                )
                .on_hover_text(
                    "Start the talk marked NEXT: what is on stays on until its picture is \
                     there, then the Slides input goes to it — on air too",
                );
            if start.clicked() {
                actions.commands.push(EngineCommand::StartTalk {
                    deck_uuid: Some(deck.clone()),
                    to: TalkRef::Next,
                });
            }
            if s.previous.is_some()
                && ui
                    .button("⏮ Previous talk")
                    .on_hover_text("Back to the talk on before this one, at the slide it was on")
                    .clicked()
            {
                actions.commands.push(EngineCommand::StartTalk {
                    deck_uuid: Some(deck.clone()),
                    to: TalkRef::Previous,
                });
            }
        });
    }
}

/// ◀ ▶, black and white — what a clicker does — and back to the first slide,
/// for the talk on.
fn render_moves(
    ui: &mut egui::Ui,
    input: Input<'_>,
    live: Option<&TalkSnapshot>,
    actions: &mut UIActions,
) {
    let deck = &input.deck.uuid;
    let steps = live.and_then(|t| t.kind).is_some_and(TalkKind::steps);
    // Wrapped: in a narrow panel Black and White go under ◀ ▶, not out of
    // sight.
    ui.horizontal_wrapped(|ui| {
        let size = egui::vec2(44.0, 28.0);
        for (text, to, hover) in [
            ("⏮", Go::First, "The first slide: the talk from its start"),
            ("◀", Go::Previous, "Back a step (PageUp)"),
            ("▶", Go::Next, "On a step: the next animation or slide (PageDown)"),
        ] {
            if ui
                .add_enabled(steps, egui::Button::new(egui::RichText::new(text).size(16.0)).min_size(size))
                .on_hover_text(hover)
                .clicked()
            {
                actions.commands.push(EngineCommand::PresentationGo {
                    deck_uuid: Some(deck.clone()),
                    to,
                });
            }
        }
        // What Nida puts over the talk, whatever plays it.
        let screen = Some(input.slides.screen);
        for (text, which, hover) in [
            ("Black", Screen::Black, "Black over the talk, and back (B)"),
            ("White", Screen::White, "White over the talk, and back"),
        ] {
            let on = screen == Some(which);
            if ui
                .add_enabled(
                    live.is_some(),
                    egui::Button::new(text).selected(on).min_size(egui::vec2(0.0, size.y)),
                )
                .on_hover_text(hover)
                .clicked()
            {
                actions.commands.push(EngineCommand::TogglePresentationScreen {
                    deck_uuid: Some(deck.clone()),
                    screen: which,
                });
            }
        }
    });
}

/// How a talk stands, in a word or two.
fn state_label(ui: &mut egui::Ui, talk: &TalkSnapshot) {
    let (text, colour) = match talk.state {
        TalkState::Live => ("● on".to_string(), ON_AIR_RED),
        TalkState::Starting => ("starting…".to_string(), WARN),
        TalkState::Ready => ("✔ ready".to_string(), READY_GREEN),
        TalkState::Waiting => ("waiting".to_string(), ui.visuals().weak_text_color()),
        TalkState::Failed => ("⚠ cannot play".to_string(), FAILED_RED),
    };
    let label = ui.label(egui::RichText::new(text).small().color(colour));
    if let Some(why) = &talk.message {
        label.on_hover_text(why);
    }
}

/// What may go wrong with a talk's file in the show, when anything: "⚠ check",
/// pressed to say it under the talk's row (`open`), and said on hover. Gives
/// back whether it was pressed.
fn warnings_button(ui: &mut egui::Ui, talk: &TalkSnapshot, open: bool) -> bool {
    if talk.warnings.is_empty() {
        return false;
    }
    ui.add(
        egui::Button::new(egui::RichText::new("⚠ check").small().color(WARN))
            .small()
            .selected(open),
    )
    .on_hover_text(format!(
        "{}\n\nIt plays all the same. Press to see it under the talk.",
        talk.warnings.join("\n")
    ))
    .clicked()
}

/// What the check found in a talk's file, under its row: each thing, and
/// what to do about it.
fn warnings_said(ui: &mut egui::Ui, talk: &TalkSnapshot) {
    for warning in &talk.warnings {
        ui.label(egui::RichText::new(warning).small().color(WARN));
    }
    ui.label(
        egui::RichText::new("It plays all the same. Put it right, then press Check again.")
            .small()
            .weak(),
    );
}

/// From one talk to the next: cut, fade or slide, and how long.
fn render_settings(ui: &mut egui::Ui, input: Input<'_>, actions: &mut UIActions) {
    let settings = &input.slides.settings;
    let deck = &input.deck.uuid;
    // Wrapped: a narrow panel puts + Presenter view under, not out of sight.
    ui.horizontal_wrapped(|ui| {
        ui.label("Between talks");
        let mut chosen = settings.transition;
        egui::ComboBox::from_id_salt("slides_panel_transition")
            .selected_text(chosen.label())
            .show_ui(ui, |ui| {
                for t in PlaylistTransition::ALL {
                    ui.selectable_value(&mut chosen, t, t.label());
                }
            });
        let mut secs = settings.transition_secs;
        let timed = chosen != PlaylistTransition::Cut;
        let dragged = ui
            .add_enabled(
                timed,
                egui::DragValue::new(&mut secs)
                    .range(0.0..=MAX_TALK_TRANSITION_SECS)
                    .speed(0.05)
                    .suffix(" s"),
            )
            .changed();
        if chosen != settings.transition || dragged {
            actions.commands.push(EngineCommand::SetSlidesSettings {
                deck_uuid: deck.clone(),
                settings: SlidesSettings {
                    transition: chosen,
                    transition_secs: secs,
                    ..settings.clone()
                },
            });
        }
        if ui
            .button("+ Presenter view")
            .on_hover_text(
                "An input for the speaker's screen: the talk on, its notes and clocks — \
                 following each talk as it comes on",
            )
            .clicked()
        {
            actions.commands.push(EngineCommand::AddInput {
                source: InputSource::PresenterView {
                    deck_uuid: deck.clone(),
                },
                name: None,
                audio: None,
            });
        }
    });
}

/// PowerPoint talks' slides saved as pictures, for the presenter view's
/// next slide: how many have them, and the button that has PowerPoint save
/// the rest — not while a PowerPoint talk is on, as PowerPoint may ask for
/// access first and holds its show till answered. Only with PowerPoint
/// talks in the list.
fn render_pictures(ui: &mut egui::Ui, input: Input<'_>, actions: &mut UIActions) {
    let s = input.slides;
    let decks: Vec<&TalkSnapshot> = s
        .talks
        .iter()
        .filter(|t| t.kind == Some(TalkKind::PowerPoint) && !t.missing)
        .collect();
    if decks.is_empty() {
        return;
    }
    let with = decks.iter().filter(|t| t.pictures).count();
    let on = s
        .live
        .and_then(|i| s.talks.get(i))
        .or(s.on.as_ref())
        .is_some_and(|t| t.kind == Some(TalkKind::PowerPoint));
    ui.horizontal_wrapped(|ui| {
        ui.label("Slide pictures");
        let can = !on && s.making_pictures == 0 && with < decks.len();
        let button = ui
            .add_enabled(can, egui::Button::new("Make"))
            .on_hover_text(
                "PowerPoint saves each PowerPoint talk's slides as pictures, so the presenter \
                 view shows the next slide. The first time, PowerPoint asks for access to \
                 Nida's folder for them: click Grant Access there.",
            )
            .on_disabled_hover_text(if on {
                "With no PowerPoint talk on: PowerPoint may ask for access first, and holds its \
                 show till that is answered"
            } else if s.making_pictures > 0 {
                "PowerPoint is making them"
            } else {
                "Every PowerPoint talk has them"
            });
        if button.clicked() {
            actions.commands.push(EngineCommand::MakeSlidePictures {
                deck_uuid: Some(input.deck.uuid.clone()),
            });
        }
        let said = if s.making_pictures > 0 {
            format!("making {}… ({with} of {} done)", s.making_pictures, decks.len())
        } else {
            format!("{with} of {} PowerPoint talks", decks.len())
        };
        ui.label(egui::RichText::new(said).small().weak());
    });
}

/// The clicker heard with another app in front: learned with one press of
/// a button on it — which device it is — and how macOS stands.
fn render_clicker(
    ui: &mut egui::Ui,
    clicker: &crate::clicker::ClickerStatus,
    actions: &mut UIActions,
) {
    use crate::clicker::{Access, ClickerAsk};
    let ask = |actions: &mut UIActions, ask| actions.session.clicker_ask = Some(ask);
    // How to let Nida hear it, once macOS has said no.
    let allow = |ui: &mut egui::Ui| {
        ui.label(
            egui::RichText::new(
                "allow Nida in System Settings → Privacy & Security → Input Monitoring, then \
                 quit and reopen Nida",
            )
            .small()
            .color(WARN),
        );
    };
    ui.horizontal_wrapped(|ui| {
        ui.label("Clicker");
        if clicker.learning {
            if clicker.access == Access::Denied {
                ui.label(
                    egui::RichText::new("macOS does not let Nida hear it yet:")
                        .small()
                        .color(WARN),
                );
                allow(ui);
            } else {
                let text = if clicker.mac_keyboard {
                    "That was the Mac's own keyboard: press a button on the clicker"
                } else {
                    "Press a button on the clicker…"
                };
                ui.label(egui::RichText::new(text).small().color(WARN));
            }
            if ui.small_button("Cancel").clicked() {
                ask(actions, ClickerAsk::Cancel);
            }
            return;
        }
        if let Some(device) = &clicker.device {
            let name = if device.name.is_empty() {
                "learned"
            } else {
                device.name.as_str()
            };
            ui.label(egui::RichText::new(name).small().strong());
            if clicker.access == Access::Granted {
                ui.label(
                    egui::RichText::new("— works with another app in front")
                        .small()
                        .weak(),
                )
                .on_hover_text(
                    "Its keys move the talk on air whatever app is in front — and do nothing \
                     in the others while Nida runs (Safari, Finder, PowerPoint itself): Nida \
                     has macOS turn them into keys it gives no app, for this clicker only. \
                     Forget gives them back. Should Nida stop unexpectedly, unplug and plug in \
                     the clicker's receiver to give them back.",
                );
            } else {
                // A Nida new to macOS — an update — is asked again.
                ui.label(
                    egui::RichText::new("— heard with Nida in front only, until macOS allows it")
                        .small()
                        .color(WARN),
                );
                if clicker.access == Access::Denied {
                    allow(ui);
                } else if ui
                    .small_button("Allow")
                    .on_hover_text(
                        "macOS asks to let Nida use Input Monitoring; then a button on the \
                         clicker",
                    )
                    .clicked()
                {
                    ask(actions, ClickerAsk::Learn);
                }
            }
            if ui
                .small_button("Forget")
                .on_hover_text("Its keys move the talk only with Nida in front")
                .clicked()
            {
                ask(actions, ClickerAsk::Forget);
            }
            return;
        }
        if ui
            .small_button("Learn clicker")
            .on_hover_text(
                "So the clicker moves the talk with another app in front — Finder, a browser: \
                 press this, then a button on the clicker. Only the clicker is heard, never the \
                 keyboard. macOS asks once to let Nida use Input Monitoring.",
            )
            .clicked()
        {
            ask(actions, ClickerAsk::Learn);
        }
        ui.label(
            egui::RichText::new("heard with Nida in front only")
                .small()
                .weak(),
        );
    });
}

/// A talk being renamed: in which input, which talk, the text so far, and
/// whether the field has been given the keyboard yet (once: asked each
/// frame, it could never lose it).
#[derive(Clone)]
struct Renaming {
    deck: String,
    talk: String,
    text: String,
    focused: bool,
}

fn edit_talk(deck: &str, talk: &str, name: Option<String>, path: Option<String>) -> EngineCommand {
    EngineCommand::EditTalk {
        deck_uuid: deck.to_string(),
        talk_id: talk.to_string(),
        name,
        path,
    }
}

/// The running order: each talk, how it stands, and what can be done to it.
fn render_list(ui: &mut egui::Ui, input: Input<'_>, actions: &mut UIActions) {
    let s = input.slides;
    let deck = &input.deck.uuid;
    ui.label(egui::RichText::new("Running order").strong());
    if s.talks.is_empty() {
        ui.label(
            egui::RichText::new("No talks yet: add files, or drop them here")
                .small()
                .weak(),
        );
    }
    let mut renaming: Option<Renaming> = ui.ctx().data(|d| d.get_temp(rename_id()));
    let mut rows: Vec<egui::Rect> = Vec::with_capacity(s.talks.len());
    let count = s.talks.len();
    for (i, talk) in s.talks.iter().enumerate() {
        let live = s.live == Some(i);
        let next = s.next == Some(i);
        let fill = if live {
            ON_AIR_RED.gamma_multiply(0.25)
        } else if next {
            READY_GREEN.gamma_multiply(0.15)
        } else {
            egui::Color32::TRANSPARENT
        };
        let mut menu: Option<egui::Response> = None;
        // What the check found, said under the row once ⚠ check is pressed.
        let said_id = egui::Id::new(("slides_talk_checks", deck.as_str(), talk.id.as_str()));
        let said = !talk.warnings.is_empty()
            && ui.ctx().data(|d| d.get_temp::<bool>(said_id)).unwrap_or(false);
        let mut pressed = false;
        let row = egui::Frame::NONE
            .fill(fill)
            .inner_margin(egui::Margin::symmetric(4, 2))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let mark = if live {
                        "▶"
                    } else if s.starting == Some(i) {
                        "…"
                    } else if next {
                        "NEXT"
                    } else {
                        ""
                    };
                    ui.add_sized(
                        egui::vec2(34.0, 18.0),
                        egui::Label::new(egui::RichText::new(mark).small().strong()),
                    );
                    ui.label(egui::RichText::new(format!("{}", i + 1)).monospace());
                    // The row's right end first — its buttons, how the talk
                    // stands — so a narrow panel never hides them; the name
                    // takes what is left, cut short to fit.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .small_button("🗑")
                            .on_hover_text("Take it out of the list (Undo puts it back)")
                            .clicked()
                        {
                            actions.commands.push(EngineCommand::RemoveTalk {
                                deck_uuid: deck.clone(),
                                talk_id: talk.id.clone(),
                            });
                        }
                        if ui
                            .add_enabled(i + 1 < count, egui::Button::new("⏷").small())
                            .on_hover_text("Later in the running order")
                            .clicked()
                        {
                            actions.commands.push(EngineCommand::MoveTalk {
                                deck_uuid: deck.clone(),
                                talk_id: talk.id.clone(),
                                to: i + 1,
                            });
                        }
                        if ui
                            .add_enabled(i > 0, egui::Button::new("⏶").small())
                            .on_hover_text("Earlier in the running order")
                            .clicked()
                        {
                            actions.commands.push(EngineCommand::MoveTalk {
                                deck_uuid: deck.clone(),
                                talk_id: talk.id.clone(),
                                to: i - 1,
                            });
                        }
                        if ui
                            .add_enabled(!live, egui::Button::new("▶").small())
                            .on_hover_text("Start this talk now")
                            .clicked()
                        {
                            actions.commands.push(EngineCommand::StartTalk {
                                deck_uuid: Some(deck.clone()),
                                to: TalkRef::Id(talk.id.clone()),
                            });
                        }
                        pressed = warnings_button(ui, talk, said);
                        if !live {
                            state_label(ui, talk);
                        }
                        let at = position_text(talk);
                        if !at.is_empty() {
                            ui.label(egui::RichText::new(at).small().monospace());
                        }
                        ui.label(egui::RichText::new(kind_label(talk.kind)).small().weak());
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            if let Some(r) = renaming
                                .as_mut()
                                .filter(|r| r.deck == *deck && r.talk == talk.id)
                            {
                                let edit = ui.add(
                                    egui::TextEdit::singleline(&mut r.text)
                                        .id_salt(("slides_rename", deck.as_str(), talk.id.as_str()))
                                        .hint_text("The speaker, the talk")
                                        .desired_width(ui.available_width()),
                                );
                                if !r.focused {
                                    edit.request_focus();
                                    r.focused = true;
                                }
                                let done = ui.input(|i| i.key_pressed(egui::Key::Enter));
                                let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
                                if cancel {
                                    renaming = None;
                                } else if done || edit.lost_focus() {
                                    let name = r.text.trim().to_string();
                                    actions
                                        .commands
                                        .push(edit_talk(deck, &talk.id, Some(name), None));
                                    renaming = None;
                                }
                            } else {
                                let name = ui
                                    .add(
                                        egui::Label::new(egui::RichText::new(&talk.name).strong())
                                            .truncate()
                                            .sense(egui::Sense::click()),
                                    )
                                    .on_hover_text(format!(
                                        "{}\n{}\nDouble-click to start it now; right-click for more",
                                        talk.name, talk.path
                                    ));
                                if name.double_clicked() {
                                    actions.commands.push(EngineCommand::StartTalk {
                                        deck_uuid: Some(deck.clone()),
                                        to: TalkRef::Id(talk.id.clone()),
                                    });
                                }
                                menu = Some(name);
                            }
                        });
                    });
                });
                if said {
                    warnings_said(ui, talk);
                }
            })
            .response;
        if pressed {
            ui.ctx().data_mut(|d| d.insert_temp(said_id, !said));
        }
        // On the talk's name: the row's buttons stay pressable.
        let Some(menu) = menu else {
            rows.push(row.rect);
            continue;
        };
        menu.context_menu(|ui| {
            if ui.button("Start now").clicked() {
                actions.commands.push(EngineCommand::StartTalk {
                    deck_uuid: Some(deck.clone()),
                    to: TalkRef::Id(talk.id.clone()),
                });
                ui.close();
            }
            if ui
                .add_enabled(!live, egui::Button::new("Start this next"))
                .on_hover_text("Mark it NEXT: Start next talk takes it")
                .clicked()
            {
                actions.commands.push(EngineCommand::SetNextTalk {
                    deck_uuid: deck.clone(),
                    talk_id: Some(talk.id.clone()),
                });
                ui.close();
            }
            if s.next_picked
                && ui
                    .button("Next in order again")
                    .on_hover_text("NEXT back to the talk after the one on")
                    .clicked()
            {
                actions.commands.push(EngineCommand::SetNextTalk {
                    deck_uuid: deck.clone(),
                    talk_id: None,
                });
                ui.close();
            }
            ui.separator();
            if ui.button("Rename…").clicked() {
                renaming = Some(Renaming {
                    deck: deck.clone(),
                    talk: talk.id.clone(),
                    text: talk.given_name.clone(),
                    focused: false,
                });
                ui.close();
            }
            if ui
                .button("Replace file…")
                .on_hover_text("A speaker's new version: the talk keeps its place and name")
                .clicked()
            {
                choose_files(ui.ctx(), Some(deck.clone()), Some(talk.id.clone()));
                ui.close();
            }
            if ui.button("Show in Finder").clicked() {
                actions.session.reveal_file = Some(PathBuf::from(&talk.path));
                ui.close();
            }
            ui.separator();
            if ui.button("Remove").clicked() {
                actions.commands.push(EngineCommand::RemoveTalk {
                    deck_uuid: deck.clone(),
                    talk_id: talk.id.clone(),
                });
                ui.close();
            }
        });
        rows.push(row.rect);
    }
    let rows_id = rows_id(ui.ctx());
    ui.ctx().data_mut(|d| {
        match renaming {
            Some(r) => {
                d.insert_temp(rename_id(), r);
            }
            None => d.remove::<Renaming>(rename_id()),
        }
        d.insert_temp(rows_id, rows);
    });
}

// ── Files ───────────────────────────────────────────────────────────

/// What the dialog was opened for: talks for a Slides input (or a new one),
/// or another file for a talk.
#[derive(Clone)]
struct ChooseFor {
    deck: Option<String>,
    replace: Option<String>,
}

type Chosen = Arc<Mutex<Option<(ChooseFor, mpsc::Receiver<Vec<PathBuf>>)>>>;

fn chosen_files_id() -> egui::Id {
    egui::Id::new("slides_panel_chosen_files")
}

/// The file extensions a talk can be.
fn talk_extensions() -> Vec<&'static str> {
    let mut all: Vec<&str> = POWERPOINT_EXTENSIONS.to_vec();
    all.extend(["pdf", "html", "htm"]);
    all.extend(crate::engine::value::media::VIDEOS);
    all.extend(crate::engine::value::media::SOUNDS);
    all.extend(crate::engine::value::media::IMAGES);
    all
}

/// Ask for files: talks for `deck` (or a new Slides input), or — `replace`
/// — one file in place of that talk's. The dialog runs on a thread.
fn choose_files(ctx: &egui::Context, deck: Option<String>, replace: Option<String>) {
    let (tx, rx) = mpsc::channel();
    let one = replace.is_some();
    let spawned = std::thread::Builder::new()
        .name("slides-files".into())
        .spawn(move || {
            let dialog = rfd::FileDialog::new()
                .set_title(if one { "The talk's new file" } else { "Talks" })
                .add_filter("Presentations, clips, pictures", &talk_extensions())
                .add_filter("PowerPoint", POWERPOINT_EXTENSIONS)
                .add_filter("PDF", &["pdf"])
                .add_filter("Web page", &["html", "htm"]);
            let picked = if one {
                dialog.pick_file().map(|p| vec![p])
            } else {
                dialog.pick_files()
            };
            if let Some(paths) = picked {
                let _ = tx.send(paths);
            }
        });
    if spawned.is_ok() {
        let chosen: Chosen = Arc::new(Mutex::new(Some((ChooseFor { deck, replace }, rx))));
        ctx.data_mut(|d| d.insert_temp(chosen_files_id(), chosen));
    }
}

/// Ask for talks for the show's Slides input — made when there is none.
pub(super) fn choose_talks(ctx: &egui::Context) {
    choose_files(ctx, None, None);
}

/// The files the dialog came back with: added as talks, or in place of a
/// talk's.
pub(super) fn take_chosen_files(ctx: &egui::Context, actions: &mut UIActions) {
    let Some(chosen) = ctx.data(|d| d.get_temp::<Chosen>(chosen_files_id())) else {
        return;
    };
    let Ok(mut slot) = chosen.lock() else {
        return;
    };
    let answer = match slot.as_ref().map(|(_, rx)| rx.try_recv()) {
        Some(Ok(paths)) => Some(paths),
        Some(Err(mpsc::TryRecvError::Empty)) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        Some(Err(mpsc::TryRecvError::Disconnected)) | None => None,
    };
    let what = slot.take().map(|(what, _)| what);
    drop(slot);
    ctx.data_mut(|d| d.remove::<Chosen>(chosen_files_id()));
    let (Some(paths), Some(what)) = (answer, what) else {
        return;
    };
    let paths: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    match (what.replace, what.deck) {
        (Some(talk), Some(deck)) => {
            // The talk keeps its place, its name and its id.
            if let Some(path) = paths.into_iter().next() {
                actions.commands.push(edit_talk(&deck, &talk, None, Some(path)));
            }
        }
        (_, deck) => actions.commands.push(EngineCommand::AddTalks {
            deck_uuid: deck,
            paths,
            at: None,
        }),
    }
}

/// Files dropped on the panel: talks of its Slides input — put between the
/// rows they were dropped between — or of a new one. Gives back whether the
/// panel took them; while files hover over it, says where they will go.
pub(super) fn take_drop(
    ctx: &egui::Context,
    pos: Option<egui::Pos2>,
    hovering: usize,
    dropped: &[PathBuf],
    actions: &mut UIActions,
) -> bool {
    let Some(pos) = pos else {
        return false;
    };
    let rect_id = panel_rect_id(ctx);
    let Some((panel, deck, drawn, layer)) =
        ctx.data(|d| d.get_temp::<(egui::Rect, Option<String>, u64, egui::LayerId)>(rect_id))
    else {
        return false;
    };
    // Drawn this frame (drops are taken after the panels are drawn): a
    // panel closed, or behind another tab, takes nothing.
    if drawn != ctx.cumulative_frame_nr() || !panel.contains(pos) {
        return false;
    }
    // On top where the files are: a window over the panel — a playlist's,
    // an input's settings — takes them instead. (Over the panels
    // themselves no window is found.)
    if ctx.layer_id_at(pos).is_some_and(|top| top != layer) {
        return false;
    }
    // The id is had first: no call on `ctx` inside its own lock.
    let rows_id = rows_id(ctx);
    let rows: Vec<egui::Rect> = ctx.data(|d| d.get_temp(rows_id)).unwrap_or_default();
    let at = rows
        .iter()
        .position(|r| pos.y < r.center().y)
        .filter(|_| !rows.is_empty());
    if hovering > 0 && dropped.is_empty() {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("slides_drop"),
        ));
        painter.rect_stroke(
            panel.shrink(2.0),
            4.0,
            egui::Stroke::new(2.0, READY_GREEN),
            egui::StrokeKind::Inside,
        );
        let y = match at {
            Some(i) => rows[i].top(),
            None => rows
                .last()
                .map_or(panel.top() + 40.0, egui::Rect::bottom),
        };
        painter.hline(panel.x_range(), y, egui::Stroke::new(2.0, READY_GREEN));
        let text = if hovering == 1 {
            "1 file → a talk here".to_string()
        } else {
            format!("{hovering} files → talks here")
        };
        painter.text(
            egui::pos2(panel.center().x, y - 4.0),
            egui::Align2::CENTER_BOTTOM,
            text,
            egui::FontId::proportional(14.0),
            READY_GREEN,
        );
    }
    if !dropped.is_empty() {
        let files = crate::deck::media::expand_folders(dropped);
        let paths: Vec<String> = files
            .iter()
            .filter(|p| plays_here(TalkKind::of(&p.to_string_lossy())))
            .map(|p| p.to_string_lossy().into_owned())
            .collect();
        if !paths.is_empty() {
            actions.commands.push(EngineCommand::AddTalks {
                deck_uuid: deck,
                paths,
                at,
            });
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use egui_kittest::Harness;

    fn talk(id: &str, name: &str, kind: TalkKind, state: TalkState) -> TalkSnapshot {
        TalkSnapshot {
            id: id.into(),
            name: name.into(),
            given_name: String::new(),
            path: format!("/Talks/{name}"),
            kind: Some(kind),
            state,
            message: None,
            missing: false,
            position: None,
            shown: None,
            presentation: None,
            clip: None,
            warnings: Vec::new(),
            pictures: false,
        }
    }

    /// Three talks: a PowerPoint deck on (slide 3 of 22), a PDF next and
    /// ready, a picture waiting.
    fn with_slides() -> UIData {
        let mut data = UIData::test_fixture();
        let mut on = talk("t1", "Keynote", TalkKind::PowerPoint, TalkState::Live);
        on.position = Some(3);
        on.shown = Some(22);
        data.channels[0].decks[0].slides = Some(SlidesSnapshot {
            talks: vec![
                on.clone(),
                talk("t2", "Results", TalkKind::Pdf, TalkState::Ready),
                talk("t3", "Break", TalkKind::Picture, TalkState::Waiting),
            ],
            settings: SlidesSettings::default(),
            live: Some(0),
            on: Some(on),
            screen: Screen::Slide,
            starting: None,
            next: Some(1),
            next_picked: false,
            previous: None,
            talk_secs: Some(65.0),
            making_pictures: 0,
        });
        data.channels[0].on_air = Some("Main".into());
        data.keymap_bindings = crate::keymap::KeymapStore::defaults();
        data
    }

    fn harness(data: &UIData) -> Harness<'_, Vec<EngineCommand>> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(760.0, 900.0))
            .build_ui_state(
                |ui, sent: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    super::super::presentation::clicker_shortcuts(ui, data, &mut actions);
                    render_slides_panel(ui, data, &mut actions);
                    sent.append(&mut actions.commands);
                },
                Vec::new(),
            )
    }

    fn sent(harness: &Harness<'_, Vec<EngineCommand>>) -> String {
        format!("{:?}", harness.state())
    }

    #[test]
    fn the_panel_shows_the_talk_on_and_starts_the_next() {
        let data = with_slides();
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = harness(&data);
        harness.get_by_label("● ON AIR · Main");
        harness.get_by_label("1:05");
        harness.get_by_label("1 · Keynote");
        harness.get_by_label("2 · Results");
        harness.get_by_label("▶  Start next talk").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            format!("[StartTalk {{ deck_uuid: Some({deck:?}), to: Next }}]")
        );
    }

    /// PowerPoint talks: a row says how many have their slides as pictures,
    /// and Make asks for the rest — not while a PowerPoint talk is on (it
    /// would hold the show), nor with every one made. No PowerPoint talks,
    /// no row.
    #[test]
    fn slide_pictures_are_asked_from_their_row_with_no_powerpoint_talk_on() {
        let mut data = with_slides();
        let deck = data.channels[0].decks[0].uuid.clone();
        let make = |data: &UIData| {
            let mut harness = harness(data);
            harness.run_steps(2);
            let shown = ["0 of 2 PowerPoint talks", "1 of 2 PowerPoint talks"]
                .into_iter()
                .find(|said| harness.query_by_label(said).is_some())
                .map(str::to_string);
            let enabled = harness
                .query_by_label("Make")
                .map(|n| !n.accesskit_node().is_disabled());
            if enabled == Some(true) {
                harness.get_by_label("Make").click();
                harness.run_steps(2);
            }
            (shown, enabled, sent(&harness))
        };
        if let Some(s) = data.channels[0].decks[0].slides.as_mut() {
            s.talks.push(talk("t4", "Closing", TalkKind::PowerPoint, TalkState::Ready));
        }
        // The keynote — PowerPoint — on: not now.
        let (shown, enabled, sent_now) = make(&data);
        assert_eq!(shown.as_deref(), Some("0 of 2 PowerPoint talks"));
        assert_eq!(enabled, Some(false));
        assert!(!sent_now.contains("MakeSlidePictures"), "{sent_now}");
        // Nothing on: asked, of this Slides input.
        if let Some(s) = data.channels[0].decks[0].slides.as_mut() {
            s.live = None;
            s.on = None;
            s.talks[0].state = TalkState::Ready;
            s.talks[0].pictures = true;
        }
        let (shown, enabled, sent_now) = make(&data);
        assert_eq!(shown.as_deref(), Some("1 of 2 PowerPoint talks"));
        assert_eq!(enabled, Some(true));
        assert_eq!(
            sent_now,
            format!("[MakeSlidePictures {{ deck_uuid: Some({deck:?}) }}]")
        );
        // No PowerPoint talks: no row.
        if let Some(s) = data.channels[0].decks[0].slides.as_mut() {
            s.talks.retain(|t| t.kind != Some(TalkKind::PowerPoint));
        }
        let mut harness = harness(&data);
        harness.run_steps(2);
        assert!(harness.query_by_label("Slide pictures").is_none());
    }

    /// A panel narrower than a talk's long name: each row keeps its buttons
    /// and how it stands in sight — the name cut short — and ◀ ▶, Black and
    /// White, + Presenter view and Check again stay in sight too.
    #[test]
    fn a_narrow_panel_hides_no_button() {
        let mut data = with_slides();
        let long = "Efektyvesnis darbas su dirbtinio intelekto (DI) įrankiais_Daugiamodaline \
                    analize ir automatizavimas, antra dalis";
        if let Some(s) = data.channels[0].decks[0].slides.as_mut() {
            s.talks[2].name = long.into();
            s.talks[2].warnings = vec!["a 4:3 talk in a 16:9 show: bars at its sides".into()];
        }
        for width in [360.0, 440.0] {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(width, 1200.0))
                .build_ui(|ui| {
                    let mut actions = UIActions::new();
                    render_slides_panel(ui, &data, &mut actions);
                });
            harness.run_steps(3);
            let inside = |rect: egui::Rect| rect.left() >= 0.0 && rect.right() <= width + 0.5;
            let starts: Vec<egui::Rect> = harness
                .get_all_by_label("▶")
                .map(|n| n.rect())
                .collect();
            assert!(starts.len() >= 3, "a ▶ for each row, and the talk's own");
            for rect in starts {
                assert!(inside(rect), "▶ at {rect:?} in a panel {width} wide");
            }
            for label in ["⏮", "⚠ check", "Black", "White", "+ Presenter view", "Check again"] {
                let rect = harness.get_by_label(label).rect();
                assert!(inside(rect), "{label} at {rect:?} in a panel {width} wide");
            }
        }
    }

    #[test]
    fn the_running_order_moves_and_picks_the_next_by_hand() {
        let data = with_slides();
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = harness(&data);
        // The first talk later in the running order: one undoable change,
        // by the talk's id — never the whole list as this frame saw it.
        harness.get_all_by_label("⏷").next().unwrap().click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            format!("[MoveTalk {{ deck_uuid: {deck:?}, talk_id: \"t1\", to: 1 }}]")
        );
        harness.state_mut().clear();
        // The break picked to come next, from its row's menu.
        harness.get_by_label("Break").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Start this next").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            format!("[SetNextTalk {{ deck_uuid: {deck:?}, talk_id: Some(\"t3\") }}]")
        );
    }

    #[test]
    fn a_rename_is_typed_once_focused_and_sent_by_the_talks_id() {
        let data = with_slides();
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = harness(&data);
        harness.get_by_label("Break").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Rename…").click();
        harness.run_steps(3);
        harness.get_by_role(egui::accesskit::Role::TextInput).type_text("Coffee");
        harness.run_steps(2);
        harness.key_press(egui::Key::Enter);
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            format!(
                "[EditTalk {{ deck_uuid: {deck:?}, talk_id: \"t3\", name: Some(\"Coffee\"), path: None }}]"
            )
        );
    }

    /// Clicking elsewhere keeps what was typed: the field is not given the
    /// keyboard back each frame.
    #[test]
    fn a_rename_is_kept_when_the_field_is_left() {
        let data = with_slides();
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = harness(&data);
        harness.get_by_label("Break").click_secondary();
        harness.run_steps(2);
        harness.get_by_label("Rename…").click();
        harness.run_steps(3);
        harness.get_by_role(egui::accesskit::Role::TextInput).type_text("Lunch");
        harness.run_steps(2);
        // Somewhere else in the panel.
        harness.get_by_label("1 · Keynote").click();
        harness.run_steps(3);
        assert_eq!(
            sent(&harness),
            format!(
                "[EditTalk {{ deck_uuid: {deck:?}, talk_id: \"t3\", name: Some(\"Lunch\"), path: None }}]"
            )
        );
    }

    #[test]
    fn a_talk_with_something_to_check_says_so_and_can_be_looked_over_again() {
        let mut data = with_slides();
        let slides = data.channels[0].decks[0].slides.as_mut().unwrap();
        slides.talks[1].warnings = vec!["a 4:3 talk in a 16:9 show: bars at its sides".into()];
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = harness(&data);
        harness.run_steps(2);
        assert_eq!(harness.get_all_by_label("⚠ check").count(), 1);
        harness.get_by_label("Check again").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            format!("[CheckTalks {{ deck_uuid: Some({deck:?}) }}]")
        );
    }

    /// ⚠ check pressed says what the check found under the talk — and what
    /// to do — till pressed again: not on hover only.
    #[test]
    fn a_talks_check_is_said_under_it_once_pressed() {
        let mut data = with_slides();
        let found = "a 4:3 talk in a 16:9 show: bars at its sides";
        if let Some(s) = data.channels[0].decks[0].slides.as_mut() {
            s.talks[1].warnings = vec![found.into()];
        }
        let mut harness = harness(&data);
        harness.run_steps(2);
        assert!(harness.query_by_label(found).is_none(), "not till pressed");
        harness.get_by_label("⚠ check").click();
        harness.run_steps(2);
        harness.get_by_label(found);
        harness.get_by_label("It plays all the same. Put it right, then press Check again.");
        harness.get_by_label("⚠ check").click();
        harness.run_steps(2);
        assert!(harness.query_by_label(found).is_none(), "pressed again: gone");
        assert_eq!(sent(&harness), "[]", "nothing asked of the engine");
    }

    /// A drop lands on the panel only in a frame it is drawn in, and only
    /// where no window is over it.
    #[test]
    fn a_panel_takes_a_drop_only_while_drawn_and_on_top() {
        let data = with_slides();
        let ctx = egui::Context::default();
        let inside = egui::pos2(300.0, 300.0);
        let frame = |panel: bool, window: bool| -> bool {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(900.0, 700.0),
                )),
                ..Default::default()
            };
            let mut took = false;
            let mut out = ctx.run_ui(input, |ui| {
                let mut actions = UIActions::new();
                if panel {
                    render_slides_panel(ui, &data, &mut actions);
                }
                if window {
                    // Over the point the files are at (its height is its
                    // content's).
                    egui::Window::new("A playlist")
                        .fixed_pos(egui::pos2(250.0, 280.0))
                        .fixed_size(egui::vec2(300.0, 300.0))
                        .show(ui.ctx(), |ui| {
                            ui.label("its list");
                        });
                }
                took = take_drop(ui.ctx(), Some(inside), 1, &[], &mut actions);
            });
            out.textures_delta.clear();
            took
        };
        assert!(frame(true, false), "drawn, nothing over it");
        assert!(!frame(false, false), "not drawn this frame (closed, behind a tab)");
        // A window over it: laid out the first frame it shows, there after.
        let _ = frame(true, true);
        assert!(!frame(true, true), "the window over it takes the files");
        // The window gone: egui knows it for a frame after.
        let _ = frame(true, false);
        assert!(frame(true, false), "the window gone");
    }

    /// The panel in a window of its own takes drops on that window, not
    /// those on the main window at the same point: where it is, is its own
    /// window's.
    #[test]
    fn a_panel_in_a_window_of_its_own_takes_no_drop_on_the_main_window() {
        let data = with_slides();
        let ctx = egui::Context::default();
        let inside = egui::pos2(300.0, 300.0);
        let window = egui::ViewportId::from_hash_of("its window");
        let input = |id: egui::ViewportId| egui::RawInput {
            viewport_id: id,
            viewports: std::iter::once((id, egui::ViewportInfo::default())).collect(),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            ..Default::default()
        };
        let mut took = (false, false);
        let mut out = ctx.run_ui(input(window), |ui| {
            let mut actions = UIActions::new();
            render_slides_panel(ui, &data, &mut actions);
            took.0 = take_drop(ui.ctx(), Some(inside), 1, &[], &mut actions);
        });
        out.textures_delta.clear();
        let mut out = ctx.run_ui(input(egui::ViewportId::ROOT), |ui| {
            let mut actions = UIActions::new();
            took.1 = take_drop(ui.ctx(), Some(inside), 1, &[], &mut actions);
        });
        out.textures_delta.clear();
        assert_eq!(took, (true, false), "its own window's, not the main window's");
    }

    /// Files from Finder dropped on the panel in a window of its own become
    /// talks there, as in the main window.
    #[test]
    fn files_dropped_on_the_panel_in_its_own_window_are_talks() {
        #[derive(Debug)]
        struct Dropped(std::path::PathBuf);
        impl egui::DroppedFile for Dropped {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                Ok(Vec::new())
            }
        }
        let data = with_slides();
        let ctx = egui::Context::default();
        let window = egui::ViewportId::from_hash_of("its window");
        let file = std::path::PathBuf::from("/Talks/keynote.pptx");
        let input = egui::RawInput {
            viewport_id: window,
            viewports: std::iter::once((window, egui::ViewportInfo::default())).collect(),
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(900.0, 700.0),
            )),
            dropped_files: vec![std::sync::Arc::new(Dropped(file.clone())) as egui::DroppedFileHandle],
            events: vec![egui::Event::PointerMoved(egui::pos2(300.0, 300.0))],
            ..Default::default()
        };
        let mut actions = UIActions::new();
        let mut out = ctx.run_ui(input, |ui| {
            render_slides_panel(ui, &data, &mut actions);
            super::super::file_drop::handle_window_file_drop(ui, &mut actions);
        });
        out.textures_delta.clear();
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::AddTalks { paths, .. } if paths == &vec![file.to_string_lossy().into_owned()]
            )),
            "{:?}",
            actions.commands
        );
    }

    /// ⏮ beside ◀ ▶ takes the talk on back to its first slide — all
    /// tried out before the show, it starts from the beginning.
    #[test]
    fn the_talk_on_goes_back_to_its_first_slide() {
        let data = with_slides();
        let deck = data.channels[0].decks[0].uuid.clone();
        let mut harness = harness(&data);
        harness.get_by_label("⏮").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            format!("[PresentationGo {{ deck_uuid: Some({deck:?}), to: First }}]")
        );
    }

    #[test]
    fn a_clickers_keys_step_the_slides_input_with_no_powerpoint_input() {
        let data = with_slides();
        let mut harness = harness(&data);
        harness.key_press(egui::Key::PageDown);
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            "[PresentationGo { deck_uuid: None, to: Next }]"
        );
    }

    #[test]
    fn with_no_slides_input_the_panel_says_how_to_start() {
        let data = UIData::test_fixture();
        let harness = harness(&data);
        harness.get_by_label("+ Add files…");
        assert!(harness.query_by_label("▶  Start next talk").is_none());
    }

    #[test]
    fn the_tile_says_where_the_talks_are_and_starts_the_next() {
        let data = with_slides();
        let ch = data.channels[0].clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(400.0, 240.0))
            .build_ui_state(
                move |ui, sent: &mut Vec<EngineCommand>| {
                    let picture =
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(380.0, 214.0));
                    let mut actions = UIActions::new();
                    overlay_on_picture(ui, picture, &ch, 0, &mut actions);
                    sent.append(&mut actions.commands);
                },
                Vec::new(),
            );
        harness.get_by_label(" 1/3 · 3 / 22 ");
        harness.get_by_label("⏭").click();
        harness.run_steps(2);
        assert!(format!("{:?}", harness.state()).contains("to: Next"));
    }

    #[test]
    fn a_talks_place_reads_as_slides_pages_or_clip_time() {
        let mut talk = TalkSnapshot {
            id: "a".into(),
            name: "a".into(),
            given_name: String::new(),
            path: "/a.pdf".into(),
            kind: Some(TalkKind::Pdf),
            state: TalkState::Live,
            message: None,
            missing: false,
            position: Some(4),
            shown: Some(31),
            presentation: None,
            clip: None,
            warnings: Vec::new(),
            pictures: false,
        };
        assert_eq!(position_text(&talk), "4 / 31");
        talk.shown = None;
        assert_eq!(position_text(&talk), "4");
        talk.position = None;
        assert_eq!(position_text(&talk), "");
        talk.clip = Some(crate::engine::value::slides::ClipPosition {
            at: 75.4,
            length: 3725.0,
            playing: true,
        });
        assert_eq!(position_text(&talk), "1:15 / 1:02:05");
    }

    /// The clicker's row: Learn, then waiting for a button (the Mac's own
    /// keyboard said not to count), then the device learned with Forget;
    /// macOS refusing, how to allow it. Each press asks the clicker, not
    /// the engine.
    #[test]
    fn the_clicker_is_learned_and_forgotten_from_its_row() {
        use crate::clicker::{Access, ClickerAsk, ClickerDevice, ClickerStatus};
        let asked = |status: ClickerStatus, press: &str| {
            let mut harness = Harness::new_ui_state(
                move |ui, asked: &mut Option<ClickerAsk>| {
                    let mut actions = UIActions::new();
                    render_clicker(ui, &status, &mut actions);
                    assert!(actions.commands.is_empty());
                    if let Some(ask) = actions.session.clicker_ask {
                        *asked = Some(ask);
                    }
                },
                None,
            );
            harness.run();
            harness.get_by_label(press).click();
            harness.run();
            *harness.state()
        };
        assert_eq!(
            asked(ClickerStatus::default(), "Learn clicker"),
            Some(ClickerAsk::Learn)
        );
        let learning = ClickerStatus {
            learning: true,
            access: Access::Granted,
            ..ClickerStatus::default()
        };
        assert_eq!(asked(learning.clone(), "Cancel"), Some(ClickerAsk::Cancel));
        let harness = Harness::new_ui(move |ui| {
            let status = ClickerStatus {
                mac_keyboard: true,
                ..learning.clone()
            };
            render_clicker(ui, &status, &mut UIActions::new());
        });
        harness.get_by_label_contains("the Mac's own keyboard");
        let learned_status = || ClickerStatus {
            device: Some(ClickerDevice {
                vendor: 0x046d,
                product: 0xc538,
                name: "Logitech USB Receiver".into(),
            }),
            access: Access::Granted,
            ..ClickerStatus::default()
        };
        let learned = learned_status();
        assert_eq!(asked(learned.clone(), "Forget"), Some(ClickerAsk::Forget));
        let harness = Harness::new_ui(move |ui| {
            render_clicker(ui, &learned, &mut UIActions::new());
        });
        harness.get_by_label("Logitech USB Receiver");
        let refused = ClickerStatus {
            learning: true,
            access: Access::Denied,
            ..ClickerStatus::default()
        };
        assert_eq!(asked(refused.clone(), "Cancel"), Some(ClickerAsk::Cancel));
        let harness = Harness::new_ui(move |ui| {
            render_clicker(ui, &refused, &mut UIActions::new());
        });
        harness.get_by_label_contains("Input Monitoring");
        // Learned, but this Nida not yet allowed (an update): said so, with
        // Allow — not "works with another app in front".
        let new_nida = ClickerStatus {
            access: Access::Unknown,
            ..learned_status()
        };
        assert_eq!(asked(new_nida.clone(), "Allow"), Some(ClickerAsk::Learn));
        let harness = Harness::new_ui(move |ui| {
            render_clicker(ui, &new_nida, &mut UIActions::new());
        });
        assert!(harness
            .query_by_label_contains("works with another app in front")
            .is_none());
        harness.get_by_label_contains("until macOS allows it");
    }

    #[test]
    fn every_kind_of_talk_can_be_chosen_in_the_dialog() {
        let all = talk_extensions();
        for ext in ["pptx", "ppt", "pdf", "html", "mp4", "mov", "png", "jpg", "wav"] {
            assert!(all.contains(&ext), "{ext}");
        }
    }
}
