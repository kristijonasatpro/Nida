//! Lower thirds in the UI: Show/Hide and Next over the picture of an input
//! holding one, and in Input Settings its names — typed in, or read from an
//! organiser's CSV/TSV list and read again as it is saved — and its look.

use super::super::{ChannelUIInfo, DeckUIInfo, UIActions};
use crate::engine::value::lower_third::{
    palette_name, LowerThirdBar, LowerThirdLook, LowerThirdMotion, LowerThirdPlace, LowerThirdRow,
    LowerThirdSettings, LowerThirdSnapshot, LowerThirdStyle, LITHUANIAN_LETTERS, PALETTE_COUNT,
};
use crate::engine::EngineCommand;

const ON_AIR_RED: egui::Color32 = egui::Color32::from_rgb(200, 40, 40);
const WARN: egui::Color32 = egui::Color32::from_rgb(230, 170, 60);

/// A channel's lower-third layer, if it has one.
pub(super) fn of_channel(ch: &ChannelUIInfo) -> Option<(&DeckUIInfo, &LowerThirdSnapshot)> {
    ch.decks
        .iter()
        .find_map(|d| d.lower_third.as_ref().map(|l| (d, l)))
}

fn set_settings(actions: &mut UIActions, deck_uuid: &str, settings: LowerThirdSettings) {
    actions.commands.push(EngineCommand::SetLowerThirdSettings {
        deck_uuid: deck_uuid.to_string(),
        settings,
    });
}

/// Its settings as an edit already made this frame left them, else as they
/// are: each send is the whole settings, so a second edit in a frame must
/// start from the first.
fn shown_settings(
    actions: &UIActions,
    deck_uuid: &str,
    lower: &LowerThirdSnapshot,
) -> LowerThirdSettings {
    actions
        .commands
        .iter()
        .rev()
        .find_map(|command| match command {
            EngineCommand::SetLowerThirdSettings {
                deck_uuid: uuid,
                settings,
            } if uuid == deck_uuid => Some(settings.clone()),
            _ => None,
        })
        .unwrap_or_else(|| lower.settings.clone())
}

fn show(actions: &mut UIActions, deck_uuid: &str, row: Option<usize>) {
    actions.commands.push(EngineCommand::LowerThirdShow {
        deck_uuid: deck_uuid.to_string(),
        row,
    });
}

fn hide(actions: &mut UIActions, deck_uuid: &str) {
    actions.commands.push(EngineCommand::LowerThirdHide {
        deck_uuid: deck_uuid.to_string(),
    });
}

fn step(actions: &mut UIActions, deck_uuid: &str, by: i64) {
    actions.commands.push(EngineCommand::LowerThirdStep {
        deck_uuid: deck_uuid.to_string(),
        by,
    });
}

/// A row as one line: "Name — Title".
fn row_text(row: &LowerThirdRow) -> String {
    if row.title.trim().is_empty() {
        row.name.clone()
    } else {
        format!("{} — {}", row.name, row.title)
    }
}

// ── On the picture ─────────────────────────────────────────────────

/// ▶ puts the cued name on air; ■ (red) takes the one on air off.
pub(super) fn show_button(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    lower: &LowerThirdSnapshot,
    size: egui::Vec2,
    actions: &mut UIActions,
) {
    let rows = &lower.settings.rows;
    let (text, hover, fill) = match &lower.live {
        Some(row) => (
            "■",
            format!("Take it off air: {}", row_text(row)),
            ON_AIR_RED,
        ),
        None => (
            "▶",
            match rows.get(lower.settings.cued) {
                Some(row) => format!("Show: {}", row_text(row)),
                None => "No names yet: add them in Input Settings".to_string(),
            },
            egui::Color32::from_black_alpha(170),
        ),
    };
    let button = egui::Button::new(
        egui::RichText::new(text)
            .size(size.y * 0.6)
            .strong()
            .color(egui::Color32::WHITE),
    )
    .min_size(size)
    .fill(fill);
    let response = ui
        .add_enabled(!rows.is_empty() || lower.live.is_some(), button)
        .on_hover_text(hover);
    if response.clicked() {
        if lower.live.is_some() {
            hide(actions, deck_uuid);
        } else {
            show(actions, deck_uuid, None);
        }
    }
}

/// ⏭ cues the next name, without showing it.
pub(super) fn next_button(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    lower: &LowerThirdSnapshot,
    size: egui::Vec2,
    actions: &mut UIActions,
) {
    let next = egui::Button::new(
        egui::RichText::new("⏭")
            .size(size.y * 0.55)
            .color(egui::Color32::WHITE),
    )
    .min_size(size)
    .fill(egui::Color32::from_black_alpha(170));
    let last = lower.settings.rows.len().saturating_sub(1);
    if ui
        .add_enabled(lower.settings.cued < last, next)
        .on_hover_text("Cue the next name")
        .clicked()
    {
        step(actions, deck_uuid, 1);
    }
}

/// Show/Hide, Next and the cued name over the foot of a channel's picture,
/// `rows_below` rows of other buttons up.
pub(super) fn overlay_on_picture(
    ui: &mut egui::Ui,
    picture: egui::Rect,
    ch: &ChannelUIInfo,
    rows_below: usize,
    actions: &mut UIActions,
) {
    let Some((deck, lower)) = of_channel(ch) else {
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
    show_button(&mut row_ui, &deck.uuid, lower, size, actions);
    next_button(&mut row_ui, &deck.uuid, lower, size, actions);
    // Who ▶ puts on next, plainly: ▶ moves the cue on by itself, so ⏭
    // pressed after it would pass someone by.
    if let Some(cued) = lower.settings.rows.get(lower.settings.cued) {
        row_ui
            .add(
                egui::Label::new(
                    egui::RichText::new(format!("▶ next: {}", cued.name))
                        .strong()
                        .color(egui::Color32::WHITE)
                        .background_color(egui::Color32::from_black_alpha(170)),
                )
                .truncate(),
            )
            .on_hover_text(
                "▶ shows this one; it moves the cue on by itself, so no ⏭ is needed after it",
            );
    }
}

// ── The Lower thirds panel ─────────────────────────────────────────

/// The Lower thirds panel: every lower third in the show, by its input —
/// on air, its names (each shown in one press), Show/Hide/Next — and, folded
/// away, its list file and look: one place for the names of the day.
pub(super) fn render_panel(
    ui: &mut egui::Ui,
    data: &super::super::UIData,
    actions: &mut UIActions,
) {
    let lowers: Vec<(&ChannelUIInfo, &DeckUIInfo, &LowerThirdSnapshot)> = data
        .channels
        .iter()
        .flat_map(|ch| {
            ch.decks
                .iter()
                .filter_map(move |d| d.lower_third.as_ref().map(|l| (ch, d, l)))
        })
        .collect();
    if lowers.is_empty() {
        ui.label(egui::RichText::new("No lower thirds yet: Add Input → Lower third.").weak());
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("lower_thirds_panel")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            for (ch, deck, lower) in lowers {
                ui.push_id(("lower_third_panel", &deck.uuid), |ui| {
                    ui.horizontal(|ui| {
                        let name = if ch.name.trim().is_empty() {
                            deck.name.as_str()
                        } else {
                            ch.name.as_str()
                        };
                        ui.strong(name);
                        if lower.live.is_some() {
                            ui.label(egui::RichText::new("ON AIR").color(ON_AIR_RED).strong());
                        }
                    });
                    if let Some(problem) = &lower.problem {
                        ui.label(egui::RichText::new(format!("⚠ {problem}")).color(WARN));
                    }
                    take_chosen_files(ui.ctx(), deck, lower, actions);
                    on_air_row(ui, deck, lower, actions);
                    targets_row(ui, data, deck, lower, actions);
                    rows_table(ui, deck, lower, actions);
                    egui::CollapsingHeader::new("List and look")
                        .id_salt(("lower_third_panel_look", &deck.uuid))
                        .default_open(false)
                        .show(ui, |ui| {
                            list_row(ui, deck, lower, actions);
                            look(ui, deck, lower, frame_of(data), actions);
                        });
                    ui.separator();
                });
            }
        });
}

/// A list or plate picture the file dialog came back with for this lower
/// third: taken in, wherever it was asked from — Input Settings or the
/// Lower thirds panel, whichever is drawn first.
fn take_chosen_files(
    ctx: &egui::Context,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    actions: &mut UIActions,
) {
    if let Some(path) = chosen_list(ctx, &deck.uuid) {
        actions.commands.push(EngineCommand::ImportLowerThirdRows {
            deck_uuid: deck.uuid.clone(),
            path,
        });
    }
    if let Some(path) = chosen_picture(ctx, &deck.uuid) {
        let mut settings = shown_settings(actions, &deck.uuid, lower);
        if !matches!(
            settings.look.style,
            LowerThirdStyle::Image | LowerThirdStyle::FullFrame
        ) {
            settings.look.style = LowerThirdStyle::Image;
            settings.look.text_after = true;
            settings.look.plate_opacity = 1.0;
        }
        // A picture chosen is to be seen: a plate left at 0% comes up whole.
        if settings.look.plate_opacity <= 0.0 {
            settings.look.plate_opacity = 1.0;
        }
        settings.look.image = path;
        set_settings(actions, &deck.uuid, settings);
    }
}

// ── Input Settings ─────────────────────────────────────────────────

/// Rows shown at once before the list scrolls.
const ROWS_SHOWN: f32 = 10.0;

/// Its controls in Input Settings: on air, its names, its list file and its
/// look.
pub(super) fn render_controls(
    ui: &mut egui::Ui,
    data: &super::super::UIData,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    actions: &mut UIActions,
) {
    ui.separator();
    ui.strong("Lower third");
    if let Some(problem) = &lower.problem {
        ui.label(egui::RichText::new(format!("⚠ {problem}")).color(WARN));
    }
    take_chosen_files(ui.ctx(), deck, lower, actions);
    on_air_row(ui, deck, lower, actions);
    targets_row(ui, data, deck, lower, actions);
    list_row(ui, deck, lower, actions);
    rows_table(ui, deck, lower, actions);
    egui::CollapsingHeader::new("Look")
        .id_salt(("lower_third_look", &deck.uuid))
        .default_open(false)
        .show(ui, |ui| look(ui, deck, lower, frame_of(data), actions));
}

fn on_air_row(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    actions: &mut UIActions,
) {
    let settings = &lower.settings;
    ui.horizontal_wrapped(|ui| {
        show_button(ui, &deck.uuid, lower, egui::vec2(34.0, 24.0), actions);
        if ui
            .add_enabled(settings.cued > 0, egui::Button::new("⏮"))
            .on_hover_text("Cue the name before")
            .clicked()
        {
            step(actions, &deck.uuid, -1);
        }
        if ui
            .add_enabled(
                settings.cued + 1 < settings.rows.len(),
                egui::Button::new("⏭"),
            )
            .on_hover_text("Cue the next name")
            .clicked()
        {
            step(actions, &deck.uuid, 1);
        }
        match &lower.live {
            Some(row) => {
                ui.label(
                    egui::RichText::new(format!("ON AIR: {}", row.name))
                        .color(ON_AIR_RED)
                        .strong(),
                );
            }
            None => {
                if let Some(row) = settings.rows.get(settings.cued) {
                    ui.label(format!("Next: {}", row_text(row)));
                }
            }
        }
    });
}

/// The buses it is shown on too, over whatever each shows — the LED wall's
/// screens, say — each a toggle.
fn targets_row(
    ui: &mut egui::Ui,
    data: &super::super::UIData,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    actions: &mut UIActions,
) {
    let before = shown_settings(actions, &deck.uuid, lower);
    let mut settings = before.clone();
    show_on_row(ui, data, &deck.uuid, &mut settings.targets, actions);
    if settings != before {
        set_settings(actions, &deck.uuid, settings);
    }
}

/// Where a layer comes on — a lower third, a text input: in Main mode Main
/// (its input an overlay over Main, switched on, at once), and each bus
/// (`targets`, edited here; the caller sends them).
pub(super) fn show_on_row(
    ui: &mut egui::Ui,
    data: &super::super::UIData,
    deck_uuid: &str,
    targets: &mut Vec<String>,
    actions: &mut UIActions,
) {
    // In Main mode, Main too: its input an overlay over Main, switched on.
    let channel = data
        .channels
        .iter()
        .find(|ch| ch.decks.iter().any(|d| d.uuid == deck_uuid));
    let main = channel.filter(|_| data.main.mode == crate::mixer::MixMode::Program);
    if data.buses.is_empty() && main.is_none() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label("Show on:").on_hover_text(
            "Where it comes on: Main (its input an overlay over Main), and buses — over \
             whatever each shows, in the part each one's screens show (an LED wall's band).",
        );
        if let Some(channel) = main {
            let was = channel.overlay && data.main.overlays_on.contains(&channel.uuid);
            let mut on = was;
            if ui
                .checkbox(&mut on, "Main")
                .on_hover_text("Its input an overlay over Main, switched on: Show and Hide bring it on and off there")
                .changed()
            {
                if on && !channel.overlay {
                    actions.commands.push(EngineCommand::SetChannelOverlay {
                        channel_uuid: channel.uuid.clone(),
                        overlay: true,
                    });
                }
                actions.commands.push(EngineCommand::SetOverlayOn {
                    channel_uuid: channel.uuid.clone(),
                    on,
                    fade: false,
                });
            }
        }
        for bus in &data.buses {
            let uuid = &bus.bus.uuid;
            let mut on = targets.iter().any(|t| t == uuid);
            if ui.checkbox(&mut on, &bus.bus.name).changed() {
                targets.retain(|t| t != uuid);
                if on {
                    targets.push(uuid.clone());
                }
            }
        }
    });
}

/// The list file the names are read from, or a button to read one.
fn list_row(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    actions: &mut UIActions,
) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .button("Read a list…")
            .on_hover_text(
                "Names from a CSV or TSV file — an Excel sheet saved as CSV works, \
                 Lithuanian letters too. Columns: Name, Title, Second line (or the \
                 first three when there are no headers).",
            )
            .clicked()
        {
            choose_list(ui.ctx(), &deck.uuid);
        }
        if let Some(path) = &lower.settings.source {
            let file = std::path::Path::new(path)
                .file_name()
                .map_or_else(|| path.clone(), |f| f.to_string_lossy().into_owned());
            ui.label(format!("From {file}"))
                .on_hover_text(format!("{path}\nRead again whenever it is saved"));
            if ui
                .small_button("Stop following")
                .on_hover_text("Keep these names and edit them here; the file is no longer read")
                .clicked()
            {
                let mut settings = shown_settings(actions, &deck.uuid, lower);
                settings.source = None;
                set_settings(actions, &deck.uuid, settings);
            }
        }
    });
}

/// The names: the one on air in red, the cued one marked; each shown in
/// one click. Typed in here — or, read from a file, read-only, as the file
/// is read again when saved.
fn rows_table(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    actions: &mut UIActions,
) {
    let settings = shown_settings(actions, &deck.uuid, lower);
    let from_file = settings.source.is_some();
    let mut edited = settings.clone();
    let mut changed = false;
    let mut typing = false;
    let mut removed = None;
    // A row's height without the spacing, which the list adds.
    let row_height = ui.spacing().interact_size.y;
    egui::ScrollArea::vertical()
        .id_salt(("lower_third_rows", &deck.uuid))
        .max_height((row_height + ui.spacing().item_spacing.y) * ROWS_SHOWN)
        .auto_shrink([false, true])
        .show_rows(ui, row_height, settings.rows.len(), |ui, range| {
            for i in range {
                ui.horizontal(|ui| {
                    let marker = if lower.on_air == Some(i) {
                        egui::RichText::new("●").color(ON_AIR_RED)
                    } else if settings.cued == i {
                        egui::RichText::new("▸").strong()
                    } else {
                        egui::RichText::new(" ")
                    };
                    ui.label(marker).on_hover_text(if lower.on_air == Some(i) {
                        "On air"
                    } else if settings.cued == i {
                        "Cued: Show puts this one on air"
                    } else {
                        ""
                    });
                    if ui
                        .small_button("▶")
                        .on_hover_text("Show this one now")
                        .clicked()
                    {
                        show(actions, &deck.uuid, Some(i));
                    }
                    let width = ((ui.available_width() - 30.0) / 3.0).clamp(60.0, 180.0);
                    let row = &mut edited.rows[i];
                    for (salt, text, hint) in [
                        ("n", &mut row.name, "Name"),
                        ("t", &mut row.title, "Title"),
                        ("s", &mut row.title2, "Second line"),
                    ] {
                        if from_file {
                            ui.add_sized(
                                [width, row_height],
                                egui::Label::new(text.as_str()).truncate(),
                            );
                        } else {
                            let field =
                                typed(ui, &format!("{}{i}{salt}", deck.uuid), text, hint, width);
                            changed |= field.changed();
                            typing |= field.has_focus();
                        }
                    }
                    if !from_file
                        && ui
                            .small_button("×")
                            .on_hover_text("Remove this name")
                            .clicked()
                    {
                        removed = Some(i);
                    }
                });
            }
        });
    if settings.rows.is_empty() {
        ui.label(egui::RichText::new("No names yet.").weak());
    }
    if !from_file && ui.button("+ Add a name").clicked() {
        edited.rows.push(LowerThirdRow::default());
        changed = true;
    }
    if let Some(i) = removed {
        edited.rows.remove(i);
        if edited.cued > i {
            edited.cued -= 1;
        }
        changed = true;
    }
    if changed {
        set_settings(actions, &deck.uuid, edited);
    }
    // A name typed is one undo step, however many letters: from its first
    // letter (that frame starts the step) for as long as a field has focus.
    let typed_id = ui.make_persistent_id(("lower_third_typing_step", &deck.uuid));
    let started = typing
        && (changed
            || ui
                .ctx()
                .data(|d| d.get_temp::<bool>(typed_id))
                .unwrap_or(false));
    ui.ctx().data_mut(|d| d.insert_temp(typed_id, started));
    if started {
        actions.session.gesture_active = true;
    }
}

/// A field typed into `text`, kept here while it is being typed, so the
/// engine's copy a frame behind does not undo a letter.
fn typed(
    ui: &mut egui::Ui,
    salt: &str,
    text: &mut String,
    hint: &str,
    width: f32,
) -> egui::Response {
    // Scoped to where it is drawn: the same name in the Lower thirds panel
    // and in Input Settings are two fields.
    let id = ui.make_persistent_id(("lower_third_typing", salt));
    // Ahead of the engine's copy, but giving way to a change made elsewhere
    // (an undo, the list read again) — see `text::Typing`.
    let (mut typing, _) = super::text::Typing::for_field(ui.ctx(), id, text);
    let response = ui.add(
        egui::TextEdit::singleline(&mut typing.text)
            .id(id.with("field"))
            .hint_text(hint)
            .desired_width(width),
    );
    if response.changed() {
        text.clone_from(&typing.text);
        typing.sent();
    }
    typing.keep(ui.ctx(), id, response.has_focus());
    response
}

/// The show's frame size, for what is sized against it.
fn frame_of(data: &super::super::UIData) -> (u32, u32) {
    (data.render_width.max(1), data.render_height.max(1))
}

fn look(
    ui: &mut egui::Ui,
    deck: &DeckUIInfo,
    lower: &LowerThirdSnapshot,
    frame: (u32, u32),
    actions: &mut UIActions,
) {
    let before = shown_settings(actions, &deck.uuid, lower);
    let mut settings = before.clone();
    let look = &mut settings.look;
    // One of its sliders or numbers being dragged; a colour changed.
    let mut held = false;
    let mut recoloured = false;
    egui::Grid::new(("lower_third_look_grid", &deck.uuid))
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Style");
            ui.horizontal_wrapped(|ui| {
                let was = look.style;
                egui::ComboBox::from_id_salt(("lower_third_style", &deck.uuid))
                    .selected_text(look.style.label())
                    .show_ui(ui, |ui| {
                        for style in LowerThirdStyle::ALL {
                            ui.selectable_value(&mut look.style, style, style.label());
                        }
                    });
                // A picture of one's own comes in first, then the name, and
                // is shown as it is: whole, not see-through.
                if look.style == LowerThirdStyle::Image && was != LowerThirdStyle::Image {
                    look.text_after = true;
                    look.plate_opacity = 1.0;
                }
                if matches!(look.style, LowerThirdStyle::Image | LowerThirdStyle::FullFrame) {
                    let hover = if look.style == LowerThirdStyle::FullFrame {
                        "A PNG made at the show's size (1920×1080 or 3840×2160), clear but for \
                         your plate: shown over the whole frame, where you drew it"
                    } else {
                        "A PNG or JPEG with your design: the plate, stretched only to fit a long name"
                    };
                    if ui
                        .button("Choose picture…")
                        .on_hover_text(hover)
                        .clicked()
                    {
                        choose_picture(ui.ctx(), &deck.uuid);
                    }
                    let file = std::path::Path::new(&look.image)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned());
                    match file {
                        Some(file) if std::path::Path::new(&look.image).exists() => {
                            ui.label(file).on_hover_text(look.image.clone());
                            // A picture made at a show's size, used as a plate
                            // that fits the text: offered as the full frame it
                            // was made for.
                            if look.style == LowerThirdStyle::Image
                                && frame_sized(ui.ctx(), &look.image, frame)
                                && ui
                                    .button("Use Full-frame picture")
                                    .on_hover_text(
                                        "This picture is the size of a frame: shown over the whole \
                                         frame as you drew it, not fitted to the text",
                                    )
                                    .clicked()
                            {
                                look.style = LowerThirdStyle::FullFrame;
                            }
                        }
                        Some(file) => {
                            ui.label(egui::RichText::new(format!("⚠ {file} is not there")).color(WARN));
                        }
                        None => {
                            ui.label(egui::RichText::new("none chosen yet").weak());
                        }
                    }
                }
            });
            ui.end_row();
            let full = look.style == LowerThirdStyle::FullFrame;
            if full {
                held |= full_frame_rows(ui, &deck.uuid, look);
            } else {
                ui.label("");
                ui.checkbox(&mut look.text_after, "Text after the plate")
                    .on_hover_text("The plate comes in first, then the name and lines; out, the other way round");
                ui.end_row();
            }
            ui.label("Colours");
            egui::ComboBox::from_id_salt(("lower_third_palette", &deck.uuid))
                .selected_text(palette_name(look.palette))
                .show_ui(ui, |ui| {
                    for p in (1..=PALETTE_COUNT).chain([0]) {
                        ui.selectable_value(&mut look.palette, p, palette_name(p));
                    }
                })
                .response
                .on_hover_text("The same palettes as the Broadcast backgrounds, or your own");
            ui.end_row();
            if look.palette == 0 {
                ui.label("Plate, bar");
                ui.horizontal(|ui| {
                    recoloured |= colour(ui, &mut look.plate, "The plate");
                    recoloured |= colour(ui, &mut look.bar_colour, "The bar");
                });
                ui.end_row();
                ui.label("Name, title");
                ui.horizontal(|ui| {
                    recoloured |= colour(ui, &mut look.name_colour, "The name");
                    recoloured |= colour(ui, &mut look.title_colour, "The title and second line");
                });
                ui.end_row();
            }
            if !full {
            ui.label("Plate");
            held |= ui.add(
                egui::Slider::new(&mut look.plate_opacity, 0.0..=1.0)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0)),
            )
            .on_hover_text("How solid the plate is: 0% is text alone")
                .dragged();
            ui.end_row();
            ui.label("Bar");
            egui::ComboBox::from_id_salt(("lower_third_bar", &deck.uuid))
                .selected_text(look.bar.label())
                .show_ui(ui, |ui| {
                    for bar in LowerThirdBar::ALL {
                        ui.selectable_value(&mut look.bar, bar, bar.label());
                    }
                });
            ui.end_row();
            ui.label("Place");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("lower_third_place", &deck.uuid))
                    .selected_text(look.place.label())
                    .show_ui(ui, |ui| {
                        for place in LowerThirdPlace::ALL {
                            ui.selectable_value(&mut look.place, place, place.label());
                        }
                    });
                ui.checkbox(&mut look.safe_area, "Safe area")
                    .on_hover_text("Kept 5% in from the edges, as a TV picture is; off: 2%");
            });
            ui.end_row();
            ui.label("Size");
            held |= ui.add(
                egui::Slider::new(&mut look.size, 0.015..=0.15)
                    .custom_formatter(|v, _| format!("{:.1}%", v * 100.0)),
            )
            .on_hover_text("The name's height, as a share of the picture's")
                .dragged();
            ui.end_row();
            }
            ui.label("Font");
            ui.horizontal(|ui| {
                super::timer::font_picker(ui, &mut look.font);
                if !has_lithuanian(ui.ctx(), &look.font) {
                    ui.label(egui::RichText::new("⚠").color(WARN)).on_hover_text(
                        "This font lacks some Lithuanian letters (ąčęėįšųūž): they would show as boxes",
                    );
                }
            });
            ui.end_row();
            if !full {
            ui.label("Motion");
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("lower_third_motion", &deck.uuid))
                    .selected_text(look.motion.label())
                    .show_ui(ui, |ui| {
                        for motion in LowerThirdMotion::ALL {
                            ui.selectable_value(&mut look.motion, motion, motion.label());
                        }
                    });
                if look.motion != LowerThirdMotion::Cut {
                    held |= ui.add(
                        egui::DragValue::new(&mut look.motion_secs)
                            .range(0.1..=3.0)
                            .speed(0.02)
                            .suffix(" s"),
                    )
                    .on_hover_text("How long it takes to come in, and to go out")
                .dragged();
                }
            });
            ui.end_row();
            }
            ui.label("Off after");
            held |= ui.add(
                egui::DragValue::new(&mut look.auto_out_secs)
                    .range(0.0..=3600.0)
                    .speed(0.2)
                    .custom_formatter(|v, _| {
                        if v <= 0.0 {
                            "stays on".to_string()
                        } else {
                            format!("{v:.0} s")
                        }
                    }),
            )
            .on_hover_text("Taken off air by itself after this long; 0 stays on until Hide")
                .dragged();
            ui.end_row();
        });
    // A slider or number dragged, or a colour dragged in its picker, is one
    // undo step, not one a frame: from its first change (that frame starts
    // the step) for as long as the pointer is held.
    let step_id = ui.make_persistent_id(("lower_third_look_step", &deck.uuid));
    let was = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(step_id))
        .unwrap_or(false);
    let stepping =
        ui.input(|i| i.pointer.any_down()) && (was || ((held || recoloured) && settings != before));
    ui.ctx().data_mut(|d| d.insert_temp(step_id, stepping));
    if stepping {
        actions.session.gesture_active = true;
    }
    if settings != before {
        set_settings(actions, &deck.uuid, settings);
    }
}

/// Whether the picture at `path` is the size of a frame — the shape of the
/// show's `frame`, at least 1920 across — read once per file as saved (kept
/// in the UI's memory): saved again, read again.
fn frame_sized(ctx: &egui::Context, path: &str, frame: (u32, u32)) -> bool {
    let saved = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let id = egui::Id::new(("frame_sized_picture", path, saved, frame));
    if let Some(known) = ctx.data(|d| d.get_temp::<bool>(id)) {
        return known;
    }
    #[allow(clippy::cast_precision_loss)]
    let shape = frame.0 as f32 / frame.1.max(1) as f32;
    #[allow(clippy::cast_precision_loss)]
    let sized = image::image_dimensions(path).is_ok_and(|(w, h)| {
        w >= 1920 && h > 0 && ((w as f32 / h as f32) - shape).abs() < 0.05
    });
    ctx.data_mut(|d| d.insert_temp(id, sized));
    sized
}

/// A share of the frame, edited as a percentage; true while dragged.
fn share(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    hover: &str,
) -> bool {
    let mut percent = *value * 100.0;
    let response = ui
        .add(
            egui::DragValue::new(&mut percent)
                .range((range.start() * 100.0)..=(range.end() * 100.0))
                .speed(0.2)
                .max_decimals(1)
                .suffix("%"),
        )
        .on_hover_text(hover);
    if response.changed() {
        *value = percent / 100.0;
    }
    response.dragged()
}

/// A part's motion — the full-frame picture's or its text's: its kind, how
/// long it takes and after how long it starts; true while dragged.
fn part_motion(
    ui: &mut egui::Ui,
    id: (&str, &str),
    motion: &mut LowerThirdMotion,
    secs: &mut f32,
    delay: &mut f32,
) -> bool {
    let mut held = false;
    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt(id)
            .selected_text(motion.label())
            .show_ui(ui, |ui| {
                for m in LowerThirdMotion::ALL {
                    ui.selectable_value(motion, m, m.label());
                }
            });
        if *motion != LowerThirdMotion::Cut {
            held |= ui
                .add(
                    egui::DragValue::new(secs)
                        .range(0.1..=3.0)
                        .speed(0.02)
                        .suffix(" s"),
                )
                .on_hover_text("How long it takes to come in, and to go out")
                .dragged();
        }
        ui.label("after");
        held |= ui
            .add(
                egui::DragValue::new(delay)
                    .range(0.0..=3.0)
                    .speed(0.02)
                    .suffix(" s"),
            )
            .on_hover_text("How long after Show it starts (going out, the same backwards)")
            .dragged();
    });
    held
}

/// A full-frame picture's rows: the picture's size and place, the text box,
/// and the picture's and the text's motions. True while one is dragged.
fn full_frame_rows(ui: &mut egui::Ui, deck_uuid: &str, look: &mut LowerThirdLook) -> bool {
    use crate::engine::value::text::TextAlign;
    let mut held = false;
    ui.label("Picture");
    ui.horizontal(|ui| {
        held |= ui
            .add(
                egui::DragValue::new(&mut look.picture_scale)
                    .range(0.1..=4.0)
                    .speed(0.005)
                    .prefix("× "),
            )
            .on_hover_text("Its size: 1 as drawn, over the whole frame")
            .dragged();
        ui.label("moved");
        held |= share(
            ui,
            &mut look.picture_offset[0],
            -1.0..=1.0,
            "Across, a share of the frame's width",
        );
        held |= share(
            ui,
            &mut look.picture_offset[1],
            -1.0..=1.0,
            "Down, a share of the frame's height",
        );
    });
    ui.end_row();
    ui.label("Text box");
    ui.horizontal(|ui| {
        let [x, y, w, h] = &mut look.text_box;
        held |= share(
            ui,
            x,
            0.0..=0.98,
            "Its left edge, a share of the frame's width",
        );
        held |= share(ui, y, 0.0..=0.98, "Its top, a share of the frame's height");
        held |= share(
            ui,
            w,
            0.02..=1.0,
            "Its width: a longer name is made smaller, then wrapped",
        );
        held |= share(
            ui,
            h,
            0.02..=1.0,
            "Its height: the lines as tall as fill it",
        );
        egui::ComboBox::from_id_salt(("lower_third_text_align", deck_uuid))
            .selected_text(look.text_align.label())
            .width(70.0)
            .show_ui(ui, |ui| {
                for align in TextAlign::ALL {
                    ui.selectable_value(&mut look.text_align, align, align.label());
                }
            });
    });
    ui.end_row();
    ui.label("Picture in");
    held |= part_motion(
        ui,
        ("lower_third_picture_motion", deck_uuid),
        &mut look.picture_motion,
        &mut look.picture_secs,
        &mut look.picture_delay,
    );
    ui.end_row();
    ui.label("Text in");
    held |= part_motion(
        ui,
        ("lower_third_text_motion", deck_uuid),
        &mut look.text_motion,
        &mut look.text_secs,
        &mut look.text_delay,
    );
    ui.end_row();
    held
}

fn colour(ui: &mut egui::Ui, rgb: &mut [f32; 3], hover: &str) -> bool {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut bytes = rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
    if egui::color_picker::color_edit_button_srgb(ui, &mut bytes)
        .on_hover_text(hover)
        .changed()
    {
        *rgb = bytes.map(|v| f32::from(v) / 255.0);
        return true;
    }
    false
}

/// Whether `font` has the Lithuanian letters, asked once per font.
pub(super) fn has_lithuanian(ctx: &egui::Context, font: &str) -> bool {
    let id = egui::Id::new(("lower_third_font_lt", font));
    if let Some(known) = ctx.data(|d| d.get_temp::<bool>(id)) {
        return known;
    }
    // Not until the system's fonts are read (on their own thread): taken as
    // fine meanwhile, and asked again.
    if crate::deck::svg::font_families().is_none() {
        return true;
    }
    let has = crate::deck::lower_third::font_has(font, LITHUANIAN_LETTERS);
    ctx.data_mut(|d| d.insert_temp(id, has));
    has
}

// ── The list file ──────────────────────────────────────────────────

type ChosenList =
    std::sync::Arc<std::sync::Mutex<Option<std::sync::mpsc::Receiver<std::path::PathBuf>>>>;

fn list_id(deck_uuid: &str) -> egui::Id {
    egui::Id::new(("lower_third_list_chosen", deck_uuid))
}

fn picture_id(deck_uuid: &str) -> egui::Id {
    egui::Id::new(("lower_third_picture_chosen", deck_uuid))
}

/// Ask for a plate picture for lower third `deck_uuid`.
fn choose_picture(ctx: &egui::Context, deck_uuid: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("lower-third-picture".into())
        .spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("A picture for the plate: PNG or JPEG")
                .add_filter("Pictures", &["png", "jpg", "jpeg"])
                .pick_file();
            if let Some(path) = picked {
                let _ = tx.send(path);
            }
        });
    if spawned.is_ok() {
        let chosen: ChosenList = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
        ctx.data_mut(|d| d.insert_temp(picture_id(deck_uuid), chosen));
    }
}

/// The plate picture the dialog came back with for `deck_uuid`, once.
fn chosen_picture(ctx: &egui::Context, deck_uuid: &str) -> Option<String> {
    let chosen: ChosenList = ctx.data(|d| d.get_temp(picture_id(deck_uuid)))?;
    let mut waiting = chosen.lock().ok()?;
    let path = waiting.as_ref()?.try_recv().ok()?;
    *waiting = None;
    Some(path.to_string_lossy().into_owned())
}

/// Ask for a list file for lower third `deck_uuid`.
fn choose_list(ctx: &egui::Context, deck_uuid: &str) {
    let (tx, rx) = std::sync::mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("lower-third-list".into())
        .spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("A list of names: CSV or TSV")
                .add_filter("Names", &["csv", "tsv", "txt"])
                .pick_file();
            if let Some(path) = picked {
                let _ = tx.send(path);
            }
        });
    if spawned.is_ok() {
        let chosen: ChosenList = std::sync::Arc::new(std::sync::Mutex::new(Some(rx)));
        ctx.data_mut(|d| d.insert_temp(list_id(deck_uuid), chosen));
    }
}

/// The list file the dialog came back with for `deck_uuid`, once.
fn chosen_list(ctx: &egui::Context, deck_uuid: &str) -> Option<String> {
    let chosen: ChosenList = ctx.data(|d| d.get_temp(list_id(deck_uuid)))?;
    let mut waiting = chosen.lock().ok()?;
    let path = waiting.as_ref()?.try_recv().ok()?;
    *waiting = None;
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::UIData;
    use egui_kittest::kittest::Queryable;

    /// Whether a frame of the controls asked for its edit to continue an
    /// undo step, and the settings it sent.
    #[derive(Default)]
    struct Frame {
        stepping: bool,
        sent: usize,
    }

    /// A name being typed, then changed elsewhere (an undo, the list read
    /// again): the field gives way, the typing not sent back.
    #[test]
    fn a_name_field_gives_way_to_a_change_made_elsewhere() {
        struct State {
            lower: LowerThirdSnapshot,
            sent: Vec<String>,
        }
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let row = |name: &str| LowerThirdRow {
            name: name.into(),
            ..LowerThirdRow::default()
        };
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 500.0))
            .build_ui_state(
                move |ui, state: &mut State| {
                    let mut actions = UIActions::new();
                    render_controls(ui, &data, &deck, &state.lower, &mut actions);
                    state
                        .sent
                        .extend(actions.commands.into_iter().filter_map(|c| match c {
                            EngineCommand::SetLowerThirdSettings { settings, .. } => {
                                settings.rows.first().map(|r| r.name.clone())
                            }
                            _ => None,
                        }));
                },
                State {
                    lower: LowerThirdSnapshot {
                        settings: LowerThirdSettings {
                            rows: vec![row("Jonas")],
                            ..LowerThirdSettings::default()
                        },
                        on_air: None,
                        live: None,
                        problem: None,
                    },
                    sent: Vec::new(),
                },
            );
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the name's field")
            .click();
        h.run_steps(2);
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the name's field")
            .type_text("X");
        h.run_steps(2);
        assert!(h.state().sent.last().is_some_and(|n| n.contains('X')));
        h.state_mut().lower.settings.rows = vec![row("Ona")];
        h.run_steps(2);
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the name's field")
            .type_text("Y");
        h.run_steps(2);
        let last = h.state().sent.last().cloned().unwrap_or_default();
        assert!(
            last.starts_with("Ona") && !last.contains('X'),
            "sent {last:?}"
        );
    }

    /// The Lower thirds panel lists every lower third with its names; ▶ on
    /// a name shows that one.
    #[test]
    fn the_panel_lists_the_names_and_shows_one() {
        let mut data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = &mut data.channels[ch].decks[dk];
        deck.lower_third = Some(LowerThirdSnapshot {
            settings: LowerThirdSettings {
                rows: vec![
                    LowerThirdRow {
                        name: "Jonas Jonaitis".into(),
                        ..LowerThirdRow::default()
                    },
                    LowerThirdRow {
                        name: "Ona Onaitė".into(),
                        ..LowerThirdRow::default()
                    },
                ],
                ..LowerThirdSettings::default()
            },
            on_air: None,
            live: None,
            problem: None,
        });
        let uuid = deck.uuid.clone();
        let mut actions = UIActions::new();
        {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(900.0, 600.0))
                .build_ui(|ui| render_panel(ui, &data, &mut actions));
            assert!(
                h.query_by_label("Jonas Jonaitis").is_some() || {
                    // Names typed in are in fields: their text is their value.
                    h.query_all_by_role(egui::accesskit::Role::TextInput)
                        .count()
                        >= 2
                }
            );
            // The first ▶ is Show (the cued name); then one a name.
            h.query_all_by_label("▶")
                .nth(2)
                .expect("the second name's ▶")
                .click();
            h.run();
        }
        let shown: Vec<Option<usize>> = actions
            .commands
            .iter()
            .filter_map(|c| match c {
                EngineCommand::LowerThirdShow { deck_uuid, row } if *deck_uuid == uuid => {
                    Some(*row)
                }
                _ => None,
            })
            .collect();
        assert_eq!(shown, [Some(1)]);
    }

    /// A picture the size of a frame used as a plate offers the full-frame
    /// style it was made for; one click switches to it. A small one does not.
    #[test]
    fn a_frame_sized_plate_picture_offers_the_full_frame_style() {
        let dir = tempfile::tempdir().expect("dir");
        let big = dir.path().join("slide.png");
        let small = dir.path().join("plate.png");
        image::RgbaImage::new(1920, 1080).save(&big).expect("saved");
        image::RgbaImage::new(800, 200).save(&small).expect("saved");
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let lower = |path: &std::path::Path| {
            let mut settings = LowerThirdSettings {
                rows: vec![LowerThirdRow {
                    name: "Jonas".into(),
                    ..LowerThirdRow::default()
                }],
                ..LowerThirdSettings::default()
            };
            settings.look.style = LowerThirdStyle::Image;
            settings.look.image = path.to_string_lossy().into_owned();
            LowerThirdSnapshot {
                settings,
                on_air: None,
                live: None,
                problem: None,
            }
        };
        let big_lower = lower(&big);
        let mut actions = UIActions::new();
        {
            let mut h = egui_kittest::Harness::builder()
                .with_size(egui::vec2(900.0, 900.0))
                .build_ui(|ui| look(ui, &deck, &big_lower, (1920, 1080), &mut actions));
            h.get_by_label("Use Full-frame picture").click();
            h.run_steps(2);
        }
        assert!(
            actions.commands.iter().any(|c| matches!(
                c,
                EngineCommand::SetLowerThirdSettings { settings, .. }
                    if settings.look.style == LowerThirdStyle::FullFrame
            )),
            "{:?}",
            actions.commands
        );
        let small_lower = lower(&small);
        let mut actions = UIActions::new();
        let h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .build_ui(|ui| look(ui, &deck, &small_lower, (1920, 1080), &mut actions));
        assert!(h.query_by_label("Use Full-frame picture").is_none());
    }

    /// In Main mode, "Main" in Show on makes its input an overlay over Main
    /// and switches it on; off, switches it off.
    #[test]
    fn main_in_show_on_switches_its_overlay() {
        let mut data = UIData::test_fixture();
        data.main.mode = crate::mixer::MixMode::Program;
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        data.channels[ch].overlay = false;
        let channel = data.channels[ch].uuid.clone();
        data.channels[ch].decks[dk].lower_third = Some(LowerThirdSnapshot {
            settings: LowerThirdSettings {
                rows: vec![LowerThirdRow {
                    name: "Jonas".into(),
                    ..LowerThirdRow::default()
                }],
                ..LowerThirdSettings::default()
            },
            on_air: None,
            live: None,
            problem: None,
        });
        let click = |data: &UIData| {
            let mut actions = UIActions::new();
            {
                let mut h = egui_kittest::Harness::builder()
                    .with_size(egui::vec2(900.0, 600.0))
                    .build_ui(|ui| render_panel(ui, data, &mut actions));
                h.get_by_label("Main").click();
                h.run();
            }
            actions.commands
        };
        let sent = click(&data);
        assert!(sent.iter().any(|c| matches!(c, EngineCommand::SetChannelOverlay { channel_uuid, overlay: true } if *channel_uuid == channel)));
        assert!(sent.iter().any(|c| matches!(c, EngineCommand::SetOverlayOn { channel_uuid, on: true, .. } if *channel_uuid == channel)));
        data.channels[ch].overlay = true;
        data.main.overlays_on = vec![channel.clone()];
        let sent = click(&data);
        assert!(sent.iter().any(|c| matches!(c, EngineCommand::SetOverlayOn { channel_uuid, on: false, .. } if *channel_uuid == channel)));
    }

    /// A name typed is one undo step: it starts with the first letter —
    /// focusing the field is no step — and goes on while the field has focus.
    #[test]
    fn a_name_typed_is_one_undo_step_from_its_first_letter() {
        let data = UIData::test_fixture();
        let (ch, dk) = data.selected_deck.expect("fixture selects a deck");
        let deck = data.channels[ch].decks[dk].clone();
        let lower = LowerThirdSnapshot {
            settings: LowerThirdSettings {
                rows: vec![LowerThirdRow {
                    name: "Jonas".into(),
                    ..LowerThirdRow::default()
                }],
                ..LowerThirdSettings::default()
            },
            on_air: None,
            live: None,
            problem: None,
        };
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 500.0))
            .build_ui_state(
                move |ui, frame: &mut Frame| {
                    let mut actions = UIActions::new();
                    render_controls(ui, &data, &deck, &lower, &mut actions);
                    frame.stepping = actions.session.gesture_active;
                    frame.sent += actions
                        .commands
                        .iter()
                        .filter(|c| matches!(c, EngineCommand::SetLowerThirdSettings { .. }))
                        .count();
                },
                Frame::default(),
            );
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the name's field")
            .click();
        h.run_steps(3);
        assert!(!h.state().stepping, "focus alone is no step");
        assert_eq!(h.state().sent, 0);
        h.query_all_by_role(egui::accesskit::Role::TextInput)
            .next()
            .expect("the name's field")
            .type_text("as");
        h.run_steps(1);
        assert!(h.state().sent > 0, "the letters sent");
        h.run_steps(2);
        assert!(h.state().stepping, "the step goes on while it has focus");
    }
}
