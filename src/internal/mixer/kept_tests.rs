//! What stands still keeps its picture. A clip between two of its frames —
//! paused, or 25 frames a second in a show of 60 — keeps the frame its deck
//! has rather than being drawn again every show frame
//! (`Mixer::keep_still_clips`, `Deck::still_frame`); and an input all of
//! whose layers are as they were last frame keeps the picture it has put
//! together rather than putting it together again
//! (`Mixer::keep_still_inputs`, `Channel::frames_kept`). Whatever changes
//! what shows — a level, a blend, another deck, another picture, another
//! size, an effect that draws, a new frame of a clip or of a live source,
//! another picture of an input it shows through a tap — has it done again;
//! and Main is the same with the switches off. A layer that draws a frame
//! of its own of such a source — a tap or a capture whose thumbnail is on
//! screen — keeps that frame the same way, by the number of the source's
//! picture, and its input with it (`Deck::still_frame`). A deck
//! that keeps its frame draws nothing: it is not timed, and costs nothing —
//! nor does one whose draw a GPU fault took back, nor is a time read back
//! later given to either.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::blank_tests::{
    drawn, headless_gpu, invert, main_picture, mixer_with, over, over_mut, render, H, OVER, UNDER,
    W,
};
use super::{GpuTimingFrame, Mixer};
use crate::channel::{BlendMode, DeckAutoTransition, DeckTransitionPhase, DurationSpec};
use crate::deck::{Deck, Effect, ScalingMode, ScreenCaptureState, TapSource};
use crate::renderer::GpuContext;
use crate::screen_capture::backend::{CaptureConfig, TargetIdentity};

const RED: [u8; 3] = [255, 0, 0];
const GREEN: [u8; 3] = [0, 255, 0];

/// A picture filling the frame, all of one colour.
fn plain(rgb: [u8; 3]) -> image::RgbaImage {
    image::RgbaImage::from_pixel(W, H, image::Rgba([rgb[0], rgb[1], rgb[2], 255]))
}

/// A picture deck of it.
fn picture(gpu: &GpuContext, rgb: [u8; 3]) -> Deck {
    Deck::new_from_pixels(gpu, &plain(rgb), "a picture".to_string(), "", W, H).expect("a picture")
}

/// How many of the next `frames` frames leave the overlay's picture as it
/// was put together before.
fn kept(gpu: &GpuContext, mixer: &mut Mixer, frames: u32) -> u32 {
    let before = over(mixer).frames_kept;
    for _ in 0..frames {
        render(gpu, mixer);
    }
    over(mixer).frames_kept - before
}

/// How many of the next `frames` frames draw the deck looked at — the
/// overlay's first — itself.
fn deck_drawn(gpu: &GpuContext, mixer: &mut Mixer, frames: u32) -> u32 {
    let before = over(mixer).decks[0].deck.rendered_frames();
    for _ in 0..frames {
        render(gpu, mixer);
    }
    over(mixer).decks[0].deck.rendered_frames() - before
}

/// How much of a frame is red.
fn red(frame: &[[f32; 4]]) -> usize {
    frame
        .iter()
        .filter(|px| px[0] > 0.5 && px[1] < 0.1 && px[2] < 0.1)
        .count()
}

/// How much of a frame is green.
fn green(frame: &[[f32; 4]]) -> usize {
    frame
        .iter()
        .filter(|px| px[1] > 0.5 && px[0] < 0.1 && px[2] < 0.1)
        .count()
}

/// How much of a frame is not the blue card under the overlay.
fn not_the_card(frame: &[[f32; 4]]) -> usize {
    frame
        .iter()
        .filter(|px| !(px[0] < 0.05 && px[1] < 0.05 && px[2] > 0.9))
        .count()
}

/// How much of a frame is coloured — neither black, grey nor white — and
/// not the card: a test clip's bars.
fn coloured(frame: &[[f32; 4]]) -> usize {
    frame
        .iter()
        .filter(|px| !(px[0] < 0.05 && px[1] < 0.05 && px[2] > 0.9))
        .filter(|px| px[0].max(px[1]).max(px[2]) - px[0].min(px[1]).min(px[2]) > 0.2)
        .count()
}

/// The most two frames differ by in any channel.
fn differ(one: &[[f32; 4]], other: &[[f32; 4]]) -> f32 {
    assert_eq!(one.len(), other.len(), "of one size");
    one.iter()
        .zip(other)
        .flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs()))
        .fold(0.0, f32::max)
}

/// Main with the input's picture kept, then the next frame with it put
/// together anew: the same. The switch is left on.
fn same_put_together(gpu: &GpuContext, mixer: &mut Mixer, what: &str) {
    render(gpu, mixer);
    let as_kept = main_picture(gpu, mixer);
    mixer.keep_still_inputs = false;
    render(gpu, mixer);
    let anew = main_picture(gpu, mixer);
    mixer.keep_still_inputs = true;
    let worst = differ(&as_kept, &anew);
    assert!(worst < 1e-3, "{what}: Main differs by {worst}");
}

/// What was just changed is put together once — the next frame — and kept
/// after; Main as with the switch off, and kept again with it back on.
fn once_then_kept(gpu: &GpuContext, mixer: &mut Mixer, what: &str) {
    assert_eq!(kept(gpu, mixer, 1), 0, "{what}: put together anew");
    assert_eq!(kept(gpu, mixer, 3), 3, "{what}: and kept after");
    same_put_together(gpu, mixer, what);
    assert_eq!(kept(gpu, mixer, 1), 0, "{what}: the switch back on, anew");
    assert_eq!(kept(gpu, mixer, 2), 2, "{what}: and kept again");
}

/// A red picture alone in the overlay, a frame drawn of it.
fn a_picture_shown(gpu: &GpuContext) -> Mixer {
    let mut mixer = mixer_with(gpu, picture(gpu, RED));
    assert_eq!(kept(gpu, &mut mixer, 1), 0, "the first frame is put together");
    mixer
}

/// A picture standing still: its input is put together once and its
/// picture kept after — counted as drawn all the same, the picture on
/// Main; with the switch off it is put together every frame, to the same
/// Main.
#[test]
fn a_picture_standing_still_is_put_together_once_and_kept() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 5), 5, "kept from then on");
    assert_eq!(drawn(&gpu, &mut mixer, 3), 3, "its frames counted all the same");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "the picture's own frame too");
    let as_kept = main_picture(&gpu, &mixer);
    assert!(red(&as_kept) > 900_000, "the picture on Main: {}", red(&as_kept));

    mixer.keep_still_inputs = false;
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "switched off: every frame anew");
    let anew = main_picture(&gpu, &mixer);
    let worst = differ(&as_kept, &anew);
    assert!(worst < 1e-3, "Main differs by {worst}");

    mixer.keep_still_inputs = true;
    assert_eq!(kept(&gpu, &mut mixer, 1), 0, "switched on: once more");
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept");
}

/// A layer's level, its blend, another layer over it: each is put together
/// the frame it changes, and kept after.
#[test]
fn a_layer_changed_is_put_together_again() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 2), 2, "standing still");

    over_mut(&mut mixer).decks[0].opacity = 0.5;
    once_then_kept(&gpu, &mut mixer, "half its level");
    let half = main_picture(&gpu, &mixer);
    assert!(red(&half) < 1000, "the card shows through: {}", red(&half));

    over_mut(&mut mixer).decks[0].blend_mode = BlendMode::Add;
    once_then_kept(&gpu, &mut mixer, "added, not laid over");

    over_mut(&mut mixer).decks[0].opacity = 1.0;
    over_mut(&mut mixer).decks[0].blend_mode = BlendMode::Normal;
    once_then_kept(&gpu, &mut mixer, "as it was");
    let whole = main_picture(&gpu, &mixer);
    assert!(red(&whole) > 900_000, "the picture again: {}", red(&whole));

    let other = picture(&gpu, GREEN);
    over_mut(&mut mixer).add_deck(other);
    once_then_kept(&gpu, &mut mixer, "another picture over it");
    let covered = main_picture(&gpu, &mixer);
    assert!(green(&covered) > 900_000, "the one over it: {}", green(&covered));

    let _ = over_mut(&mut mixer).remove_deck(1);
    once_then_kept(&gpu, &mut mixer, "taken away again");
    let bare = main_picture(&gpu, &mixer);
    assert!(red(&bare) > 900_000, "the first again: {}", red(&bare));
}

/// Another picture in the deck, the show another size, the layers drawn
/// one a pass: each is put together the frame it changes, and kept after.
#[test]
fn a_picture_or_a_size_changed_is_put_together_again() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 2), 2, "standing still");

    over_mut(&mut mixer).decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    once_then_kept(&gpu, &mut mixer, "another picture");
    let other = main_picture(&gpu, &mixer);
    assert!(green(&other) > 900_000, "the new one: {}", green(&other));

    mixer.merge_layer_passes = false;
    once_then_kept(&gpu, &mut mixer, "a pass a layer");
    mixer.merge_layer_passes = true;
    once_then_kept(&gpu, &mut mixer, "one pass again");

    mixer.resize(&gpu, W / 2, H / 2);
    once_then_kept(&gpu, &mut mixer, "half the size");
    let small = main_picture(&gpu, &mixer);
    assert_eq!(small.len(), (W / 2 * (H / 2)) as usize, "Main at the new size");
    assert!(green(&small) > 200_000, "the picture there: {}", green(&small));
}

/// An effect on the input draws every frame: nothing is kept while it is
/// on, and its picture is kept again once it is off.
#[test]
fn an_input_with_an_effect_on_is_put_together_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 2), 2, "standing still");

    over_mut(&mut mixer).add_effect(invert(&gpu));
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "an effect on: every frame");
    let inverted = main_picture(&gpu, &mixer);
    assert!(red(&inverted) < 1000, "inverted: {}", red(&inverted));

    over_mut(&mut mixer).effects[0].enabled = false;
    once_then_kept(&gpu, &mut mixer, "the effect off");
    let plain = main_picture(&gpu, &mixer);
    assert!(red(&plain) > 900_000, "the picture as it is: {}", red(&plain));
}

/// A deck made anew in another's place — the same UUID, the same count of
/// frames drawn, textures as new — is another deck: its picture is put
/// together, not the old one's kept.
#[test]
fn a_deck_made_anew_in_its_place_is_put_together() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "standing still");

    let old = over_mut(&mut mixer).remove_deck(0).expect("the picture");
    assert_eq!(old.rendered_frames(), 1, "drawn once");
    let mut anew = picture(&gpu, GREEN);
    anew.set_uuid(old.uuid().to_string());
    assert_eq!(anew.texture_generation(), old.texture_generation());
    over_mut(&mut mixer).add_deck(anew);

    render(&gpu, &mut mixer);
    let deck = &over(&mixer).decks[0].deck;
    assert_eq!(deck.rendered_frames(), 1, "drawn once, as the old one was");
    let main = main_picture(&gpu, &mixer);
    assert!(green(&main) > 900_000, "the new deck's picture: {}", green(&main));
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept after");
}

/// An input shown in itself — a tap of it among its own layers — has two
/// pictures, this frame's and the one before, turn about: neither is kept,
/// it is put together every frame, and what changes shows the frame it
/// changes.
#[test]
fn an_input_that_shows_itself_is_put_together_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 2), 2, "standing still");

    let over_uuid = over(&mixer).uuid().to_string();
    let tap = Deck::new_from_tap(&gpu, TapSource::Channel(over_uuid), "itself", W, H)
        .expect("a tap");
    let index = over_mut(&mut mixer).add_deck(tap);
    over_mut(&mut mixer).decks[index].mute = true;
    for frame in 0..4 {
        assert_eq!(kept(&gpu, &mut mixer, 1), 0, "frame {frame}: put together");
        let main = main_picture(&gpu, &mixer);
        assert!(red(&main) > 900_000, "frame {frame}: the picture: {}", red(&main));
    }
    assert!(
        over(&mixer).decks[index].deck.tapped_picture.is_none(),
        "the frame before: which picture it is, is not told"
    );

    over_mut(&mut mixer).decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    for frame in 0..4 {
        assert_eq!(kept(&gpu, &mut mixer, 1), 0, "frame {frame}: put together");
        let main = main_picture(&gpu, &mixer);
        assert!(green(&main) > 900_000, "frame {frame}: the new one: {}", green(&main));
    }
}

/// A red picture alone in a third input, which Main shows only through
/// the overlay: a tap of it, alone there, in place of the picture. The
/// first frame drawn; where the third input is.
fn an_input_shown_in_the_overlay(gpu: &GpuContext) -> (Mixer, usize) {
    let mut mixer = mixer_with(gpu, picture(gpu, RED));
    let shown = mixer.add_channel(gpu, W, H).expect("a third input");
    let its_picture = over_mut(&mut mixer).remove_deck(0).expect("the picture");
    let uuid = {
        let channel = mixer.channel_mut(shown).expect("the third input");
        channel.add_deck(its_picture);
        channel.uuid().to_string()
    };
    let tap = Deck::new_from_tap(gpu, TapSource::Channel(uuid), "the third input", W, H)
        .expect("a tap");
    over_mut(&mut mixer).add_deck(tap);
    assert_eq!(kept(gpu, &mut mixer, 1), 0, "the first frame is put together");
    (mixer, shown)
}

/// An input that shows another through a tap, drawn straight from the
/// other's picture, keeps what it has put together while the other stands
/// still — and is put together the very frame the other is: what the other
/// shows is on Main that frame, never one behind.
#[test]
fn an_input_showing_another_is_kept_while_that_one_stands_still() {
    let gpu = headless_gpu();
    let (mut mixer, shown) = an_input_shown_in_the_overlay(&gpu);
    assert_eq!(over(&mixer).drawn_onto, 1, "drawn straight from the other's picture");
    assert!(
        over(&mixer).decks[0].deck.tapped_picture.is_some(),
        "told which picture it shows"
    );
    assert_eq!(kept(&gpu, &mut mixer, 5), 5, "the other standing still: kept");
    let first = main_picture(&gpu, &mixer);
    assert!(red(&first) > 900_000, "the other's picture on Main: {}", red(&first));
    same_put_together(&gpu, &mut mixer, "another input shown");
    assert_eq!(kept(&gpu, &mut mixer, 3), 2, "the switch back on");

    mixer.channel_mut(shown).expect("the third input").decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    assert_eq!(kept(&gpu, &mut mixer, 1), 0, "the other changed: put together that frame");
    let second = main_picture(&gpu, &mixer);
    assert!(green(&second) > 900_000, "its new picture at once: {}", green(&second));
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept after");
    let later = main_picture(&gpu, &mixer);
    assert!(green(&later) > 900_000, "the new picture still: {}", green(&later));
}

/// The other input moving — an effect on it draws every frame — the input
/// that shows it is put together every frame too, and kept again once the
/// other stands still.
#[test]
fn an_input_showing_one_that_moves_is_put_together_every_frame() {
    let gpu = headless_gpu();
    let (mut mixer, shown) = an_input_shown_in_the_overlay(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 2), 2, "standing still");

    mixer
        .channel_mut(shown)
        .expect("the third input")
        .add_effect(invert(&gpu));
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "the other every frame: this one too");
    let inverted = main_picture(&gpu, &mixer);
    assert!(red(&inverted) < 1000, "the other inverted: {}", red(&inverted));

    mixer.channel_mut(shown).expect("the third input").effects[0].enabled = false;
    once_then_kept(&gpu, &mut mixer, "the other still again");
    let plain = main_picture(&gpu, &mixer);
    assert!(red(&plain) > 900_000, "the picture as it is: {}", red(&plain));
}

/// A tap of Main, or of an input drawn after its own — round a loop, the
/// frame before — is not told which picture it shows: its input is put
/// together every frame, as before, and what changes comes through.
#[test]
fn an_input_showing_main_or_one_drawn_after_it_is_put_together_every_frame() {
    let gpu = headless_gpu();
    let main = Deck::new_from_tap(&gpu, TapSource::MasterProgram, "Main", W, H).expect("a tap");
    let mut mixer = mixer_with(&gpu, main);
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "Main shown: every frame");
    assert!(
        over(&mixer).decks[0].deck.tapped_picture.is_none(),
        "Main's picture is not told"
    );

    // The overlay shows the third input and the third input the overlay:
    // the overlay, first found on the loop, is drawn first, and shows the
    // third input as it was the frame before.
    let (mut mixer, shown) = an_input_shown_in_the_overlay(&gpu);
    let over_uuid = over(&mixer).uuid().to_string();
    let back = Deck::new_from_tap(&gpu, TapSource::Channel(over_uuid), "the overlay", W, H)
        .expect("a tap");
    let third = mixer.channel_mut(shown).expect("the third input");
    let index = third.add_deck(back);
    third.decks[index].mute = true;
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "round a loop: every frame");
    assert!(
        over(&mixer).decks[0].deck.tapped_picture.is_none(),
        "a frame late: not told"
    );
    assert!(
        mixer.channel(shown).expect("the third input").decks[index]
            .deck
            .tapped_picture
            .is_some(),
        "the one drawn after is told the overlay's"
    );
    mixer.channel_mut(shown).expect("the third input").decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    assert_eq!(kept(&gpu, &mut mixer, 2), 0, "put together still");
    let after = main_picture(&gpu, &mixer);
    assert!(green(&after) > 900_000, "the new picture, a frame late: {}", green(&after));
}

/// The number of the picture the overlay holds now (`Channel::picture`).
fn number(mixer: &Mixer) -> u64 {
    over(mixer).picture().load(Ordering::Relaxed)
}

/// No two inputs' pictures have the same number, from the first — before
/// either is drawn: one count for every picture, a capture's too.
#[test]
fn no_two_inputs_pictures_have_the_same_number() {
    let gpu = headless_gpu();
    let mixer = Mixer::new(&gpu, W, H).expect("mixer");
    let under = mixer
        .channel(UNDER)
        .expect("a channel")
        .picture()
        .load(Ordering::Relaxed);
    assert_ne!(number(&mixer), under, "no two the same");
    let next = crate::deck::next_picture();
    assert!(next > number(&mixer) && next > under, "one count for all: {next}");
}

/// An input's picture keeps its number while it is kept, and has another
/// the frame it is put together anew.
#[test]
fn an_inputs_picture_put_together_anew_has_another_number() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    let stood = number(&mixer);
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "standing still");
    assert_eq!(number(&mixer), stood, "kept: the same picture");

    over_mut(&mut mixer).decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    assert_eq!(kept(&gpu, &mut mixer, 1), 0, "another picture in it");
    assert!(number(&mixer) > stood, "put together anew: another number");
}

/// An input's picture changed for the one before — the tap's swap, where
/// something shows it a frame late — has another number; with nothing to
/// change it for, the same.
#[test]
fn an_inputs_picture_changed_for_the_one_before_has_another_number() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    let stood = number(&mixer);
    over_mut(&mut mixer).swap_tap();
    assert_eq!(number(&mixer), stood, "nothing to change it for: the same");
    over_mut(&mut mixer).ensure_tap(&gpu, W, H);
    over_mut(&mut mixer).swap_tap();
    assert!(number(&mixer) > stood, "changed for the other: another number");
}

/// An input's picture made anew — the show another size — has another
/// number.
#[test]
fn an_inputs_picture_made_anew_has_another_number() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    let stood = number(&mixer);
    over_mut(&mut mixer).resize(&gpu, W / 2, H / 2);
    assert!(number(&mixer) > stood, "made anew: another number");
}

/// A layer in a transition — fading out, a picture standing still all the
/// while — is put together every frame: how far it has gone is not in what
/// is kept.
#[test]
fn a_layer_in_a_transition_is_put_together_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    assert_eq!(kept(&gpu, &mut mixer, 2), 2, "standing still");

    // Ten minutes of it: under way for as long as the test looks.
    over_mut(&mut mixer).decks[0].auto_transition = Some(DeckAutoTransition {
        enabled: true,
        transition_duration: DurationSpec::Seconds(600.0),
        phase: DeckTransitionPhase::Transitioning { progress: 0.0 },
        ..DeckAutoTransition::new()
    });
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "in a transition: every frame");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 2), 0, "the picture's frame kept");
    let phase = over(&mixer).decks[0].transition_phase();
    assert!(
        matches!(phase, DeckTransitionPhase::Transitioning { .. }),
        "under way still: {phase:?}"
    );

    over_mut(&mut mixer).decks[0].auto_transition = None;
    assert_eq!(kept(&gpu, &mut mixer, 1), 0, "taken off: put together");
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept after");
    let whole = main_picture(&gpu, &mixer);
    assert!(red(&whole) > 900_000, "the picture, whole: {}", red(&whole));
}

/// An input put together of nothing — its one layer muted — and kept so
/// is, once inputs showing nothing are left alone, drawn clear the once
/// that takes, not kept in its place: and then it is left.
#[test]
fn an_input_drawn_to_be_left_clear_is_not_kept_instead() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    mixer.skip_blank_channels = false;
    over_mut(&mut mixer).decks[0].mute = true;
    assert_eq!(kept(&gpu, &mut mixer, 1), 0, "muted: put together, of nothing");
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept so");
    let clear = main_picture(&gpu, &mixer);
    assert!(red(&clear) < 1000, "none of the picture: {}", red(&clear));

    mixer.skip_blank_channels = true;
    let kept_before = over(&mixer).frames_kept;
    assert_eq!(drawn(&gpu, &mut mixer, 4), 1, "drawn clear once, then left");
    assert_eq!(over(&mixer).frames_kept, kept_before, "not kept for it");
    let left = main_picture(&gpu, &mixer);
    assert!(red(&left) < 1000, "clear still: {}", red(&left));
}

/// A deck's textures made anew — empty — with no drawing of it after (it
/// is kept from the GPU after a fault): the picture put together of the
/// old ones is not kept.
#[test]
fn a_decks_textures_made_anew_are_put_together() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    over_mut(&mut mixer).decks[0].deck.fault("a test's fault");
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "its last frame, kept");
    let frozen = main_picture(&gpu, &mixer);
    assert!(red(&frozen) > 900_000, "the picture still: {}", red(&frozen));

    let drawn_before = over(&mixer).decks[0].deck.rendered_frames();
    over_mut(&mut mixer).decks[0].deck.resize(&gpu, W, H);
    assert_eq!(kept(&gpu, &mut mixer, 1), 0, "new textures: put together");
    let drawn_after = over(&mixer).decks[0].deck.rendered_frames();
    assert_eq!(drawn_after, drawn_before, "with no drawing of it");
    let empty = main_picture(&gpu, &mixer);
    assert!(red(&empty) < 1000, "what they hold — nothing: {}", red(&empty));
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept after");
}

/// One frame of the overlay alone, as the mixer draws one, and how many
/// pairs of time stamps its decks are left holding.
fn pairs_held(gpu: &GpuContext, mixer: &mut Mixer) -> usize {
    let audio = crate::audio::AudioData::default();
    let mut timing = GpuTimingFrame::new(128);
    mixer.channels[OVER]
        .render(
            gpu,
            &audio,
            &mixer.modulation,
            OVER,
            0.0,
            1.0 / 60.0,
            60,
            2,
            1.0,
            false,
            &mut Vec::new(),
            Some(&mut timing),
            mixer.query_set.as_ref(),
        )
        .expect("the overlay drawn");
    timing.allocations.len()
}

/// The overlay's first deck given a cost, as a draw leaves one.
fn give_a_cost(mixer: &mut Mixer) {
    let slot = &mut over_mut(mixer).decks[0];
    slot.render_cost_us = 5.0;
    slot.gpu_render_cost_us = 5.0;
}

/// What the overlay's first deck costs: by the clock, and by the GPU's.
fn cost(mixer: &Mixer) -> (f32, f32) {
    let slot = &over(mixer).decks[0];
    (slot.render_cost_us, slot.gpu_render_cost_us)
}

/// A deck that keeps its frame — a picture standing still, a deck kept
/// from the GPU after a fault — draws nothing: it holds no pair of time
/// stamps (neither would be written, and what was read back would be
/// another draw's time), and it costs nothing in the frame's time. One
/// that draws holds its pair.
#[test]
fn a_deck_that_keeps_its_frame_is_not_timed_and_costs_nothing() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    let timed = usize::from(mixer.query_set.is_some());

    give_a_cost(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 0, "its frame kept");
    let (by_clock, on_gpu) = cost(&mixer);
    assert!(by_clock <= 0.0 && on_gpu <= 0.0, "no cost: {by_clock} {on_gpu}");
    assert_eq!(pairs_held(&gpu, &mut mixer), 0, "and no pair held");

    over_mut(&mut mixer).decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    let drawn_before = over(&mixer).decks[0].deck.rendered_frames();
    assert_eq!(pairs_held(&gpu, &mut mixer), timed, "drawn: its pair held");
    let drawn_after = over(&mixer).decks[0].deck.rendered_frames();
    assert_eq!(drawn_after - drawn_before, 1, "drawn it was");

    over_mut(&mut mixer).decks[0].deck.fault("a test's fault");
    give_a_cost(&mut mixer);
    assert_eq!(pairs_held(&gpu, &mut mixer), 0, "kept from the GPU: no pair");
    let (by_clock, on_gpu) = cost(&mixer);
    assert!(by_clock <= 0.0 && on_gpu <= 0.0, "nor a cost: {by_clock} {on_gpu}");
}

/// A deck faulted in its draw has drawn nothing either, though the draw
/// was counted: what it encoded is taken back, its time stamps with it —
/// so its pair is given back, and it costs nothing.
#[test]
fn a_deck_faulted_in_its_draw_is_not_timed_and_costs_nothing() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);

    over_mut(&mut mixer).decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    let scope = format!("deck {}", over(&mixer).decks[0].deck.uuid());
    gpu.errors.fault_in_scope(&scope, "a test's fault");
    give_a_cost(&mut mixer);
    let drawn_before = over(&mixer).decks[0].deck.rendered_frames();
    assert_eq!(pairs_held(&gpu, &mut mixer), 0, "faulted: no pair held");
    let deck = &over(&mixer).decks[0].deck;
    assert_eq!(deck.gpu_error(), Some("a test's fault"), "in its draw");
    let counted = deck.rendered_frames() - drawn_before;
    assert_eq!(counted, 1, "the draw was counted, and taken back");
    let (by_clock, on_gpu) = cost(&mixer);
    assert!(by_clock <= 0.0 && on_gpu <= 0.0, "no cost: {by_clock} {on_gpu}");
}

/// A time the GPU took, read back a frame or two after the draw, is given
/// to a deck that draws still, and smoothed into the time it has; not to
/// one that costs nothing by the clock: it has drawn nothing since, and
/// covered or its input left out, nothing would take the time off it.
#[test]
fn a_time_read_back_is_not_given_to_a_deck_that_draws_nothing() {
    let gpu = headless_gpu();
    let mut mixer = a_picture_shown(&gpu);
    mixer.last_frame_gpu_times.clear();
    mixer.last_frame_gpu_times.insert((OVER, 0), 700.0);

    let slot = &mut over_mut(&mut mixer).decks[0];
    slot.render_cost_us = 0.0;
    slot.gpu_render_cost_us = 0.0;
    mixer.apply_gpu_times();
    let (by_clock, on_gpu) = cost(&mixer);
    assert!(by_clock <= 0.0 && on_gpu <= 0.0, "no cost: {by_clock} {on_gpu}");

    over_mut(&mut mixer).decks[0].render_cost_us = 5.0;
    mixer.apply_gpu_times();
    let (_, on_gpu) = cost(&mixer);
    assert!((on_gpu - 700.0).abs() < 1e-3, "one that draws: {on_gpu}");
    mixer.last_frame_gpu_times.insert((OVER, 0), 200.0);
    mixer.apply_gpu_times();
    let (_, on_gpu) = cost(&mixer);
    assert!((on_gpu - 600.0).abs() < 1e-2, "smoothed in: {on_gpu}");
}

/// A pair of time stamps given back is the next one handed out: none is
/// left to be read back unwritten.
#[test]
fn a_pair_of_time_stamps_given_back_is_handed_out_again() {
    let mut timing = GpuTimingFrame::new(8);
    assert_eq!(timing.allocate(0, 0), Some((0, 1)));
    assert_eq!(timing.allocate(0, 1), Some((2, 3)));
    timing.release_last();
    assert_eq!(timing.query_count(), 2, "one pair written");
    assert_eq!(timing.allocations, vec![(0, 0, 0, 1)], "the first deck's");
    assert_eq!(timing.allocate(1, 0), Some((2, 3)), "handed out again");
    for _ in 0..3 {
        timing.release_last();
    }
    assert_eq!(timing.query_count(), 0, "none left, and no fewer than none");
    assert!(timing.allocations.is_empty());
}

/// A live source `w` × `h`, as a camera or a capture brings one: its
/// texture, filled with `fill`, and the view a deck is shown.
fn live_source(gpu: &GpuContext, w: u32, h: u32) -> (wgpu::Texture, wgpu::TextureView) {
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("a live source"),
        size: wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    (texture, view)
}

/// Another frame of a live source: all of it `rgb`.
fn fill(gpu: &GpuContext, texture: &wgpu::Texture, rgb: [u8; 3]) {
    let (w, h) = (texture.width(), texture.height());
    let data = [rgb[0], rgb[1], rgb[2], 255].repeat((w * h) as usize);
    gpu.queue.write_texture(
        texture.as_image_copy(),
        &data,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(w * 4),
            rows_per_image: Some(h),
        },
        texture.size(),
    );
}

/// A live source drawn straight onto its input (`direct_layers`) is kept
/// by the number of its picture: none known, the input is put together
/// every frame; the same number, kept; another, put together with the new
/// picture. And the show another size, it is put together whatever the
/// number.
#[test]
fn a_live_source_is_kept_while_its_picture_is_the_same() {
    let gpu = headless_gpu();
    let (texture, view) = live_source(&gpu, W, H);
    fill(&gpu, &texture, RED);
    let mut deck = Deck::new_from_camera(&gpu, 1, "Camera", W, H, W, H).expect("a camera");
    deck.external_source_view = Some(view);
    deck.set_external_source_size(W, H);
    let mut mixer = mixer_with(&gpu, deck);

    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "no number: every frame anew");
    assert_eq!(over(&mixer).drawn_onto, 1, "drawn straight onto its input");
    let first = main_picture(&gpu, &mixer);
    assert!(red(&first) > 900_000, "its picture on Main: {}", red(&first));

    over_mut(&mut mixer).decks[0].deck.external_source_picture = Some(1);
    assert_eq!(kept(&gpu, &mut mixer, 4), 3, "a number: once, then kept");
    same_put_together(&gpu, &mut mixer, "a live source kept");
    assert_eq!(kept(&gpu, &mut mixer, 3), 2, "the switch back on");

    fill(&gpu, &texture, GREEN);
    over_mut(&mut mixer).decks[0].deck.external_source_picture = Some(2);
    assert_eq!(kept(&gpu, &mut mixer, 4), 3, "another number: once, then kept");
    let second = main_picture(&gpu, &mixer);
    assert!(green(&second) > 900_000, "the new picture: {}", green(&second));

    // Half the size, its source too — drawn straight onto its input still —
    // and the number as it was: the input's picture is a new, empty one.
    mixer.resize(&gpu, W / 2, H / 2);
    let (small, small_view) = live_source(&gpu, W / 2, H / 2);
    fill(&gpu, &small, GREEN);
    let deck = &mut over_mut(&mut mixer).decks[0].deck;
    deck.external_source_view = Some(small_view);
    deck.set_external_source_size(W / 2, H / 2);
    render(&gpu, &mut mixer);
    assert_eq!(over(&mixer).drawn_onto, 1, "straight onto its input still");
    let resized = main_picture(&gpu, &mixer);
    assert!(green(&resized) > 200_000, "put together: {}", green(&resized));
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "and kept after");
}

/// A clip of four seconds, 25 frames a second, with no sound, in `dir`;
/// `None` where there is no ffmpeg to make one.
fn a_clip(dir: &Path) -> Option<PathBuf> {
    let path = dir.join("clip.mp4");
    let made = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
        .args(["-i", "testsrc2=size=320x180:rate=25", "-t", "4"])
        .args([
            "-c:v", "libx264", "-preset", "veryfast", "-qp", "4", "-bf", "2", "-g", "50",
        ])
        .args(["-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    made.then_some(path)
}

/// How many frames the clip looked at has given.
fn clip_frames(mixer: &Mixer) -> u64 {
    over(mixer).decks[0].deck.clip_frames()
}

/// One frame, and time for the clip's next: what it decoded is handed
/// over between frames.
fn frame(gpu: &GpuContext, mixer: &mut Mixer) {
    render(gpu, mixer);
    let _ = gpu.device.poll(wgpu::PollType::wait_indefinitely());
    std::thread::sleep(Duration::from_millis(10));
}

/// Frames drawn till the clip has given none for fifteen on end: it stands
/// still.
fn till_it_stands(gpu: &GpuContext, mixer: &mut Mixer) {
    let until = Instant::now() + Duration::from_secs(20);
    let mut last = clip_frames(mixer);
    let mut still = 0;
    while still < 15 {
        assert!(Instant::now() < until, "the clip never stood still");
        frame(gpu, mixer);
        let now = clip_frames(mixer);
        if now == last {
            still += 1;
        } else {
            still = 0;
            last = now;
        }
    }
}

/// A clip, first in the overlay, and `over_it` over it: paused on its
/// first frame, as a clip added is, that frame in and drawn. No deck is
/// left out of a frame for the time the last one took
/// (`slow_layers_less_often`): what is counted here is what the clip's
/// frames ask for, however loaded the machine.
fn a_clip_standing(gpu: &GpuContext, path: &Path, over_it: Option<Deck>) -> Mixer {
    let deck = Deck::new_from_video(gpu, path, W, H).expect("a clip");
    let mut mixer = mixer_with(gpu, deck);
    mixer.slow_layers_less_often = false;
    if let Some(over_it) = over_it {
        over_mut(&mut mixer).add_deck(over_it);
    }
    let until = Instant::now() + Duration::from_secs(20);
    while !over(&mixer).decks[0].deck.has_video_frame() {
        assert!(Instant::now() < until, "the clip gave no frame");
        frame(gpu, &mut mixer);
    }
    till_it_stands(gpu, &mut mixer);
    mixer
}

/// A clip paused keeps its frame, and its input the picture put together
/// of it; with `keep_still_clips` off it is drawn every frame, to the same
/// Main. Played, it is drawn once for each frame it gives and no more, its
/// input put together as often.
#[test]
fn a_clip_is_drawn_once_for_each_of_its_frames() {
    let gpu = headless_gpu();
    let dir = tempfile::tempdir().expect("a folder");
    let Some(path) = a_clip(dir.path()) else {
        eprintln!("ffmpeg not available — skipping");
        return;
    };
    let mut mixer = a_clip_standing(&gpu, &path, None);

    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "paused: its frame kept");
    assert_eq!(kept(&gpu, &mut mixer, 5), 5, "and its input's picture");
    let as_kept = main_picture(&gpu, &mixer);
    let of_the_clip = not_the_card(&as_kept);
    assert!(of_the_clip > 460_000, "the clip on Main: {of_the_clip}");
    let bars = coloured(&as_kept);
    assert!(bars > 150_000, "its picture, not a black frame: {bars}");
    // Kept from one of its frames to the next, not for good: the UI gives
    // it a rate still, as it gives a picture none.
    let clip = &over(&mixer).decks[0].deck;
    assert!(!clip.is_still(mixer.modulation()), "a clip is not a still");
    assert!(
        picture(&gpu, RED).is_still(mixer.modulation()),
        "a picture is"
    );

    mixer.keep_still_clips = false;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "switched off: every frame");
    assert_eq!(kept(&gpu, &mut mixer, 4), 0, "and its input put together");
    let every_frame = main_picture(&gpu, &mixer);
    let worst = differ(&as_kept, &every_frame);
    assert!(worst < 1e-3, "Main differs by {worst}");

    mixer.keep_still_clips = true;
    let kept_before = over(&mixer).frames_kept;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "switched on: once more");
    assert_eq!(over(&mixer).frames_kept - kept_before, 3, "then kept");

    // Played: a draw for each frame the clip gives, the input put together
    // then and kept between.
    let frames_before = clip_frames(&mixer);
    let drawn_before = over(&mixer).decks[0].deck.rendered_frames();
    let kept_before = over(&mixer).frames_kept;
    assert!(over(&mixer).decks[0].deck.video_set_playing(true), "played");
    let until = Instant::now() + Duration::from_secs(20);
    let mut frames = 0;
    while clip_frames(&mixer) - frames_before < 3 {
        assert!(Instant::now() < until, "the clip does not play");
        frame(&gpu, &mut mixer);
        frames += 1;
    }
    let given = clip_frames(&mixer) - frames_before;
    let redrawn = over(&mixer).decks[0].deck.rendered_frames() - drawn_before;
    assert_eq!(u64::from(redrawn), given, "a draw for each of its frames");
    assert_eq!(
        over(&mixer).frames_kept - kept_before,
        frames - redrawn,
        "put together as often, kept between"
    );
}

/// A clip whose frame is kept is shown all the same: each frame of the
/// show counts towards its rate, as it did drawn every frame.
#[test]
fn a_clip_kept_still_counts_its_frames_shown() {
    let gpu = headless_gpu();
    let dir = tempfile::tempdir().expect("a folder");
    let Some(path) = a_clip(dir.path()) else {
        eprintln!("ffmpeg not available — skipping");
        return;
    };
    let mut mixer = a_clip_standing(&gpu, &path, None);
    let drawn_before = over(&mixer).decks[0].deck.rendered_frames();

    // Each frame of the show counts the clip as shown then (its rate is
    // read from the time between two such), though none draws it —
    // whatever the time a frame takes.
    for _ in 0..8 {
        let before = over(&mixer).decks[0].deck.last_shown();
        render(&gpu, &mut mixer);
        let after = over(&mixer).decks[0].deck.last_shown();
        assert!(after > before, "counted as shown in this frame");
    }

    let drawn_after = over(&mixer).decks[0].deck.rendered_frames();
    assert_eq!(drawn_after, drawn_before, "its frame kept throughout");
}

/// A picture in a picture: a plain colour, cropped and placed as every new
/// deck is, at a little over half the size in the middle of the frame.
fn a_picture_in_picture(gpu: &GpuContext) -> Deck {
    let effect = |name: &str| {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("shaders")
            .join(format!("{name}.fs"));
        let shader = crate::isf::ISFShader::from_file(path).expect("a shader");
        Effect::new(gpu, shader).expect("an effect")
    };
    let mut deck = Deck::new_solid_color(gpu, [0.9, 0.6, 0.1, 1.0], W, H).expect("a colour");
    deck.add_effect(effect("crop"));
    deck.add_effect(effect("transform"));
    deck.effects[1].params.set_float("scale", 0.55);
    deck
}

/// A clip under a picture in a picture is drawn only where it shows, kept
/// or not, as it was before it was kept; and the picture over it taken
/// away, all of it is drawn the very next frame — more of it shows —
/// though the clip has given no new frame.
#[test]
fn a_clip_kept_is_drawn_where_it_shows_and_again_when_more_shows() {
    let gpu = headless_gpu();
    let dir = tempfile::tempdir().expect("a folder");
    let Some(path) = a_clip(dir.path()) else {
        eprintln!("ffmpeg not available — skipping");
        return;
    };
    let mut mixer = a_clip_standing(&gpu, &path, Some(a_picture_in_picture(&gpu)));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 0, "paused: its frame kept");
    let share = over(&mixer).decks[0].deck.drawn_share();
    assert!(share < 0.9, "drawn round the picture over it only: {share}");
    assert!(share > 0.5, "and all of what shows round it: {share}");

    let _ = over_mut(&mut mixer).remove_deck(1);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "uncovered: drawn at once");
    let share = over(&mixer).decks[0].deck.drawn_share();
    assert!(share >= 1.0, "all of it: {share}");
    let at_once = main_picture(&gpu, &mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
    let as_kept = main_picture(&gpu, &mixer);
    let worst = differ(&at_once, &as_kept);
    assert!(worst < 1e-3, "kept, Main differs by {worst}");
    mixer.keep_still_clips = false;
    render(&gpu, &mut mixer);
    let every_frame = main_picture(&gpu, &mixer);
    let worst = differ(&as_kept, &every_frame);
    assert!(worst < 1e-3, "Main differs by {worst}");
}

/// The thumbnail of the deck looked at — the overlay's first — is on
/// screen: the deck draws a frame of its own, whatever it is, rather than
/// being drawn straight onto its input from its source.
fn thumbnail_on_screen(mixer: &mut Mixer) {
    mixer.previewed_decks = vec![over(mixer).decks[0].deck.uuid().to_string()];
}

/// A camera's deck `W` × `H`, shown `view` or nothing yet.
fn a_camera(gpu: &GpuContext, view: Option<wgpu::TextureView>) -> Deck {
    let mut deck = Deck::new_from_camera(gpu, 1, "Camera", W, H, W, H).expect("a camera");
    deck.external_source_view = view;
    deck.set_external_source_size(W, H);
    deck
}

/// A live source, red, alone in the overlay, its thumbnail on screen: it
/// draws a frame of its own. No frame drawn yet, no number told; its
/// texture, for another picture.
fn a_live_source_in_its_own_frame(gpu: &GpuContext) -> (Mixer, wgpu::Texture) {
    let (texture, view) = live_source(gpu, W, H);
    fill(gpu, &texture, RED);
    let mut mixer = mixer_with(gpu, a_camera(gpu, Some(view)));
    thumbnail_on_screen(&mut mixer);
    (mixer, texture)
}

/// An input showing another through a tap that draws a frame of its own —
/// its thumbnail is on screen — is kept while the other stands still, the
/// tap's frame with it: drawn once of each picture of the other, the very
/// frame the other has it. With `keep_still_inputs` off the tap draws
/// every frame, to the same Main.
#[test]
fn a_tap_drawing_its_own_frame_keeps_it_while_the_other_stands_still() {
    let gpu = headless_gpu();
    let (mut mixer, shown) = an_input_shown_in_the_overlay(&gpu);
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "in a frame of its own: drawn");
    assert_eq!(over(&mixer).drawn_onto, 0, "not straight from the other's picture");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 5), 0, "the other standing still: its frame kept");
    assert_eq!(kept(&gpu, &mut mixer, 5), 5, "and its input's picture with it");
    let first = main_picture(&gpu, &mixer);
    assert!(red(&first) > 900_000, "the other's picture on Main: {}", red(&first));
    same_put_together(&gpu, &mut mixer, "a tap's own frame kept");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 1, "the switch back on: once more");

    mixer.channel_mut(shown).expect("the third input").decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "the other changed: drawn that frame");
    let second = main_picture(&gpu, &mixer);
    assert!(green(&second) > 900_000, "its new picture at once: {}", green(&second));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
    let later = main_picture(&gpu, &mixer);
    assert!(green(&later) > 900_000, "the new picture still: {}", green(&later));

    mixer.keep_still_inputs = false;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "the switch off: every frame");
    mixer.keep_still_inputs = true;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "on again: once, then kept");
}

/// The other input moving — an effect on it draws every frame — the tap
/// that draws a frame of its own draws every one of its pictures, and
/// keeps its frame again once the other stands still.
#[test]
fn a_tap_drawing_its_own_frame_draws_every_picture_of_one_that_moves() {
    let gpu = headless_gpu();
    let (mut mixer, shown) = an_input_shown_in_the_overlay(&gpu);
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 1, "standing still: once, then kept");

    mixer
        .channel_mut(shown)
        .expect("the third input")
        .add_effect(invert(&gpu));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "the other every frame: this one too");
    let inverted = main_picture(&gpu, &mixer);
    assert!(red(&inverted) < 1000, "the other inverted: {}", red(&inverted));

    mixer.channel_mut(shown).expect("the third input").effects[0].enabled = false;
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "the other still again: once, then kept");
    let plain = main_picture(&gpu, &mixer);
    assert!(red(&plain) > 900_000, "the picture as it is: {}", red(&plain));
}

/// Drawn straight from the other's picture for a while — its thumbnail off
/// screen — a tap's own frame is as it was left. Back in it, that frame is
/// this frame's only if the other's picture is the one it was drawn of;
/// another, and it is drawn at once — not shown a picture long gone.
#[test]
fn a_tap_back_in_its_own_frame_keeps_it_only_of_the_same_picture() {
    let gpu = headless_gpu();
    let (mut mixer, shown) = an_input_shown_in_the_overlay(&gpu);
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 1, "a frame of its own, once");

    mixer.previewed_decks.clear();
    assert_eq!(deck_drawn(&gpu, &mut mixer, 2), 0, "drawn straight: no frame of its own");
    assert_eq!(over(&mixer).drawn_onto, 1, "straight from the other's picture");
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 2), 0, "back, the same picture: the frame it has");
    assert_eq!(over(&mixer).drawn_onto, 0, "its own frame put on its input");
    let same = main_picture(&gpu, &mixer);
    assert!(red(&same) > 900_000, "the other's picture: {}", red(&same));

    mixer.previewed_decks.clear();
    render(&gpu, &mut mixer);
    mixer.channel_mut(shown).expect("the third input").decks[0]
        .deck
        .replace_picture(&gpu, &plain(GREEN));
    render(&gpu, &mut mixer);
    assert_eq!(over(&mixer).drawn_onto, 1, "the new picture, drawn straight");
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "back, another picture: drawn at once");
    let now = main_picture(&gpu, &mixer);
    assert!(green(&now) > 900_000, "the other's picture now: {}", green(&now));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
}

/// A live source drawing a frame of its own is kept by the number of its
/// picture, as one drawn straight onto its input is: none known, it is
/// drawn every frame; the same number, its frame is kept, and its input's
/// picture; another, drawn with the new picture that frame.
#[test]
fn a_live_source_drawing_its_own_frame_keeps_it_by_its_picture() {
    let gpu = headless_gpu();
    let (mut mixer, texture) = a_live_source_in_its_own_frame(&gpu);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "no number: drawn every frame");
    assert_eq!(over(&mixer).drawn_onto, 0, "in a frame of its own");
    let first = main_picture(&gpu, &mixer);
    assert!(red(&first) > 900_000, "its picture on Main: {}", red(&first));

    over_mut(&mut mixer).decks[0].deck.external_source_picture = Some(1);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "a number: once, then kept");
    assert_eq!(kept(&gpu, &mut mixer, 3), 3, "its input's picture with it");
    same_put_together(&gpu, &mut mixer, "a live source's own frame kept");
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 1, "the switch back on: once more");

    fill(&gpu, &texture, GREEN);
    over_mut(&mut mixer).decks[0].deck.external_source_picture = Some(2);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "another number: drawn that frame");
    let second = main_picture(&gpu, &mixer);
    assert!(green(&second) > 900_000, "the new picture at once: {}", green(&second));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
}

/// A live source in a frame of its own, its picture told: drawn once and
/// kept. Its texture.
fn a_live_source_kept(gpu: &GpuContext) -> (Mixer, wgpu::Texture) {
    let (mut mixer, texture) = a_live_source_in_its_own_frame(gpu);
    over_mut(&mut mixer).decks[0].deck.external_source_picture = Some(1);
    assert_eq!(deck_drawn(gpu, &mut mixer, 3), 1, "once, then kept");
    (mixer, texture)
}

/// What was just changed of the deck looked at has it drawn once — the
/// next frame — and kept after, without a fault.
fn drawn_again(gpu: &GpuContext, mixer: &mut Mixer, what: &str) {
    assert_eq!(deck_drawn(gpu, mixer, 1), 1, "{what}: drawn again");
    assert_eq!(deck_drawn(gpu, mixer, 3), 0, "{what}: and kept after");
    assert!(
        over(mixer).decks[0].deck.gpu_error().is_none(),
        "{what}: drawn without a fault"
    );
}

/// The frame kept is the picture as the deck draws it: fitted another
/// way, it is drawn again, though the picture's number stands.
#[test]
fn a_live_source_kept_is_drawn_again_fitted_another_way() {
    let gpu = headless_gpu();
    let (mut mixer, _texture) = a_live_source_kept(&gpu);
    over_mut(&mut mixer).decks[0]
        .deck
        .set_scaling_mode(ScalingMode::Fit);
    drawn_again(&gpu, &mut mixer, "fitted another way");
}

/// Its own transparency kept, where it was flattened: drawn again, though
/// the picture's number stands.
#[test]
fn a_live_source_kept_is_drawn_again_its_transparency_kept() {
    let gpu = headless_gpu();
    let (mut mixer, _texture) = a_live_source_kept(&gpu);
    over_mut(&mut mixer).decks[0].deck.set_transparent(true);
    drawn_again(&gpu, &mut mixer, "its own transparency kept");
}

/// Told another size of its source — placed by it: drawn again, though the
/// picture's number stands.
#[test]
fn a_live_source_kept_is_drawn_again_told_another_size() {
    let gpu = headless_gpu();
    let (mut mixer, _texture) = a_live_source_kept(&gpu);
    over_mut(&mut mixer).decks[0]
        .deck
        .set_external_source_size(W / 2, H / 2);
    drawn_again(&gpu, &mut mixer, "told another size");
}

/// Given a view that reads its picture as it was coded, it is drawn from
/// that one: drawn again, though the picture's number stands.
#[test]
fn a_live_source_kept_is_drawn_again_read_as_it_was_coded() {
    let gpu = headless_gpu();
    let (mut mixer, texture) = a_live_source_kept(&gpu);
    let coded = texture.create_view(&wgpu::TextureViewDescriptor::default());
    over_mut(&mut mixer).decks[0].deck.external_source_coded_view = Some(coded);
    drawn_again(&gpu, &mut mixer, "read as it was coded");
}

/// A screen captured and shown centred is placed by the size it is asked
/// for, not the one it comes at (`placed_size`): asked for another, the
/// frame kept is drawn again, though the picture and its size stand.
#[test]
fn a_capture_kept_is_drawn_again_sized_another_way() {
    let gpu = headless_gpu();
    let (texture, view) = live_source(&gpu, 2 * W, 2 * H);
    fill(&gpu, &texture, RED);
    let state = ScreenCaptureState {
        capture_id: 1,
        identity: TargetIdentity::Display {
            label: "a screen".to_string(),
            uuid: None,
        },
        config: CaptureConfig {
            scale_to: Some((W, H)),
            ..Default::default()
        },
        given: Vec::new(),
        listed_as: None,
        presentation: None,
    };
    let mut deck = Deck::new_from_screen_capture(&gpu, state, "a screen", 2 * W, 2 * H, W, H)
        .expect("a screen's input");
    deck.set_scaling_mode(ScalingMode::Center);
    deck.external_source_view = Some(view);
    deck.external_source_picture = Some(1);
    let mut mixer = mixer_with(&gpu, deck);
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "its picture the same: once, then kept");
    let whole = main_picture(&gpu, &mixer);
    assert!(red(&whole) > 900_000, "fitted within the show: {}", red(&whole));

    over_mut(&mut mixer).decks[0]
        .deck
        .screen_capture
        .as_mut()
        .expect("its capture")
        .config
        .scale_to = Some((W / 2, H / 2));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "asked for half the size: drawn again");
    let half = main_picture(&gpu, &mixer);
    assert!(
        red(&half) > 200_000 && red(&half) < 260_000,
        "half the size at once: {}",
        red(&half)
    );
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
    assert!(
        over(&mixer).decks[0].deck.gpu_error().is_none(),
        "drawn without a fault"
    );
}

/// A live source under a picture in a picture, drawing a frame of its own,
/// is drawn only where it shows, kept or not; and the picture over it
/// taken away, all of it is drawn the very next frame — more of it shows —
/// though its picture's number stands.
#[test]
fn a_live_source_kept_is_drawn_where_it_shows_and_again_when_more_shows() {
    let gpu = headless_gpu();
    let (texture, view) = live_source(&gpu, W, H);
    fill(&gpu, &texture, RED);
    let mut deck = a_camera(&gpu, Some(view));
    deck.external_source_picture = Some(1);
    let mut mixer = mixer_with(&gpu, deck);
    // Every deck in a frame of its own, drawn where it shows.
    mixer.direct_layers = false;
    over_mut(&mut mixer).add_deck(a_picture_in_picture(&gpu));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 1, "its picture the same: once, then kept");
    assert_eq!(over(&mixer).drawn_onto, 0, "in a frame of its own");
    let share = over(&mixer).decks[0].deck.drawn_share();
    assert!(share < 0.9, "drawn round the picture over it only: {share}");
    assert!(share > 0.5, "and all of what shows round it: {share}");

    let _ = over_mut(&mut mixer).remove_deck(1);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "uncovered: drawn at once");
    let share = over(&mixer).decks[0].deck.drawn_share();
    assert!(share >= 1.0, "all of it: {share}");
    let at_once = main_picture(&gpu, &mixer);
    assert!(red(&at_once) > 900_000, "all of its picture: {}", red(&at_once));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
}

/// A live source with nothing to draw from yet has no frame to keep,
/// whatever number it is told: it is drawn every frame, and its picture
/// the frame its view comes.
#[test]
fn a_live_source_with_nothing_to_draw_from_is_drawn_every_frame() {
    let gpu = headless_gpu();
    let (texture, view) = live_source(&gpu, W, H);
    fill(&gpu, &texture, RED);
    let mut deck = a_camera(&gpu, None);
    deck.external_source_picture = Some(1);
    let mut mixer = mixer_with(&gpu, deck);
    thumbnail_on_screen(&mut mixer);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "no view yet: every frame");
    let nothing = main_picture(&gpu, &mixer);
    assert!(red(&nothing) < 1000, "nothing of it yet: {}", red(&nothing));

    over_mut(&mut mixer).decks[0].deck.external_source_view = Some(view);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 1), 1, "its view come: drawn");
    let first = main_picture(&gpu, &mixer);
    assert!(red(&first) > 900_000, "its picture at once: {}", red(&first));
    assert_eq!(deck_drawn(&gpu, &mut mixer, 3), 0, "and kept after");
}

/// A depth sensor's picture is made in its deck, of more than the one
/// view: whatever number it were told, it is drawn every frame.
#[test]
fn a_depth_sensor_is_drawn_every_frame_whatever_its_number() {
    let gpu = headless_gpu();
    let (_texture, view) = live_source(&gpu, W, H);
    let mut deck =
        Deck::new_from_depth_sensor(&gpu, 0, "Sensor", W, H, W, H).expect("a depth sensor");
    deck.external_source_view = Some(view);
    deck.external_source_picture = Some(1);
    let mut mixer = mixer_with(&gpu, deck);
    assert_eq!(deck_drawn(&gpu, &mut mixer, 4), 4, "every frame");
    assert_eq!(over(&mixer).drawn_onto, 0, "in a frame of its own");
}

/// A live source whose frame is kept is shown all the same: each frame of
/// the show counts towards its rate, as it did drawn every frame.
#[test]
fn a_live_source_kept_counts_its_frames_shown() {
    let gpu = headless_gpu();
    let (mut mixer, _texture) = a_live_source_kept(&gpu);
    let drawn_before = over(&mixer).decks[0].deck.rendered_frames();

    for _ in 0..8 {
        let before = over(&mixer).decks[0].deck.last_shown();
        render(&gpu, &mut mixer);
        let after = over(&mixer).decks[0].deck.last_shown();
        assert!(after > before, "counted as shown in this frame");
    }

    let drawn_after = over(&mixer).decks[0].deck.rendered_frames();
    assert_eq!(drawn_after, drawn_before, "its frame kept throughout");
}
