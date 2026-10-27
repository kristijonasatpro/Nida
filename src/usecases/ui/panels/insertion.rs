//! Where something dropped into a channel's deck stack goes: above the card
//! the pointer is in the upper half of, or below the last one — shown while
//! dragging as a line between the cards. The cards are listed front first,
//! as in Premiere: above a card is in front of it.
//!
//! The spot is named by the deck it goes before (a UUID), never by position:
//! a file that takes a moment to load, or a list that changes mid-drag, must
//! not land next to the wrong deck.

/// Every deck card's rect in one channel, drawn this frame, front first.
pub(super) type Cards = Vec<(String, egui::Rect)>;

/// A channel's cards as one of its views drew them this frame: the cards,
/// the area they are in (only a drop in it goes between them), and whether
/// they run across (a VJ layer's row) rather than down (a Classic column).
#[derive(Clone)]
pub(super) struct Stack {
    pub cards: Cards,
    pub area: egui::Rect,
    pub across: bool,
    /// The layer the view is drawn in.
    layer: egui::LayerId,
    pass: u64,
}

pub(super) fn stacks_id(ch_idx: usize) -> egui::Id {
    egui::Id::new("deck_card_rects").with(ch_idx)
}

/// Record where a channel's cards were drawn this frame by `ui`, down a
/// column, and the column they are in — what of it is in sight. Another view
/// of the channel adds its own.
pub(super) fn publish_cards(ui: &egui::Ui, ch_idx: usize, cards: Cards, area: egui::Rect) {
    let area = area.intersect(ui.clip_rect());
    publish(ui.ctx(), ch_idx, cards, area, false, ui.layer_id());
}

/// [`publish_cards`] for cards running across a row (a VJ layer's).
pub(super) fn publish_cards_across(ui: &egui::Ui, ch_idx: usize, cards: Cards, area: egui::Rect) {
    let area = area.intersect(ui.clip_rect());
    publish(ui.ctx(), ch_idx, cards, area, true, ui.layer_id());
}

fn publish(
    ctx: &egui::Context,
    ch_idx: usize,
    cards: Cards,
    area: egui::Rect,
    across: bool,
    layer: egui::LayerId,
) {
    if !area.is_positive() {
        return;
    }
    let pass = ctx.cumulative_pass_nr();
    ctx.memory_mut(|m| {
        let stacks = m.data.get_temp_mut_or_default::<Vec<Stack>>(stacks_id(ch_idx));
        // A view drawn again (a second pass) replaces what it drew before.
        stacks.retain(|s| s.pass == pass && s.area != area);
        stacks.push(Stack {
            cards,
            area,
            across,
            layer,
            pass,
        });
    });
}

/// Where a drop at `at` in channel `ch_idx` would land.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Insertion {
    /// The deck it goes before in the stack, which is drawn from the back:
    /// the card above (or left of) the spot. `None` is the end, in front of
    /// them all.
    pub before: Option<String>,
    /// Where to draw the marker, from one end to the other. `None` away
    /// from the cards, or for an empty channel, where the whole view lights
    /// up instead.
    pub line: Option<[egui::Pos2; 2]>,
}

pub(super) fn insertion_at(ctx: &egui::Context, ch_idx: usize, at: egui::Pos2) -> Insertion {
    // Cards drawn a while ago are of a view no longer shown; of those shown,
    // the one the pointer is in — of the layer in front there (see
    // `front_layer`): a view without cards there (an Inputs tile, the Stream
    // window) has no spot between them, though cards lie beneath it.
    let now = ctx.cumulative_pass_nr();
    let front = super::front_layer(ctx, at);
    let stack: Option<Stack> = ctx.memory(|m| {
        m.data
            .get_temp::<Vec<Stack>>(stacks_id(ch_idx))?
            .into_iter()
            .find(|s| {
                s.pass + 2 >= now && s.area.contains(at) && front.is_none_or(|f| s.layer == f)
            })
    });
    match stack {
        Some(s) if s.across => insertion_across(&s.cards, s.area, at),
        Some(s) => insertion_in(&s.cards, s.area, at),
        None => insertion_in(&[], egui::Rect::NOTHING, at),
    }
}

/// Where something new dropped on a channel lands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Landing {
    /// Between two of its cards: before this deck (see
    /// [`Insertion::before`]; `None` in front of them all).
    Between(Option<String>),
    /// Behind its layers, the foot of its lists.
    Back,
}

/// Where something new dropped on channel `ch_idx` lands: the spot between
/// the cards under the pointer (`at`) in the channel's column; dropped
/// anywhere else on the channel — its tile, its settings window
/// (`on_window`), its column away from the cards — behind its layers: what
/// the channel shows, on air perhaps, stays as it is until the new deck is
/// moved up.
pub(super) fn place_at(
    ctx: &egui::Context,
    ch_idx: usize,
    at: Option<egui::Pos2>,
    on_window: bool,
) -> Landing {
    match at.filter(|_| !on_window).map(|at| insertion_at(ctx, ch_idx, at)) {
        // A marker between the cards: there.
        Some(insertion) if insertion.line.is_some() => Landing::Between(insertion.before),
        _ => Landing::Back,
    }
}

/// A new deck's place in channel `channel_uuid`, landing as [`place_at`]
/// says.
pub(super) fn deck_place(channel_uuid: String, landing: Landing) -> crate::usecases::ui::DeckPlace {
    match landing {
        Landing::Between(before_deck_uuid) => crate::usecases::ui::DeckPlace {
            channel_uuid,
            before_deck_uuid,
            back: false,
        },
        Landing::Back => crate::usecases::ui::DeckPlace::back_of(channel_uuid),
    }
}

/// [`insertion_at`] on a given set of cards down the column `area`.
/// Outside it — over the channel's tile, or with its column not shown — it
/// goes in front, with no marker (see [`place_at`]).
pub(super) fn insertion_in(
    cards: &[(String, egui::Rect)],
    area: egui::Rect,
    at: egui::Pos2,
) -> Insertion {
    let in_front = Insertion {
        before: None,
        line: None,
    };
    let Some(first) = cards.first() else {
        return in_front;
    };
    if !area.contains(at) {
        return in_front;
    }
    let x = first.1.x_range();
    let across = |y: f32| {
        let y = y.clamp(area.top(), area.bottom());
        Some([egui::pos2(x.min, y), egui::pos2(x.max, y)])
    };
    match cards.iter().position(|(_, r)| at.y < r.center().y) {
        Some(k) => {
            let top = cards[k].1.top();
            let line_y = if k == 0 {
                top - 2.0
            } else {
                f32::midpoint(cards[k - 1].1.bottom(), top)
            };
            Insertion {
                before: k.checked_sub(1).map(|above| cards[above].0.clone()),
                line: across(line_y),
            }
        }
        None => Insertion {
            before: cards.last().map(|(uuid, _)| uuid.clone()),
            line: cards.last().and_then(|(_, r)| across(r.bottom() + 3.0)),
        },
    }
}

/// [`insertion_in`] for cards running across the row `area`, front first
/// from the left: left of a card is in front of it.
pub(super) fn insertion_across(
    cards: &[(String, egui::Rect)],
    area: egui::Rect,
    at: egui::Pos2,
) -> Insertion {
    let in_front = Insertion {
        before: None,
        line: None,
    };
    let Some(first) = cards.first() else {
        return in_front;
    };
    if !area.contains(at) {
        return in_front;
    }
    let y = first.1.y_range();
    let down = |x: f32| {
        let x = x.clamp(area.left(), area.right());
        Some([egui::pos2(x, y.min), egui::pos2(x, y.max)])
    };
    match cards.iter().position(|(_, r)| at.x < r.center().x) {
        Some(k) => {
            let left = cards[k].1.left();
            let line_x = if k == 0 {
                left - 2.0
            } else {
                f32::midpoint(cards[k - 1].1.right(), left)
            };
            Insertion {
                before: k.checked_sub(1).map(|ahead| cards[ahead].0.clone()),
                line: down(line_x),
            }
        }
        None => Insertion {
            before: cards.last().map(|(uuid, _)| uuid.clone()),
            line: cards.last().and_then(|(_, r)| down(r.right() + 3.0)),
        },
    }
}

/// Draw the marker for `insertion`, above everything else.
pub(super) fn paint(ctx: &egui::Context, insertion: &Insertion, color: egui::Color32) {
    let Some([from, to]) = insertion.line else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("deck_insertion_marker"),
    ));
    painter.line_segment([from, to], egui::Stroke::new(3.0, color));
    painter.circle_filled(from + (to - from).normalized() * 3.0, 4.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cards() -> Cards {
        (0..3)
            .map(|i| {
                let top = 100.0 + i as f32 * 110.0;
                (
                    format!("deck{i}"),
                    egui::Rect::from_min_max(egui::pos2(10.0, top), egui::pos2(200.0, top + 100.0)),
                )
            })
            .collect()
    }

    fn at(y: f32) -> egui::Pos2 {
        egui::pos2(100.0, y)
    }

    /// The column the cards are in, down to its foot.
    fn column() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(10.0, 60.0), egui::pos2(200.0, 600.0))
    }

    /// Something new dropped between the cards goes there — the top spot in
    /// front of them all — and anywhere else on the channel (its tile, its
    /// settings window, no pointer) behind its layers, before its rearmost
    /// deck: what the channel shows, on air perhaps, stays. An empty channel
    /// just takes it.
    #[test]
    fn a_new_deck_goes_between_the_cards_it_is_dropped_between_else_behind_them() {
        let ctx = egui::Context::default();
        // The cards are front first: deck0 in front, deck2 at the back.
        publish(&ctx, 0, cards(), column(), false, egui::LayerId::background());
        assert_eq!(
            place_at(&ctx, 0, Some(at(215.0)), false),
            Landing::Between(Some("deck0".to_string())),
            "between the first two cards"
        );
        assert_eq!(place_at(&ctx, 0, Some(at(70.0)), false), Landing::Between(None), "the top spot");
        let tile = egui::pos2(700.0, 300.0);
        assert_eq!(place_at(&ctx, 0, Some(tile), false), Landing::Back, "behind them");
        assert_eq!(place_at(&ctx, 0, Some(at(215.0)), true), Landing::Back, "its settings window");
        assert_eq!(place_at(&ctx, 0, None, false), Landing::Back, "no pointer");
        assert_eq!(place_at(&ctx, 1, Some(tile), false), Landing::Back, "an empty channel");
        assert!(deck_place("ch".into(), Landing::Back).back);
        assert!(!deck_place("ch".into(), Landing::Between(None)).back, "the top spot is in front");
    }

    /// Front first, as in Premiere: above a card is in front of it, so it
    /// goes before the card above the spot in the stack — the end, in front
    /// of them all, above the first.
    #[test]
    fn the_upper_half_of_a_card_goes_in_front_of_it() {
        let c = cards();
        assert_eq!(
            insertion_in(&c, column(), at(120.0)).before,
            None,
            "in front of all"
        );
        assert_eq!(
            insertion_in(&c, column(), at(170.0)).before.as_deref(),
            Some("deck0")
        );
        assert_eq!(
            insertion_in(&c, column(), at(215.0)).before.as_deref(),
            Some("deck0")
        );
        assert_eq!(
            insertion_in(&c, column(), at(330.0)).before.as_deref(),
            Some("deck1")
        );
    }

    #[test]
    fn below_the_middle_of_the_last_card_goes_to_the_back() {
        let c = cards();
        let ins = insertion_in(&c, column(), at(400.0));
        assert_eq!(ins.before.as_deref(), Some("deck2"), "behind the last");
        let [from, _] = ins.line.unwrap();
        assert!(from.y > c[2].1.bottom(), "the line sits under the last card");
    }

    /// Dropped on the channel's tile, outside its column of cards — beside
    /// it, or under it in the same panel stack: no spot between them (see
    /// `place_at`: behind its layers).
    #[test]
    fn away_from_the_cards_there_is_no_spot_between_them() {
        for spot in [egui::pos2(600.0, 400.0), egui::pos2(100.0, 700.0)] {
            let ins = insertion_in(&cards(), column(), spot);
            assert_eq!(ins.before, None, "{spot:?}");
            assert_eq!(ins.line, None, "{spot:?}");
        }
    }

    #[test]
    fn the_line_sits_in_the_gap_between_cards() {
        let c = cards();
        let [from, to] = insertion_in(&c, column(), at(170.0)).line.unwrap();
        assert!((from.y - 205.0).abs() < 1e-3, "halfway between 200 and 210: {from:?}");
        assert_eq!((from.x, to.x), (c[0].1.left(), c[0].1.right()));
        assert_eq!(from.y, to.y);
    }

    /// A VJ layer's cards run across, front first from the left: left of a
    /// card's middle is in front of it; right of the last, behind them all;
    /// the marker stands upright in the gap.
    #[test]
    fn cards_across_a_row_go_by_left_and_right() {
        let row: Cards = (0..3)
            .map(|i| {
                let left = 200.0 + i as f32 * 150.0;
                (
                    format!("deck{i}"),
                    egui::Rect::from_min_max(egui::pos2(left, 20.0), egui::pos2(left + 140.0, 120.0)),
                )
            })
            .collect();
        let area = egui::Rect::from_min_max(egui::pos2(190.0, 10.0), egui::pos2(900.0, 130.0));
        let at = |x: f32| egui::pos2(x, 70.0);
        assert_eq!(insertion_across(&row, area, at(210.0)).before, None, "in front of all");
        let between = insertion_across(&row, area, at(330.0));
        assert_eq!(between.before.as_deref(), Some("deck0"));
        let [from, to] = between.line.unwrap();
        assert!((from.x - 345.0).abs() < 1e-3, "halfway between 340 and 350: {from:?}");
        assert_eq!(from.x, to.x);
        assert_eq!(insertion_across(&row, area, at(800.0)).before.as_deref(), Some("deck2"));
        assert_eq!(insertion_across(&row, area, egui::pos2(100.0, 70.0)).line, None, "outside");
    }

    /// Two views of one channel (Channels and Layers side by side): a drop
    /// goes between the cards of the view it is in.
    #[test]
    fn of_two_views_the_one_the_pointer_is_in_decides() {
        let ctx = egui::Context::default();
        publish(&ctx, 0, cards(), column(), false, egui::LayerId::background());
        let row: Cards = vec![
            ("deck0".into(), egui::Rect::from_min_max(egui::pos2(400.0, 20.0), egui::pos2(500.0, 80.0))),
            ("deck1".into(), egui::Rect::from_min_max(egui::pos2(510.0, 20.0), egui::pos2(610.0, 80.0))),
        ];
        let strip = egui::Rect::from_min_max(egui::pos2(390.0, 10.0), egui::pos2(900.0, 90.0));
        publish(&ctx, 0, row, strip, true, egui::LayerId::background());
        assert_eq!(insertion_at(&ctx, 0, egui::pos2(540.0, 50.0)).before.as_deref(), Some("deck0"));
        assert_eq!(insertion_at(&ctx, 0, egui::pos2(100.0, 215.0)).before.as_deref(), Some("deck0"));
        assert_eq!(insertion_at(&ctx, 0, egui::pos2(100.0, 400.0)).before.as_deref(), Some("deck2"));
    }

    #[test]
    fn an_empty_channel_has_no_line() {
        let ins = insertion_in(&[], column(), at(100.0));
        assert_eq!(ins.before, None);
        assert_eq!(ins.line, None);
    }
}
