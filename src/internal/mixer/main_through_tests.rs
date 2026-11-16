//! Main that is one input, whole, is handed out as that input's own
//! picture, with no pass drawn (`Mixer::pass_main_through`): the same
//! picture to the bit as the pass would have made. Anything more on Main —
//! an overlay, a fade, a master effect that runs, a tone map, a LUT, a tap
//! of Main — or an input that is not Main's size, and the pass is drawn as
//! before, from that frame on; and when what is on Main changes the
//! picture handed out is the new one at once.

use super::blank_tests::{
    cut_lower_third, headless_gpu, invert, main_picture, mixer_with, over, over_mut, plate,
    read_texture, render, show_lower,
};
use super::main_pass_tests::{
    assert_near, card_input, input_pixel, main_bits, main_pixel, mixer_of, BLUE, H, UNDER, W,
};
use super::{MainSource, MixMode, Mixer, TonemapMode};
use crate::channel::BlendMode;
use crate::deck::{Deck, TapSource};
use crate::renderer::GpuContext;

const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
/// A card that leaves what is under it showing.
const SHEER: [f32; 4] = [1.0, 0.5, 0.0, 0.6];

/// A frame drawn: whether Main was handed out as its one input's picture.
fn through(gpu: &GpuContext, mixer: &mut Mixer) -> bool {
    render(gpu, mixer);
    mixer.main_passed_through()
}

/// Whether Main, as handed out, is the input at `at`'s own texture and
/// view.
fn is_input(mixer: &Mixer, at: usize) -> bool {
    let input = mixer.channel(at).expect("the input");
    *mixer.composite_texture() == input.composite_texture
        && *mixer.composite_view() == input.composite_view
}

/// Main's size, as handed out.
fn main_size(mixer: &Mixer) -> (u32, u32) {
    let main = mixer.composite_texture();
    (main.width(), main.height())
}

/// One input on Main: no pass at all, Main handed out is that input's own
/// texture, and the picture is the same to the bit as the pass draws — for
/// a card that covers and for one that leaves Main part clear.
#[test]
fn main_of_one_input_is_that_inputs_picture() {
    let gpu = headless_gpu();
    for colour in [BLUE, SHEER] {
        let (mut mixer, _) = mixer_of(&gpu, Some(colour), &[]);
        assert!(mixer.pass_main_through, "on to begin with");
        assert!(through(&gpu, &mut mixer));
        assert_eq!(mixer.main_passes, 0, "no pass drawn");
        assert!(is_input(&mixer, UNDER), "the input's own picture");
        let handed = main_bits(&gpu, &mixer);

        mixer.pass_main_through = false;
        assert!(!through(&gpu, &mut mixer));
        assert_eq!(mixer.main_passes, 1, "the pass, as before");
        assert!(!is_input(&mixer, UNDER), "Main's own");
        assert_eq!(handed, main_bits(&gpu, &mixer), "the same to the bit");
        let card = input_pixel(&gpu, &mixer, UNDER);
        assert!(card[3] > 0.5, "the card is in it: {card:?}");
        assert_near(main_pixel(&gpu, &mixer), card, "the card");

        mixer.pass_main_through = true;
        assert!(through(&gpu, &mut mixer), "and back");
        assert_eq!(mixer.main_passes, 0, "no pass again");
        assert_eq!(handed, main_bits(&gpu, &mixer));
    }
}

/// A picture with edges in it — a lower third's plate and letters, its
/// input taken to Main — is handed out as the pass draws it: nothing
/// moved, turned or scaled.
#[test]
fn a_picture_with_edges_is_handed_out_as_the_pass_draws_it() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    let uuid = over(&mixer).uuid().to_string();
    over_mut(&mut mixer).overlay = false;
    mixer.main_mut().set_overlay(&uuid, false, false);
    mixer.main_mut().take(MainSource::Channel(uuid), false);
    show_lower(&gpu, &mut mixer);
    assert!(through(&gpu, &mut mixer));
    let handed = main_picture(&gpu, &mixer);
    mixer.pass_main_through = false;
    assert!(!through(&gpu, &mut mixer));
    let drawn = main_picture(&gpu, &mixer);
    assert!(plate(&handed) > 5_000, "the plate is in it");
    assert_eq!(plate(&handed), plate(&drawn));
    let worst = handed
        .iter()
        .zip(&drawn)
        .flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs()))
        .fold(0.0, f32::max);
    assert!(worst < 1e-3, "apart by {worst}");
}

/// An overlay shown ends it — the pass is drawn that frame, the overlay in
/// it — and hidden again Main is the input's picture again: the right
/// picture at once both ways.
#[test]
fn an_overlay_showing_is_drawn_and_hidden_is_through_again() {
    let gpu = headless_gpu();
    let (mut mixer, overlays) = mixer_of(&gpu, Some(BLUE), &[(SHEER, BlendMode::Normal, 1.0)]);
    let overlay = mixer.channel(overlays[0]).expect("the overlay");
    let uuid = overlay.uuid().to_string();
    assert!(!through(&gpu, &mut mixer), "two layers");
    assert_eq!(mixer.main_passes, 1);
    let both = main_bits(&gpu, &mixer);

    mixer.main_mut().set_overlay(&uuid, false, false);
    assert!(through(&gpu, &mut mixer), "the card alone");
    assert!(is_input(&mixer, UNDER));
    let card = main_bits(&gpu, &mixer);
    assert_ne!(card, both);
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card alone");

    mixer.main_mut().set_overlay(&uuid, true, false);
    assert!(!through(&gpu, &mut mixer), "the overlay on again");
    assert!(!is_input(&mixer, UNDER));
    assert_eq!(main_bits(&gpu, &mixer), both, "with the overlay, at once");

    mixer.main_mut().set_overlay(&uuid, false, false);
    assert!(through(&gpu, &mut mixer));
    assert_eq!(main_bits(&gpu, &mixer), card, "without it, at once");
}

/// Nothing taken to Main and one overlay over it at full level, whatever
/// its blend — the bottom layer is drawn as it is — is that overlay's
/// picture. Below full level it is drawn.
#[test]
fn a_lone_overlay_at_full_level_is_through() {
    let gpu = headless_gpu();
    for blend in [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply] {
        let (mut mixer, overlays) = mixer_of(&gpu, None, &[(SHEER, blend, 1.0)]);
        assert!(through(&gpu, &mut mixer), "{blend:?}");
        assert!(is_input(&mixer, overlays[0]), "{blend:?}");
        let handed = main_bits(&gpu, &mixer);
        mixer.pass_main_through = false;
        assert!(!through(&gpu, &mut mixer));
        assert_eq!(handed, main_bits(&gpu, &mixer), "{blend:?}: to the bit");
    }

    let (mut mixer, overlays) = mixer_of(&gpu, None, &[(SHEER, BlendMode::Normal, 0.5)]);
    assert!(!through(&gpu, &mut mixer), "at half level");
    assert_eq!(mixer.main_passes, 1);
    let whole = input_pixel(&gpu, &mixer, overlays[0]);
    assert!(whole[3] > 0.5, "the card is in it: {whole:?}");
    assert_near(main_pixel(&gpu, &mixer), whole.map(|c| c * 0.5), "half of it");
}

/// A fade is drawn while it runs — a dissolve's two pictures, and a fade
/// up from nothing, one layer below full level — and Main is the new
/// input's picture once it is over.
#[test]
fn a_fade_is_drawn_and_its_end_is_through() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    let (to, to_uuid) = card_input(&gpu, &mut mixer, GREEN);
    assert!(through(&gpu, &mut mixer));
    // So long a fade that a frame's time is nothing of it: held where put.
    mixer.main_mut().fade_secs = 1.0e12;
    mixer.main_mut().take(MainSource::Channel(to_uuid.clone()), true);
    let fade = mixer.main_mut().fade.as_mut().expect("a fade running");
    fade.progress = 0.4;
    assert!(!through(&gpu, &mut mixer), "a dissolve");
    assert_near(main_pixel(&gpu, &mixer), [0.0, 0.4, 0.6, 1.0], "part way");
    // Cut to where it was going.
    mixer.main_mut().take(MainSource::Channel(to_uuid), false);
    assert!(through(&gpu, &mut mixer), "over");
    assert!(is_input(&mixer, to));
    assert_near(main_pixel(&gpu, &mixer), GREEN, "the new input");

    mixer.main_mut().take(MainSource::Black, false);
    assert!(!through(&gpu, &mut mixer), "nothing on Main");
    let under = mixer.channel(UNDER).expect("an input").uuid().to_string();
    mixer.main_mut().take(MainSource::Channel(under), true);
    let fade = mixer.main_mut().fade.as_mut().expect("a fade running");
    fade.progress = 0.4;
    assert!(!through(&gpu, &mut mixer), "a fade up");
    assert_eq!(mixer.main_passes, 1);
    assert_near(main_pixel(&gpu, &mixer), [0.0, 0.0, 0.4, 0.4], "part way up");
}

/// A master effect that runs works on Main's own picture: the pass is
/// drawn under it. One turned off, or kept offline, does nothing to Main.
#[test]
fn a_master_effect_that_runs_is_drawn_under() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    mixer.add_master_effect(invert(&gpu));
    assert!(!through(&gpu, &mut mixer), "inverted");
    assert_eq!(mixer.main_passes, 1);
    assert!(main_pixel(&gpu, &mixer)[0] > 0.5, "blue inverted is yellow");

    mixer.master_effects_mut()[0].enabled = false;
    assert!(through(&gpu, &mut mixer), "turned off");
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card as it is");

    mixer.master_effects_mut()[0].enabled = true;
    assert!(!through(&gpu, &mut mixer), "on again");
    assert!(main_pixel(&gpu, &mixer)[0] > 0.5, "inverted again, at once");

    mixer.master_effects_mut()[0].set_offline("its shader is gone".to_string());
    assert!(through(&gpu, &mut mixer), "kept offline");
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card as it is");
}

/// A tone map works on Main's own picture: the pass is drawn under it.
#[test]
fn a_tone_map_is_drawn_under() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    assert!(through(&gpu, &mut mixer));
    mixer.set_tonemap_mode(&gpu.queue, TonemapMode::Reinhard);
    assert!(!through(&gpu, &mut mixer), "tone mapped");
    assert_eq!(mixer.main_passes, 1);
    let mapped = main_pixel(&gpu, &mixer);
    assert!(mapped[2] < 0.75, "the curve is on it: {mapped:?}");

    mixer.set_tonemap_mode(&gpu.queue, TonemapMode::Bypass);
    assert!(through(&gpu, &mut mixer), "none again");
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card as it is");
}

/// A LUT works on Main's own picture: the pass is drawn under it.
#[test]
fn a_lut_is_drawn_under() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    assert!(through(&gpu, &mut mixer));
    let white = crate::renderer::lut::ParsedLut {
        title: None,
        size_3d: 2,
        data_3d: vec![1.0; 24],
        domain_min: [0.0; 3],
        domain_max: [1.0; 3],
        shaper: None,
    };
    mixer.load_lut(&gpu.device, &gpu.queue, &white, "white.cube".to_string());
    assert!(!through(&gpu, &mut mixer), "graded");
    assert_eq!(mixer.main_passes, 1);
    let graded = main_pixel(&gpu, &mixer);
    assert!(graded[0] > 0.5, "everything to white: {graded:?}");

    mixer.unload_lut();
    assert!(through(&gpu, &mut mixer), "none again");
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card as it is");
}

/// A layer that shows Main reads Main's own picture, kept a frame: the
/// pass is drawn while there is one.
#[test]
fn a_tap_of_main_is_drawn_under() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    // In an input that is not on Main.
    let (other, _) = card_input(&gpu, &mut mixer, GREEN);
    let tap = Deck::new_from_tap(&gpu, TapSource::MasterProgram, "Main", W, H).expect("a tap");
    let at = mixer.channel_mut(other).expect("the input").add_deck(tap);
    assert!(!through(&gpu, &mut mixer), "Main tapped");
    assert_eq!(mixer.main_passes, 1);
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card");

    mixer.channel_mut(other).expect("the input").decks.remove(at);
    assert!(through(&gpu, &mut mixer), "the tap gone");
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the card");
}

/// A layer taken to Main by itself is drawn — its frame is not an input's
/// picture — and so is an input of another size than Main's.
#[test]
fn a_layer_alone_and_an_input_of_another_size_are_drawn() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    let under = mixer.channel(UNDER).expect("an input");
    let layer = under.decks[0].deck.uuid().to_string();
    mixer.main_mut().take(MainSource::Deck(layer), false);
    assert!(!through(&gpu, &mut mixer), "a layer by itself");
    assert_eq!(mixer.main_passes, 1);
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the layer");

    let small = mixer.add_channel(&gpu, W / 2, H / 2).expect("an input");
    let card = Deck::new_solid_color(&gpu, GREEN, W / 2, H / 2).expect("a card");
    let input = mixer.channel_mut(small).expect("the input");
    input.add_deck(card);
    let uuid = input.uuid().to_string();
    mixer.main_mut().take(MainSource::Channel(uuid), false);
    assert!(!through(&gpu, &mut mixer), "half Main's size");
    assert_eq!(mixer.main_passes, 1);
    assert_eq!(main_size(&mixer), (W, H), "Main's own");
    assert_near(main_pixel(&gpu, &mixer), GREEN, "drawn to Main's size");
}

/// Another input taken, Main is that one's picture the same frame. An
/// input removed while Main is its picture leaves that picture standing
/// until the next frame, which draws Main clear.
#[test]
fn main_follows_what_is_taken_and_outlives_its_input() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    let (other, other_uuid) = card_input(&gpu, &mut mixer, GREEN);
    assert!(through(&gpu, &mut mixer));
    assert!(is_input(&mixer, UNDER));
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the first");

    mixer.main_mut().take(MainSource::Channel(other_uuid), false);
    assert!(through(&gpu, &mut mixer));
    assert!(is_input(&mixer, other), "the input taken");
    assert_near(main_pixel(&gpu, &mixer), GREEN, "at once");

    assert!(mixer.remove_channel(other), "removed");
    assert!(mixer.main_passed_through(), "until the next frame");
    assert_near(main_pixel(&gpu, &mixer), GREEN, "its picture still");
    assert!(!through(&gpu, &mut mixer), "nothing on Main now");
    assert_eq!(mixer.main_passes, 0);
    assert_near(main_pixel(&gpu, &mixer), [0.0; 4], "clear");
}

/// Resized, Main handed out is its own picture, at the new size, until a
/// frame is drawn; then the input's again, at that size, the same to the
/// bit as the pass draws it.
#[test]
fn a_resize_hands_out_mains_own_until_the_next_frame() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    assert!(through(&gpu, &mut mixer));
    mixer.resize(&gpu, W * 2, H * 2);
    assert!(!mixer.main_passed_through(), "not the picture at the old size");
    assert_eq!(main_size(&mixer), (W * 2, H * 2));

    assert!(through(&gpu, &mut mixer), "the input's, resized with Main");
    assert!(is_input(&mixer, UNDER));
    assert_eq!(main_size(&mixer), (W * 2, H * 2));
    let handed = main_bits(&gpu, &mixer);
    let card_pixels = read_texture(&gpu, mixer.composite_texture())
        .iter()
        .filter(|px| px[2] > 0.9)
        .count();
    assert!(card_pixels > 0, "the card is in it");
    mixer.pass_main_through = false;
    assert!(!through(&gpu, &mut mixer));
    assert_eq!(main_size(&mixer), (W * 2, H * 2));
    assert_eq!(handed, main_bits(&gpu, &mixer), "the same to the bit");
}

/// The layered mix draws Main as it always did — and with no input at any
/// level it draws Main clear: not what Main's own texture last held, a
/// picture from before an input was handed out as Main.
#[test]
fn the_layered_mix_is_drawn() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    // A third input: more than two are mixed each at its own level.
    let (_, green) = card_input(&gpu, &mut mixer, GREEN);
    let blue = mixer.channel(UNDER).expect("an input").uuid().to_string();
    // Main's own texture drawn green, then left as it is while the blue
    // input is handed out.
    mixer.pass_main_through = false;
    mixer.main_mut().take(MainSource::Channel(green), false);
    assert!(!through(&gpu, &mut mixer));
    assert_near(main_pixel(&gpu, &mixer), GREEN, "drawn");
    mixer.pass_main_through = true;
    mixer.main_mut().take(MainSource::Channel(blue), false);
    assert!(through(&gpu, &mut mixer));
    assert_near(main_pixel(&gpu, &mixer), BLUE, "handed out");

    for at in 0..mixer.channels().len() {
        mixer.channel_mut(at).expect("an input").opacity = 0.0;
    }
    mixer.set_mix_mode(MixMode::Layered);
    assert!(!through(&gpu, &mut mixer));
    assert!(!is_input(&mixer, UNDER), "Main's own");
    assert_near(main_pixel(&gpu, &mixer), [0.0; 4], "no input at any level: clear");

    mixer.channel_mut(UNDER).expect("an input").opacity = 1.0;
    assert!(!through(&gpu, &mut mixer));
    assert!(!is_input(&mixer, UNDER), "Main's own");
    assert_near(main_pixel(&gpu, &mixer), BLUE, "the one input at a level, drawn");
}
