//! An effect through a mask: it shows inside the mask's shape and not
//! outside — or the other way round, inverted — with a fade across a
//! feathered edge. A white layer through Invert (black) shows where.

use std::path::Path;
use std::sync::mpsc;

use varda::{
    audio::AudioData,
    deck::{Deck, Effect},
    engine::value::mask::{EffectMask, MaskShape},
    isf::ISFShader,
    mixer::Mixer,
    modulation::{AnalyzerValues, AudioValues},
    renderer::context::GpuContext,
    renderer::tonemap::TonemapMode,
};

mod common;
use common::headless_gpu;

const W: u32 = 160;
const H: u32 = 90;

fn rect(x: f32, y: f32, width: f32, height: f32) -> MaskShape {
    MaskShape::Rectangle {
        x,
        y,
        width,
        height,
    }
}

/// White through Invert, masked; the first channel's composite, one byte of
/// red per pixel (0 black: the effect; 255 white: not).
fn masked(ctx: &GpuContext, mask: Option<EffectMask>) -> Vec<u8> {
    let mut mixer = Mixer::new(ctx, W, H).expect("mixer");
    mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
    let mut deck = Deck::new_solid_color(ctx, [1.0, 1.0, 1.0, 1.0], W, H).expect("white");
    let shader =
        ISFShader::from_file(Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders/invert.fs"))
            .expect("invert.fs");
    let mut invert = Effect::new(ctx, shader).expect("invert");
    invert.mask = mask;
    deck.add_effect(invert);
    mixer.channel_mut(0).unwrap().add_deck(deck);
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
    red_channel(ctx, &mixer.channels()[0].composite_texture)
}

fn red_channel(ctx: &GpuContext, texture: &wgpu::Texture) -> Vec<u8> {
    let unpadded = W * 8;
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
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
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((W * H) as usize);
    for row in 0..H {
        for col in 0..W {
            let at = (row * padded + col * 8) as usize;
            let red = half::f16::from_le_bytes([data[at], data[at + 1]]).to_f32();
            out.push((red.clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    out
}

fn at(pixels: &[u8], x: u32, y: u32) -> u8 {
    pixels[(y * W + x) as usize]
}

#[test]
fn without_a_mask_the_effect_is_everywhere() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let px = masked(&ctx, None);
    assert!(px.iter().all(|&v| v == 0), "all black");
}

#[test]
fn a_rectangle_shows_the_effect_inside_only() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let px = masked(
        &ctx,
        Some(EffectMask {
            shape: rect(0.25, 0.25, 0.5, 0.5),
            feather: 0.0,
            invert: false,
        }),
    );
    assert_eq!(at(&px, 80, 45), 0, "inside: the effect");
    assert_eq!(at(&px, 5, 5), 255, "outside: as it was");
    assert_eq!(at(&px, 41, 45), 0, "just inside the left edge");
    assert_eq!(at(&px, 38, 45), 255, "just outside it");
}

#[test]
fn inverted_the_effect_is_outside_only() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let px = masked(
        &ctx,
        Some(EffectMask {
            shape: rect(0.25, 0.25, 0.5, 0.5),
            feather: 0.0,
            invert: true,
        }),
    );
    assert_eq!(at(&px, 80, 45), 255);
    assert_eq!(at(&px, 5, 5), 0);
}

#[test]
fn a_feathered_edge_fades_across_the_shape_edge() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    // A 240-pixel feather at 1080 lines: 20 pixels here, 10 each side.
    let px = masked(
        &ctx,
        Some(EffectMask {
            shape: rect(0.25, 0.0, 0.5, 1.0),
            feather: 240.0,
            invert: false,
        }),
    );
    let row: Vec<u8> = (25..56).map(|x| at(&px, x, 45)).collect();
    assert!(at(&px, 25, 45) > 250 && at(&px, 55, 45) < 5, "{row:?}");
    let edge = at(&px, 40, 45);
    assert!((100..=155).contains(&edge), "half way at the edge: {row:?}");
    assert!(
        row.windows(2).all(|w| w[1] <= w[0]),
        "falling steadily: {row:?}"
    );
}

#[test]
fn an_ellipse_leaves_the_corners_of_its_rectangle() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let px = masked(
        &ctx,
        Some(EffectMask {
            shape: MaskShape::Ellipse {
                x: 0.25,
                y: 0.25,
                width: 0.5,
                height: 0.5,
            },
            feather: 0.0,
            invert: false,
        }),
    );
    assert_eq!(at(&px, 80, 45), 0, "the middle");
    assert_eq!(at(&px, 42, 24), 255, "a corner of its rectangle is outside");
    assert_eq!(at(&px, 44, 45), 0, "near its side, inside");
}

#[test]
fn a_drawn_outline_shows_the_effect_inside_it() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    // A triangle: top middle, bottom right, bottom left.
    let px = masked(
        &ctx,
        Some(EffectMask {
            shape: MaskShape::Path {
                points: vec![[0.5, 0.1], [0.9, 0.9], [0.1, 0.9]],
            },
            feather: 0.0,
            invert: false,
        }),
    );
    assert_eq!(at(&px, 80, 60), 0, "inside the triangle");
    assert_eq!(at(&px, 20, 20), 255, "above its left side");
    assert_eq!(at(&px, 140, 20), 255, "above its right side");
}
