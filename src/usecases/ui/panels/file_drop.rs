//! Files dragged in from Finder (or any file manager).
//!
//! While files hover over the window, the place they would land is
//! highlighted with a label saying what will happen. On the drop, in the
//! order they were dropped:
//!
//! - over a channel — its tile, column or settings window — they are layers
//!   in it: between the two cards in its column they are dropped between,
//!   else behind its layers, so what it shows (on air perhaps) stays;
//! - anywhere else — the empty tile, a "Drop here to add channel" zone, or
//!   nothing — each is an input of its own, as Add Input makes them.
//!
//! Folders are replaced by the files inside them. Where the pointer is during
//! the drag comes from the runner, which asks the OS for it; without that
//! (no position reported this frame) each is an input of its own.

use std::fmt::Write as _;
use std::path::PathBuf;

use super::super::{UIActions, UIData};
use super::insertion::insertion_at;
use crate::deck::media::{expand_folders, MediaKind};
use crate::engine::value::slides::TalkKind;
use crate::engine::EngineCommand;

/// The files hovering over the window this frame, and those dropped on it.
fn files(ctx: &egui::Context) -> (Vec<PathBuf>, Vec<PathBuf>) {
    ctx.input(|i| {
        let hovered = i.raw.hovered_files.iter().filter_map(|f| f.path.clone());
        let dropped = i.raw.dropped_files.iter().map(|f| f.path().to_path_buf());
        (hovered.collect(), dropped.collect())
    })
}

/// Where the pointer is, when reported this frame: a stale place from before
/// the drag would aim the files at wherever the mouse last was.
fn fresh_pointer(ctx: &egui::Context) -> Option<egui::Pos2> {
    ctx.input(|i| {
        i.events
            .iter()
            .any(|e| matches!(e, egui::Event::PointerMoved(_)))
            .then(|| i.pointer.latest_pos())
            .flatten()
    })
}

/// Files dragged onto a panel's window of its own: the Presentations panel
/// there takes them, as in the main window — no other panel that can have a
/// window takes files.
pub(super) fn handle_window_file_drop(ui: &egui::Ui, actions: &mut UIActions) {
    let ctx = ui.ctx();
    let (hovered, dropped) = files(ctx);
    if hovered.is_empty() && dropped.is_empty() {
        return;
    }
    let pos = fresh_pointer(ctx);
    if super::slides::take_drop(ctx, pos, hovered.len(), &dropped, actions) {
        ctx.request_repaint();
    }
}

/// Where dropped files will land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// The channel under the pointer: layers in it.
    Channel(usize),
    /// No channel: an input each.
    NewInputs,
}

pub(super) fn handle_file_drop(ui: &egui::Ui, data: &UIData, actions: &mut UIActions) {
    let ctx = ui.ctx();
    let (hovered, dropped) = files(ctx);
    if hovered.is_empty() && dropped.is_empty() {
        return;
    }
    let pos = fresh_pointer(ctx);
    // On the Presentations panel: talks in its running order.
    // Files counted as hovered, folders not looked into: a large folder on a
    // slow drive would hold the window each frame of the drag.
    if super::slides::take_drop(ctx, pos, hovered.len(), &dropped, actions) {
        ctx.request_repaint();
        return;
    }
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
    // On a Slides input: every file it plays is a talk of it.
    let slides_here = match target {
        Target::Channel(i) => data
            .channels
            .get(i)
            .and_then(super::slides::of_channel)
            .map(|(deck, _)| deck.uuid.clone()),
        Target::NewInputs => None,
    };
    // Presentations — PowerPoint decks, PDFs, web pages — go to the running
    // order wherever they land: the Slides input they are dropped on, else
    // the show's first, made when there is none.
    let (talks, files): (Vec<PathBuf>, Vec<PathBuf>) = files.into_iter().partition(|f| {
        let kind = TalkKind::of(&f.to_string_lossy());
        (cfg!(target_os = "macos") && is_presentation(kind))
            || (slides_here.is_some() && super::slides::plays_here(kind))
    });
    if !talks.is_empty() {
        log::info!("{} talk(s) dropped -> Slides", talks.len());
        actions.commands.push(EngineCommand::AddTalks {
            deck_uuid: slides_here,
            paths: talks
                .iter()
                .map(|p| p.to_string_lossy().into_owned())
                .collect(),
            at: None,
        });
    }
    if files.is_empty() {
        return;
    }
    log::info!("{} file(s) dropped -> {target:?}", files.len());
    match target {
        Target::Channel(i) => {
            if let Some(ch) = data.channels.get(i) {
                // Between the cards under the pointer; anywhere else on the
                // channel — its tile, its settings window — behind its
                // layers, so what it shows on air stays.
                let on_window = pos.is_some_and(|p| super::drop_window_at(ctx, p).is_some());
                let between = super::insertion::place_at(ctx, i, pos, on_window);
                actions.session.files_to_add.push((
                    super::insertion::deck_place(ch.uuid.clone(), between),
                    files,
                ));
            }
        }
        Target::NewInputs => actions.session.files_as_inputs.extend(files),
    }
}

/// A PowerPoint deck, a PDF or a web page: a talk, wherever it is dropped.
fn is_presentation(kind: Option<TalkKind>) -> bool {
    matches!(
        kind,
        Some(TalkKind::PowerPoint | TalkKind::Pdf | TalkKind::Web)
    )
}

fn resolve_target(ctx: &egui::Context, data: &UIData, pos: Option<egui::Pos2>) -> Target {
    pos.and_then(|pos| super::drop_channel_at(ctx, pos, data.channels.len()))
        .map_or(Target::NewInputs, Target::Channel)
}

/// The empty tile, or a "Drop here to add channel" zone, under the pointer.
fn new_channel_zone_at(ctx: &egui::Context, pos: egui::Pos2) -> Option<egui::Rect> {
    (0..2).find_map(|side| {
        ctx.memory(|m| {
            m.data
                .get_temp::<egui::Rect>(egui::Id::new("new_ch_drop_rect").with(side))
        })
        .filter(|r| r.contains(pos))
    })
}

/// What the files are, in words: "2 images, 1 video".
fn describe(files: &[PathBuf]) -> (String, usize) {
    let (mut images, mut videos, mut sounds, mut shaders, mut other) = (0, 0, 0, 0, 0);
    let mut decks = 0;
    for f in files {
        if cfg!(target_os = "macos") && is_presentation(TalkKind::of(&f.to_string_lossy())) {
            decks += 1;
            continue;
        }
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
    if decks > 0 {
        // Talks of the Slides input, wherever they land.
        parts.push(plural(
            decks,
            "presentation (a talk in Slides)",
            "presentations (talks in Slides)",
        ));
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
        Target::NewInputs if files.len() - other == 1 => "a new input".to_string(),
        Target::NewInputs => "a new input each".to_string(),
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
        // The view of the channel the files are over, of those it is in.
        Target::Channel(i) => {
            let rects = super::drop_rects(ctx, i);
            pos.and_then(|p| rects.iter().find(|r| r.contains(p)).copied())
                .or_else(|| rects.first().copied())
        }
        Target::NewInputs => pos.and_then(|p| new_channel_zone_at(ctx, p)),
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
        super::insertion::paint(ctx, &insertion_at(ctx, i, p), accent);
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
    use crate::usecases::ui::DeckPlace;

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
            // As the runner runs it: a second pass would see the frame
            // without its drop, and its actions would replace the drop's.
            crate::usecases::ui::one_pass_a_frame(&harness.ctx);
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
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1600.0, 900.0))
            .build_ui(|ui| {
                super::super::render_ui(ui, data);
            });
        harness.run();
        let rect = super::super::drop_rects(&harness.ctx, ch_idx).first().copied();
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
                    // Below both of Ch B's cards: behind them, at the back.
                    before_deck_uuid: Some(data.channels[1].decks[0].uuid.clone()),
                    back: false,
                },
                vec![
                    PathBuf::from("/show/slide 1.png"),
                    PathBuf::from("/show/intro.mov"),
                    PathBuf::from("/show/slide 2.png"),
                ]
            )]
        );
    }

    /// Dropped on a channel away from its cards — its header, its tile — the
    /// files go behind its layers, not in front: what it shows, on air
    /// perhaps, stays.
    #[test]
    fn files_dropped_on_a_channel_away_from_its_cards_go_behind_its_layers() {
        let data = UIData::test_fixture();
        let column = channel_rect(&data, 0);
        let cards = card_rects(&data, 0);
        let top_card = cards.first().expect("Ch A has cards").1;
        let header = egui::pos2(column.center().x, column.top() + 4.0);
        assert!(header.y < top_card.top() - 8.0, "precondition: above its cards");
        let (actions, _) = drop_at(&data, &["/show/logo.png"], Some(header));
        assert_eq!(
            actions.session.files_to_add,
            [(
                DeckPlace::back_of("ca000001"),
                vec![PathBuf::from("/show/logo.png")]
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
                m.data.get_temp::<Vec<super::super::insertion::Stack>>(
                    super::super::insertion::stacks_id(ch_idx),
                )
            })
            .and_then(|stacks| stacks.into_iter().next())
            .expect("the column publishes its cards")
            .cards
    }

    #[test]
    fn files_dropped_between_two_decks_go_between_them() {
        let data = UIData::test_fixture();
        let cards = card_rects(&data, 0);
        assert!(cards.len() >= 2, "precondition: Ch A has two decks");
        assert_eq!(
            cards[0].0,
            data.channels[0].decks.last().unwrap().uuid,
            "the layer in front is the top card"
        );
        // The upper half of Ch A's second card: in front of it, behind the
        // first — before the first in the stack, drawn from the back.
        let at = egui::pos2(cards[1].1.center().x, cards[1].1.top() + 8.0);
        let (actions, _) = drop_at(&data, &["/show/lower third.png"], Some(at));
        assert_eq!(
            actions.session.files_to_add,
            [(
                DeckPlace {
                    channel_uuid: "ca000001".to_string(),
                    before_deck_uuid: Some(cards[0].0.clone()),
                    back: false,
                },
                vec![PathBuf::from("/show/lower third.png")]
            )]
        );
    }

    /// As vMix: files dropped beside the inputs are inputs, one each — not
    /// layers in whichever channel was selected.
    #[test]
    fn files_dropped_on_nothing_are_an_input_each() {
        let mut data = UIData::test_fixture();
        data.selected_deck = Some((1, 0));
        let files = ["/show/logo.png", "/show/intro.mov", "/show/notes.txt"];
        let (actions, label) = drop_at(&data, &files, None);
        assert_eq!(
            label,
            "1 image, 1 video → a new input each  (1 other file skipped)"
        );
        assert!(actions.session.files_to_add.is_empty(), "into no channel");
        assert_eq!(
            actions.session.files_as_inputs,
            files.map(PathBuf::from),
            "in the order dropped; the runner says what it cannot use"
        );
    }

    /// PowerPoint decks, PDFs and web pages are talks of the Slides input,
    /// wherever they land; clips and pictures beside them are inputs.
    #[cfg(target_os = "macos")]
    #[test]
    fn presentations_dropped_anywhere_are_talks_of_the_slides_input() {
        let data = UIData::test_fixture();
        let files = [
            "/show/keynote.pptx",
            "/show/logo.png",
            "/show/paper.pdf",
            "/show/deck.html",
        ];
        let (actions, label) = drop_at(&data, &files, None);
        assert!(label.contains("3 presentations (talks in Slides)"), "{label}");
        assert_eq!(
            format!("{:?}", actions.commands),
            r#"[AddTalks { deck_uuid: None, paths: ["/show/keynote.pptx", "/show/paper.pdf", "/show/deck.html"], at: None }]"#
        );
        assert_eq!(
            actions.session.files_as_inputs,
            vec![PathBuf::from("/show/logo.png")]
        );
    }

    #[test]
    fn files_dropped_on_the_empty_tile_are_an_input_each() {
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
        let at = zone.map(|r| r.center());
        let (actions, label) = drop_at(&data, &["/show/intro.mp4"], at);
        assert_eq!(label, "1 video → a new input");
        let (actions2, label2) = drop_at(&data, &["/show/a.png", "/show/b.png"], at);
        assert_eq!(label2, "2 images → a new input each");
        assert_eq!(
            actions2.session.files_as_inputs,
            [PathBuf::from("/show/a.png"), PathBuf::from("/show/b.png")]
        );
        assert_eq!(
            actions.session.files_as_inputs,
            [PathBuf::from("/show/intro.mp4")]
        );
        assert!(
            !actions
                .commands
                .iter()
                .any(|c| matches!(c, EngineCommand::AddChannel)),
            "the runner makes the inputs, named after their files"
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
