//! Playlists in the UI, as vMix's List: a window listing the clips and
//! pictures and how one gives way to the next, and a play/stop button on
//! the picture of every channel holding one — its tile, its column — so a
//! list starts without opening anything.
//!
//! Files come from a file dialog or dropped from Finder on the list window.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};

use super::super::{ChannelUIInfo, DeckUIInfo, UIActions, UIData};
use crate::deck::media::{expand_folders, MediaKind};
use crate::engine::value::playlist::{
    PlaylistItem, PlaylistSettings, PlaylistSnapshot, PlaylistTransition, SlideFrom,
    MAX_TRANSITION_SECS, MIN_PICTURE_SECS,
};
use crate::engine::{EngineCommand, InputSource};

const PLAYING_GREEN: egui::Color32 = egui::Color32::from_rgb(40, 150, 60);
const FAILED_RED: egui::Color32 = egui::Color32::from_rgb(220, 80, 80);

/// A channel's playlist layer, if it has one.
pub(super) fn of_channel(ch: &ChannelUIInfo) -> Option<(&DeckUIInfo, &PlaylistSnapshot)> {
    ch.decks
        .iter()
        .find_map(|d| d.playlist.as_ref().map(|list| (d, list)))
}

// ── Play / stop ─────────────────────────────────────────────────────

/// ▶ for a stopped playlist, ■ (green) for a playing one.
/// ▶ plays the playlist; ■ stops it, its sound fading out first as a
/// Stop's right-click sets.
pub(super) fn play_button(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    list: &PlaylistSnapshot,
    size: egui::Vec2,
    data: &UIData,
    actions: &mut UIActions,
) -> egui::Response {
    let (text, hover) = if list.playing {
        ("■", super::transport::stop_hover(data, deck_uuid, "Stop"))
    } else {
        ("▶", "Play the playlist".to_string())
    };
    let mut button = egui::Button::new(
        egui::RichText::new(text)
            .size(size.y * 0.6)
            .strong()
            .color(egui::Color32::WHITE),
    )
    .min_size(size);
    button = button.fill(if !list.playing {
        egui::Color32::from_black_alpha(170)
    } else if super::transport::stopping(data, deck_uuid) {
        super::transport::FADING_AMBER
    } else {
        PLAYING_GREEN
    });
    let response = ui.add(button).on_hover_text(hover);
    if list.playing {
        if response.clicked() {
            super::transport::stop(actions, deck_uuid);
        }
        super::transport::stop_menu(&response, data, actions);
    } else if response.clicked() {
        actions.commands.push(EngineCommand::SetPlaylistPlaying {
            deck_uuid: deck_uuid.to_string(),
            playing: true,
        });
    }
    response
}

/// ☰: the playlist's window.
pub(super) fn list_button(ui: &mut egui::Ui, deck_uuid: &str, size: egui::Vec2) -> egui::Response {
    let response = ui
        .add(
            egui::Button::new(
                egui::RichText::new("☰")
                    .size(size.y * 0.6)
                    .color(egui::Color32::WHITE),
            )
            .fill(egui::Color32::from_black_alpha(170))
            .min_size(size),
        )
        .on_hover_text("The playlist: its clips and pictures, transition and times");
    if response.clicked() {
        open(ui.ctx(), deck_uuid);
    }
    response
}

/// Play/stop and the list button over the foot of a channel's picture, with
/// where the list has got: for a tile or a column showing a playlist.
pub(super) fn overlay_on_picture(
    ui: &mut egui::Ui,
    picture: egui::Rect,
    ch: &ChannelUIInfo,
    data: &UIData,
    actions: &mut UIActions,
) {
    let Some((deck, list)) = of_channel(ch) else {
        return;
    };
    let size = egui::vec2(30.0, 24.0);
    let row = egui::Rect::from_min_size(
        picture.left_bottom() + egui::vec2(4.0, -size.y - 4.0),
        egui::vec2(picture.width() - 8.0, size.y),
    );
    let mut row_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    row_ui.spacing_mut().item_spacing.x = 3.0;
    play_button(&mut row_ui, &deck.uuid, list, size, data, actions);
    list_button(&mut row_ui, &deck.uuid, size);
    if let Some(text) = position_text(list) {
        row_ui.label(
            egui::RichText::new(format!(" {text} "))
                .small()
                .color(egui::Color32::WHITE)
                .background_color(egui::Color32::from_black_alpha(170)),
        );
    }
}

/// "3 / 12 · 0:04": the item on screen and the time before it moves on.
fn position_text(list: &PlaylistSnapshot) -> Option<String> {
    if list.items.is_empty() {
        return Some("empty".to_string());
    }
    let shown = list.shown?;
    let mut text = format!("{} / {}", shown + 1, list.items.len());
    if list.playing {
        if let Some(left) = list.remaining {
            let _ = write!(text, " · {}", clock(left));
        }
    }
    Some(text)
}

fn clock(secs: f64) -> String {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let whole = secs.max(0.0).ceil() as u64;
    format!("{}:{:02}", whole / 60, whole % 60)
}

/// ⏮ ▶/■ ⏭ in a row, for the window and the layer's settings.
fn transport(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    list: &PlaylistSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    let size = egui::vec2(34.0, 26.0);
    let len = list.items.len();
    let step = |by: isize| -> Option<usize> {
        let at = list.coming.or(list.shown)?;
        let len = isize::try_from(len).ok()?;
        usize::try_from((isize::try_from(at).ok()? + by).rem_euclid(len.max(1))).ok()
    };
    let mut picked = None;
    let mut skip = |ui: &mut egui::Ui, text: &str, hover: &str, to: Option<usize>| {
        let button = egui::Button::new(egui::RichText::new(text).size(15.0)).min_size(size);
        if ui
            .add_enabled(to.is_some() && len > 1, button)
            .on_hover_text(hover)
            .clicked()
        {
            picked = to;
        }
    };
    skip(ui, "⏮", "The item before", step(-1));
    play_button(ui, deck_uuid, list, size, data, actions);
    skip(ui, "⏭", "The next item, now", step(1));
    if let Some(index) = picked {
        actions.commands.push(EngineCommand::ShowPlaylistItem {
            deck_uuid: deck_uuid.to_string(),
            index,
        });
    }
}

// ── The window ──────────────────────────────────────────────────────

fn window_id() -> egui::Id {
    egui::Id::new("playlist_window")
}

fn rect_id() -> egui::Id {
    egui::Id::new("playlist_window_rect")
}

/// Open the window on the playlist of deck `deck_uuid`.
pub(super) fn open(ctx: &egui::Context, deck_uuid: &str) {
    ctx.data_mut(|d| d.insert_temp(window_id(), deck_uuid.to_string()));
}

fn showing(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(window_id()))
}

fn close(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        d.remove::<String>(window_id());
        d.remove::<egui::Rect>(rect_id());
    });
}

/// Where the window is on screen, while it is open: files dropped there go
/// into the list.
fn window_rect(ctx: &egui::Context) -> Option<egui::Rect> {
    showing(ctx)?;
    ctx.data(|d| d.get_temp::<egui::Rect>(rect_id()))
}

pub(super) fn render_playlist_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    take_chosen(ctx, data, actions);
    open_new_playlist(ctx, data);
    let Some(deck_uuid) = showing(ctx) else {
        return;
    };
    let found = data.channels.iter().find_map(|ch| {
        ch.decks
            .iter()
            .find(|d| d.uuid == deck_uuid)
            .and_then(|d| d.playlist.as_ref().map(|list| (ch, list)))
    });
    // Its layer went away.
    let Some((ch, list)) = found else {
        close(ctx);
        return;
    };
    let mut open = true;
    let window = egui::Window::new(format!("Playlist — {}", ch.name))
        .id(egui::Id::new("playlist_window_frame"))
        .open(&mut open)
        .default_size(egui::vec2(480.0, 460.0))
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| body(ui, &deck_uuid, list, data, actions));
    if let Some(window) = window {
        ctx.data_mut(|d| d.insert_temp(rect_id(), window.response.rect));
    }
    if !open {
        close(ctx);
    }
}

fn body(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    list: &PlaylistSnapshot,
    data: &UIData,
    actions: &mut UIActions,
) {
    ui.horizontal(|ui| {
        transport(ui, deck_uuid, list, data, actions);
        ui.add_space(8.0);
        let now = list
            .shown
            .and_then(|i| list.items.get(i))
            .map_or("—", PlaylistItem::name);
        let mut text = egui::RichText::new(now).strong();
        if list.playing {
            text = text.color(PLAYING_GREEN);
        }
        ui.vertical(|ui| {
            ui.label(text);
            if let Some(position) = position_text(list) {
                ui.label(egui::RichText::new(position).small().weak());
            }
        });
    });
    ui.separator();
    settings(ui, deck_uuid, list.settings, actions);
    ui.separator();
    items(ui, deck_uuid, list, actions);
    ui.separator();
    ui.horizontal(|ui| {
        if ui
            .button(egui::RichText::new("+ Add files…").size(14.0))
            .clicked()
        {
            choose_files(ui.ctx(), FilesFor::List(deck_uuid.to_string()));
        }
        ui.label(
            egui::RichText::new("or drop clips, pictures or folders from Finder here")
                .small()
                .weak(),
        );
    });
}

/// Transition, its time, the speed ramp, how long pictures stay, loop.
fn settings(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    settings: PlaylistSettings,
    actions: &mut UIActions,
) {
    let mut next = settings;
    egui::Grid::new("playlist_settings")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            ui.label("Transition");
            ui.horizontal(|ui| {
                for transition in PlaylistTransition::ALL {
                    ui.selectable_value(&mut next.transition, transition, transition.label());
                }
                if next.transition == PlaylistTransition::Slide {
                    egui::ComboBox::from_id_salt("playlist_slide_from")
                        .selected_text(next.slide_from.label())
                        .show_ui(ui, |ui| {
                            for from in SlideFrom::ALL {
                                ui.selectable_value(&mut next.slide_from, from, from.label());
                            }
                        });
                }
            });
            ui.end_row();

            let timed = next.transition != PlaylistTransition::Cut;
            ui.add_enabled(timed, egui::Label::new("Transition time"));
            ui.horizontal(|ui| {
                seconds(
                    ui,
                    &mut next.transition_secs,
                    0.0..=MAX_TRANSITION_SECS,
                    timed,
                    actions,
                );
                ui.add_enabled(
                    timed,
                    egui::Checkbox::new(&mut next.speed_ramp, "Speed ramp"),
                )
                .on_hover_text(
                    "Start slowly, speed up, and slow down into place — rather than \
                         moving at one speed",
                );
            });
            ui.end_row();

            ui.label("Pictures stay");
            ui.horizontal(|ui| {
                seconds(
                    ui,
                    &mut next.picture_secs,
                    MIN_PICTURE_SECS..=3600.0,
                    true,
                    actions,
                );
                ui.label(
                    egui::RichText::new("clips play to their end")
                        .small()
                        .weak(),
                );
            });
            ui.end_row();

            ui.label("");
            ui.checkbox(&mut next.loop_list, "Loop — start again after the last");
            ui.end_row();
        });
    if next != settings {
        actions.commands.push(EngineCommand::SetPlaylistSettings {
            deck_uuid: deck_uuid.to_string(),
            settings: next,
        });
    }
}

/// A time in seconds; dragging it is one undo step.
fn seconds(
    ui: &mut egui::Ui,
    value: &mut f64,
    range: std::ops::RangeInclusive<f64>,
    enabled: bool,
    actions: &mut UIActions,
) {
    let response = ui.add_enabled(
        enabled,
        egui::DragValue::new(value)
            .range(range)
            .speed(0.05)
            .max_decimals(2)
            .suffix(" s"),
    );
    if response.dragged() {
        actions.session.gesture_active = true;
    }
}

/// The items, in order: the one on screen marked, each moved, taken out or
/// shown now from its row.
fn items(ui: &mut egui::Ui, deck_uuid: &str, list: &PlaylistSnapshot, actions: &mut UIActions) {
    if list.items.is_empty() {
        ui.label(egui::RichText::new("No clips or pictures yet — add some below.").weak());
        return;
    }
    let set = |actions: &mut UIActions, items: Vec<PlaylistItem>| {
        actions.commands.push(EngineCommand::SetPlaylistItems {
            deck_uuid: deck_uuid.to_string(),
            items,
        });
    };
    egui::ScrollArea::vertical()
        .id_salt("playlist_items")
        .max_height(ui.available_height() - 40.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for (i, item) in list.items.iter().enumerate() {
                ui.horizontal(|ui| {
                    let failed = list.failed.contains(&i);
                    let (mark, color) = if list.shown == Some(i) {
                        ("▶", PLAYING_GREEN)
                    } else if list.coming == Some(i) {
                        ("→", PLAYING_GREEN)
                    } else if failed {
                        ("⚠", FAILED_RED)
                    } else {
                        ("", ui.visuals().weak_text_color())
                    };
                    ui.add_sized(
                        egui::vec2(18.0, 20.0),
                        egui::Label::new(egui::RichText::new(mark).color(color)),
                    );
                    let icon = match MediaKind::of(std::path::Path::new(&item.path)) {
                        Some(MediaKind::Image) => "🖼",
                        Some(MediaKind::Audio) => "🔊",
                        _ => "🎬",
                    };
                    let mut name =
                        egui::RichText::new(format!("{:>2}  {icon} {}", i + 1, item.name()));
                    if failed {
                        name = name.color(FAILED_RED);
                    }
                    let row = ui
                        .add(
                            // The grow after the name keeps it at the left.
                            egui::Button::selectable(list.shown == Some(i), name)
                                .right_text("")
                                .min_size(egui::vec2(
                                    (ui.available_width() - 90.0).max(80.0),
                                    20.0,
                                )),
                        )
                        .on_hover_text(if failed {
                            format!("{}\nWould not open — skipped", item.path)
                        } else {
                            format!("{}\nDouble-click to show it now", item.path)
                        });
                    if row.double_clicked() {
                        actions.commands.push(EngineCommand::ShowPlaylistItem {
                            deck_uuid: deck_uuid.to_string(),
                            index: i,
                        });
                    }
                    if ui
                        .add_enabled(i > 0, egui::Button::new("⏶").small())
                        .on_hover_text("Earlier")
                        .clicked()
                    {
                        let mut items = list.items.clone();
                        items.swap(i, i - 1);
                        set(actions, items);
                    }
                    if ui
                        .add_enabled(i + 1 < list.items.len(), egui::Button::new("⏷").small())
                        .on_hover_text("Later")
                        .clicked()
                    {
                        let mut items = list.items.clone();
                        items.swap(i, i + 1);
                        set(actions, items);
                    }
                    if ui
                        .add(egui::Button::new("🗑").small())
                        .on_hover_text("Take it out of the list")
                        .clicked()
                    {
                        let mut items = list.items.clone();
                        items.remove(i);
                        set(actions, items);
                    }
                });
            }
        });
}

/// The layer's settings column for a playlist: where it has got, its
/// transport, and the window.
pub(super) fn render_deck_column(
    ui: &mut egui::Ui,
    data: &UIData,
    deck: &DeckUIInfo,
    list: &PlaylistSnapshot,
    actions: &mut UIActions,
) {
    ui.label(egui::RichText::new("Playlist").strong());
    ui.horizontal(|ui| transport(ui, &deck.uuid, list, data, actions));
    let now = list
        .shown
        .and_then(|i| list.items.get(i))
        .map_or("—", PlaylistItem::name);
    ui.label(egui::RichText::new(now).small());
    if let Some(position) = position_text(list) {
        ui.label(egui::RichText::new(position).small().weak());
    }
    if ui.button("Edit list…").clicked() {
        open(ui.ctx(), &deck.uuid);
    }
}

// ── Files ───────────────────────────────────────────────────────────

/// Where files being chosen go.
#[derive(Clone, Debug)]
pub(super) enum FilesFor {
    /// A new playlist input, called this (after what it is, if not).
    NewInput(Option<String>),
    /// A new playlist layer in this channel.
    Layer(String),
    /// The list of this deck, after what is there.
    List(String),
}

type Chosen = Arc<Mutex<Option<(FilesFor, mpsc::Receiver<Vec<PathBuf>>)>>>;

fn chosen_id() -> egui::Id {
    egui::Id::new("playlist_files_chosen")
}

/// Ask for clips and pictures to go `into` a playlist. The dialog runs on a
/// thread; its answer is taken as the UI renders.
pub(super) fn choose_files(ctx: &egui::Context, into: FilesFor) {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("playlist-files".into())
        .spawn(move || {
            let picked = rfd::FileDialog::new()
                .set_title("Clips and pictures for the playlist")
                .add_filter(
                    "Clips, sound and pictures",
                    &[
                        "mov", "mp4", "m4v", "avi", "mkv", "webm", "gif", "mxf", "mpg", "mpeg",
                        "ts", "mts", "mp3", "wav", "m4a", "aac", "flac", "ogg", "aif", "aiff",
                        "png", "jpg", "jpeg", "bmp", "tif", "tiff", "tga", "webp", "svg", "heic",
                    ],
                )
                .pick_files();
            if let Some(paths) = picked {
                let _ = tx.send(paths);
            }
        });
    if spawned.is_ok() {
        let chosen: Chosen = Arc::new(Mutex::new(Some((into, rx))));
        ctx.data_mut(|d| d.insert_temp(chosen_id(), chosen));
    }
}

/// Files the dialog came back with, as items: folders opened, anything
/// but a clip, sound file or picture left out.
fn as_items(paths: &[PathBuf]) -> Vec<PlaylistItem> {
    expand_folders(paths)
        .into_iter()
        .filter(|p| {
            matches!(
                MediaKind::of(p),
                Some(MediaKind::Image | MediaKind::Video | MediaKind::Audio)
            )
        })
        .map(|p| PlaylistItem::new(p.to_string_lossy()))
        .collect()
}

/// Put files into deck `deck_uuid`'s list, after what is there.
fn add_items(data: &UIData, deck_uuid: &str, new: Vec<PlaylistItem>, actions: &mut UIActions) {
    let Some(list) = data
        .channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .find(|d| d.uuid == deck_uuid)
        .and_then(|d| d.playlist.as_ref())
    else {
        return;
    };
    if new.is_empty() {
        return;
    }
    let mut items = list.items.clone();
    items.extend(new);
    actions.commands.push(EngineCommand::SetPlaylistItems {
        deck_uuid: deck_uuid.to_string(),
        items,
    });
}

fn awaiting_new_id() -> egui::Id {
    egui::Id::new("playlist_awaiting_new")
}

/// Every playlist layer there is.
fn playlist_decks(data: &UIData) -> Vec<String> {
    data.channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .filter(|d| d.playlist.is_some())
        .map(|d| d.uuid.clone())
        .collect()
}

/// A playlist is about to be made: open its window once it is there.
pub(super) fn open_when_made(ctx: &egui::Context, data: &UIData) {
    let known = playlist_decks(data);
    ctx.data_mut(|d| d.insert_temp(awaiting_new_id(), (known, 0_u32)));
}

/// Open the window of the playlist just made, once it is there — given up
/// after a while if it never is (the file could not be added).
fn open_new_playlist(ctx: &egui::Context, data: &UIData) {
    let Some((known, frames)) = ctx.data(|d| d.get_temp::<(Vec<String>, u32)>(awaiting_new_id()))
    else {
        return;
    };
    let made = playlist_decks(data)
        .into_iter()
        .find(|uuid| !known.contains(uuid));
    match made {
        Some(uuid) => {
            ctx.data_mut(|d| d.remove::<(Vec<String>, u32)>(awaiting_new_id()));
            open(ctx, &uuid);
        }
        None if frames > 120 => {
            ctx.data_mut(|d| d.remove::<(Vec<String>, u32)>(awaiting_new_id()));
        }
        None => {
            ctx.data_mut(|d| d.insert_temp(awaiting_new_id(), (known, frames + 1)));
        }
    }
}

fn take_chosen(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let Some(chosen) = ctx.data(|d| d.get_temp::<Chosen>(chosen_id())) else {
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
        // Cancelled.
        Some(Err(mpsc::TryRecvError::Disconnected)) | None => None,
    };
    let into = slot.take().map(|(into, _)| into);
    drop(slot);
    ctx.data_mut(|d| d.remove::<Chosen>(chosen_id()));
    let (Some(paths), Some(into)) = (answer, into) else {
        return;
    };
    let items = as_items(&paths);
    if items.is_empty() {
        return;
    }
    match into {
        FilesFor::List(deck_uuid) => add_items(data, &deck_uuid, items, actions),
        FilesFor::NewInput(name) => {
            actions.commands.push(EngineCommand::AddInput {
                source: InputSource::Playlist { items },
                name,
                audio: None,
            });
            open_when_made(ctx, data);
        }
        FilesFor::Layer(channel_uuid) => {
            actions.commands.push(EngineCommand::AddPlaylistDeck {
                channel_uuid,
                items,
                settings: None,
            });
            open_when_made(ctx, data);
        }
    }
}

/// Files dropped from Finder on the window go into its list. True when
/// they did (or, `dropped` empty, would): the window was under `pos`.
pub(super) fn take_drop(
    ctx: &egui::Context,
    data: &UIData,
    pos: Option<egui::Pos2>,
    hovered: usize,
    dropped: &[PathBuf],
    actions: &mut UIActions,
) -> bool {
    let (Some(pos), Some(rect), Some(deck_uuid)) = (pos, window_rect(ctx), showing(ctx)) else {
        return false;
    };
    if !rect.contains(pos) {
        return false;
    }
    if dropped.is_empty() {
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Tooltip,
            egui::Id::new("playlist_drop_hover"),
        ));
        painter.rect_stroke(
            rect,
            4.0,
            egui::Stroke::new(3.0, PLAYING_GREEN),
            egui::StrokeKind::Inside,
        );
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("Add {hovered} file(s) to the playlist"),
            egui::FontId::proportional(18.0),
            egui::Color32::WHITE,
        );
    } else {
        add_items(data, &deck_uuid, as_items(dropped), actions);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::Queryable;

    fn playlist(playing: bool) -> PlaylistSnapshot {
        PlaylistSnapshot {
            items: vec![
                PlaylistItem::new("/show/intro.mp4"),
                PlaylistItem::new("/show/slide.png"),
                PlaylistItem::new("/show/end.png"),
            ],
            settings: PlaylistSettings::default(),
            playing,
            shown: Some(1),
            coming: None,
            next_ready: Some(2),
            failed: vec![2],
            remaining: Some(3.2),
        }
    }

    /// The fixture with Ch A's first layer a playlist.
    fn data(playing: bool) -> UIData {
        let mut data = UIData::test_fixture();
        data.channels[0].decks[0].playlist = Some(playlist(playing));
        data
    }

    fn window(data: &UIData, click: &str) -> UIActions {
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(700.0, 700.0))
                .build_ui(|ui| {
                    let uuid = data.channels[0].decks[0].uuid.clone();
                    open(ui.ctx(), &uuid);
                    render_playlist_window(ui.ctx(), data, &mut actions);
                });
            harness.run_steps(2);
            harness
                .get_by_role_and_label(egui::accesskit::Role::Button, click)
                .click();
            harness.run_steps(2);
        }
        actions
    }

    #[test]
    fn a_channel_with_a_playlist_is_found_by_it() {
        let data = data(false);
        assert!(of_channel(&data.channels[0]).is_some());
        assert!(of_channel(&data.channels[1]).is_none());
    }

    /// ▶ starts a stopped list; ■ stops a playing one as a Stop does, its
    /// sound fading out first.
    #[test]
    fn play_starts_a_stopped_list_and_stop_stops_a_playing_one() {
        let deck = data(false).channels[0].decks[0].uuid.clone();
        let started = window(&data(false), "▶");
        assert!(started.commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetPlaylistPlaying { deck_uuid, playing: true } if *deck_uuid == deck
        )));
        let stopped = window(&data(true), "■");
        assert!(
            matches!(
                stopped.commands.as_slice(),
                [EngineCommand::StopWithFade { deck_uuid, secs: None }] if *deck_uuid == deck
            ),
            "{:?}",
            stopped.commands
        );
    }

    #[test]
    fn next_shows_the_item_after_the_one_on_screen() {
        let actions = window(&data(true), "⏭");
        assert!(actions
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::ShowPlaylistItem { index: 2, .. })));
    }

    #[test]
    fn choosing_slide_changes_the_transition() {
        let actions = window(&data(false), "Slide");
        assert!(actions.commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetPlaylistSettings { settings, .. }
                if settings.transition == PlaylistTransition::Slide
        )));
    }

    #[test]
    fn an_item_is_taken_out_from_its_row() {
        let data = data(false);
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(700.0, 700.0))
                .build_ui(|ui| {
                    let uuid = data.channels[0].decks[0].uuid.clone();
                    open(ui.ctx(), &uuid);
                    render_playlist_window(ui.ctx(), &data, &mut actions);
                });
            harness.run_steps(2);
            harness.get_all_by_label("🗑").next().unwrap().click();
            harness.run_steps(2);
        }
        assert!(actions.commands.iter().any(|c| matches!(
            c,
            EngineCommand::SetPlaylistItems { items, .. }
                if items.len() == 2 && items[0].path == "/show/slide.png"
        )));
    }

    #[test]
    fn the_position_says_the_item_and_time_left() {
        assert_eq!(
            position_text(&playlist(true)).as_deref(),
            Some("2 / 3 · 0:04")
        );
        assert_eq!(position_text(&playlist(false)).as_deref(), Some("2 / 3"));
    }

    #[test]
    fn only_clips_sound_and_pictures_go_in() {
        let dir = tempfile::tempdir().unwrap();
        let (clip, text) = (dir.path().join("a.mp4"), dir.path().join("notes.txt"));
        std::fs::write(&clip, b"").unwrap();
        std::fs::write(&text, b"").unwrap();
        let items = as_items(&[clip, text]);
        assert_eq!(items.len(), 1);
        assert!(items[0].path.ends_with("a.mp4"));
    }
}
