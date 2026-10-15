//! Files dragged in from Finder (or any file manager).
//!
//! While files hover over the window, the place they would land is
//! highlighted with a label saying what will happen. On the drop they are
//! added as decks, in the order they were dropped:
//!
//! - over a channel column — into that channel;
//! - over a "Drop here to add channel" zone — into a new channel;
//! - anywhere else — into the selected channel, else the first one.
//!
//! Folders are replaced by the files inside them. Where the pointer is during
//! the drag comes from the runner, which asks the OS for it; without that
//! (no position reported this frame) the drop goes to the selected channel.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::super::{DeckPlace, UIActions, UIData};
use super::insertion::insertion_at;
use crate::deck::media::{expand_folders, MediaKind};
use crate::engine::EngineCommand;

/// Where dropped files will land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The channel column under the pointer.
    Channel(usize),
    /// A new channel, made for them.
    NewChannel,
    /// Not over anything: the selected channel, else the first.
    Default(usize),
}

fn awaiting_id() -> egui::Id {
    egui::Id::new("__file_drop_awaiting_new_channel")
}

/// Files parked until the channel they were dropped into exists: the channel
/// count that means it does, and the files.
type Awaiting = (usize, Vec<PathBuf>);

pub(super) fn handle_file_drop(ui: &egui::Ui, data: &UIData, actions: &mut UIActions) {
    let ctx = ui.ctx();

    // A drop on a new-channel zone waits one frame for the engine to make the
    // channel, then goes into it. No UUID exists until then, and guessing an
    // index could put the files into someone else's channel.
    if let Some((expected, files)) = ctx.memory(|m| m.data.get_temp::<Awaiting>(awaiting_id())) {
        ctx.memory_mut(|m| m.data.remove::<Awaiting>(awaiting_id()));
        match data.channels.last() {
            Some(ch) if data.channels.len() >= expected => {
                actions
                    .session
                    .files_to_add
                    .push((DeckPlace::end_of(ch.uuid.clone()), files));
            }
            _ => log::warn!("Dropped files not added: the new channel was never created"),
        }
    }

    let hovered: Vec<PathBuf> = ctx.input(|i| {
        i.raw
            .hovered_files
            .iter()
            .filter_map(|f| f.path.clone())
            .collect()
    });
    let dropped: Vec<PathBuf> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .map(|f| f.path().to_path_buf())
            .collect()
    });
    if hovered.is_empty() && dropped.is_empty() {
        return;
    }

    // Only a position reported this frame counts: a stale one from before
    // the drag would aim the files at wherever the mouse last was.
    let fresh = ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::PointerMoved(_)))
    });
    let pos = if fresh {
        ctx.input(|i| i.pointer.latest_pos())
    } else {
        None
    };
    // On a playlist's window: into its list.
    if super::playlist::take_drop(
        ctx,
        data,
        pos,
        expand_folders(&hovered).len(),
        &dropped,
        actions,
    ) {
        ctx.request_repaint();
        return;
    }
    let target = resolve_target(ctx, data, pos);

    if !hovered.is_empty() {
        paint_hover(ctx, data, target, &expand_folders(&hovered), pos);
        ctx.request_repaint();
    }
    if dropped.is_empty() {
        return;
    }
    let files = expand_folders(&dropped);
    log::info!("{} file(s) dropped -> {target:?}", files.len());
    match target {
        Target::Channel(i) | Target::Default(i) => {
            if let Some(ch) = data.channels.get(i) {
                // Over a channel, between the cards under the pointer; when
                // not pointing at one, after its last deck.
                // On the input settings window, a new layer goes on top.
                let before_deck_uuid = match (target, pos) {
                    (Target::Channel(_), Some(p)) if super::drop_window_at(ctx, p).is_none() => {
                        insertion_at(ctx, i, p.y).before
                    }
                    _ => None,
                };
                actions.session.files_to_add.push((
                    DeckPlace {
                        channel_uuid: ch.uuid.clone(),
                        before_deck_uuid,
                    },
                    files,
                ));
            }
        }
        Target::NewChannel => {
            actions.commands.push(EngineCommand::AddChannel);
            ctx.memory_mut(|m| {
                m.data
                    .insert_temp::<Awaiting>(awaiting_id(), (data.channels.len() + 1, files));
            });
        }
    }
}

fn resolve_target(ctx: &egui::Context, data: &UIData, pos: Option<egui::Pos2>) -> Target {
    if let Some(pos) = pos {
        if let Some(ch_idx) = super::drop_channel_at(ctx, pos, data.channels.len()) {
            return Target::Channel(ch_idx);
        }
        for side in 0..2 {
            let key = egui::Id::new("new_ch_drop_rect").with(side);
            if ctx
                .memory(|m| m.data.get_temp::<egui::Rect>(key))
                .is_some_and(|r| r.contains(pos))
            {
                return Target::NewChannel;
            }
        }
    }
    let selected = data
        .selected_deck
        .map(|(ch, _)| ch)
        .or(data.selected_channel)
        .filter(|ch| *ch < data.channels.len());
    match selected {
        Some(ch) => Target::Default(ch),
        None if !data.channels.is_empty() => Target::Default(0),
        None => Target::NewChannel,
    }
}

/// What the files are, in words: "2 images, 1 video".
fn describe(files: &[PathBuf]) -> (String, usize) {
    let (mut images, mut videos, mut sounds, mut shaders, mut other) = (0, 0, 0, 0, 0);
    for f in files {
        match MediaKind::of(f) {
            Some(MediaKind::Image) => images += 1,
            Some(MediaKind::Video) => videos += 1,
            Some(MediaKind::Audio) => sounds += 1,
            Some(MediaKind::Shader) => shaders += 1,
            None => other += 1,
        }
    }
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut parts = Vec::new();
    if images > 0 {
        parts.push(plural(images, "image", "images"));
    }
    if videos > 0 {
        parts.push(plural(videos, "video", "videos"));
    }
    if sounds > 0 {
        parts.push(plural(sounds, "sound", "sounds"));
    }
    if shaders > 0 {
        parts.push(plural(shaders, "shader", "shaders"));
    }
    (parts.join(", "), other)
}

/// The label for a hovering drag, e.g. "2 images, 1 video → Ch A".
fn hover_label(data: &UIData, target: Target, files: &[PathBuf]) -> String {
    let (what, other) = describe(files);
    let place = match target {
        Target::Channel(i) => data
            .channels
            .get(i)
            .map_or_else(String::new, |c| c.name.clone()),
        Target::Default(i) => data
            .channels
            .get(i)
            .map_or_else(String::new, |c| format!("{} (selected)", c.name)),
        Target::NewChannel => "a new channel".to_string(),
    };
    let mut label = if what.is_empty() {
        "Nothing here can be added".to_string()
    } else {
        format!("{what} → {place}")
    };
    if other > 0 && !what.is_empty() {
        let files = if other == 1 { "file" } else { "files" };
        let _ = write!(label, "  ({other} other {files} skipped)");
    }
    label
}

fn paint_hover(
    ctx: &egui::Context,
    data: &UIData,
    target: Target,
    files: &[PathBuf],
    pos: Option<egui::Pos2>,
) {
    let accent = egui::Color32::from_rgb(90, 170, 255);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("file_drop_overlay"),
    ));
    let window = pos.and_then(|p| super::drop_window_at(ctx, p));
    let rect = match target {
        Target::Channel(i) if window.is_some_and(|(w, _)| w == i) => window.map(|(_, r)| r),
        Target::Channel(i) | Target::Default(i) => ctx.memory(|m| {
            m.data
                .get_temp::<egui::Rect>(egui::Id::new("ch_drop_rect").with(i))
        }),
        Target::NewChannel => (0..2).find_map(|side| {
            ctx.memory(|m| {
                m.data
                    .get_temp::<egui::Rect>(egui::Id::new("new_ch_drop_rect").with(side))
            })
        }),
    };
    if let Some(rect) = rect {
        painter.rect_filled(
            rect,
            6.0,
            egui::Color32::from_rgba_unmultiplied(90, 170, 255, 40),
        );
        painter.rect_stroke(
            rect,
            6.0,
            egui::Stroke::new(2.0, accent),
            egui::StrokeKind::Inside,
        );
    }
    if let (Target::Channel(i), Some(p), None) = (target, pos, window) {
        super::insertion::paint(ctx, &insertion_at(ctx, i, p.y), accent);
    }
    let label = hover_label(data, target, files);
    let at = pos
        .map(|p| p + egui::vec2(16.0, 16.0))
        .or_else(|| rect.map(|r| r.center_top() + egui::vec2(0.0, 24.0)))
        .unwrap_or_else(|| ctx.content_rect().center());
    let galley = painter.layout_no_wrap(
        label,
        egui::FontId::proportional(15.0),
        egui::Color32::WHITE,
    );
    let bg = egui::Rect::from_min_size(at, galley.size()).expand(8.0);
    painter.rect_filled(
        bg,
        6.0,
        egui::Color32::from_rgba_unmultiplied(20, 30, 50, 235),
    );
    painter.rect_stroke(
        bg,
        6.0,
        egui::Stroke::new(1.0, accent),
        egui::StrokeKind::Inside,
    );
    painter.galley(at, galley, egui::Color32::WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Dropped(PathBuf);

    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &std::path::Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            std::fs::read(&self.0).map_err(|e| e.to_string())
        }
    }

    /// Run the whole UI with files hovering at `pos` and then dropped there,
    /// returning what it asked for.
    fn drop_at(data: &UIData, files: &[&str], pos: Option<egui::Pos2>) -> (UIActions, String) {
        let mut actions = UIActions::new();
        let label;
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1600.0, 900.0))
                .build_ui(|ui| {
                    actions = super::super::render_ui(ui, data);
                });
            harness.run();
            let paths: Vec<PathBuf> = files.iter().map(PathBuf::from).collect();
            harness.input_mut().hovered_files = paths
                .iter()
                .map(|p| egui::HoveredFile {
                    path: Some(p.clone()),
                    ..Default::default()
                })
                .collect();
            if let Some(pos) = pos {
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::PointerMoved(pos));
            }
            harness.step();
            let target = resolve_target(&harness.ctx, data, pos);
            label = hover_label(data, target, &paths);
            harness.input_mut().hovered_files.clear();
            harness.input_mut().dropped_files = paths
                .iter()
                .map(|p| std::sync::Arc::new(Dropped(p.clone())) as egui::DroppedFileHandle)
                .collect();
            if let Some(pos) = pos {
                harness
                    .input_mut()
                    .events
                    .push(egui::Event::PointerMoved(pos));
            }
            harness.step();
        }
        (actions, label)
    }

    fn channel_rect(data: &UIData, ch_idx: usize) -> egui::Rect {
        let mut rect = None;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1600.0, 900.0))
            .build_ui(|ui| {
                super::super::render_ui(ui, data);
            });
        harness.run();
        harness.ctx.memory(|m| {
            rect = m
                .data
                .get_temp::<egui::Rect>(egui::Id::new("ch_drop_rect").with(ch_idx));
        });
        rect.expect("the channel column publishes its rect")
    }

    #[test]
    fn files_dropped_on_a_channel_go_into_it_in_order() {
        let data = UIData::test_fixture();
        let column = channel_rect(&data, 1);
        let over_b = egui::pos2(column.center().x, column.bottom() - 20.0);
        let (actions, label) = drop_at(
            &data,
            &["/show/slide 1.png", "/show/intro.mov", "/show/slide 2.png"],
            Some(over_b),
        );
        assert_eq!(label, "2 images, 1 video → Ch B");
        assert_eq!(
            actions.session.files_to_add,
            [(
                DeckPlace {
                    channel_uuid: "cb000001".to_string(),
                    // Below both of Ch B's cards: after the last one.
                    before_deck_uuid: None,
                },
                vec![
                    PathBuf::from("/show/slide 1.png"),
                    PathBuf::from("/show/intro.mov"),
                    PathBuf::from("/show/slide 2.png"),
                ]
            )]
        );
    }

    fn card_rects(data: &UIData, ch_idx: usize) -> super::super::insertion::Cards {
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1600.0, 900.0))
            .build_ui(|ui| {
                super::super::render_ui(ui, data);
            });
        harness.run();
        harness
            .ctx
            .memory(|m| {
                m.data.get_temp::<super::super::insertion::Cards>(
                    egui::Id::new("deck_card_rects").with(ch_idx),
                )
            })
            .expect("the column publishes its cards")
    }

    #[test]
    fn files_dropped_between_two_decks_go_between_them() {
        let data = UIData::test_fixture();
        let cards = card_rects(&data, 0);
        assert!(cards.len() >= 2, "precondition: Ch A has two decks");
        // The upper half of Ch A's second card.
        let at = egui::pos2(cards[1].1.center().x, cards[1].1.top() + 8.0);
        let (actions, _) = drop_at(&data, &["/show/lower third.png"], Some(at));
        assert_eq!(
            actions.session.files_to_add,
            [(
                DeckPlace {
                    channel_uuid: "ca000001".to_string(),
                    before_deck_uuid: Some(cards[1].0.clone()),
                },
                vec![PathBuf::from("/show/lower third.png")]
            )]
        );
    }

    #[test]
    fn files_dropped_on_nothing_go_to_the_selected_channel() {
        let mut data = UIData::test_fixture();
        data.selected_deck = Some((1, 0));
        let (actions, label) = drop_at(&data, &["/show/logo.png", "/show/notes.pdf"], None);
        assert_eq!(label, "1 image → Ch B (selected)  (1 other file skipped)");
        assert_eq!(actions.session.files_to_add.len(), 1);
        assert_eq!(
            actions.session.files_to_add[0].0,
            DeckPlace::end_of("cb000001"),
            "not pointing at the channel: after its last deck"
        );
    }

    #[test]
    fn files_dropped_on_the_new_channel_zone_make_a_channel_first() {
        let data = UIData::test_fixture();
        let mut zone = None;
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1600.0, 900.0))
                .build_ui(|ui| {
                    super::super::render_ui(ui, &data);
                });
            harness.run();
            harness.ctx.memory(|m| {
                zone = m
                    .data
                    .get_temp::<egui::Rect>(egui::Id::new("new_ch_drop_rect").with(0));
            });
        }
        let (actions, label) = drop_at(&data, &["/show/intro.mp4"], zone.map(|r| r.center()));
        assert_eq!(label, "1 video → a new channel");
        assert!(actions
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::AddChannel)));
        assert!(
            actions.session.files_to_add.is_empty(),
            "the files wait for the channel to exist"
        );
    }

    #[test]
    fn describing_what_arrives() {
        let files: Vec<PathBuf> = ["a.png", "b.jpg", "c.fs", "d.txt", "e.doc"]
            .iter()
            .map(PathBuf::from)
            .collect();
        assert_eq!(describe(&files), ("2 images, 1 shader".to_string(), 2));
    }
}
