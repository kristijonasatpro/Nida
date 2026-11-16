//! A channel nothing of which shows — an overlay holding a lower third
//! that is off, a text hidden — is drawn clear once and then left: neither
//! drawn nor layered on Main until something of it shows
//! (`Mixer::skip_blank_channels`, `Channel::blank`). Whatever could show —
//! a lower third coming in or going out, an effect that draws on a clear
//! frame, a deck shown on its own, a quarantined deck's last frame — keeps
//! it drawn; and Main is the same with the switch off.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::{FrameInputs, MainSource, MixMode, Mixer, TonemapMode};
use crate::channel::{BlendMode, Channel, DeckAutoTransition, DeckRenderFps};
use crate::deck::lower_third::LowerThird;
use crate::deck::text::Text;
use crate::deck::{Deck, Effect, TapSource};
use crate::engine::value::lower_third::{
    LowerThirdBar, LowerThirdMotion, LowerThirdRow, LowerThirdSettings,
};
use crate::engine::value::text::TextSettings;
use crate::renderer::GpuContext;

pub(super) const W: u32 = 1280;
pub(super) const H: u32 = 720;
/// The input on Main: a blue card.
pub(super) const UNDER: usize = 0;
/// The overlay over it, holding the input looked at.
pub(super) const OVER: usize = 1;

pub(super) fn headless_gpu() -> GpuContext {
    GpuContext::new_headless().expect("headless GPU required for tests")
}

fn f16_to_f32(bits: u16) -> f32 {
    let exp = (bits >> 10) & 0x1f;
    let frac = bits & 0x3ff;
    let mag = if exp == 0 {
        f32::from(frac) * 2f32.powi(-24)
    } else {
        (1.0 + f32::from(frac) / 1024.0) * 2f32.powi(i32::from(exp) - 15)
    };
    if bits >> 15 == 1 {
        -mag
    } else {
        mag
    }
}

/// The texture's pixels, linear RGBA, row by row.
pub(super) fn read_texture(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (tex.width(), tex.height());
    let unpadded = w * 8;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("blank channel readback"),
        size: u64::from(padded * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture: tex,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(h),
            },
        },
        wgpu::Extent3d {
            width: w,
            height: h,
            depth_or_array_layers: 1,
        },
    );
    ctx.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    ctx.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().expect("map channel").expect("map ok");
    let mut out = Vec::with_capacity((w * h) as usize);
    {
        let data = buffer.slice(..).get_mapped_range().expect("mapped");
        let channel = |px: usize, i: usize| {
            f16_to_f32(u16::from_le_bytes([data[px + i * 2], data[px + i * 2 + 1]]))
        };
        for row in 0..h {
            let base = (row * padded) as usize;
            for col in 0..w {
                let px = base + (col as usize) * 8;
                out.push([
                    channel(px, 0),
                    channel(px, 1),
                    channel(px, 2),
                    channel(px, 3),
                ]);
            }
        }
    }
    buffer.unmap();
    out
}

/// The lower third's plate: sRGB 0.8, 0.2, 0.2 — linear ≈ 0.6, 0.03, 0.03.
fn is_plate(px: [f32; 4]) -> bool {
    px[0] > 0.4 && px[1] < 0.1 && px[2] < 0.1
}

/// How much of a lower third's plate shows.
pub(super) fn plate(frame: &[[f32; 4]]) -> usize {
    frame.iter().filter(|px| is_plate(**px)).count()
}

/// How much of white letters shows.
pub(super) fn letters(frame: &[[f32; 4]]) -> usize {
    frame
        .iter()
        .filter(|px| px[0] > 0.6 && px[1] > 0.6 && px[2] > 0.6)
        .count()
}

pub(super) fn settings(motion: LowerThirdMotion, secs: f32) -> LowerThirdSettings {
    let mut settings = LowerThirdSettings {
        rows: vec![
            LowerThirdRow {
                name: "Justina Prunskienė".into(),
                title: "Seimo narė".into(),
                ..LowerThirdRow::default()
            },
            LowerThirdRow {
                name: "Ona Šimkūnaitė".into(),
                title: "Moderatorė".into(),
                ..LowerThirdRow::default()
            },
        ],
        ..LowerThirdSettings::default()
    };
    let look = &mut settings.look;
    look.palette = 0;
    look.plate = [0.8, 0.2, 0.2];
    look.name_colour = [1.0, 1.0, 1.0];
    look.title_colour = [1.0, 1.0, 1.0];
    look.plate_opacity = 1.0;
    look.bar = LowerThirdBar::None;
    look.motion = motion;
    look.motion_secs = secs;
    settings
}

/// A lower third with a red plate, off, that cuts on and off. (Coming in and
/// going out are not off: `LowerThird::is_off`'s own test.)
pub(super) fn cut_lower_third(gpu: &GpuContext) -> Deck {
    Deck::new_lower_third(gpu, settings(LowerThirdMotion::Cut, 0.0), W, H)
        .expect("a lower third")
}

/// Main: a blue card in `UNDER` taken to it, and `deck`, alone in `OVER`,
/// an overlay on over it.
pub(super) fn mixer_with(gpu: &GpuContext, deck: Deck) -> Mixer {
    let mut mixer = Mixer::new(gpu, W, H).expect("mixer");
    mixer.set_tonemap_mode(&gpu.queue, TonemapMode::Bypass);
    mixer.set_mix_mode(MixMode::Program);
    let card = Deck::new_solid_color(gpu, [0.0, 0.0, 1.0, 1.0], W, H).expect("a card");
    mixer.channel_mut(UNDER).expect("a channel").add_deck(card);
    let under = mixer.channel(UNDER).expect("a channel").uuid().to_string();
    mixer.main_mut().take(MainSource::Channel(under), false);
    let overlay = mixer.channel_mut(OVER).expect("a second channel");
    overlay.overlay = true;
    overlay.add_deck(deck);
    let overlay = overlay.uuid().to_string();
    mixer.main_mut().set_overlay(&overlay, true, false);
    mixer
}

pub(super) fn over(mixer: &Mixer) -> &Channel {
    mixer.channel(OVER).expect("the overlay")
}

pub(super) fn over_mut(mixer: &mut Mixer) -> &mut Channel {
    mixer.channel_mut(OVER).expect("the overlay")
}

pub(super) fn lower(mixer: &mut Mixer) -> &mut LowerThird {
    over_mut(mixer).decks[0]
        .deck
        .lower_third_mut()
        .expect("a lower third")
}

pub(super) fn text(mixer: &mut Mixer) -> &mut Text {
    over_mut(mixer).decks[0].deck.text_mut().expect("a text")
}

/// One frame, as the app draws one: the taps bound, then the mix.
pub(super) fn render(gpu: &GpuContext, mixer: &mut Mixer) {
    let audio = crate::audio::AudioData::default();
    let audio_values = crate::modulation::AudioValues::default();
    let analyzer_values = crate::modulation::AnalyzerValues::default();
    let inputs = FrameInputs {
        audio_data: &audio,
        audio_values: &audio_values,
        analyzer_values: &analyzer_values,
        beat_time: None,
        transport: None,
        free_run_time: None,
    };
    mixer.prepare_taps(gpu);
    mixer.render(gpu, &inputs, 60, &[]).expect("mixer render");
}

/// How many of the next `frames` frames draw `OVER`.
pub(super) fn drawn(gpu: &GpuContext, mixer: &mut Mixer, frames: u32) -> u32 {
    let before = over(mixer).frames_drawn();
    for _ in 0..frames {
        render(gpu, mixer);
    }
    over(mixer).frames_drawn() - before
}

/// Main as the last frame left it.
pub(super) fn main_picture(gpu: &GpuContext, mixer: &Mixer) -> Vec<[f32; 4]> {
    read_texture(gpu, mixer.composite_texture())
}

/// `OVER`'s own picture as the last frame left it.
fn over_picture(gpu: &GpuContext, mixer: &Mixer) -> Vec<[f32; 4]> {
    read_texture(gpu, &over(mixer).composite_texture)
}

/// `OVER`'s first deck's frame as it was last drawn.
pub(super) fn deck_picture(gpu: &GpuContext, mixer: &Mixer) -> Vec<[f32; 4]> {
    read_texture(gpu, &over(mixer).decks[0].deck.texture)
}

/// Shown, and frames drawn till its plate is on Main: it comes on though
/// its input was left blank till then — ticked all the same.
pub(super) fn show_lower(gpu: &GpuContext, mixer: &mut Mixer) {
    lower(mixer).show(None);
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        render(gpu, mixer);
        if plate(&main_picture(gpu, mixer)) > 5_000 {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the lower third never came on Main");
}

/// Hidden: a cut, so off by the next frame.
pub(super) fn hide_lower(mixer: &mut Mixer) {
    lower(mixer).hide();
    // Past the cut's millisecond.
    std::thread::sleep(Duration::from_millis(5));
}

/// A lower third that is off: its input is drawn clear the first frame,
/// then left — neither drawn nor layered on Main, which shows the card.
#[test]
fn an_input_whose_lower_third_is_off_is_drawn_once_and_left() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "drawn clear once");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER], "not layered");
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "then left");
    let main = main_picture(&gpu, &mixer);
    assert_eq!(plate(&main), 0, "no plate on Main");
    let px = main[(H / 2 * W + W / 2) as usize];
    assert!(px[2] > 0.99 && px[0] < 0.01, "the card on Main: {px:?}");
}

/// Main with the switch on, then the next frame with it off: the same.
fn same_either_way(gpu: &GpuContext, mixer: &mut Mixer, what: &str) {
    render(gpu, mixer);
    let left = main_picture(gpu, mixer);
    mixer.skip_blank_channels = false;
    render(gpu, mixer);
    let every_frame = main_picture(gpu, mixer);
    mixer.skip_blank_channels = true;
    let worst = left
        .iter()
        .zip(&every_frame)
        .flat_map(|(a, b)| (0..4).map(move |c| (a[c] - b[c]).abs()))
        .fold(0.0, f32::max);
    assert!(worst < 1e-3, "{what}: Main differs by {worst}");
}

/// Left undrawn or drawn, the overlay leaves Main the same: off, shown,
/// and hidden again.
#[test]
fn main_is_the_same_with_a_blank_input_left_or_drawn() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    render(&gpu, &mut mixer);
    same_either_way(&gpu, &mut mixer, "off");
    show_lower(&gpu, &mut mixer);
    same_either_way(&gpu, &mut mixer, "shown");
    hide_lower(&mut mixer);
    render(&gpu, &mut mixer);
    same_either_way(&gpu, &mut mixer, "hidden again");
}

/// Shown, its input is drawn every frame and layered on Main, the plate
/// there; hidden, it is drawn clear the next frame — no deck put on its
/// picture, and none of the plate left on it or on Main — and then left.
#[test]
fn a_lower_third_shown_is_drawn_and_hidden_is_left_clear() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    render(&gpu, &mut mixer);
    show_lower(&gpu, &mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 3), 3, "shown: drawn every frame");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER, OVER], "layered");
    assert_eq!(over(&mixer).composited_deck_count, 1, "its deck put on it");
    assert!(plate(&main_picture(&gpu, &mixer)) > 5_000, "the plate on Main");
    hide_lower(&mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "hidden: drawn clear");
    assert_eq!(over(&mixer).composited_deck_count, 0, "no deck put on it");
    assert_eq!(plate(&over_picture(&gpu, &mixer)), 0, "none of the plate on it");
    assert_eq!(plate(&main_picture(&gpu, &mixer)), 0, "nor on Main");
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "then left");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER], "not layered");
    assert_eq!(plate(&main_picture(&gpu, &mixer)), 0, "still none on Main");
}

/// The switch off, an input showing nothing is drawn and layered every
/// frame, as before; on again, it is drawn clear once and left.
#[test]
fn with_the_switch_off_a_blank_input_is_drawn_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    mixer.skip_blank_channels = false;
    assert_eq!(drawn(&gpu, &mut mixer, 5), 5, "off: drawn every frame");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER, OVER], "and layered");
    mixer.skip_blank_channels = true;
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "on: drawn clear");
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "then left");
}

/// Its picture made anew — the show's size changed — a blank input is
/// drawn clear again before it is left.
#[test]
fn a_blank_input_resized_is_drawn_clear_again() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    assert_eq!(drawn(&gpu, &mut mixer, 2), 1, "drawn clear once, then left");
    mixer.resize(&gpu, W / 2, H / 2);
    assert_eq!(drawn(&gpu, &mut mixer, 4), 1, "drawn clear again, then left");
}

/// An input shown through a tap, not an overlay itself: drawn clear once
/// and left, the tap showing its clear picture; shown, its plate is on
/// Main through the tap, and gone when it is hidden.
#[test]
fn a_blank_input_shown_through_a_tap_is_left_clear() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    let over_uuid = over(&mixer).uuid().to_string();
    over_mut(&mut mixer).overlay = false;
    mixer.main_mut().set_overlay(&over_uuid, false, false);
    let tap = Deck::new_from_tap(&gpu, TapSource::Channel(over_uuid), "the lower third", W, H)
        .expect("a tap");
    mixer.channel_mut(UNDER).expect("a channel").add_deck(tap);
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "drawn clear once");
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "then left");
    assert_eq!(plate(&main_picture(&gpu, &mixer)), 0, "no plate on Main");
    show_lower(&gpu, &mut mixer);
    hide_lower(&mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "hidden: drawn clear");
    assert_eq!(plate(&main_picture(&gpu, &mixer)), 0, "the tap shows it clear");
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "then left");
    assert_eq!(plate(&main_picture(&gpu, &mixer)), 0, "still clear");
}

/// A muted deck beside the lower third is not drawn: the input is still
/// blank. Shown on its own somewhere (`Mixer::decks_shown_alone`) it is
/// drawn, and its input with it, every frame; the lower third shown on its
/// own, off, is still nothing.
#[test]
fn a_muted_deck_counts_only_when_shown_on_its_own() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    let green = Deck::new_solid_color(&gpu, [0.0, 1.0, 0.0, 1.0], W, H).expect("a card");
    let green_uuid = green.uuid().to_string();
    let index = over_mut(&mut mixer).add_deck(green);
    over_mut(&mut mixer).decks[index].mute = true;
    let lower_uuid = over(&mixer).decks[0].deck.uuid().to_string();
    assert_eq!(drawn(&gpu, &mut mixer, 5), 1, "the card muted: blank");
    mixer.decks_shown_alone = vec![lower_uuid];
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "the lower third alone: still nothing");
    mixer.decks_shown_alone = vec![green_uuid];
    assert_eq!(drawn(&gpu, &mut mixer, 4), 4, "the card alone: drawn");
}

/// An effect of `shader`'s (one of the app's own).
fn effect(gpu: &GpuContext, shader: &str) -> Effect {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("shaders")
        .join(format!("{shader}.fs"));
    let shader = crate::isf::ISFShader::from_file(path).expect("a shader");
    Effect::new(gpu, shader).expect("an effect")
}

/// How many of 4 frames after the first draw an input whose lower third
/// is off, with `on_deck` on the lower third and `on_channel` on the input.
fn drawn_with(gpu: &GpuContext, on_deck: Vec<Effect>, on_channel: Vec<Effect>) -> u32 {
    let mut deck = cut_lower_third(gpu);
    for effect in on_deck {
        deck.add_effect(effect);
    }
    let mut mixer = mixer_with(gpu, deck);
    for effect in on_channel {
        over_mut(&mut mixer).add_effect(effect);
    }
    render(gpu, &mut mixer);
    drawn(gpu, &mut mixer, 4)
}

/// Invert: it fills a clear frame.
pub(super) fn invert(gpu: &GpuContext) -> Effect {
    effect(gpu, "invert")
}

/// Invert turned off.
fn turned_off(gpu: &GpuContext) -> Effect {
    let mut effect = invert(gpu);
    effect.enabled = false;
    effect
}

/// Invert kept offline, as a show keeps one whose shader is gone.
fn offline(gpu: &GpuContext) -> Effect {
    let mut effect = invert(gpu);
    effect.set_offline("gone".into());
    effect
}

/// An effect that draws on a clear frame, on the lower third, keeps its
/// input drawn.
#[test]
fn invert_on_the_lower_third_keeps_its_input_drawn() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, vec![invert(&gpu)], Vec::new()), 4);
}

/// So does one on the input itself.
#[test]
fn invert_on_the_input_keeps_it_drawn() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, Vec::new(), vec![invert(&gpu)]), 4);
}

/// One turned off, on the lower third, changes nothing: the input is left.
#[test]
fn an_effect_turned_off_on_the_lower_third_changes_nothing() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, vec![turned_off(&gpu)], Vec::new()), 0);
}

/// Nor does one offline: it is not run.
#[test]
fn an_offline_effect_on_the_lower_third_changes_nothing() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, vec![offline(&gpu)], Vec::new()), 0);
}

/// One turned off on the input changes nothing.
#[test]
fn an_effect_turned_off_on_the_input_changes_nothing() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, Vec::new(), vec![turned_off(&gpu)]), 0);
}

/// Nor one offline on the input.
#[test]
fn an_offline_effect_on_the_input_changes_nothing() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, Vec::new(), vec![offline(&gpu)]), 0);
}

/// A crop and a Transform only place the picture: a clear frame stays
/// clear, and the input is left.
#[test]
fn a_crop_and_a_transform_keep_a_clear_frame_clear() {
    let gpu = headless_gpu();
    let framing = vec![effect(&gpu, "crop"), effect(&gpu, "transform")];
    assert_eq!(drawn_with(&gpu, framing, Vec::new()), 0);
}

/// A layer's mask with no shape does nothing: the input is left.
#[test]
fn a_mask_with_no_shape_keeps_a_clear_frame_clear() {
    let gpu = headless_gpu();
    assert_eq!(drawn_with(&gpu, vec![effect(&gpu, "cutout")], Vec::new()), 0);
}

/// A crop with its border on draws the border on a clear frame: drawn.
#[test]
fn a_crops_border_keeps_its_input_drawn() {
    let gpu = headless_gpu();
    let mut bordered = effect(&gpu, "crop");
    bordered.params.set_bool("border_mode", true);
    assert_eq!(drawn_with(&gpu, vec![bordered], Vec::new()), 4);
}

/// Big white letters with no shadow, hidden, shown or hidden at once.
/// (Fading out is not off: `Text::is_off`'s own test.)
pub(super) fn text_settings() -> TextSettings {
    let mut settings = TextSettings {
        text: "PERTRAUKA".into(),
        visible: false,
        fade_secs: 0.0,
        ..TextSettings::default()
    };
    settings.look.size = 0.12;
    settings.look.shadow_opacity = 0.0;
    settings
}

/// A text hidden and faded out is nothing: its input is drawn clear once
/// and left, no letters on Main. Shown, its letters are, drawn every frame.
#[test]
fn a_text_hidden_is_left_and_shown_is_drawn() {
    let gpu = headless_gpu();
    let mut settings = text_settings();
    let deck = Deck::new_text(&gpu, settings.clone(), W, H).expect("a text");
    let mut mixer = mixer_with(&gpu, deck);
    assert_eq!(drawn(&gpu, &mut mixer, 5), 1, "hidden: drawn clear once, then left");
    assert_eq!(letters(&main_picture(&gpu, &mixer)), 0, "no letters on Main");
    settings.visible = true;
    text(&mut mixer).set_settings(settings);
    assert!(text(&mut mixer).wait_drawn(Duration::from_secs(5)), "drawn");
    assert_eq!(drawn(&gpu, &mut mixer, 3), 3, "shown: drawn every frame");
    let shown = letters(&main_picture(&gpu, &mixer));
    assert!(shown > 500, "its letters on Main: {shown}");
}

/// Left blank, an overlay that multiplies what is under it keeps its layer
/// on Main: a clear picture multiplied may change what it is over.
#[test]
fn a_blank_overlay_that_multiplies_keeps_its_layer() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    over_mut(&mut mixer).blend_mode = BlendMode::Multiply;
    assert_eq!(drawn(&gpu, &mut mixer, 5), 1, "left all the same");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER, OVER], "layered");
}

/// So does one over nothing (Main black): the bottom layer is the one that
/// clears Main.
#[test]
fn a_blank_overlay_over_black_keeps_its_layer() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    mixer.main_mut().take(MainSource::Black, false);
    assert_eq!(drawn(&gpu, &mut mixer, 5), 1, "left all the same");
    assert_eq!(mixer.main_layer_channels(), vec![OVER], "layered");
}

/// A deck in a timed sequence (auto-transition on) may move on by itself:
/// its input is drawn every frame.
#[test]
fn an_input_in_a_timed_sequence_is_drawn() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    over_mut(&mut mixer).decks[0].auto_transition = Some(DeckAutoTransition {
        enabled: true,
        ..DeckAutoTransition::new()
    });
    render(&gpu, &mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 4), 4);
}

/// A deck kept from the GPU after a fault holds its last frame — the plate
/// — whatever its lower third does: its input is drawn every frame, and
/// layered.
#[test]
fn a_quarantined_deck_keeps_its_input_drawn() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    render(&gpu, &mut mixer);
    show_lower(&gpu, &mut mixer);
    over_mut(&mut mixer).decks[0].deck.fault("a test's fault");
    hide_lower(&mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 4), 4, "drawn every frame");
    assert_eq!(mixer.main_layer_channels(), vec![UNDER, OVER], "layered");
}

/// An analyzer reading the deck's frames keeps its input from being taken
/// for blank — a clear frame is a reading too; stopped, it is blank again,
/// and left as it was. Asked of the channel (`Channel::blank`), not drawn
/// with the analyzer running: its copy of a deck's frame is a GPU fault
/// today (an eight-bit buffer for a half-float frame), which quarantines
/// the deck — never blank either, so a drawn test would pass for that.
#[test]
fn an_analyzer_on_the_deck_keeps_its_input_drawn() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    assert_eq!(drawn(&gpu, &mut mixer, 2), 1, "drawn clear once, then left");
    let running = over_mut(&mut mixer).decks[0].deck.analyzers.request(
        "brightness",
        &crate::analyzer::default_registry(),
        &serde_json::Value::Null,
    );
    assert!(running.is_some(), "an analyzer running");
    assert!(!over(&mixer).blank(&mixer.modulation), "read: not blank");
    over_mut(&mut mixer).decks[0]
        .deck
        .analyzers
        .release("brightness");
    assert!(over(&mixer).blank(&mixer.modulation), "stopped: blank again");
    assert_eq!(drawn(&gpu, &mut mixer, 5), 0, "and left");
    let deck = &over(&mixer).decks[0].deck;
    assert!(deck.gpu_error().is_none(), "never faulted");
}

/// An input showing itself (a tap of its own, muted) has its picture of the
/// frame before swapped in each frame: it is drawn clear every frame,
/// never left, and its picture is clear after each.
#[test]
fn an_input_that_shows_itself_is_drawn_clear_every_frame() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    let over_uuid = over(&mixer).uuid().to_string();
    let tap = Deck::new_from_tap(&gpu, TapSource::Channel(over_uuid), "itself", W, H)
        .expect("a tap");
    let index = over_mut(&mut mixer).add_deck(tap);
    over_mut(&mut mixer).decks[index].mute = true;
    render(&gpu, &mut mixer);
    show_lower(&gpu, &mut mixer);
    for _ in 0..3 {
        render(&gpu, &mut mixer);
    }
    hide_lower(&mut mixer);
    for frame in 0..4 {
        assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "frame {frame}: drawn");
        let left = plate(&over_picture(&gpu, &mixer));
        assert_eq!(left, 0, "frame {frame}: none of the plate on it");
    }
}

/// A deck drawn at a low rate of its own holds its frame between turns:
/// the frame that leaves its input clear draws it whatever its turn, so
/// none of the plate is held — and then it is left.
#[test]
fn a_deck_at_a_low_rate_is_drawn_clear_whatever_its_turn() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    over_mut(&mut mixer).decks[0].render_fps = DeckRenderFps::Fixed(1);
    render(&gpu, &mut mixer);
    show_lower(&gpu, &mut mixer);
    // Its next turn a second's frames away.
    over_mut(&mut mixer).decks[0].skip_counter = 1;
    hide_lower(&mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "hidden: drawn clear");
    assert_eq!(plate(&deck_picture(&gpu, &mixer)), 0, "its frame drawn clear");
    assert_eq!(drawn(&gpu, &mut mixer, 4), 0, "then left");
}

/// A deck muted with a name on air keeps that frame: shown on its own
/// later, off by then, it is drawn clear before its input is left.
#[test]
fn a_muted_deck_shown_on_its_own_later_is_drawn_clear_first() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    render(&gpu, &mut mixer);
    show_lower(&gpu, &mut mixer);
    assert!(plate(&deck_picture(&gpu, &mixer)) > 5_000, "the plate in its frame");
    over_mut(&mut mixer).decks[0].mute = true;
    render(&gpu, &mut mixer);
    hide_lower(&mut mixer);
    assert_eq!(drawn(&gpu, &mut mixer, 3), 0, "muted: left");
    assert!(plate(&deck_picture(&gpu, &mixer)) > 5_000, "the plate still in its frame");
    let lower_uuid = over(&mixer).decks[0].deck.uuid().to_string();
    mixer.decks_shown_alone = vec![lower_uuid];
    assert_eq!(drawn(&gpu, &mut mixer, 1), 1, "shown on its own: drawn clear");
    assert_eq!(plate(&deck_picture(&gpu, &mixer)), 0, "its frame clear");
    assert_eq!(drawn(&gpu, &mut mixer, 3), 0, "then left");
}

/// Left, an input counts no deck drawn and took no time.
#[test]
fn a_blank_input_left_counts_no_deck_drawn() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    render(&gpu, &mut mixer);
    assert_eq!(over(&mixer).active_deck_count, 1, "drawn clear: its deck drawn");
    over_mut(&mut mixer).render_time_ms = 5.0;
    assert_eq!(drawn(&gpu, &mut mixer, 1), 0, "left");
    assert_eq!(over(&mixer).active_deck_count, 0, "no deck drawn");
    assert!(over(&mixer).render_time_ms <= 0.0, "no time");
}

/// Left, its decks cost nothing: the frame's budget (`decks_over_budget`)
/// is the other decks'.
#[test]
fn a_blank_input_left_costs_its_decks_nothing() {
    let gpu = headless_gpu();
    let mut mixer = mixer_with(&gpu, cut_lower_third(&gpu));
    render(&gpu, &mut mixer);
    let slot = &mut over_mut(&mut mixer).decks[0];
    slot.render_cost_us = 5.0;
    slot.gpu_render_cost_us = 5.0;
    assert_eq!(drawn(&gpu, &mut mixer, 1), 0, "left");
    let slot = &over(&mixer).decks[0];
    assert!(
        slot.render_cost_us <= 0.0 && slot.gpu_render_cost_us <= 0.0,
        "no cost"
    );
}
