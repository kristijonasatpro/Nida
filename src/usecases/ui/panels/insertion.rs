//! Where something dropped into a channel's deck stack goes: before the card
//! the pointer is in the upper half of, or after the last one — shown while
//! dragging as a line between the cards.
//!
//! The spot is named by the deck it goes before (a UUID), never by position:
//! a file that takes a moment to load, or a list that changes mid-drag, must
//! not land next to the wrong deck.

/// Every deck card's rect in one channel, drawn this frame, in stack order.
pub(super) type Cards = Vec<(String, egui::Rect)>;

fn cards_id(ch_idx: usize) -> egui::Id {
    egui::Id::new("deck_card_rects").with(ch_idx)
}

/// Record where a channel's cards were drawn this frame.
pub(super) fn publish_cards(ctx: &egui::Context, ch_idx: usize, cards: Cards) {
    ctx.memory_mut(|m| m.data.insert_temp(cards_id(ch_idx), cards));
}

/// Where a drop at height `y` in channel `ch_idx` would land.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Insertion {
    /// The deck it goes before; `None` is after the last one.
    pub before: Option<String>,
    /// Where to draw the marker: across these x, at this y. `None` for an
    /// empty channel, where the whole column lights up instead.
    pub line: Option<(egui::Rangef, f32)>,
}

pub(super) fn insertion_at(ctx: &egui::Context, ch_idx: usize, y: f32) -> Insertion {
    let cards: Cards = ctx
        .memory(|m| m.data.get_temp(cards_id(ch_idx)))
        .unwrap_or_default();
    insertion_in(&cards, y)
}

/// [`insertion_at`] on a given set of cards.
pub(super) fn insertion_in(cards: &[(String, egui::Rect)], y: f32) -> Insertion {
    let Some(first) = cards.first() else {
        return Insertion {
            before: None,
            line: None,
        };
    };
    let x = first.1.x_range();
    match cards.iter().position(|(_, r)| y < r.center().y) {
        Some(k) => {
            let top = cards[k].1.top();
            let line_y = if k == 0 {
                top - 2.0
            } else {
                f32::midpoint(cards[k - 1].1.bottom(), top)
            };
            Insertion {
                before: Some(cards[k].0.clone()),
                line: Some((x, line_y)),
            }
        }
        None => Insertion {
            before: None,
            line: cards.last().map(|(_, r)| (x, r.bottom() + 3.0)),
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

    #[test]
    fn the_upper_half_of_a_card_goes_before_it() {
        let c = cards();
        assert_eq!(insertion_in(&c, 120.0).before.as_deref(), Some("deck0"));
        assert_eq!(insertion_in(&c, 170.0).before.as_deref(), Some("deck1"));
        assert_eq!(insertion_in(&c, 215.0).before.as_deref(), Some("deck1"));
        assert_eq!(insertion_in(&c, 330.0).before.as_deref(), Some("deck2"));
    }

    #[test]
    fn below_the_middle_of_the_last_card_goes_to_the_end() {
        let c = cards();
        let ins = insertion_in(&c, 400.0);
        assert_eq!(ins.before, None);
        let (_, y) = ins.line.unwrap();
        assert!(y > c[2].1.bottom(), "the line sits under the last card");
    }

    #[test]
    fn the_line_sits_in_the_gap_between_cards() {
        let c = cards();
        let (x, y) = insertion_in(&c, 170.0).line.unwrap();
        assert!((y - 205.0).abs() < 1e-3, "halfway between 200 and 210: {y}");
        assert_eq!(x, c[0].1.x_range());
    }

    #[test]
    fn an_empty_channel_has_no_line() {
        let ins = insertion_in(&[], 100.0);
        assert_eq!(ins.before, None);
        assert_eq!(ins.line, None);
    }
}
