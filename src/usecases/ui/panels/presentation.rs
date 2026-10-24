//! PowerPoint inputs in the UI: a deck chosen in Add Input; ◀ ▶ and where
//! the show is on the picture of every channel holding one (its tile, its
//! column header); and in the layer's settings the steps, black and white,
//! the slide's notes and what comes next. `PageDown` and `PageUp` — what a
//! presentation clicker sends — and B step and black out the show on air.

use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use super::super::{ChannelUIInfo, DeckUIInfo, UIActions, UIData};
use crate::engine::value::presentation::{
    ClocksAt, Go, PlaysOn, PresentationSnapshot, PresentationStatus, Screen, VideoControl,
    VideoSnapshot, VideoState,
};
use crate::engine::{EngineCommand, InputSource};

const WARN: egui::Color32 = egui::Color32::from_rgb(230, 170, 60);

/// The files PowerPoint opens.
const DECK_EXTENSIONS: &[&str] = &[
    "pptx", "ppt", "pptm", "ppsx", "pps", "ppsm", "potx", "pot", "potm",
];

/// A channel's PowerPoint layer, if it has one.
pub(super) fn of_channel(ch: &ChannelUIInfo) -> Option<(&DeckUIInfo, &PresentationSnapshot)> {
    ch.decks
        .iter()
        .find_map(|d| d.presentation.as_ref().map(|p| (d, p)))
}

/// "3 / 24", with black, white or the end screen; or what the show waits on.
fn position_text(p: &PresentationSnapshot) -> String {
    match p.status {
        PresentationStatus::Opening => "Opening in PowerPoint…".into(),
        PresentationStatus::Failed => "⚠ PowerPoint".into(),
        PresentationStatus::NoWindow => "⚠ No window".into(),
        PresentationStatus::Showing => {
            let at = match (p.position, p.shown) {
                (Some(at), Some(of)) => format!("{at} / {of}"),
                (Some(at), None) => at.to_string(),
                _ => "—".into(),
            };
            match p.screen {
                Screen::Slide => at,
                Screen::Black => format!("{at} · black"),
                Screen::White => format!("{at} · white"),
                Screen::Ended => format!("{at} · end"),
            }
        }
    }
}

fn go(deck_uuid: &str, to: Go) -> EngineCommand {
    EngineCommand::PresentationGo {
        deck_uuid: Some(deck_uuid.to_string()),
        to,
    }
}

/// ◀ ▶ over a picture or in a header.
pub(super) fn step_buttons(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    size: egui::Vec2,
    actions: &mut UIActions,
) {
    for (to, text, hover) in [
        (Go::Previous, "◀", "Back a step (PageUp)"),
        (Go::Next, "▶", "The next animation or slide (PageDown)"),
    ] {
        let button = egui::Button::new(
            egui::RichText::new(text)
                .size(size.y * 0.55)
                .color(egui::Color32::WHITE),
        )
        .fill(egui::Color32::from_black_alpha(170))
        .min_size(size);
        if ui.add(button).on_hover_text(hover).clicked() {
            actions.commands.push(go(deck_uuid, to));
        }
    }
}

/// ◀ ▶ and where the show is, over the foot of a channel's picture: for a
/// tile showing a PowerPoint input.
pub(super) fn overlay_on_picture(
    ui: &mut egui::Ui,
    picture: egui::Rect,
    ch: &ChannelUIInfo,
    rows_below: usize,
    actions: &mut UIActions,
) {
    let Some((deck, p)) = of_channel(ch) else {
        return;
    };
    let size = egui::vec2(30.0, 24.0);
    // Over a timer's row, if the input has one.
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
    step_buttons(&mut row_ui, &deck.uuid, size, actions);
    let colour = if p.status == PresentationStatus::Showing {
        egui::Color32::WHITE
    } else {
        WARN
    };
    let label = row_ui.label(
        egui::RichText::new(format!(" {} ", position_text(p)))
            .small()
            .color(colour)
            .background_color(egui::Color32::from_black_alpha(170)),
    );
    if let Some(message) = &p.message {
        label.on_hover_text(message);
    }
}

/// The layer's settings column: the show's steps and screen, the slide's
/// title and notes, and what comes next.
pub(super) fn render_deck_column(
    ui: &mut egui::Ui,
    data: &UIData,
    deck: &DeckUIInfo,
    p: &PresentationSnapshot,
    actions: &mut UIActions,
) {
    use crate::keymap::ActionId;
    // Keyboard learn: ◀ ▶ and Black pick the key's action rather than move
    // the show — a clicker's own keys learned here.
    let learning = data.keyboard_learn_active;
    ui.label(egui::RichText::new("PowerPoint").strong());
    let file = std::path::Path::new(&p.path)
        .file_name()
        .map_or_else(|| p.path.clone(), |f| f.to_string_lossy().into_owned());
    ui.label(egui::RichText::new(file).small().weak())
        .on_hover_text(&p.path);
    if let Some(message) = &p.message {
        ui.label(egui::RichText::new(message).small().color(WARN));
    }

    let showing = p.status == PresentationStatus::Showing;
    let size = egui::vec2(34.0, 26.0);
    ui.horizontal(|ui| {
        for (to, text, hover, action) in [
            (Go::First, "⏮", "The first slide", None),
            (
                Go::Previous,
                "◀",
                "Back a step (PageUp)",
                Some(ActionId::PresentationPrevious),
            ),
            (
                Go::Next,
                "▶",
                "The next animation or slide (PageDown)",
                Some(ActionId::PresentationNext),
            ),
            (Go::Last, "⏭", "The last slide", None),
        ] {
            let button = egui::Button::new(egui::RichText::new(text).size(15.0)).min_size(size);
            let usable = if learning { action.is_some() } else { showing };
            let response = ui.add_enabled(usable, button).on_hover_text(hover);
            if learning {
                if let Some(action) = action {
                    learn(ui, data, &response, action, actions);
                }
            } else if response.clicked() {
                actions.commands.push(go(&deck.uuid, to));
            }
        }
    });
    ui.horizontal(|ui| {
        for (screen, text, hover, action) in [
            (
                Screen::Black,
                "Black",
                "Black the screen out (B), and back",
                Some(ActionId::PresentationBlack),
            ),
            (
                Screen::White,
                "White",
                "White the screen out, and back",
                None,
            ),
        ] {
            let on = p.screen == screen;
            let usable = if learning { action.is_some() } else { showing };
            let response = ui
                .add_enabled(usable, egui::Button::selectable(on, text))
                .on_hover_text(hover);
            if learning {
                if let Some(action) = action {
                    learn(ui, data, &response, action, actions);
                }
            } else if response.clicked() {
                actions.commands.push(EngineCommand::SetPresentationScreen {
                    deck_uuid: Some(deck.uuid.clone()),
                    screen: if on { Screen::Slide } else { screen },
                });
            }
        }
    });

    if let Some(video) = p.video {
        video_row(ui, deck, video, actions);
    }

    ui.label(egui::RichText::new(position_text(p)).small());
    if let Some(title) = &p.title {
        ui.label(egui::RichText::new(title).small().strong());
    }
    egui::ScrollArea::vertical()
        .id_salt(("presentation notes", &deck.uuid))
        .max_height(140.0)
        .show(ui, |ui| {
            if p.notes.is_empty() {
                ui.label(egui::RichText::new("No notes").small().weak());
            } else {
                ui.label(egui::RichText::new(&p.notes).small());
            }
        });
    if let Some(next) = p.next_slide {
        let text = match &p.next_title {
            Some(title) => format!("Next: slide {next} — {title}"),
            None => format!("Next: slide {next}"),
        };
        ui.label(egui::RichText::new(text).small().weak());
    }
    plays_on(ui, data, deck, p, actions);
    powerpoint_sound(ui, data, deck, actions);
    ui.horizontal(|ui| {
        if ui
            .button("Restart show")
            .on_hover_text("End PowerPoint's show and play it again where it was")
            .clicked()
        {
            actions.commands.push(EngineCommand::RestartPresentation {
                deck_uuid: deck.uuid.clone(),
            });
        }
        if ui
            .button("+ Presenter view")
            .on_hover_text(
                "A new input for the speaker's screen: this slide, live, with the notes, \
                 what comes next and the talk's clocks",
            )
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
    });
}

/// PowerPoint's own sound — a slide's video's — taken into the audio mixer
/// in one click, heard with this input on air (Audio Auto); once it is
/// there, said so.
fn powerpoint_sound(ui: &mut egui::Ui, data: &UIData, deck: &DeckUIInfo, actions: &mut UIActions) {
    use crate::audio_mix::AudioSourceKind;
    use crate::presentation::powerpoint::POWERPOINT_ID;
    // PowerPoint's sound is one, whichever PowerPoint input shows it: its
    // strip, tied to one — or to one gone since — is heard while any is on
    // air, and a second would add it twice. One tied to no input is heard as
    // it is set; one tied to another kind of input follows that one alone.
    let shared = |tied: &str| {
        data.channels
            .iter()
            .flat_map(|c| &c.decks)
            .find(|d| d.uuid == tied)
            .is_none_or(|d| d.presentation.is_some())
    };
    let strip = |tied: bool| {
        data.audio_mix.sources.iter().any(|s| {
            matches!(&s.kind, AudioSourceKind::Input { app: Some(app), deck, .. }
                if app == POWERPOINT_ID
                    && deck.as_deref().map_or(!tied, |d| tied && shared(d)))
        })
    };
    if strip(true) {
        ui.label(
            egui::RichText::new("PowerPoint's sound: in the audio mixer")
                .small()
                .weak(),
        );
    } else if strip(false) {
        ui.label(
            egui::RichText::new("PowerPoint's sound: in the audio mixer, tied to no input")
                .small()
                .weak(),
        );
    } else if ui
        .button("PowerPoint's sound to the mixer")
        .on_hover_text(
            "A slide's video sounds through PowerPoint: take it into the audio mixer, heard \
             with this input on air (Audio Auto). This Mac's own speakers then stay quiet.",
        )
        .clicked()
    {
        actions.commands.push(EngineCommand::AddAppSound {
            app: POWERPOINT_ID.to_string(),
            name: "PowerPoint".into(),
            deck_uuid: Some(deck.uuid.clone()),
        });
    }
}

/// The video on the slide on screen: start again, back, play or pause,
/// forward — PowerPoint's own player, so the show's sound and picture stay
/// one — and where it has got.
fn video_row(ui: &mut egui::Ui, deck: &DeckUIInfo, video: VideoSnapshot, actions: &mut UIActions) {
    let playing = video.state == VideoState::Playing;
    let size = egui::vec2(34.0, 24.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Video").small().strong());
        for (control, text, hover) in [
            (VideoControl::Restart, "⏮", "From its start"),
            (VideoControl::Skip(-10), "⏪", "Back 10 seconds"),
            (
                VideoControl::Toggle,
                if playing { "⏸" } else { "▶" },
                if playing { "Pause" } else { "Play" },
            ),
            (VideoControl::Skip(10), "⏩", "Forward 10 seconds"),
        ] {
            let button = egui::Button::new(egui::RichText::new(text).size(14.0)).min_size(size);
            if ui.add(button).on_hover_text(hover).clicked() {
                actions.commands.push(EngineCommand::PresentationVideo {
                    deck_uuid: Some(deck.uuid.clone()),
                    control,
                });
            }
        }
        let seconds = video.position_ms / 1000;
        let state = match video.state {
            VideoState::Playing => "",
            VideoState::Paused => " · paused",
            VideoState::Stopped => " · stopped",
            VideoState::NotReady => " · not started",
        };
        ui.label(
            egui::RichText::new(format!("{}:{:02}{state}", seconds / 60, seconds % 60)).small(),
        );
    });
}

/// Which screen the show plays on — a choice — and where it does, at what
/// size: PowerPoint draws the slide the size of its window, which macOS
/// keeps within its screen.
fn plays_on(
    ui: &mut egui::Ui,
    data: &UIData,
    deck: &DeckUIInfo,
    p: &PresentationSnapshot,
    actions: &mut UIActions,
) {
    let named = |identity: &str| {
        data.available_monitors
            .iter()
            .find(|m| m.identity == identity)
            .map(|m| m.label.clone())
    };
    let chosen = match &p.plays_on {
        PlaysOn::Auto => "Automatic".to_string(),
        PlaysOn::Main => "This Mac's screen".to_string(),
        PlaysOn::Screen(identity) => {
            named(identity).unwrap_or_else(|| "A screen not connected".to_string())
        }
    };
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Plays on").small());
        egui::ComboBox::from_id_salt(("presentation plays on", &deck.uuid))
            .selected_text(chosen)
            .show_ui(ui, |ui| {
                let auto = ui
                    .selectable_label(p.plays_on == PlaysOn::Auto, "Automatic")
                    .on_hover_text(
                        "Where it plays sharpest: a spare screen — connected, with no output \
                         on it — when it is as sharp as this Mac's",
                    );
                if auto.clicked() {
                    picked = Some(PlaysOn::Auto);
                }
                if ui
                    .selectable_label(p.plays_on == PlaysOn::Main, "This Mac's screen")
                    .clicked()
                {
                    picked = Some(PlaysOn::Main);
                }
                for screen in data.available_monitors.iter().filter(|m| !m.main) {
                    let this = PlaysOn::Screen(screen.identity.clone());
                    if ui
                        .selectable_label(p.plays_on == this, &screen.label)
                        .on_hover_text("This screen while no output is on it")
                        .clicked()
                    {
                        picked = Some(this);
                    }
                }
            });
    });
    if let Some(plays_on) = picked.filter(|picked| *picked != p.plays_on) {
        actions
            .commands
            .push(EngineCommand::SetPresentationPlaysOn {
                deck_uuid: deck.uuid.clone(),
                plays_on,
            });
    }
    if let Some(on) = &p.playing_on {
        let text = match p.slide_pixels {
            Some([w, h]) => format!("On {on} · slide {w} × {h}"),
            None => format!("On {on}"),
        };
        ui.label(egui::RichText::new(text).small().weak());
    }
}

/// A presenter view layer's settings column: which show it follows, and
/// its talk clock back to 0:00.
pub(super) fn render_presenter_column(
    ui: &mut egui::Ui,
    data: &UIData,
    deck: &DeckUIInfo,
    follows: &str,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("Presenter view").strong());
    let shown = data
        .channels
        .iter()
        .find(|ch| ch.decks.iter().any(|d| d.uuid == follows))
        .map_or_else(|| "(input gone)".to_string(), |ch| ch.name.clone());
    ui.label(egui::RichText::new(format!("Shows {shown}")).small().weak());
    if ui
        .button("Restart talk clock")
        .on_hover_text("The time the talk has run, back to 0:00 from now")
        .clicked()
    {
        actions.commands.push(EngineCommand::RestartPresenterClock {
            deck_uuid: deck.uuid.clone(),
        });
    }

    // What it shows, where, and how big.
    let mut layout = deck.presenter_layout.unwrap_or_default();
    let before = layout;
    ui.separator();
    ui.horizontal(|ui| {
        ui.checkbox(&mut layout.timer, "Talk timer")
            .on_hover_text("How long the talk has run");
        ui.checkbox(&mut layout.clock, "Time of day");
    });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Clocks").small());
        ui.selectable_value(&mut layout.clocks_at, ClocksAt::Top, "Top");
        ui.selectable_value(&mut layout.clocks_at, ClocksAt::Bottom, "Bottom");
    });
    ui.checkbox(&mut layout.next, "Next slide")
        .on_hover_text("What comes next, under the slide");
    ui.label(egui::RichText::new("Sizes").small().weak());
    // A drag is one change to undo, not one a frame.
    let sliders = [
        ui.add(
            egui::Slider::new(&mut layout.slide_share, 0.35..=0.8)
                .text("Slide")
                .show_value(false),
        )
        .on_hover_text("The slide's share of the screen; the notes have the rest"),
        ui.add(
            egui::Slider::new(&mut layout.notes_size, 0.5..=2.0)
                .text("Notes")
                .show_value(false),
        ),
        ui.add_enabled(
            layout.next,
            egui::Slider::new(&mut layout.next_size, 0.5..=2.0)
                .text("Next")
                .show_value(false),
        ),
    ];
    if sliders.iter().any(egui::Response::dragged) {
        actions.session.gesture_active = true;
    }
    if layout != before {
        actions.commands.push(EngineCommand::SetPresenterLayout {
            deck_uuid: deck.uuid.clone(),
            layout,
        });
    }
}

/// In keyboard learn: the button glows, and a click picks its action for the
/// next key pressed.
fn learn(
    ui: &egui::Ui,
    data: &UIData,
    response: &egui::Response,
    action: crate::keymap::ActionId,
    actions: &mut UIActions,
) {
    use crate::usecases::ui::widgets;
    if data.keyboard_learn_target.as_deref() == Some(format!("{action:?}").as_str()) {
        widgets::draw_keyboard_learn_selected(ui, response.rect);
    } else {
        widgets::draw_keyboard_learn_glow(ui, response.rect);
    }
    if response.clicked() {
        actions.session.keyboard_learn_select = Some(crate::keymap::KeyTarget::Action(action));
    }
}

// ── Choosing a deck ─────────────────────────────────────────────────

/// Where a deck being chosen goes.
#[derive(Clone, Debug)]
pub(super) enum DeckFor {
    /// A new input, called this (after the file, if not).
    NewInput(Option<String>),
    /// A layer of this channel.
    Layer(String),
}

type Chosen = Arc<Mutex<Option<(DeckFor, mpsc::Receiver<PathBuf>)>>>;

fn chosen_id() -> egui::Id {
    egui::Id::new("presentation_deck_chosen")
}

/// Ask for a PowerPoint deck to go `into` an input. The dialog runs on a
/// thread; its answer is taken as the UI renders.
pub(super) fn choose_deck(ctx: &egui::Context, into: DeckFor) {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("presentation-deck".into())
        .spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("A PowerPoint deck")
                .add_filter("PowerPoint", DECK_EXTENSIONS)
                .pick_file();
            if let Some(path) = picked {
                let _ = tx.send(path);
            }
        });
    if spawned.is_ok() {
        let chosen: Chosen = Arc::new(Mutex::new(Some((into, rx))));
        ctx.data_mut(|d| d.insert_temp(chosen_id(), chosen));
    }
}

/// The deck the dialog came back with, as a new input or layer.
pub(super) fn take_chosen(ctx: &egui::Context, actions: &mut UIActions) {
    let Some(chosen) = ctx.data(|d| d.get_temp::<Chosen>(chosen_id())) else {
        return;
    };
    let Ok(mut slot) = chosen.lock() else {
        return;
    };
    let answer = match slot.as_ref().map(|(_, rx)| rx.try_recv()) {
        Some(Ok(path)) => Some(path),
        Some(Err(mpsc::TryRecvError::Empty)) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            return;
        }
        // Cancelled.
        Some(Err(mpsc::TryRecvError::Disconnected)) | None => None,
    };
    let into = slot.take().map(|(into, _)| into);
    drop(slot);
    ctx.data_mut(|d| d.remove::<Chosen>(chosen_id()));
    let (Some(path), Some(into)) = (answer, into) else {
        return;
    };
    let path = path.to_string_lossy().into_owned();
    actions.commands.push(match into {
        DeckFor::NewInput(name) => EngineCommand::AddInput {
            source: InputSource::PowerPoint { path },
            name,
            audio: None,
        },
        DeckFor::Layer(channel_uuid) => EngineCommand::AddPresentationDeck { channel_uuid, path },
    });
}

/// Whether `path` is a file PowerPoint opens.
pub(super) fn is_deck(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| DECK_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

// ── The clicker ─────────────────────────────────────────────────────

/// `PageDown`, `PageUp` and B — or the keys the keymap binds to the PowerPoint
/// actions — step and black out the show on air (else the one moved last),
/// when there is a PowerPoint input and no text field is being typed into.
/// The key is the show's alone: it does not also scroll or press a button.
pub(super) fn clicker_shortcuts(ui: &egui::Ui, data: &UIData, actions: &mut UIActions) {
    use crate::keymap::{ActionId, KeyTarget};
    if data.keyboard_learn_active || data.stage_editor_open || super::typing(ui.ctx()) {
        return;
    }
    if !data.channels.iter().any(|ch| of_channel(ch).is_some()) {
        return;
    }
    for combo in crate::usecases::ui::keyboard::collect_pressed_keys(ui.ctx()) {
        let command = match data.keymap_bindings.get(&combo) {
            Some(KeyTarget::Action(ActionId::PresentationNext)) => EngineCommand::PresentationGo {
                deck_uuid: None,
                to: Go::Next,
            },
            Some(KeyTarget::Action(ActionId::PresentationPrevious)) => {
                EngineCommand::PresentationGo {
                    deck_uuid: None,
                    to: Go::Previous,
                }
            }
            Some(KeyTarget::Action(ActionId::PresentationBlack)) => {
                EngineCommand::TogglePresentationScreen {
                    deck_uuid: None,
                    screen: Screen::Black,
                }
            }
            _ => continue,
        };
        super::consume(ui.ctx(), &combo);
        actions.commands.push(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable as _;

    fn showing() -> PresentationSnapshot {
        PresentationSnapshot {
            path: "/Talks/talk.pptx".into(),
            status: PresentationStatus::Showing,
            message: None,
            slide: Some(3),
            slides: Some(24),
            position: Some(3),
            shown: Some(22),
            screen: Screen::Slide,
            title: Some("Results".into()),
            notes: "Pause for questions.".into(),
            next_slide: Some(4),
            next_title: None,
            plays_on: PlaysOn::Auto,
            playing_on: Some("this Mac's screen".into()),
            slide_pixels: Some([1675, 942]),
            video: None,
        }
    }

    /// The test show, its first layer a PowerPoint input, keys as Nida
    /// binds them.
    fn with_deck() -> UIData {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0].presentation = Some(showing());
        data.keymap_bindings = crate::keymap::KeymapStore::defaults();
        data
    }

    /// Over a timer's row at the foot of the tile, the slides' row sits a
    /// row higher — not on it, where the timer's buttons were covered.
    #[test]
    fn the_slides_row_sits_over_a_timers() {
        let mut ch = UIData::test_fixture().channels[0].clone();
        ch.decks[0].presentation = Some(showing());
        let back_top = |rows_below: usize| {
            let ch = ch.clone();
            let harness = egui_kittest::Harness::new_ui(move |ui| {
                let picture =
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(320.0, 180.0));
                overlay_on_picture(ui, picture, &ch, rows_below, &mut UIActions::new());
            });
            harness.get_by_label("◀").rect().top()
        };
        let (alone, raised) = (back_top(0), back_top(1));
        assert!(
            raised <= alone - 24.0,
            "a row higher: {raised} against {alone}"
        );
    }

    type Harness<'a> = egui_kittest::Harness<'a, Vec<EngineCommand>>;

    /// The clicker's keys and the first layer's column, with a text field
    /// to type into; the commands sent kept as state.
    fn harness(data: &UIData) -> Harness<'_> {
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(320.0, 460.0))
            .build_ui_state(
                |ui, sent: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    clicker_shortcuts(ui, data, &mut actions);
                    let mut typed = String::new();
                    ui.add(egui::TextEdit::singleline(&mut typed).hint_text("type here"));
                    if let Some((deck, p)) = of_channel(&data.channels[0]) {
                        render_deck_column(ui, data, deck, p, &mut actions);
                    }
                    sent.append(&mut actions.commands);
                },
                Vec::new(),
            )
    }

    fn sent(harness: &Harness<'_>) -> String {
        format!("{:?}", harness.state())
    }

    #[test]
    fn a_clickers_keys_step_and_black_out_the_show_on_air() {
        let data = with_deck();
        let mut harness = harness(&data);
        for key in [egui::Key::PageDown, egui::Key::PageUp, egui::Key::B] {
            harness.key_press(key);
            harness.run_steps(2);
        }
        assert_eq!(
            sent(&harness),
            "[PresentationGo { deck_uuid: None, to: Next }, \
             PresentationGo { deck_uuid: None, to: Previous }, \
             TogglePresentationScreen { deck_uuid: None, screen: Black }]"
        );
    }

    #[test]
    fn not_while_typing_nor_without_a_powerpoint_input() {
        let data = with_deck();
        let mut harness = harness(&data);
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .focus();
        harness.run_steps(2);
        harness.key_press(egui::Key::PageDown);
        harness.key_press(egui::Key::B);
        harness.run_steps(2);
        assert_eq!(sent(&harness), "[]", "typing, B is a letter");
        drop(harness);

        let mut plain = UIData::test_fixture();
        plain.keymap_bindings = crate::keymap::KeymapStore::defaults();
        let mut harness = self::harness(&plain);
        harness.key_press(egui::Key::PageDown);
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            "[]",
            "no PowerPoint input: PageDown is PageDown"
        );
    }

    #[test]
    fn the_column_steps_its_own_show_and_shows_its_notes() {
        let data = with_deck();
        let mut harness = harness(&data);
        harness.get_by_label("3 / 22");
        harness.get_by_label("Results");
        harness.get_by_label("Pause for questions.");
        harness.get_by_label("Next: slide 4");
        harness.get_by_label("▶").click();
        harness.run_steps(2);
        harness.get_by_label("Black").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            r#"[PresentationGo { deck_uuid: Some("a0000001"), to: Next }, SetPresentationScreen { deck_uuid: Some("a0000001"), screen: Black }]"#
        );
    }

    /// PowerPoint's sound into the mixer in one click, as this input's;
    /// once it is there, said so — no second.
    #[test]
    fn powerpoints_sound_is_taken_into_the_mixer_in_one_click() {
        use crate::audio_mix::{AudioSourceKind, AudioSourceSnapshot};
        let mut data = with_deck();
        let mut before = harness(&data);
        before
            .get_by_label("PowerPoint's sound to the mixer")
            .click();
        before.run_steps(2);
        assert_eq!(
            sent(&before),
            r#"[AddAppSound { app: "com.microsoft.Powerpoint", name: "PowerPoint", deck_uuid: Some("a0000001") }]"#
        );
        drop(before);
        data.audio_mix.sources.push(AudioSourceSnapshot {
            peak_db: [-96.0; 2],
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            uuid: "a0000099".into(),
            name: "PowerPoint".into(),
            kind: AudioSourceKind::Input {
                device: "app".into(),
                left: 0,
                right: 1,
                deck: Some("a0000001".into()),
                app: Some("com.microsoft.Powerpoint".into()),
            },
            gain_db: 0.0,
            muted: true,
            sends: vec![],
            auto: true,
            follow: None,
            balance: 0.0,
            delay_ms: 0.0,
            on_air: 0.0,
            gate: 1.0,
            solo: false,
            live: true,
            meter_db: [-96.0, -96.0],
            refused: None,
        });
        let after = harness(&data);
        after.get_by_label("PowerPoint's sound: in the audio mixer");
        assert!(after
            .query_by_label("PowerPoint's sound to the mixer")
            .is_none());
        drop(after);

        // Another PowerPoint input's strip is this one's too: heard while
        // any is on air, not a second.
        let strip = data.audio_mix.sources.last_mut().unwrap();
        if let AudioSourceKind::Input { deck, .. } = &mut strip.kind {
            *deck = Some("a0000777".into());
        }
        let other = harness(&data);
        other.get_by_label("PowerPoint's sound: in the audio mixer");
        assert!(other
            .query_by_label("PowerPoint's sound to the mixer")
            .is_none());
        drop(other);

        // PowerPoint's sound tied to another kind of input follows that one
        // alone: the PowerPoint input is offered its own.
        let strip = data.audio_mix.sources.last_mut().unwrap();
        if let AudioSourceKind::Input { deck, .. } = &mut strip.kind {
            *deck = Some(data.channels[0].decks[1].uuid.clone());
        }
        harness(&data).get_by_label("PowerPoint's sound to the mixer");
        // One tied to no input is heard as it is set: said so.
        let strip = data.audio_mix.sources.last_mut().unwrap();
        if let AudioSourceKind::Input { deck, .. } = &mut strip.kind {
            *deck = None;
        }
        harness(&data).get_by_label("PowerPoint's sound: in the audio mixer, tied to no input");
    }

    /// A presenter view's time of day switched off, and its clocks moved
    /// to the foot, from its column: each a `SetPresenterLayout` carrying
    /// the whole layout.
    #[test]
    fn a_presenter_views_clocks_are_switched_and_moved_from_its_column() {
        let data = with_deck();
        let mut presenter = data.channels[0].decks[0].clone();
        presenter.uuid = "a0000009".into();
        presenter.presentation = None;
        presenter.presenter_of = Some(data.channels[0].decks[0].uuid.clone());
        presenter.presenter_layout =
            Some(crate::engine::value::presentation::PresenterLayout::default());
        let follows = data.channels[0].decks[0].uuid.clone();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(320.0, 460.0))
            .build_ui_state(
                |ui, sent: &mut Vec<EngineCommand>| {
                    let mut actions = UIActions::new();
                    render_presenter_column(ui, &data, &presenter, &follows, &mut actions);
                    sent.append(&mut actions.commands);
                },
                Vec::new(),
            );
        harness.get_by_label("Time of day").click();
        harness.run_steps(2);
        harness.get_by_label("Bottom").click();
        harness.run_steps(2);
        let sent = harness.state();
        assert_eq!(sent.len(), 2, "{sent:?}");
        assert!(matches!(
            &sent[0],
            EngineCommand::SetPresenterLayout { deck_uuid, layout }
                if deck_uuid == "a0000009" && !layout.clock && layout.timer
        ));
        assert!(matches!(
            &sent[1],
            EngineCommand::SetPresenterLayout { layout, .. }
                if layout.clocks_at == ClocksAt::Bottom
        ));
    }

    /// A slide with a video: where it has got, and buttons to pause, move
    /// and start it again; a slide without one has none.
    #[test]
    fn a_slides_video_is_paused_and_moved_from_the_column() {
        let mut data = with_deck();
        assert!(
            harness(&data).query_by_label("Video").is_none(),
            "none on this slide"
        );
        if let Some(p) = data.channels[0].decks[0].presentation.as_mut() {
            p.video = Some(VideoSnapshot {
                state: VideoState::Playing,
                position_ms: 75_400,
            });
        }
        let mut harness = harness(&data);
        harness.get_by_label("1:15");
        harness.get_by_label("⏸").click();
        harness.run_steps(2);
        harness.get_by_label("⏩").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            r#"[PresentationVideo { deck_uuid: Some("a0000001"), control: Toggle }, PresentationVideo { deck_uuid: Some("a0000001"), control: Skip(10) }]"#
        );
    }

    #[test]
    fn where_the_show_plays_is_chosen_and_shown() {
        let mut data = with_deck();
        let screen = |name: &str, identity: &str, main: bool| crate::usecases::ui::MonitorInfo {
            name: name.into(),
            index: 0,
            width: 1920,
            height: 1080,
            is_control: main,
            label: format!("{name} (1920×1080)"),
            model: Some(name.into()),
            given_name: None,
            identity: identity.into(),
            main,
        };
        data.available_monitors = vec![
            screen("Color LCD", "LCD-UUID", true),
            screen("DELL U2720Q", "DELL-UUID", false),
        ];
        let mut harness = harness(&data);
        harness.get_by_label("On this Mac's screen · slide 1675 × 942");
        // The combo box reads as what is chosen.
        harness.get_by_value("Automatic").click();
        harness.run_steps(2);
        // This Mac's own is offered as itself, not by its name.
        assert!(harness.query_by_label("Color LCD (1920×1080)").is_none());
        harness.get_by_label("DELL U2720Q (1920×1080)").click();
        harness.run_steps(2);
        assert_eq!(
            sent(&harness),
            r#"[SetPresentationPlaysOn { deck_uuid: "a0000001", plays_on: Screen("DELL-UUID") }]"#
        );
    }

    #[test]
    fn in_keyboard_learn_the_buttons_pick_their_key_and_move_nothing() {
        let mut data = with_deck();
        data.keyboard_learn_active = true;
        // Not showing yet: a clicker's key can be learned before the talk.
        if let Some(p) = data.channels[0].decks[0].presentation.as_mut() {
            p.status = PresentationStatus::Opening;
        }
        let mut picked = Vec::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(320.0, 460.0))
                .build_ui(|ui| {
                    let mut actions = UIActions::new();
                    if let Some((deck, p)) = of_channel(&data.channels[0]) {
                        render_deck_column(ui, &data, deck, p, &mut actions);
                    }
                    assert!(actions.commands.is_empty(), "nothing moves while learning");
                    if let Some(target) = actions.session.keyboard_learn_select.take() {
                        picked.push(target.to_string());
                    }
                });
            harness.get_by_label("▶").click();
            harness.run_steps(2);
            harness.get_by_label("Black").click();
            harness.run_steps(2);
        }
        assert_eq!(picked, vec!["PresentationNext", "PresentationBlack"]);
    }

    #[test]
    fn where_the_show_is_reads_as_it_stands() {
        let mut p = showing();
        assert_eq!(position_text(&p), "3 / 22");
        p.screen = Screen::Black;
        assert_eq!(position_text(&p), "3 / 22 · black");
        p.status = PresentationStatus::Opening;
        assert_eq!(position_text(&p), "Opening in PowerPoint…");
        p.status = PresentationStatus::Failed;
        assert_eq!(position_text(&p), "⚠ PowerPoint");
        assert!(is_deck(std::path::Path::new("/a/Talk.PPTX")));
        assert!(!is_deck(std::path::Path::new("/a/talk.pdf")));
    }
}
