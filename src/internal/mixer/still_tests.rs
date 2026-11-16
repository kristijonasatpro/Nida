//! A lower third or a text standing still — a name on air, a text shown,
//! nothing of it moving — keeps the frame its deck has, rather than being
//! drawn again every frame (`Mixer::keep_still_text`, `Deck::still_frame`).
//! Whatever changes what it shows — coming in, going out, a fade, another
//! drawing, another place, its frame made anew, an effect that draws — has
//! it drawn; and Main is the same with the switch off.

use std::time::{Duration, Instant};

use super::blank_tests::{
    cut_lower_third, deck_picture, drawn, headless_gpu, hide_lower, invert, letters, lower,
    main_picture, mixer_with, over, over_mut, plate, render, settings, show_lower, text,
    text_settings, H, OVER, UNDER, W,
};
use super::Mixer;
use crate::deck::Deck;
use crate::engine::value::lower_third::{LowerThirdMotion, LowerThirdPlace};
use crate::engine::value::timer::Anchor;
use crate::renderer::GpuContext;

/// How many of the next `frames` frames draw the deck looked at — the
/// overlay's first — itself.
fn deck_drawn(gpu: &GpuContext, mixer: &mut Mixer, frames: u32) -> u32 {
    let before = over(mixer).decks[0].deck.rendered_frames();
    for _ in 0..frames {
        render(gpu, mixer);
    }
    over(mixer).decks[0].deck.rendered_frames() - before
}

/// A name on air, standing still: shown — a cut — and a frame drawn of it
/// on.
fn on_air(gpu: &GpuContext) -> Mixer {
    let mut mixer = mixer_with(gpu, cut_lower_third(gpu));
    render(gpu, &mut mixer);
    show_lower(gpu, &mut mixer);
    // Past the cut's millisecond: on, and drawn so.
    std::thread::sleep(Duration::from_millis(5));
    render(gpu, &mut mixer);
    mixer
}

/// Frames drawn till one leaves the lower third's frame as it is with a
/// name on air: it stands still.
fn till_it_stands(gpu: &GpuContext, mixer: &mut Mixer) {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        let drew = deck_drawn(gpu, mixer, 1);
        if drew == 0 && !lower(mixer).is_off() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the lower third never stood still");
}

/// A text shown, its drawing in, and a frame drawn of it.
fn text_shown(gpu: &GpuContext) -> Mixer {
    let mut shown = text_settings();
    shown.visible = true;
    let deck = Deck::new_text(gpu, shown, W, H).expect("a text");
    let mut mixer = mixer_with(gpu, deck);
    assert!(text(&mut mixer).wait_drawn(Duration::from_secs(20)), "drawn");
    render(gpu, &mut mixer);
    mixer
}

/// How much of a green plate (0.2, 0.8, 0.2) shows.
fn green_plate(frame: &[[f32; 4]]) -> usize {
    frame
        .iter()
        .filter(|px| px[1] > 0.4 && px[0] < 0.1 && px[2] < 0.1)
        .count()
}

/// Main with the switch on, then the next frame with it off: the same.
fn same_either_way(gpu: &GpuContext, mixer: &mut Mixer, what: &str) {
    render(gpu, mixer);
    let kept = main_picture(gpu, mixer);
    mixer.keep_still_text = false;
    render(gpu, mixer);
    let every_frame = main_picture(gpu, mixer);
    mixer.keep_still_text = true;
    let worst = kept
        .iter()
        .zip(&every_frame)
        .flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs()))
        .fold(0.0, f32::max);
    assert!(worst < 1e-3, "{what}: Main differs by {worst}");
}

/// A name on air: its input is drawn and layered on Main every frame, the
/// plate there — and its own frame, drawn once with it on, is kept.
#[test]
fn a_name_on_air_is_drawn_once_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "on air: kept");
    assert_eq!(drawn(&gpu, &mut mixer, 3), 3, "its input drawn every frame");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER, OVER], "layered");
    assert!(plate(&deck_picture(&gpu, &mixer)) > 5_000, "the plate in its frame");
    assert!(plate(&main_picture(&gpu, &mixer)) > 5_000, "the plate on Main");
}

/// The switch off, a name on air is drawn every frame, as before; on
/// again, once more and then kept.
#[test]
fn with_the_switch_off_a_name_on_air_is_drawn_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "on: kept");
    mixer.keep_still_text = false;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "off: drawn every frame");
    mixer.keep_still_text = true;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "on: once more, then kept");
}

/// Coming in (a fade of three seconds, the longest there is: five frames
/// are well within it) it is drawn every frame; in, it is kept; going out,
/// drawn every frame again.
#[test]
fn coming_in_and_going_out_it_is_drawn_every_frame() {
    let gpu = headless_gpu();
    let slow = settings(LowerThirdMotion::Fade, 3.0);
    let deck = Deck::new_lower_third(&gpu, slow.clone(), W, H).expect("a lower third");
    let mut mixer = mixer_with(&gpu, deck);
    render(&gpu, &mut mixer);
    lower(&mut mixer).show(None);
    // Till its drawing is in, and it comes in.
    let until = Instant::now() + Duration::from_secs(20);
    while lower(&mut mixer).is_off() {
        assert!(Instant::now() < until, "the lower third never came in");
        render(&gpu, &mut mixer);
        std::thread::sleep(Duration::from_millis(5));
    }
    let began = Instant::now();
    let drew = deck_drawn(&gpu, &mut mixer, 5);
    let took = began.elapsed();
    assert_eq!(drew, 5, "coming in: drawn every frame (they took {took:?} of its 3 s)");
    // Not waited for: its fade made a short one, it is in at once.
    let mut quick = slow.clone();
    quick.look.motion_secs = 0.05;
    lower(&mut mixer).set_settings(quick);
    let drawn = lower(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(drawn, "drawn with the short fade");
    till_it_stands(&gpu, &mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "in: kept");
    assert!(plate(&main_picture(&gpu, &mixer)) > 5_000, "the plate on Main");
    // The long fade again — drawn for it at most once, then kept — and out.
    lower(&mut mixer).set_settings(slow);
    let drawn = lower(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(drawn, "drawn with the long fade");
    till_it_stands(&gpu, &mut mixer);
    lower(&mut mixer).hide();
    let began = Instant::now();
    let drew = deck_drawn(&gpu, &mut mixer, 5);
    let took = began.elapsed();
    assert_eq!(drew, 5, "going out: drawn every frame (they took {took:?} of its 3 s)");
}

/// Its look changed with the name on air — the plate green for red — its
/// new drawing is drawn once, nothing of the old one left, and kept.
#[test]
fn another_look_is_drawn_once_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 2), 0, "on air: kept");
    let mut green = lower(&mut mixer).settings().clone();
    green.look.plate = [0.2, 0.8, 0.2];
    lower(&mut mixer).set_settings(green);
    let redrawn = lower(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(redrawn, "drawn in the new look");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "another drawing: drawn");
    let picture = deck_picture(&gpu, &mixer);
    assert_eq!(plate(&picture), 0, "none of the red plate left");
    assert!(green_plate(&picture) > 5_000, "the green plate in its frame");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "then kept");
}

/// Placed otherwise with the name on air, it is drawn at once — before
/// the drawing for the new place is in — then once more at most, and kept.
#[test]
fn placed_otherwise_it_is_drawn_at_once() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 2), 0, "on air: kept");
    let mut right = lower(&mut mixer).settings().clone();
    right.look.place = LowerThirdPlace::LowerRight;
    lower(&mut mixer).set_settings(right);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "placed otherwise: drawn");
    let redrawn = lower(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(redrawn, "drawn for the new place");
    assert!(deck_drawn(&gpu, &mut mixer, 2) <= 1, "its new drawing: once");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "then kept");
}

/// Another name shown over the one on air: out, in, and the new name's
/// frame — not the old one's — is the one kept.
#[test]
fn another_name_is_drawn_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    let first = deck_picture(&gpu, &mixer);
    lower(&mut mixer).show(Some(1));
    till_it_stands(&gpu, &mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "the other name on air: kept");
    let second = deck_picture(&gpu, &mixer);
    assert!(plate(&second) > 5_000, "its plate in its frame");
    assert!(second != first, "the other name's frame, not the first's");
}

/// Off, beside a card that shows — so its input is drawn every frame —
/// its frame is drawn clear once and kept.
#[test]
fn off_beside_a_card_it_is_drawn_clear_once_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    let green = Deck::new_solid_color(&gpu, [0.0, 1.0, 0.0, 1.0], W, H).expect("a card");
    let index = over_mut(&mut mixer).add_deck(green);
    // Seen through: it covers nothing.
    over_mut(&mut mixer).decks[index].opacity = 0.5;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "drawn clear once");
    assert_eq!(drawn(&gpu, &mut mixer, 3), 3, "its input drawn every frame");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "its own frame kept");
    assert_eq!(plate(&deck_picture(&gpu, &mixer)), 0, "and clear");
}

/// The show's size changed with the name on air: its frame, made anew, is
/// drawn — the drawing before, scaled — then its drawing for the new size
/// once, and kept.
#[test]
fn resized_it_is_drawn_anew_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 2), 0, "on air: kept");
    mixer.resize(&gpu, W / 2, H / 2);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "its frame made anew: drawn");
    let redrawn = lower(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(redrawn, "drawn for the new size");
    assert!(deck_drawn(&gpu, &mut mixer, 2) <= 1, "its new drawing: once");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "then kept");
    assert!(plate(&deck_picture(&gpu, &mixer)) > 500, "the plate in its frame");
}

/// An effect that draws — invert — on the lower third: its frame may be
/// another each frame, and is drawn every frame.
#[test]
fn an_effect_that_draws_keeps_a_lower_third_drawn() {
    let gpu = headless_gpu();
    let mut deck = cut_lower_third(&gpu);
    deck.add_effect(invert(&gpu));
    let mut mixer = mixer_with(&gpu, deck);
    render(&gpu, &mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4);
}

/// Kept or drawn every frame, Main is the same: a name on air, hidden
/// again, and a text shown.
#[test]
fn main_is_the_same_kept_or_drawn_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = on_air(&gpu);
    same_either_way(&gpu, &mut mixer, "a name on air");
    hide_lower(&mut mixer);
    render(&gpu, &mut mixer);
    same_either_way(&gpu, &mut mixer, "hidden again");
    let mut mixer = text_shown(&gpu);
    same_either_way(&gpu, &mut mixer, "a text shown");
}

/// A text shown: its letters on Main, its own frame drawn once and kept.
/// The switch off, it is drawn every frame; on again, once more and kept.
#[test]
fn a_text_shown_is_drawn_once_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = text_shown(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "shown: kept");
    assert_eq!(drawn(&gpu, &mut mixer, 3), 3, "its input drawn every frame");
    let shown = letters(&main_picture(&gpu, &mixer));
    assert!(shown > 500, "its letters on Main: {shown}");
    mixer.keep_still_text = false;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "off: drawn every frame");
    mixer.keep_still_text = true;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "on: once more, then kept");
}

/// Its colour changed — a drawing the size of the one before, written
/// where that was — it is drawn once, none of the white letters left, and
/// kept.
#[test]
fn a_text_drawn_again_is_drawn_once_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = text_shown(&gpu);
    let white = letters(&deck_picture(&gpu, &mixer));
    assert!(white > 500, "its white letters in its frame: {white}");
    let mut red = text(&mut mixer).settings().clone();
    red.look.colour = [255, 0, 0];
    text(&mut mixer).set_settings(red);
    let redrawn = text(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(redrawn, "drawn in red");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "another drawing: drawn");
    let left = letters(&deck_picture(&gpu, &mixer));
    assert_eq!(left, 0, "none of the white letters left");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "then kept");
}

/// Put at another spot, it is drawn at once — before the drawing for the
/// new spot is in — then once more at most, and kept.
#[test]
fn a_text_placed_otherwise_is_drawn_at_once() {
    let gpu = headless_gpu();
    let mut mixer = text_shown(&gpu);
    let middle = deck_picture(&gpu, &mixer);
    let mut corner = text(&mut mixer).settings().clone();
    corner.look.anchor = Anchor::TopLeft;
    text(&mut mixer).set_settings(corner);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "placed otherwise: drawn");
    assert!(deck_picture(&gpu, &mixer) != middle, "its frame another");
    let redrawn = text(&mut mixer).wait_drawn(Duration::from_secs(20));
    assert!(redrawn, "drawn for the new spot");
    assert!(deck_drawn(&gpu, &mut mixer, 2) <= 1, "its new drawing: once");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "then kept");
}

/// Fading in (five seconds, the longest there is: five frames are well
/// within it) a text is drawn every frame; faded in, it is kept; fading
/// out, drawn every frame again.
#[test]
fn a_text_fading_is_drawn_every_frame() {
    let gpu = headless_gpu();
    let mut fading = text_settings();
    let deck = Deck::new_text(&gpu, fading.clone(), W, H).expect("a text");
    let mut mixer = mixer_with(&gpu, deck);
    assert!(text(&mut mixer).wait_drawn(Duration::from_secs(20)), "drawn");
    render(&gpu, &mut mixer);
    fading.visible = true;
    fading.fade_secs = 5.0;
    text(&mut mixer).set_settings(fading.clone());
    let began = Instant::now();
    let drew = deck_drawn(&gpu, &mut mixer, 5);
    let took = began.elapsed();
    assert_eq!(drew, 5, "fading in: drawn every frame (they took {took:?} of its 5 s)");
    // Not waited for: its fade made a short one, from where it is.
    fading.fade_secs = 0.05;
    text(&mut mixer).set_settings(fading.clone());
    let until = Instant::now() + Duration::from_secs(20);
    while deck_drawn(&gpu, &mut mixer, 1) > 0 {
        assert!(Instant::now() < until, "the text never stood still");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "faded in: kept");
    let shown = letters(&main_picture(&gpu, &mixer));
    assert!(shown > 500, "its letters on Main: {shown}");
    fading.visible = false;
    fading.fade_secs = 5.0;
    text(&mut mixer).set_settings(fading);
    let began = Instant::now();
    let drew = deck_drawn(&gpu, &mut mixer, 5);
    let took = began.elapsed();
    assert_eq!(drew, 5, "fading out: drawn every frame (they took {took:?} of its 5 s)");
}
