//! A deck partly covered by opaque decks over it is drawn only where it
//! shows — its shader, its effects and its blend into the channel all
//! limited to those parts — and the channel's picture comes out the same to
//! the bit as with every deck drawn whole.
//!
//! Each case renders a channel with the parts on, then whole, then in parts
//! again (from textures left stale by the whole frame), and compares the raw
//! half-float composites. The pattern underneath is a fine rotated checker,
//! which does not change from frame to frame, so any pixel drawn from the
//! wrong place, or not drawn, shows.

use std::path::Path;
use std::sync::mpsc;

use varda::{
    audio::AudioData,
    channel::{Channel, DeckRenderFps},
    deck::{Deck, DeckRenderScale, Effect},
    isf::ISFShader,
    mixer::Mixer,
    modulation::{AnalyzerValues, AudioValues},
    renderer::context::GpuContext,
    renderer::tonemap::TonemapMode,
    BlendMode,
};

mod common;
use common::headless_gpu;

// Not a round size, so parts meet the frame at fractions of a pixel.
const W: u32 = 330;
const H: u32 = 186;

fn shader(name: &str) -> ISFShader {
    ISFShader::from_file(Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("shaders/{name}.fs")))
        .unwrap_or_else(|e| panic!("{name}.fs: {e}"))
}

fn effect(ctx: &GpuContext, name: &str) -> Effect {
    Effect::new(ctx, shader(name)).expect("effect")
}

/// A fine checker turned off the pixel grid, soft-edged so neighbouring
/// pixels differ; still from frame to frame.
fn checker(ctx: &GpuContext, grid: f32) -> Deck {
    let mut deck = Deck::new(ctx, shader("checkerboard"), W, H).expect("checker");
    deck.generator_params.set_float("grid_size", grid);
    deck.generator_params.set_float("rotation", 0.37);
    deck.generator_params.set_float("softness", 0.3);
    deck
}

fn solid(ctx: &GpuContext, color: [f32; 4]) -> Deck {
    Deck::new_solid_color(ctx, color, W, H).expect("solid")
}

/// A deck with crop then transform, as every new deck has, `set` applied.
fn framed(
    ctx: &GpuContext,
    mut deck: Deck,
    set: impl FnOnce(&mut varda::params::ShaderParams, &mut varda::params::ShaderParams),
) -> Deck {
    deck.add_effect(effect(ctx, "crop"));
    deck.add_effect(effect(ctx, "transform"));
    let (crop, transform) = deck.effects.split_at_mut(1);
    set(&mut crop[0].params, &mut transform[0].params);
    deck
}

fn mixer(ctx: &GpuContext) -> Mixer {
    let mut mixer = Mixer::new(ctx, W, H).expect("mixer");
    mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
    mixer
}

fn render(ctx: &GpuContext, mixer: &mut Mixer) {
    let audio = AudioData::default();
    let audio_values = AudioValues {
        sources: std::collections::HashMap::default(),
    };
    let analyzer_values = AnalyzerValues::default();
    let inputs = varda::mixer::FrameInputs {
        audio_data: &audio,
        audio_values: &audio_values,
        analyzer_values: &analyzer_values,
        beat_time: None,
        transport: None,
        free_run_time: Some(0.0),
    };
    mixer.render(ctx, &inputs, 60, &[]).expect("render");
}

/// The channel's composite as raw half-float bits, row by row.
fn bits(ctx: &GpuContext, channel: &Channel) -> Vec<u16> {
    let texture = &channel.composite_texture;
    let unpadded = W * 8;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("visible parts readback"),
        size: u64::from(padded * H),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = ctx
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded),
                rows_per_image: Some(H),
            },
        },
        texture.size(),
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
    let data = buffer.slice(..).get_mapped_range().expect("mapped");
    let mut out = Vec::with_capacity((W * H * 4) as usize);
    for row in 0..H {
        let base = (row * padded) as usize;
        out.extend(
            data[base..base + unpadded as usize]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b)),
        );
    }
    out
}

/// Where two composites differ, for the failure message.
fn differences(a: &[u16], b: &[u16]) -> String {
    let wrong: Vec<(u32, u32)> = a
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .enumerate()
        .filter(|(_, (x, y))| x != y)
        .map(|(i, _)| (i as u32 % W, i as u32 / W))
        .collect();
    format!(
        "{} pixels differ, first {:?}",
        wrong.len(),
        &wrong[..wrong.len().min(8)]
    )
}

/// Render `decks` (bottom first) on the first channel in parts, whole, and in
/// parts again, and require the same picture each time. Returns the share
/// of the frame each deck drew in the first frame.
fn same_either_way(ctx: &GpuContext, decks: Vec<Deck>, set: impl FnOnce(&mut Channel)) -> Vec<f32> {
    let mut mixer = mixer(ctx);
    let ch = mixer.channel_mut(0).unwrap();
    for deck in decks {
        ch.add_deck(deck);
    }
    set(ch);
    render(ctx, &mut mixer);
    let shares: Vec<f32> = mixer.channels()[0]
        .decks
        .iter()
        .map(|slot| slot.deck.drawn_share())
        .collect();
    let in_parts = bits(ctx, &mixer.channels()[0]);

    mixer.draw_visible_parts = false;
    render(ctx, &mut mixer);
    let whole = bits(ctx, &mixer.channels()[0]);
    assert!(
        mixer.channels()[0]
            .decks
            .iter()
            .all(|slot| slot.deck.drawn_share() >= 1.0),
        "switched off, every deck draws whole"
    );

    mixer.draw_visible_parts = true;
    render(ctx, &mut mixer);
    let again = bits(ctx, &mixer.channels()[0]);

    assert!(
        in_parts == whole,
        "in parts vs whole: {}",
        differences(&in_parts, &whole)
    );
    assert!(
        again == whole,
        "in parts again vs whole: {}",
        differences(&again, &whole)
    );
    shares
}

#[test]
fn a_background_behind_a_picture_in_picture_is_drawn_round_it_only() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let pip = framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), |_, t| {
        t.set_float("scale", 0.55);
        t.set_float("pos_x", 0.12);
        t.set_float("pos_y", -0.07);
    });
    let shares = same_either_way(&ctx, vec![checker(&ctx, 40.0), pip], |_| {});
    assert!(
        shares[0] < 0.8,
        "the background draws only round the picture: {shares:?}"
    );
    assert!(
        shares[0] > 0.6,
        "and all of what shows round it: {shares:?}"
    );
}

#[test]
fn a_zoomed_turned_background_under_a_cropped_bordered_picture() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    // The background through a pointwise filter, then zoomed and turned:
    // its shader draws the parts the transform reads from.
    let mut background = checker(&ctx, 25.0);
    background.add_effect(effect(&ctx, "invert"));
    background.add_effect(effect(&ctx, "transform"));
    let params = &mut background.effects[1].params;
    params.set_float("scale", 1.35);
    params.set_float("rotation", 0.2);
    params.set_float("pos_x", 0.05);
    let picture = framed(&ctx, solid(&ctx, [0.9, 0.6, 0.1, 1.0]), |c, t| {
        c.set_float("crop_left", 0.23);
        c.set_float("crop_right", 0.91);
        c.set_float("crop_bottom", 0.1);
        c.set_float("crop_top", 0.83);
        c.set_bool("border_mode", true);
        c.set_float("border_radius", 20.0);
        t.set_float("scale", 0.9);
        t.set_float("pos_x", -0.03);
        t.set_float("pos_y", 0.02);
    });
    let shares = same_either_way(&ctx, vec![background, picture], |_| {});
    assert!(
        shares[0] < 0.9,
        "the background is drawn in parts: {shares:?}"
    );
}

/// A flipped picture, cropped off to one side, covers the mirrored part of
/// the frame: the background is drawn where the picture's hole went, not
/// where it was before the flip.
#[test]
fn a_background_under_a_flipped_cropped_picture() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let picture = framed(&ctx, solid(&ctx, [0.9, 0.6, 0.1, 1.0]), |c, t| {
        c.set_float("crop_left", 0.23);
        c.set_float("crop_right", 0.6);
        t.set_float("scale", 0.9);
        t.set_float("pos_x", 0.04);
        t.set_bool("flip_x", true);
        t.set_bool("flip_y", true);
    });
    let shares = same_either_way(&ctx, vec![checker(&ctx, 30.0), picture], |_| {});
    assert!(
        shares[0] < 0.9,
        "the background is drawn in parts: {shares:?}"
    );
}

/// A flipped crop that keeps nothing hides nothing: its empty area, mirrored,
/// must not turn into the strip between its edges.
#[test]
fn a_flipped_crop_that_keeps_nothing_hides_nothing() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let picture = framed(&ctx, solid(&ctx, [0.9, 0.6, 0.1, 1.0]), |c, t| {
        c.set_float("crop_left", 0.8);
        c.set_float("crop_right", 0.2);
        t.set_bool("flip_x", true);
    });
    let shares = same_either_way(&ctx, vec![checker(&ctx, 30.0), picture], |_| {});
    assert!(
        shares[0] >= 1.0,
        "the background is drawn whole: {shares:?}"
    );
}

/// A flipped, zoomed background: its shader draws the parts the transform
/// reads from — mirrored — for what shows round a picture over it.
#[test]
fn a_flipped_zoomed_background_under_a_picture() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut background = checker(&ctx, 25.0);
    background.add_effect(effect(&ctx, "invert"));
    background.add_effect(effect(&ctx, "transform"));
    let params = &mut background.effects[1].params;
    params.set_float("scale", 1.35);
    params.set_float("pos_x", 0.21);
    params.set_bool("flip_x", true);
    let pip = framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), |_, t| {
        t.set_float("scale", 0.5);
        t.set_float("pos_x", -0.2);
    });
    let shares = same_either_way(&ctx, vec![background, pip], |_| {});
    assert!(
        shares[0] < 0.9,
        "the background is drawn in parts: {shares:?}"
    );
}

/// A circle hides only what is inside it: what is under it round the
/// circle, within the crop, is still drawn.
#[test]
fn a_background_under_a_circled_picture() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let picture = framed(&ctx, solid(&ctx, [0.9, 0.6, 0.1, 1.0]), |c, _| {
        c.set_float("crop_left", 0.1);
        c.set_float("crop_right", 0.9);
        c.set_bool("border_mode", true);
        c.set_bool("border_circle", true);
    });
    same_either_way(&ctx, vec![checker(&ctx, 25.0), picture], |_| {});
}

#[test]
fn layers_between_blend_as_before_where_they_show() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let pip = framed(&ctx, solid(&ctx, [0.0, 0.8, 0.3, 1.0]), |_, t| {
        t.set_float("scale", 0.4);
        t.set_float("pos_x", -0.2);
        t.set_float("pos_y", 0.15);
    });
    let decks = vec![
        checker(&ctx, 40.0),
        // Multiplied in: the composite shader, with a snapshot of below.
        checker(&ctx, 13.0),
        // Translucent over all of it.
        solid(&ctx, [1.0, 0.0, 0.5, 0.4]),
        // Added in: the blend unit.
        checker(&ctx, 7.0),
        pip,
    ];
    let shares = same_either_way(&ctx, decks, |ch| {
        ch.set_deck_blend_mode(1, BlendMode::Multiply);
        ch.set_deck_opacity(3, 0.7);
        ch.set_deck_blend_mode(3, BlendMode::Add);
    });
    assert!(
        shares[0] < 1.0 && shares[1] < 1.0 && shares[3] < 1.0,
        "{shares:?}"
    );
}

#[test]
fn a_layer_covered_by_two_pictures_side_by_side_shows_only_the_seam() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let left = framed(&ctx, solid(&ctx, [1.0, 0.0, 0.0, 1.0]), |c, _| {
        c.set_float("crop_right", 0.5);
    });
    let right = framed(&ctx, solid(&ctx, [0.0, 0.0, 1.0, 1.0]), |c, _| {
        c.set_float("crop_left", 0.5);
    });
    let shares = same_either_way(&ctx, vec![checker(&ctx, 40.0), left, right], |_| {});
    // The seam, and thin strips at the frame's edges the crops leave soft.
    assert!(
        shares[0] < 0.15,
        "only the seam of the background: {shares:?}"
    );
}

/// Two pictures that between them cover the frame hide what is under them
/// as one covering the frame alone would: it is not drawn at all.
#[test]
fn a_layer_covered_by_two_pictures_together_is_not_drawn() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    // Each a little larger than the frame, so no edge of theirs but the
    // inner ones falls inside it.
    let left = framed(&ctx, solid(&ctx, [1.0, 0.0, 0.0, 1.0]), |_, t| {
        t.set_float("scale", 1.2);
        t.set_float("pos_x", -0.3);
    });
    let right = framed(&ctx, solid(&ctx, [0.0, 0.0, 1.0, 1.0]), |_, t| {
        t.set_float("scale", 1.2);
        t.set_float("pos_x", 0.3);
    });
    let mut mixer = mixer(&ctx);
    let ch = mixer.channel_mut(0).unwrap();
    ch.add_deck(checker(&ctx, 40.0));
    ch.add_deck(left);
    ch.add_deck(right);
    render(&ctx, &mut mixer);
    let hidden = bits(&ctx, &mixer.channels()[0]);
    assert_eq!(
        mixer.channels()[0].active_deck_count,
        2,
        "the pattern is not rendered"
    );
    assert_eq!(
        mixer.channels()[0].composited_deck_count,
        2,
        "nor composited"
    );
    mixer.draw_visible_parts = false;
    render(&ctx, &mut mixer);
    let whole = bits(&ctx, &mixer.channels()[0]);
    assert!(hidden == whole, "{}", differences(&hidden, &whole));
}

/// An opaque picture fitted with bars at its sides covers only between the
/// bars, and zoomed only where it is zoomed to, less its soft edges; a
/// picture partly covered is still drawn whole, since it is kept from frame
/// to frame.
#[test]
fn an_opaque_picture_covers_where_it_is_fitted() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("portrait.png");
    image::RgbaImage::from_fn(100, 150, |x, y| {
        image::Rgba([(x * 2) as u8, (y * 255 / 150) as u8, 90, 255])
    })
    .save(&path)
    .unwrap();
    let fitted = Deck::new_from_image(&ctx, &path, W, H).expect("png");
    let shares = same_either_way(&ctx, vec![checker(&ctx, 40.0), fitted], |_| {});
    assert!(
        shares[0] < 0.7,
        "the background shows only in the bars: {shares:?}"
    );

    let scaled = framed(
        &ctx,
        Deck::new_from_image(&ctx, &path, W, H).expect("png"),
        // Zoomed in, so the edges where the bars were are resampled soft:
        // the left one lands 0.79 of the way across a pixel, the next pixel
        // wholly inside the edge still part soft.
        |_, t| {
            t.set_float("scale", 1.73);
            t.set_float("pos_x", 0.0547);
        },
    );
    let pip = framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), |_, t| {
        t.set_float("scale", 0.3);
    });
    let shares = same_either_way(&ctx, vec![checker(&ctx, 40.0), scaled, pip], |_| {});
    assert!(shares[0] < 0.9, "{shares:?}");
    assert!(
        shares[1] >= 1.0,
        "the picture itself is drawn whole: {shares:?}"
    );
}

/// A masked effect is still one pixel at a time: a layer under a picture,
/// its colour effect limited to an ellipse, is drawn in parts all the same.
#[test]
fn a_masked_effect_on_a_covered_layer() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut background = checker(&ctx, 40.0);
    let mut invert = effect(&ctx, "invert");
    invert.mask = Some(varda::engine::value::mask::EffectMask {
        shape: varda::engine::value::mask::MaskShape::Ellipse {
            x: 0.1,
            y: 0.2,
            width: 0.6,
            height: 0.7,
        },
        feather: 80.0,
        invert: false,
    });
    background.add_effect(invert);
    let pip = framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), |_, t| {
        t.set_float("scale", 0.5);
        t.set_float("pos_x", 0.1);
    });
    let shares = same_either_way(&ctx, vec![background, pip], |_| {});
    assert!(shares[0] < 0.9, "{shares:?}");
}

#[test]
fn a_layer_whose_effect_reads_round_about_is_drawn_whole() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut background = checker(&ctx, 40.0);
    background.add_effect(effect(&ctx, "blur"));
    let pip = framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), |_, t| {
        t.set_float("scale", 0.5);
    });
    let shares = same_either_way(&ctx, vec![background, pip], |_| {});
    assert!(shares[0] >= 1.0, "blur reads outside the part: {shares:?}");
}

#[test]
fn a_layer_at_half_resolution_is_drawn_in_parts_too() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut background = checker(&ctx, 40.0);
    background.set_render_scale(&ctx, DeckRenderScale::Half);
    let pip = framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), |_, t| {
        t.set_float("scale", 0.6);
        t.set_float("pos_x", 0.07);
    });
    let shares = same_either_way(&ctx, vec![background, pip], |_| {});
    assert!(shares[0] < 0.8, "{shares:?}");
}

/// A picture that moves uncovers background its last render left out: the
/// background renders that frame even when it would have been skipped.
#[test]
fn a_part_uncovered_by_a_moving_picture_is_drawn_at_once() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let pip_at = |pos_x: f32| {
        framed(&ctx, solid(&ctx, [0.1, 0.2, 0.9, 1.0]), move |_, t| {
            t.set_float("scale", 0.5);
            t.set_float("pos_x", pos_x);
        })
    };
    let mut moving = mixer(&ctx);
    let ch = moving.channel_mut(0).unwrap();
    ch.add_deck(checker(&ctx, 40.0));
    ch.add_deck(pip_at(-0.2));
    render(&ctx, &mut moving);
    // From now on the background would render once a second.
    let ch = moving.channel_mut(0).unwrap();
    ch.decks[0].render_fps = DeckRenderFps::Fixed(1);
    ch.decks[1].deck.effects[1].params.set_float("pos_x", 0.2);
    render(&ctx, &mut moving);
    let moved = bits(&ctx, &moving.channels()[0]);

    let mut reference = mixer(&ctx);
    reference.draw_visible_parts = false;
    let ch = reference.channel_mut(0).unwrap();
    ch.add_deck(checker(&ctx, 40.0));
    ch.add_deck(pip_at(0.2));
    render(&ctx, &mut reference);
    let expected = bits(&ctx, &reference.channels()[0]);
    assert!(moved == expected, "{}", differences(&moved, &expected));
}

/// How much a generator mostly covered costs, drawn in parts and whole —
/// a probe to read, not a pass/fail: `cargo test --test visible_parts
/// -- --ignored --nocapture`.
#[test]
#[ignore = "timing probe"]
fn probe_cost_of_a_covered_generator() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let (w, h) = (3840, 2160);
    let mut mixer = Mixer::new(&ctx, w, h).expect("mixer");
    let ch = mixer.channel_mut(0).unwrap();
    ch.add_deck(Deck::new(&ctx, shader("abstract_field"), w, h).expect("generator"));
    let mut pip = Deck::new_solid_color(&ctx, [0.1, 0.2, 0.9, 1.0], w, h).expect("pip");
    pip.add_effect(effect(&ctx, "transform"));
    pip.effects[0].params.set_float("scale", 0.85);
    ch.add_deck(pip);
    let time = |mixer: &mut Mixer, parts: bool| {
        mixer.draw_visible_parts = parts;
        for _ in 0..5 {
            render(&ctx, mixer);
        }
        let frames = 60;
        let start = std::time::Instant::now();
        for _ in 0..frames {
            render(&ctx, mixer);
            ctx.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .ok();
        }
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(frames)
    };
    let whole = time(&mut mixer, false);
    let parts = time(&mut mixer, true);
    let share = mixer.channels()[0].decks[0].deck.drawn_share();
    println!(
        "4K generator under an 85% picture: whole {whole:.2} ms/frame, in parts {parts:.2} ms/frame (drew {:.0}% of it)",
        share * 100.0
    );
}
