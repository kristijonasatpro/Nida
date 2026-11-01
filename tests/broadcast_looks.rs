//! The Broadcast backgrounds (`shaders/broadcast_*.fs`) at a real size: each
//! renders, keeps to the brightness an LED wall and a stream can take, moves
//! at Speed 1 and stands still at 0, and takes every palette.
//!
//! `NIDA_SAVE_LOOKS=<dir>` also writes each look as a PNG, to be looked at.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use varda::{
    audio::AudioData,
    deck::Deck,
    isf::ISFShader,
    mixer::Mixer,
    modulation::{AnalyzerValues, AudioValues},
    renderer::context::GpuContext,
    renderer::tonemap::TonemapMode,
};

mod common;
use common::headless_gpu;

const W: u32 = 1280;
const H: u32 = 720;

fn looks() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("shaders dir")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "fs")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("broadcast_"))
        })
        .collect();
    paths.sort();
    paths
}

fn f16_to_f32(bits: u16) -> f32 {
    let sign = if (bits >> 15) & 1 == 1 { -1.0 } else { 1.0 };
    let exp = (bits >> 10) & 0x1f;
    let frac = bits & 0x3ff;
    let mag = if exp == 0 {
        f32::from(frac) * 2f32.powi(-24)
    } else if exp == 0x1f {
        if frac == 0 {
            f32::INFINITY
        } else {
            f32::NAN
        }
    } else {
        (1.0 + f32::from(frac) / 1024.0) * 2f32.powi(i32::from(exp) - 15)
    };
    sign * mag
}

fn read_texture(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<[f32; 4]> {
    let unpadded = W * 8;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("broadcast looks readback"),
        size: u64::from(padded * H),
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
                rows_per_image: Some(H),
            },
        },
        wgpu::Extent3d {
            width: W,
            height: H,
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
    let mut out = Vec::with_capacity((W * H) as usize);
    {
        let data = buffer.slice(..).get_mapped_range().expect("mapped");
        let channel = |px: usize, i: usize| {
            f16_to_f32(u16::from_le_bytes([data[px + i * 2], data[px + i * 2 + 1]]))
        };
        for row in 0..H {
            let base = (row * padded) as usize;
            for col in 0..W {
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

/// The mean difference in brightness between two frames.
fn mean_diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (luma(*x) - luma(*y)).abs())
        .sum::<f32>()
        / a.len() as f32
}

/// Draw one frame of `mixer` and read it back.
fn draw(ctx: &GpuContext, mixer: &mut Mixer) -> Vec<[f32; 4]> {
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
        free_run_time: Some(1.0),
    };
    mixer.render(ctx, &inputs, 60, &[]).expect("render");
    read_texture(ctx, mixer.composite_texture())
}

/// One frame of `path` at phase `phase`, its parameters set by `setup`.
fn render(
    ctx: &GpuContext,
    path: &Path,
    phase: f32,
    setup: impl FnOnce(&mut varda::params::ShaderParams),
) -> Vec<[f32; 4]> {
    let shader = ISFShader::from_file(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    // The phase reached in the one frame drawn: Speed set to take it there
    // (the engine adds a frame's time × Speed × the look's scale).
    let scale = shader
        .metadata
        .phase_inputs
        .as_ref()
        .and_then(|inputs| inputs.first())
        .map_or(1.0, |input| input.scale);
    let mut deck = Deck::new(ctx, shader, W, H).expect("deck");
    setup(&mut deck.generator_params);
    deck.generator_params
        .set_float("speed", phase * 60.0 / scale);
    let mut mixer = Mixer::new(ctx, W, H).expect("mixer");
    mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
    mixer.channel_mut(0).expect("a channel").add_deck(deck);
    draw(ctx, &mut mixer)
}

fn srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Brightness as the eye sees it (sRGB-encoded luma), 0..1.
fn luma(px: [f32; 4]) -> f32 {
    0.2126 * srgb(px[0]) + 0.7152 * srgb(px[1]) + 0.0722 * srgb(px[2])
}

fn save(pixels: &[[f32; 4]], name: &str) {
    let Some(dir) = std::env::var_os("NIDA_SAVE_LOOKS") else {
        return;
    };
    let mut img = image::RgbImage::new(W, H);
    for (i, px) in pixels.iter().enumerate() {
        let (x, y) = (i as u32 % W, i as u32 / W);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let to8 = |c: f32| (srgb(c) * 255.0).round() as u8;
        img.put_pixel(x, y, image::Rgb([to8(px[0]), to8(px[1]), to8(px[2])]));
    }
    let path = Path::new(&dir).join(format!("{name}.png"));
    img.save(&path).expect("save png");
}

/// Each look renders at its defaults with an average brightness a wall and a
/// stream take — dark enough for slides and names over it, never black —
/// and no blown-out highlights.
#[test]
fn each_look_keeps_to_broadcast_brightness() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let looks = looks();
    assert!(looks.len() >= 10, "the broadcast looks: {}", looks.len());
    for path in &looks {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let pixels = render(&ctx, path, 12.3, |_| {});
        save(&pixels, &name);
        assert!(
            pixels.iter().all(|p| p.iter().all(|c| c.is_finite())),
            "{name}: a pixel not a number"
        );
        let lumas: Vec<f32> = pixels.iter().map(|p| luma(*p)).collect();
        let mean = lumas.iter().sum::<f32>() / lumas.len() as f32;
        let mut sorted = lumas.clone();
        sorted.sort_by(f32::total_cmp);
        let peak = sorted[(sorted.len() as f32 * 0.999) as usize];
        println!("{name}: mean {mean:.3}, 99.9% peak {peak:.3}");
        assert!(
            (0.04..=0.40).contains(&mean),
            "{name}: mean brightness {mean:.3}"
        );
        assert!(peak <= 0.9, "{name}: highlights {peak:.3}");
    }
}

/// One deck drawn frame after frame: it moves while Speed is up, and at
/// Speed 0 stands still where it got to.
#[test]
fn each_moving_look_moves_then_stands_still_at_speed_zero() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let moving: Vec<PathBuf> = looks()
        .into_iter()
        .filter(|p| {
            !p.to_string_lossy().contains("gradient") && !p.to_string_lossy().contains("vignette")
        })
        .collect();
    for path in &moving {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let shader = ISFShader::from_file(path).expect("shader");
        let deck = Deck::new(&ctx, shader, W, H).expect("deck");
        let mut mixer = Mixer::new(&ctx, W, H).expect("mixer");
        mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
        mixer.channel_mut(0).expect("a channel").add_deck(deck);
        let mut frame = |speed: f32| {
            mixer.channel_mut(0).expect("a channel").decks[0]
                .deck
                .generator_params
                .set_float("speed", speed);
            draw(&ctx, &mut mixer)
        };
        let a = frame(120.0);
        let b = frame(120.0);
        let moved = mean_diff(&a, &b);
        assert!(moved > 0.0005, "{name}: hardly moved ({moved})");
        let c = frame(0.0);
        let d = frame(0.0);
        let still = mean_diff(&c, &d);
        assert!(still < 1e-6, "{name}: moved at Speed 0 ({still})");
    }
}

/// Past its wrap a look draws as it did from 0 — the phase kept small all
/// day, its motion as smooth hours in as at the start.
#[test]
fn a_look_past_its_wrap_draws_as_from_zero() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    for path in &looks() {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let wrap = ISFShader::from_file(path)
            .expect("shader")
            .metadata
            .phase_inputs
            .as_ref()
            .and_then(|inputs| inputs.first())
            .and_then(|input| input.wrap)
            .unwrap_or_else(|| panic!("{name}: no wrap"));
        let early = wrap * 0.25;
        let a = render(&ctx, path, early, |_| {});
        let b = render(&ctx, path, wrap + early, |_| {});
        let diff = mean_diff(&a, &b);
        assert!(
            diff < 0.002,
            "{name}: past its wrap it draws otherwise ({diff})"
        );
    }
}

/// Density brings each curtain, ribbon or band in gradually: a little more
/// Density, a little more of it — never a whole one at once.
#[test]
fn density_brings_things_in_gradually() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    for look in ["aurora", "silk_ribbons", "sunset_waves"] {
        let path = looks()
            .into_iter()
            .find(|p| p.to_string_lossy().contains(look))
            .expect("the look");
        // 0.5 is where the second curtain or ribbon is just whole.
        let a = render(&ctx, &path, 7.0, |params| params.set_float("density", 0.49));
        let b = render(&ctx, &path, 7.0, |params| params.set_float("density", 0.51));
        let step = mean_diff(&a, &b);
        assert!(
            step < 0.004,
            "{look}: a small Density change jumped ({step})"
        );
    }
}

/// Every palette, and Custom, renders with no pixel out of range; each
/// preset changes the picture.
#[test]
fn every_palette_renders() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let path = looks()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("sunset_waves"))
        .expect("sunset waves");
    let mut means = Vec::new();
    for preset in 0..=12 {
        let pixels = render(&ctx, &path, 5.0, |params| {
            params.set_long("palette", preset);
        });
        assert!(pixels
            .iter()
            .all(|p| p.iter().all(|c| c.is_finite() && *c >= 0.0)));
        means.push(pixels.iter().map(|p| luma(*p)).sum::<f32>() / pixels.len() as f32);
    }
    means.sort_by(f32::total_cmp);
    means.dedup_by(|a, b| (*a - *b).abs() < 1e-4);
    assert!(means.len() >= 10, "presets look alike: {means:?}");
}

/// The moving looks (Gradient and Vignette plate have no centre to calm,
/// and are their colours already).
fn moving_looks() -> Vec<PathBuf> {
    looks()
        .into_iter()
        .filter(|p| {
            !p.to_string_lossy().contains("gradient") && !p.to_string_lossy().contains("vignette")
        })
        .collect()
}

/// Custom colours show as picked: over a red background, the lines, discs
/// and streaks drawn in a green Main and a blue Accent come out green and
/// blue where they are whole — not gold and pink, the background mixed in.
#[test]
fn custom_colours_show_as_picked() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    for path in looks() {
        let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let pixels = render(&ctx, &path, 7.0, |params| {
            params.set_long("palette", 0);
            params.set_float("density", 1.0);
            params.set_float("calm", 0.0);
            params.set_color("bg_colour", [0.5, 0.04, 0.04, 1.0]);
            params.set_color("main_colour", [0.1, 0.85, 0.2, 1.0]);
            params.set_color("accent_colour", [0.15, 0.3, 0.95, 1.0]);
        });
        save(&pixels, &format!("{name}_custom"));
        // Pixels whose colour is the Main's or the Accent's, not the red
        // background's: green or blue the strongest of the three.
        let picked = pixels
            .iter()
            .filter(|p| p[1].max(p[2]) > p[0] * 1.5 && p[1].max(p[2]) > 0.02)
            .count();
        let share = picked as f32 / pixels.len() as f32;
        println!("{name}: {:.2}% in the picked colours", share * 100.0);
        // The background as picked where nothing is drawn: the darkest
        // corner red, not tinted toward the Main.
        let corner = pixels[(4 * W + 4) as usize];
        if !name.contains("gradient") {
            assert!(
                pixels.iter().any(|p| p[0] > 0.1 && p[1] < p[0] * 0.15 && p[2] < p[0] * 0.15),
                "{name}: the background not as picked (corner {corner:?})"
            );
        }
        if name.contains("vignette") {
            // A plate of one colour: the Background as picked.
            let middle = pixels[(H / 2 * W + W / 2) as usize];
            assert!(middle[0] > middle[1] * 4.0, "{name}: the plate not red: {middle:?}");
        } else {
            assert!(share > 0.005, "{name}: the picked colours hardly show ({:.3}%)", share * 100.0);
        }
    }
}

/// The palettes the UI and the lower thirds know are the ones the looks
/// draw: a colour picked on a preset starts the others as that preset's.
#[test]
fn the_palettes_match_the_looks() {
    let path = looks()
        .into_iter()
        .find(|p| p.to_string_lossy().contains("hex_glow"))
        .expect("hex field");
    let text = std::fs::read_to_string(&path).expect("read");
    let start = text.find("BC_PALETTES[36] = vec3[36](").expect("the palettes");
    let end = start + text[start..].find(");").expect("their end");
    let numbers: Vec<f32> = text[start..end]
        .split("vec3(")
        .skip(1)
        .flat_map(|v| {
            v.split(')')
                .next()
                .unwrap_or_default()
                .split(',')
                .map(|n| n.trim().parse::<f32>().expect("a number"))
                .collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(numbers.len(), 36 * 3);
    for palette in 1..=12u8 {
        let srgb = varda::engine::value::lower_third::palette_srgb(palette).expect("a palette");
        for (i, colour) in srgb.iter().enumerate() {
            for (c, v) in colour.iter().enumerate() {
                let shader = numbers[(usize::from(palette) - 1) * 9 + i * 3 + c];
                assert!(
                    (shader - f32::from(*v) / 255.0).abs() < 0.001,
                    "palette {palette}, colour {i}: {shader} vs {v}"
                );
            }
        }
    }
}

/// A colour picked on a preset switches the look to Custom, the others
/// starting as the preset showed them (Input Settings does this: the
/// background lifted toward the Main, 5% on a moving look, 35% on
/// Vignette plate, none on Gradient). Drawn so, Custom is the preset's
/// picture: nothing moves but the colour picked.
#[test]
fn custom_from_a_preset_draws_the_same_picture() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let lin = |c: f32| {
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let enc = |c: f32| {
        let c = c.clamp(0.0, 1.0);
        if c <= 0.003_130_8 {
            c * 12.92
        } else {
            1.055 * c.powf(1.0 / 2.4) - 0.055
        }
    };
    for path in looks() {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let lift = if name.contains("vignette") {
            0.35
        } else if name.contains("gradient") {
            0.0
        } else {
            0.05
        };
        for preset in [4u8, 7, 12] {
            let [bg, main, accent] = varda::engine::value::lower_third::palette_srgb(preset)
                .expect("a palette")
                .map(|c| c.map(|v| f32::from(v) / 255.0));
            let shown_bg: [f32; 3] =
                std::array::from_fn(|i| enc(lin(bg[i]) * (1.0 - lift) + lin(main[i]) * lift));
            let rgba = |c: [f32; 3]| [c[0], c[1], c[2], 1.0];
            let as_preset = render(&ctx, &path, 4.0, |params| {
                params.set_long("palette", i32::from(preset));
            });
            let as_custom = render(&ctx, &path, 4.0, |params| {
                params.set_long("palette", 0);
                params.set_color("bg_colour", rgba(shown_bg));
                params.set_color("main_colour", rgba(main));
                params.set_color("accent_colour", rgba(accent));
            });
            let diff = mean_diff(&as_preset, &as_custom);
            assert!(
                diff < 0.004,
                "{name}, palette {preset}: Custom differs by {diff:.4}"
            );
        }
    }
}

/// Black picked for every colour, calmed as far as it goes: still never
/// near black anywhere — LED panels go patchy there.
#[test]
fn no_look_goes_near_black() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    for path in looks() {
        let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let pixels = render(&ctx, &path, 3.0, |params| {
            params.set_long("palette", 0);
            params.set_float("calm", 1.0);
            for colour in ["bg_colour", "main_colour", "accent_colour"] {
                params.set_color(colour, [0.0, 0.0, 0.0, 1.0]);
            }
        });
        let darkest = pixels.iter().map(|p| luma(*p)).fold(f32::MAX, f32::min);
        // The floor is 4%; dither moves a pixel a step either way.
        assert!(darkest >= 0.033, "{name}: a pixel at {darkest:.3}");
    }
}

/// Calm centre calms every look alike: at 1 the middle and the lower
/// third are at least a quarter dimmer than at 0 (or as dim as its calm
/// plate already), whatever the look.
#[test]
fn calm_centre_calms_every_look() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mean = |pixels: &[[f32; 4]], x: (f32, f32), y: (f32, f32)| {
        let mut sum = 0.0;
        let mut n = 0;
        for (i, px) in pixels.iter().enumerate() {
            let (u, v) = ((i as u32 % W) as f32 / W as f32, (i as u32 / W) as f32 / H as f32);
            if (x.0..x.1).contains(&u) && (y.0..y.1).contains(&v) {
                sum += luma(*px);
                n += 1;
            }
        }
        sum / n.max(1) as f32
    };
    for path in moving_looks() {
        let name = path.file_stem().and_then(|n| n.to_str()).unwrap_or_default().to_string();
        let at = |calm: f32| {
            render(&ctx, &path, 9.0, |params| {
                params.set_float("density", 0.8);
                params.set_float("calm", calm);
            })
        };
        let (open, calmed) = (at(0.0), at(1.0));
        for (place, x, y) in [
            ("middle", (0.38, 0.62), (0.36, 0.58)),
            ("lower third", (0.15, 0.85), (0.72, 0.9)),
        ] {
            let (before, after) = (mean(&open, x, y), mean(&calmed, x, y));
            let drop = 1.0 - after / before.max(1e-6);
            println!("{name} {place}: {before:.3} -> {after:.3} ({:.0}% dimmer)", drop * 100.0);
            // A quarter dimmer — or as dim already as the calm plate (near
            // the floor LED panels need, which it does not go under).
            assert!(
                drop >= 0.25 || after <= 0.09,
                "{name}: the {place} only {:.0}% dimmer, at {after:.3}",
                drop * 100.0
            );
        }
        // Never black there: LED panels go patchy near it.
        assert!(mean(&calmed, (0.38, 0.62), (0.36, 0.58)) > 0.02, "{name}: calmed to black");
    }
}

/// What each look costs the GPU at 3840×2160: frames drawn and waited for,
/// less a plain colour's — a measurement, run by hand
/// (`cargo test --release --test broadcast_looks -- --ignored --nocapture`).
#[test]
#[ignore = "a measurement"]
fn cost_of_each_look_at_4k() {
    const W4: u32 = 3840;
    const H4: u32 = 2160;
    const FRAMES: u32 = 30;
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let time = |shader: Option<&Path>| -> f64 {
        let mut mixer = Mixer::new(&ctx, W4, H4).expect("mixer");
        mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
        let deck = match shader {
            Some(path) => {
                let shader = ISFShader::from_file(path).expect("shader");
                Deck::new(&ctx, shader, W4, H4).expect("deck")
            }
            None => Deck::new_solid_color(&ctx, [0.1, 0.2, 0.3, 1.0], W4, H4).expect("deck"),
        };
        mixer.channel_mut(0).expect("a channel").add_deck(deck);
        let audio = AudioData::default();
        let audio_values = AudioValues {
            sources: std::collections::HashMap::default(),
        };
        let analyzer_values = AnalyzerValues::default();
        let mut frame = |n: u32| {
            let inputs = varda::mixer::FrameInputs {
                audio_data: &audio,
                audio_values: &audio_values,
                analyzer_values: &analyzer_values,
                beat_time: None,
                transport: None,
                free_run_time: Some(f64::from(n) as f32 / 60.0),
            };
            mixer.render(&ctx, &inputs, 60, &[]).expect("render");
            ctx.device
                .poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: None,
                })
                .ok();
        };
        for n in 0..5 {
            frame(n);
        }
        let started = std::time::Instant::now();
        for n in 5..5 + FRAMES {
            frame(n);
        }
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES)
    };
    let base = time(None);
    println!("plain colour: {base:.2} ms a frame (taken off each look below)");
    for path in &looks() {
        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        let ms = time(Some(path));
        println!("{name}: {:.2} ms", (ms - base).max(0.0));
    }
}
