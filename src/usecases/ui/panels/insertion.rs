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

fn cards_id(ch_idx: usize) -> egui::Id {
    egui::Id::new("deck_card_rects").with(ch_idx)
}

fn cards_pass_id(ch_idx: usize) -> egui::Id {
    egui::Id::new("deck_card_rects_pass").with(ch_idx)
}

fn cards_area_id(ch_idx: usize) -> egui::Id {
    egui::Id::new("deck_card_rects_area").with(ch_idx)
}

/// Record where a channel's cards were drawn this frame, and the column
/// they are in: only a drop in it goes between them.
pub(super) fn publish_cards(ctx: &egui::Context, ch_idx: usize, cards: Cards, area: egui::Rect) {
    let pass = ctx.cumulative_pass_nr();
    ctx.memory_mut(|m| {
        m.data.insert_temp(cards_id(ch_idx), cards);
        m.data.insert_temp(cards_area_id(ch_idx), area);
        m.data.insert_temp(cards_pass_id(ch_idx), pass);
    });
}

/// Where a drop at height `y` in channel `ch_idx` would land.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Insertion {
    /// The deck it goes before in the stack, which is drawn from the back:
    /// the card above the spot. `None` is the end, in front of them all.
    pub before: Option<String>,
    /// Where to draw the marker: across these x, at this y. `None` for an
    /// empty channel, where the whole column lights up instead.
    pub line: Option<(egui::Rangef, f32)>,
}

pub(super) fn insertion_at(ctx: &egui::Context, ch_idx: usize, at: egui::Pos2) -> Insertion {
    // Cards drawn a while ago are of a column no longer shown.
    let now = ctx.cumulative_pass_nr();
    let published: Option<(Cards, egui::Rect)> = ctx.memory(|m| {
        let pass: u64 = m.data.get_temp(cards_pass_id(ch_idx))?;
        if pass + 2 < now {
            return None;
        }
        Some((
            m.data.get_temp(cards_id(ch_idx))?,
            m.data.get_temp(cards_area_id(ch_idx))?,
        ))
    });
    match published {
        Some((cards, area)) => insertion_in(&cards, area, at),
        None => insertion_in(&[], egui::Rect::NOTHING, at),
    }
}

/// [`insertion_at`] on a given set of cards in the column `area`. Outside
/// it — over the channel's tile, or with its column not shown — it goes in
/// front.
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
    let y = at.y;
    match cards.iter().position(|(_, r)| y < r.center().y) {
        Some(k) => {
            let top = cards[k].1.top();
            let line_y = if k == 0 {
                top - 2.0
            } else {
                f32::midpoint(cards[k - 1].1.bottom(), top)
            };
            Insertion {
                before: k.checked_sub(1).map(|above| cards[above].0.clone()),
                line: Some((x, line_y.clamp(area.top(), area.bottom()))),
            }
        }
        None => Insertion {
            before: cards.last().map(|(uuid, _)| uuid.clone()),
            line: cards
                .last()
                .map(|(_, r)| (x, (r.bottom() + 3.0).clamp(area.top(), area.bottom()))),
        },
    }
}

/// Draw the marker for `insertion`, above everything else.
pub(super) fn paint(ctx: &egui::Context, insertion: &Insertion, color: egui::Color32) {
    let Some((x, y)) = insertion.line else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("deck_insertion_marker"),
    ));
    painter.line_segment(
        [egui::pos2(x.min, y), egui::pos2(x.max, y)],
        egui::Stroke::new(3.0, color),
    );
    painter.circle_filled(egui::pos2(x.min + 3.0, y), 4.0, color);
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
        let (_, y) = ins.line.unwrap();
        assert!(y > c[2].1.bottom(), "the line sits under the last card");
    }

    /// Dropped on the channel's tile, outside its column of cards — beside
    /// it, or under it in the same panel stack: in front, where it is seen.
    #[test]
    fn away_from_the_cards_it_goes_in_front() {
        for spot in [egui::pos2(600.0, 400.0), egui::pos2(100.0, 700.0)] {
            let ins = insertion_in(&cards(), column(), spot);
            assert_eq!(ins.before, None, "{spot:?}");
            assert_eq!(ins.line, None, "{spot:?}");
        }
    }

    #[test]
    fn the_line_sits_in_the_gap_between_cards() {
        let c = cards();
        let (x, y) = insertion_in(&c, column(), at(170.0)).line.unwrap();
        assert!((y - 205.0).abs() < 1e-3, "halfway between 200 and 210: {y}");
        assert_eq!(x, c[0].1.x_range());
    }

    #[test]
    fn an_empty_channel_has_no_line() {
        let ins = insertion_in(&[], column(), at(100.0));
        assert_eq!(ins.before, None);
        assert_eq!(ins.line, None);
    }
}
