//! Main's bottom layer and the layers the blend unit composites over it —
//! Normal, Add and Screen overlays, a dissolve's second picture — go out
//! in one render pass, which loads and stores Main once
//! (`Mixer::merge_main_passes`). The picture is the same to the bit as with
//! a pass a layer, and is what its layers blended bottom first make; an
//! overlay whose blend the shader computes ends the pass, and the bottom
//! layer's pass is the one that clears Main.

use super::blank_tests::{headless_gpu, read_texture, render};
use super::{MainSource, MixMode, Mixer, TonemapMode};
use crate::channel::BlendMode;
use crate::deck::Deck;
use crate::renderer::GpuContext;

pub(super) const W: u32 = 64;
pub(super) const H: u32 = 36;
/// The input on Main: a blue card.
pub(super) const UNDER: usize = 0;
pub(super) const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// An overlay: a card of this colour, blended so, at this level.
pub(super) type Overlay = ([f32; 4], BlendMode, f32);

/// Main's pixels, bit for bit, and the passes its blend-unit layers took.
type Drawn = (Vec<[u32; 4]>, u32);

/// A new input holding a card of `colour`: its index and UUID.
pub(super) fn card_input(gpu: &GpuContext, mixer: &mut Mixer, colour: [f32; 4]) -> (usize, String) {
    let at = mixer.add_channel(gpu, W, H).expect("an input");
    let card = Deck::new_solid_color(gpu, colour, W, H).expect("a card");
    let channel = mixer.channel_mut(at).expect("the input");
    channel.add_deck(card);
    (at, channel.uuid().to_string())
}

/// Main: a blue card in `UNDER` taken to it — or nothing, for `None` — and
/// over it an overlay for each of `over`, on. The overlays' indices.
pub(super) fn mixer_of(
    gpu: &GpuContext,
    under: Option<[f32; 4]>,
    over: &[Overlay],
) -> (Mixer, Vec<usize>) {
    let mut mixer = Mixer::new(gpu, W, H).expect("mixer");
    mixer.set_tonemap_mode(&gpu.queue, TonemapMode::Bypass);
    mixer.set_mix_mode(MixMode::Program);
    if let Some(colour) = under {
        let card = Deck::new_solid_color(gpu, colour, W, H).expect("a card");
        mixer.channel_mut(UNDER).expect("an input").add_deck(card);
        let under = mixer.channel(UNDER).expect("an input").uuid().to_string();
        mixer.main_mut().take(MainSource::Channel(under), false);
    }
    let mut overlays = Vec::new();
    for &(colour, blend, level) in over {
        let (at, uuid) = card_input(gpu, &mut mixer, colour);
        let channel = mixer.channel_mut(at).expect("the overlay");
        channel.overlay = true;
        channel.blend_mode = blend;
        channel.opacity = level;
        mixer.main_mut().set_overlay(&uuid, true, false);
        overlays.push(at);
    }
    (mixer, overlays)
}

/// Main's pixels as the last frame left them, bit for bit.
pub(super) fn main_bits(gpu: &GpuContext, mixer: &Mixer) -> Vec<[u32; 4]> {
    read_texture(gpu, mixer.composite_texture())
        .iter()
        .map(|px| px.map(f32::to_bits))
        .collect()
}

/// Main drawn with its blend-unit layers in one pass, then each in a pass
/// of its own: both pictures, bit for bit, and the passes each took.
fn merged_and_apart(gpu: &GpuContext, mixer: &mut Mixer) -> (Drawn, Drawn) {
    assert!(mixer.merge_main_passes, "on to begin with");
    render(gpu, mixer);
    render(gpu, mixer);
    let merged = (main_bits(gpu, mixer), mixer.main_passes);
    mixer.merge_main_passes = false;
    render(gpu, mixer);
    let apart = (main_bits(gpu, mixer), mixer.main_passes);
    mixer.merge_main_passes = true;
    (merged, apart)
}

/// The middle pixel of an input's own picture (premultiplied, as Main is
/// given it).
pub(super) fn input_pixel(gpu: &GpuContext, mixer: &Mixer, at: usize) -> [f32; 4] {
    let channel = mixer.channel(at).expect("the input");
    read_texture(gpu, &channel.composite_texture)[middle()]
}

fn middle() -> usize {
    (H / 2 * W + W / 2) as usize
}

/// How a layer goes over what is under it.
#[derive(Clone, Copy)]
enum How {
    Over,
    Add,
    Screen,
    Sum,
}

/// `src` (premultiplied) at `level` blended onto `dst`, as the blend unit
/// does it (see `renderer::blit`'s blend states).
fn blended(dst: [f32; 4], src: [f32; 4], level: f32, how: How) -> [f32; 4] {
    let s = src.map(|c| c * level);
    let over = |c: usize| s[c] + dst[c] * (1.0 - s[3]);
    let colour = |c: usize| match how {
        How::Over => over(c),
        How::Add | How::Sum => s[c] + dst[c],
        How::Screen => s[c] + dst[c] * (1.0 - s[c]),
    };
    let coverage = match how {
        How::Sum => s[3] + dst[3],
        _ => over(3),
    };
    [colour(0), colour(1), colour(2), coverage]
}

pub(super) fn assert_near(got: [f32; 4], want: [f32; 4], what: &str) {
    let worst = (0..4).map(|c| (got[c] - want[c]).abs()).fold(0.0, f32::max);
    assert!(worst < 5e-3, "{what}: {got:?}, not {want:?}");
}

/// Main's middle pixel now.
pub(super) fn main_pixel(gpu: &GpuContext, mixer: &Mixer) -> [f32; 4] {
    read_texture(gpu, mixer.composite_texture())[middle()]
}

const THREE_OVER: [Overlay; 3] = [
    ([1.0, 0.5, 0.0, 0.6], BlendMode::Normal, 0.7),
    ([0.2, 0.9, 0.1, 0.5], BlendMode::Add, 0.8),
    ([0.6, 0.1, 0.3, 0.8], BlendMode::Screen, 0.9),
];

/// A card with a Normal, an Add and a Screen overlay over it: one pass for
/// the four, the same picture to the bit as a pass each.
#[test]
fn mains_layers_in_one_pass_draw_the_same_picture() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &THREE_OVER);
    let ((merged, merged_passes), (apart, apart_passes)) = merged_and_apart(&gpu, &mut mixer);
    assert_eq!(apart_passes, 4, "a pass a layer");
    assert_eq!(merged_passes, 1, "one pass for all four");
    assert!(merged == apart, "the same picture, bit for bit");
}

/// And that picture is its layers blended bottom first, each as its blend
/// says, at its level — in one pass and in a pass each.
#[test]
fn main_is_its_layers_blended_bottom_first() {
    let gpu = headless_gpu();
    let (mut mixer, overlays) = mixer_of(&gpu, Some(BLUE), &THREE_OVER);
    render(&gpu, &mut mixer);
    render(&gpu, &mut mixer);
    let mut want = blended([0.0; 4], input_pixel(&gpu, &mixer, UNDER), 1.0, How::Over);
    assert_near(want, BLUE, "the card alone");
    for (&at, &(_, blend, level)) in overlays.iter().zip(&THREE_OVER) {
        let how = match blend {
            BlendMode::Add => How::Add,
            BlendMode::Screen => How::Screen,
            _ => How::Over,
        };
        let before = want;
        want = blended(want, input_pixel(&gpu, &mixer, at), level, how);
        let moved = (0..4).map(|c| (want[c] - before[c]).abs()).fold(0.0, f32::max);
        assert!(moved > 0.05, "each overlay changes the picture: {moved}");
    }
    assert_near(main_pixel(&gpu, &mixer), want, "in one pass");
    mixer.merge_main_passes = false;
    render(&gpu, &mut mixer);
    assert_near(main_pixel(&gpu, &mixer), want, "a pass each");
}

/// An overlay whose blend the shader computes reads Main so far: the pass
/// ends before it, and starts again after.
#[test]
fn a_shader_blend_ends_mains_pass_and_the_picture_stays_the_same() {
    let gpu = headless_gpu();
    let over = [
        ([0.1, 0.2, 0.9, 0.5], BlendMode::Normal, 0.8),
        ([0.5, 0.9, 0.2, 0.8], BlendMode::Multiply, 0.8),
        ([0.4, 0.0, 0.3, 0.5], BlendMode::Add, 0.6),
        ([0.0, 0.7, 0.2, 0.4], BlendMode::Screen, 0.9),
    ];
    let (mut mixer, overlays) = mixer_of(&gpu, Some([0.9, 0.8, 0.1, 1.0]), &over);
    let ((merged, merged_passes), (apart, apart_passes)) = merged_and_apart(&gpu, &mut mixer);
    assert_eq!(apart_passes, 4, "the blend unit's four, a pass each");
    assert_eq!(merged_passes, 2, "two before the Multiply, two after");
    assert!(merged == apart, "the same picture, bit for bit");
    // The Multiply is in it: without that overlay Main is another picture.
    mixer
        .channel_mut(overlays[1])
        .expect("the Multiply")
        .opacity = 0.0;
    render(&gpu, &mut mixer);
    assert!(main_bits(&gpu, &mixer) != merged, "the Multiply shows");
    assert_eq!(mixer.main_passes, 1, "and without it, one pass");
}

/// Nothing on Main and one translucent overlay: it is Main's bottom layer,
/// drawn over a clear every frame — the picture does not build up frame
/// after frame — in one pass or a pass each.
#[test]
fn the_bottom_layer_is_drawn_over_a_clear() {
    let gpu = headless_gpu();
    let over = [([1.0, 0.5, 0.25, 0.5], BlendMode::Normal, 0.8)];
    let (mut mixer, overlays) = mixer_of(&gpu, None, &over);
    for merge in [true, false] {
        mixer.merge_main_passes = merge;
        for frame in 0..4 {
            render(&gpu, &mut mixer);
            let want = blended([0.0; 4], input_pixel(&gpu, &mixer, overlays[0]), 0.8, How::Over);
            assert!(want[3] > 0.3 && want[3] < 0.5, "translucent: {want:?}");
            assert_near(
                main_pixel(&gpu, &mixer),
                want,
                &format!("one pass {merge}, frame {frame}"),
            );
            assert_eq!(mixer.main_passes, 1);
        }
    }
}

/// Nothing on Main at all: cleared, no pass of the blend unit's.
#[test]
fn nothing_on_main_takes_no_layer_pass() {
    let gpu = headless_gpu();
    let (mut mixer, _) = mixer_of(&gpu, Some(BLUE), &[]);
    // The card alone would be handed out as it is, with no pass at all
    // (`main_through_tests`).
    mixer.pass_main_through = false;
    render(&gpu, &mut mixer);
    assert_eq!(mixer.main_passes, 1, "the card");
    mixer.main_mut().take(MainSource::Black, false);
    render(&gpu, &mut mixer);
    assert_eq!(mixer.main_passes, 0);
    assert_near(main_pixel(&gpu, &mixer), [0.0; 4], "clear");
}

/// A dissolve held part way — what Main is leaving, what it is taking
/// added to it, and an overlay over both — is one pass too, the same
/// picture to the bit as three, and the weighted sum it should be.
#[test]
fn a_dissolve_and_what_is_over_it_go_in_one_pass() {
    let gpu = headless_gpu();
    let over = [([1.0, 0.5, 0.0, 0.5], BlendMode::Normal, 0.8)];
    let (mut mixer, overlays) = mixer_of(&gpu, Some(BLUE), &over);
    let (to, to_uuid) = card_input(&gpu, &mut mixer, [0.0, 1.0, 0.0, 1.0]);
    render(&gpu, &mut mixer);
    // So long a fade that a frame's time is nothing of it: held where put.
    mixer.main_mut().fade_secs = 1.0e12;
    mixer.main_mut().take(MainSource::Channel(to_uuid), true);
    let fade = mixer.main_mut().fade.as_mut().expect("a fade running");
    fade.progress = 0.4;
    let ((merged, merged_passes), (apart, apart_passes)) = merged_and_apart(&gpu, &mut mixer);
    let fade = mixer.main().fade.as_ref().expect("the fade still running");
    assert!((fade.shown() - 0.4).abs() < 1e-6, "held: {}", fade.shown());
    assert_eq!(apart_passes, 3, "a pass a layer");
    assert_eq!(merged_passes, 1, "one pass for the three");
    assert!(merged == apart, "the same picture, bit for bit");
    let mut want = blended([0.0; 4], input_pixel(&gpu, &mixer, UNDER), 0.6, How::Over);
    want = blended(want, input_pixel(&gpu, &mixer, to), 0.4, How::Sum);
    assert_near(want, [0.0, 0.4, 0.6, 1.0], "the dissolve alone");
    want = blended(want, input_pixel(&gpu, &mixer, overlays[0]), 0.8, How::Over);
    assert_near(main_pixel(&gpu, &mixer), want, "the dissolve and the overlay");
}
