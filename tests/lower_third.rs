//! Lower thirds: a name comes on air with its motion and goes off, on real
//! frames; another shown meanwhile waits for the first to go; the names
//! come from an organiser's list (Windows-1257 Excel CSV), read again as it
//! is saved; and it is kept with the show, coming back off air.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::audio::AudioData;
use varda::deck::Deck;
use varda::engine::value::lower_third::{
    LowerThirdBar, LowerThirdMotion, LowerThirdRow, LowerThirdSettings,
};
use varda::engine::{CommandResult, EngineCommand, InputSource};
use varda::mixer::Mixer;
use varda::modulation::{AnalyzerValues, AudioValues};
use varda::renderer::context::GpuContext;
use varda::renderer::tonemap::TonemapMode;

mod common;
use common::headless_gpu;

const W: u32 = 1280;
const H: u32 = 720;

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

/// The frame's pixels, linear RGBA, row by row.
fn read_texture(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (tex.width(), tex.height());
    let unpadded = w * 8;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded = unpadded.div_ceil(align) * align;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("lower third readback"),
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

/// The plate's red (sRGB 0.8, 0.2, 0.2 → linear ≈ 0.6, 0.03, 0.03).
fn is_plate(px: [f32; 4]) -> bool {
    px[0] > 0.4 && px[1] < 0.1 && px[2] < 0.1
}

fn is_text(px: [f32; 4]) -> bool {
    px[0] > 0.8 && px[1] > 0.8 && px[2] > 0.8
}

/// The plate's leftmost and rightmost columns, and how many of its pixels
/// show; none when none do.
fn plate_extent(frame: &[[f32; 4]]) -> Option<(u32, u32, usize)> {
    plate_extent_at(frame, W)
}

/// [`plate_extent`] of a frame `width` wide.
fn plate_extent_at(frame: &[[f32; 4]], width: u32) -> Option<(u32, u32, usize)> {
    let mut extent: Option<(u32, u32, usize)> = None;
    for (i, px) in frame.iter().enumerate() {
        if is_plate(*px) {
            let x = (i as u32) % width;
            extent = Some(match extent {
                None => (x, x, 1),
                Some((l, r, n)) => (l.min(x), r.max(x), n + 1),
            });
        }
    }
    extent
}

fn settings(motion: LowerThirdMotion, secs: f32) -> LowerThirdSettings {
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

fn mixer_with(ctx: &GpuContext, settings: LowerThirdSettings) -> Mixer {
    let deck = Deck::new_lower_third(ctx, settings, W, H).expect("deck");
    let mut mixer = Mixer::new(ctx, W, H).expect("mixer");
    mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
    mixer.channel_mut(0).expect("a channel").add_deck(deck);
    mixer
}

fn lower(mixer: &mut Mixer) -> &mut varda::deck::lower_third::LowerThird {
    mixer.channel_mut(0).expect("a channel").decks[0]
        .deck
        .lower_third_mut()
        .expect("a lower third")
}

/// Frames drawn until `wanted` holds of one, for at most `within`.
fn frames_until(
    ctx: &GpuContext,
    mixer: &mut Mixer,
    within: Duration,
    wanted: impl Fn(&[[f32; 4]]) -> bool,
) -> Option<Vec<[f32; 4]>> {
    let until = Instant::now() + within;
    while Instant::now() < until {
        let frame = draw(ctx, mixer);
        if wanted(&frame) {
            return Some(frame);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    None
}

/// Off air nothing shows; Show cuts the cued name on, in the lower left,
/// plate and text; Hide takes it off.
#[test]
fn a_name_cuts_on_in_the_lower_left_and_off() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Cut, 0.5));
    let off = draw(&ctx, &mut mixer);
    assert!(plate_extent(&off).is_none(), "nothing off air");
    lower(&mut mixer).show(None);
    let on = frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("the name comes on");
    let (left, right, plate) = plate_extent(&on).expect("its plate shows");
    // A solid plate, not a few red edges of the letters: about 300 × 87
    // pixels at this size, less the letters.
    assert!(plate > 10_000, "a whole plate shows: {plate} pixels");
    assert!(
        left >= (0.05 * W as f32) as u32 - 1,
        "inside the safe area: {left}"
    );
    assert!(right < W / 2 + W / 10, "on the left: {right}");
    let rows: Vec<u32> = on
        .iter()
        .enumerate()
        .filter(|(_, px)| is_plate(**px))
        .map(|(i, _)| i as u32 / W)
        .collect();
    assert!(rows.iter().all(|y| *y > H / 2), "in the lower half");
    assert_eq!(lower(&mut mixer).row_on_air(), Some(0));
    assert_eq!(lower(&mut mixer).settings().cued, 1, "the next one cued");

    lower(&mut mixer).hide();
    let gone = frames_until(&ctx, &mut mixer, Duration::from_secs(2), |f| {
        plate_extent(f).is_none()
    });
    assert!(gone.is_some(), "Hide takes it off");
    assert_eq!(lower(&mut mixer).row_on_air(), None);
}

/// The look a new lower third has — a palette's plate, its edge bar, a
/// wipe — comes on in about its half second and shows the name.
/// `NIDA_SAVE_LOOKS=<dir>` writes it as a PNG, to be looked at.
#[test]
fn the_default_look_comes_on() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut settings = settings(LowerThirdMotion::Wipe, 0.5);
    settings.look = varda::engine::value::lower_third::LowerThirdLook::default();
    let mut mixer = mixer_with(&ctx, settings);
    lower(&mut mixer).show(None);
    let started = Instant::now();
    let on = frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("the name comes on");
    std::thread::sleep(Duration::from_millis(600));
    let on = {
        let _ = on;
        draw(&ctx, &mut mixer)
    };
    assert!(started.elapsed() < Duration::from_secs(4));
    let lit = on
        .iter()
        .filter(|px| px[3] > 0.5 && (px[0] + px[1] + px[2]) > 0.01)
        .count();
    assert!(lit > 5000, "a plate shows: {lit} pixels");
    if let Some(dir) = std::env::var_os("NIDA_SAVE_LOOKS") {
        let mut img = image::RgbaImage::new(W, H);
        for (dst, px) in img.pixels_mut().zip(&on) {
            let enc = |c: f32| {
                let c = c.clamp(0.0, 1.0);
                let s = if c <= 0.003_130_8 {
                    c * 12.92
                } else {
                    1.055 * c.powf(1.0 / 2.4) - 0.055
                };
                (s * 255.0).round() as u8
            };
            // Over grey, so the plate's own transparency shows.
            let a = px[3].clamp(0.0, 1.0);
            let over = |c: f32| c * a + 0.2 * (1.0 - a);
            *dst = image::Rgba([enc(over(px[0])), enc(over(px[1])), enc(over(px[2])), 255]);
        }
        img.save(std::path::Path::new(&dir).join("lower_third_default.png"))
            .expect("saved");
    }
}

/// With "text after the plate", a fade brings the plate in first — no
/// text while it comes — then the text.
#[test]
fn text_after_the_plate_comes_in_second() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut settings = settings(LowerThirdMotion::Fade, 3.0);
    settings.look.text_after = true;
    let mut mixer = mixer_with(&ctx, settings);
    lower(&mut mixer).show(None);
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        plate_extent(f).is_some()
    })
    .expect("the plate starts");
    let since = Instant::now();
    std::thread::sleep(Duration::from_millis(700));
    let early = draw(&ctx, &mut mixer);
    if since.elapsed() < Duration::from_millis(1250) {
        assert!(plate_extent(&early).is_some(), "the plate coming");
        assert!(!early.iter().any(|px| is_text(*px)), "no text yet");
    }
    let late = frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    });
    assert!(late.is_some(), "then the text");
}

/// Every style, in the look a new lower third has, comes on and shows the
/// name. `NIDA_SAVE_LOOKS=<dir>` writes each as a PNG, to be looked at.
#[test]
fn every_style_comes_on() {
    use varda::engine::value::lower_third::LowerThirdStyle;
    let Some(ctx) = headless_gpu() else {
        return;
    };
    for style in LowerThirdStyle::ALL {
        let mut settings = settings(LowerThirdMotion::Cut, 0.3);
        settings.look = varda::engine::value::lower_third::LowerThirdLook {
            style,
            ..varda::engine::value::lower_third::LowerThirdLook::default()
        };
        let mut mixer = mixer_with(&ctx, settings);
        lower(&mut mixer).show(None);
        let on = frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
            f.iter().any(|px| is_text(*px))
        })
        .unwrap_or_else(|| panic!("{style:?}: the name comes on"));
        if let Some(dir) = std::env::var_os("NIDA_SAVE_LOOKS") {
            let mut img = image::RgbaImage::new(W, H);
            for (dst, px) in img.pixels_mut().zip(&on) {
                let enc = |c: f32| {
                    let c = c.clamp(0.0, 1.0);
                    let s = if c <= 0.003_130_8 {
                        c * 12.92
                    } else {
                        1.055 * c.powf(1.0 / 2.4) - 0.055
                    };
                    (s * 255.0).round() as u8
                };
                let a = px[3].clamp(0.0, 1.0);
                let over = |c: f32| c * a + 0.2 * (1.0 - a);
                *dst = image::Rgba([enc(over(px[0])), enc(over(px[1])), enc(over(px[2])), 255]);
            }
            img.save(std::path::Path::new(&dir).join(format!("lower_third_{style:?}.png")))
                .expect("saved");
        }
    }
}

/// A wipe opens the plate from its leading edge, the text fading in only
/// once the plate is most of the way open.
#[test]
fn a_wipe_opens_the_plate_from_its_edge_before_the_text() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    // Its full width, cut on.
    let mut full = mixer_with(&ctx, settings(LowerThirdMotion::Cut, 0.5));
    lower(&mut full).show(None);
    let on = frames_until(&ctx, &mut full, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("on");
    let (full_left, full_right, _) = plate_extent(&on).expect("plate");

    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Wipe, 3.0));
    lower(&mut mixer).show(None);
    // The drawing is made on its own thread: the wipe starts once it is.
    let started = frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        plate_extent(f).is_some()
    });
    assert!(started.is_some(), "the wipe starts");
    let since = Instant::now();
    std::thread::sleep(Duration::from_millis(400));
    let part = draw(&ctx, &mut mixer);
    let elapsed = since.elapsed();
    let (left, right, _) = plate_extent(&part).expect("part of the plate");
    assert!(
        left <= full_left + 1,
        "opened from its left edge: {left} vs {full_left}"
    );
    let width = (full_right - full_left) as f32;
    let open = (right - full_left) as f32 / width;
    assert!(open > 0.05 && open < 0.97, "part way open: {open}");
    // The text comes in only from 45% of the motion (1.35 s of 3).
    if elapsed < Duration::from_millis(1200) {
        assert!(!part.iter().any(|px| is_text(*px)), "no text yet");
    }
}

/// Showing another name while one is on takes the first off first; then
/// the other comes on.
#[test]
fn showing_another_name_takes_the_first_off_first() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Fade, 0.3));
    lower(&mut mixer).show(Some(0));
    let until = Instant::now() + Duration::from_secs(5);
    while lower(&mut mixer).row_on_air() == Some(0) && Instant::now() < until {
        draw(&ctx, &mut mixer);
        if plate_extent(&draw(&ctx, &mut mixer)).is_some_and(|(_, _, n)| n > 1000) {
            break;
        }
    }
    std::thread::sleep(Duration::from_millis(400));
    draw(&ctx, &mut mixer);
    lower(&mut mixer).show(Some(1));
    assert_eq!(
        lower(&mut mixer).row_on_air(),
        Some(0),
        "the first is still going out"
    );
    let until = Instant::now() + Duration::from_secs(5);
    while lower(&mut mixer).row_on_air() != Some(1) && Instant::now() < until {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        lower(&mut mixer).row_on_air(),
        Some(1),
        "then the other comes on"
    );
}

/// The organiser inserts a name above while one is on air and the next is
/// cued: the same person stays on air and the same one cued — not whoever
/// now has their place in the list.
#[test]
fn a_row_inserted_above_keeps_who_is_on_air_and_cued() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut settings = settings(LowerThirdMotion::Cut, 0.5);
    settings.rows.push(LowerThirdRow {
        name: "Petras Vileišis".into(),
        ..LowerThirdRow::default()
    });
    let mut mixer = mixer_with(&ctx, settings);
    lower(&mut mixer).show(Some(0));
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("on");
    let before = draw(&ctx, &mut mixer);
    let mut rows = lower(&mut mixer).settings().rows.clone();
    rows.insert(
        0,
        LowerThirdRow {
            name: "Naujas Pranešėjas".into(),
            ..LowerThirdRow::default()
        },
    );
    lower(&mut mixer).set_rows(rows);
    let l = lower(&mut mixer);
    assert_eq!(l.row_on_air(), Some(1), "followed to where it moved");
    assert_eq!(l.settings().rows[1].name, "Justina Prunskienė");
    assert_eq!(l.settings().cued, 2, "the same one cued");
    assert_eq!(l.settings().rows[2].name, "Ona Šimkūnaitė");
    // On air, the picture has not changed.
    draw(&ctx, &mut mixer);
    std::thread::sleep(Duration::from_millis(300));
    let after = draw(&ctx, &mut mixer);
    let changed = before
        .iter()
        .zip(&after)
        .filter(|(a, b)| (a[0] - b[0]).abs() > 0.01)
        .count();
    assert_eq!(changed, 0, "the same name on air");

    // Its own row removed from the list: still on air as it was, until
    // taken off.
    let mut rows = lower(&mut mixer).settings().rows.clone();
    rows.remove(1);
    lower(&mut mixer).set_rows(rows);
    // Time for a drawing asked for to come back and be shown, were one asked.
    draw(&ctx, &mut mixer);
    std::thread::sleep(Duration::from_millis(300));
    draw(&ctx, &mut mixer);
    std::thread::sleep(Duration::from_millis(50));
    let kept = draw(&ctx, &mut mixer);
    assert!(lower(&mut mixer).is_live());
    assert_eq!(lower(&mut mixer).row_on_air(), None, "not in the list now");
    assert!(
        before
            .iter()
            .zip(&kept)
            .all(|(a, b)| (a[0] - b[0]).abs() <= 0.01),
        "what is on air stays as it is"
    );
    lower(&mut mixer).hide();
    assert!(
        frames_until(&ctx, &mut mixer, Duration::from_secs(2), |f| plate_extent(
            f
        )
        .is_none())
        .is_some()
    );
}

/// The green in a frame: only the white letters have it (the test plate is
/// red), so it is how much of the text shows.
fn text_light(frame: &[[f32; 4]]) -> f32 {
    frame.iter().map(|px| px[1]).sum()
}

/// Taken off part way through a wipe, the text carries on from where it
/// was — no jump to nothing.
#[test]
fn hiding_part_way_through_a_wipe_carries_on_from_where_it_was() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Wipe, 3.0));
    lower(&mut mixer).show(None);
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        plate_extent(f).is_some()
    })
    .expect("the wipe starts");
    // The text fades in over the last 55% of the 3 s: just past half of
    // it, where a mirrored going out would have had none left.
    std::thread::sleep(Duration::from_millis(1750));
    let coming = text_light(&draw(&ctx, &mut mixer));
    lower(&mut mixer).hide();
    let going = text_light(&draw(&ctx, &mut mixer));
    assert!(coming > 100.0, "the text is part way in: {coming}");
    assert!(
        going > coming * 0.8,
        "going out from where it was: {coming} then {going}"
    );
}

/// The picture made smaller while a name is on air: until it is drawn
/// again at the new size, the drawing there is scaled to it — not left at
/// its old place, off the foot of the smaller picture.
#[test]
fn a_smaller_picture_keeps_the_name_on_it_while_it_is_drawn_again() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Cut, 0.5));
    lower(&mut mixer).show(None);
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("on");
    let (w, h) = (W / 2, H / 2);
    mixer.resize(&ctx, w, h);
    mixer.channel_mut(0).expect("a channel").decks[0]
        .deck
        .resize(&ctx, w, h);
    let first = draw(&ctx, &mut mixer);
    let (_, _, plate) = plate_extent_at(&first, w).expect("still on the picture");
    assert!(
        plate > 2_000,
        "a whole plate, at a quarter the area: {plate}"
    );
}

/// A name shown while another goes out, then taken out of the list before
/// its turn: it comes on as asked — by its words — and no one else does.
#[test]
fn a_queued_name_comes_on_as_asked_though_the_list_changed() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut settings = settings(LowerThirdMotion::Fade, 0.3);
    settings.rows.push(LowerThirdRow {
        name: "Petras Vileišis".into(),
        ..LowerThirdRow::default()
    });
    let mut mixer = mixer_with(&ctx, settings);
    lower(&mut mixer).show(Some(0));
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("on");
    std::thread::sleep(Duration::from_millis(350));
    draw(&ctx, &mut mixer);
    lower(&mut mixer).show(Some(2));
    let first = lower(&mut mixer).settings().rows[0].clone();
    lower(&mut mixer).set_rows(vec![
        LowerThirdRow {
            name: "Kitas Žmogus".into(),
            ..LowerThirdRow::default()
        },
        first,
    ]);
    let until = Instant::now() + Duration::from_secs(3);
    let live = |m: &mut Mixer| lower(m).snapshot().live.map(|r| r.name);
    while live(&mut mixer).as_deref() != Some("Petras Vileišis") && Instant::now() < until {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(live(&mut mixer).as_deref(), Some("Petras Vileišis"));
    assert_eq!(lower(&mut mixer).row_on_air(), None, "not in the list now");
}

/// The name on air taken out of the list: it stays on air as it is —
/// through a change of look too — and the one now in its place in the list
/// can be shown.
#[test]
fn a_name_taken_out_of_the_list_stays_on_air_until_another_is_shown() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Cut, 0.3));
    lower(&mut mixer).show(Some(0));
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("on");
    let width = |f: &[[f32; 4]]| plate_extent(f).map(|(l, r, _)| r - l);
    let before = width(&draw(&ctx, &mut mixer)).expect("a plate");
    let mut changed = lower(&mut mixer).settings().clone();
    changed.rows.remove(0);
    lower(&mut mixer).set_settings(changed.clone());
    changed.look.plate = [0.7, 0.1, 0.1];
    lower(&mut mixer).set_settings(changed);
    let live = |m: &mut Mixer| lower(m).snapshot().live.map(|r| r.name);
    for _ in 0..10 {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(live(&mut mixer).as_deref(), Some("Justina Prunskienė"));
    // The same name drawn — its plate as wide — in the new colour.
    assert_eq!(
        width(&draw(&ctx, &mut mixer)),
        Some(before),
        "the same name drawn"
    );
    // Row 0 is Ona now: shown, she comes on.
    lower(&mut mixer).show(Some(0));
    let until = Instant::now() + Duration::from_secs(3);
    while live(&mut mixer).as_deref() != Some("Ona Šimkūnaitė") && Instant::now() < until {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(live(&mut mixer).as_deref(), Some("Ona Šimkūnaitė"));
}

/// The name on air fixed where it is (typed, or sent over the API): it
/// changes on air at once, in place — no going out and coming in. An edit
/// of another row leaves it alone.
#[test]
fn fixing_the_name_on_air_changes_it_in_place() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut mixer = mixer_with(&ctx, settings(LowerThirdMotion::Fade, 0.3));
    lower(&mut mixer).show(Some(0));
    frames_until(&ctx, &mut mixer, Duration::from_secs(5), |f| {
        f.iter().any(|px| is_text(*px))
    })
    .expect("on");
    for _ in 0..30 {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(20));
    }
    let width = |f: &[[f32; 4]]| plate_extent(f).map(|(l, r, n)| (r - l, n));
    let (before, plate) = width(&draw(&ctx, &mut mixer)).expect("a plate");
    let live = |m: &mut Mixer| lower(m).snapshot().live.map(|r| r.name);
    // Another row edited: the name on air stays.
    let mut edited = lower(&mut mixer).settings().clone();
    edited.rows[1].name = "Ona Šimkūnaitė-Kitaitė".into();
    lower(&mut mixer).edit_settings(edited.clone());
    for _ in 0..10 {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(live(&mut mixer).as_deref(), Some("Justina Prunskienė"));
    // The row on air fixed: on air at once, its plate there all along.
    edited.rows[0].name = "Justina Prunskienė-Ilgavardė".into();
    lower(&mut mixer).edit_settings(edited);
    let until = Instant::now() + Duration::from_secs(3);
    let mut wider = false;
    while !wider && Instant::now() < until {
        let now = width(&draw(&ctx, &mut mixer));
        assert!(
            now.is_some_and(|(w, n)| w >= before && n >= plate * 9 / 10),
            "the plate stayed on air while it changed: {now:?} (was {before}, {plate})"
        );
        wider = now.is_some_and(|(w, _)| w > before);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(wider, "the fixed name on air");
    assert_eq!(
        live(&mut mixer).as_deref(),
        Some("Justina Prunskienė-Ilgavardė")
    );
    assert!(lower(&mut mixer).is_live());
    // A row put above it, edited too: not a fix of the row on air.
    let mut moved = lower(&mut mixer).settings().clone();
    moved.rows.insert(
        0,
        LowerThirdRow {
            name: "Petras Vileišis".into(),
            ..LowerThirdRow::default()
        },
    );
    moved.rows[1].title = "Kita pareigybė".into();
    lower(&mut mixer).edit_settings(moved);
    for _ in 0..10 {
        draw(&ctx, &mut mixer);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        live(&mut mixer).as_deref(),
        Some("Justina Prunskienė-Ilgavardė"),
        "rows moved leave it alone"
    );
}

/// Two rows alike, the second shown, a row put above: it is the second
/// that is marked on air, not the first.
#[test]
fn of_two_rows_alike_the_one_shown_is_marked() {
    let Some(ctx) = headless_gpu() else {
        return;
    };
    let mut settings = settings(LowerThirdMotion::Cut, 0.3);
    let twin = settings.rows[0].clone();
    settings.rows.insert(1, twin);
    let mut mixer = mixer_with(&ctx, settings);
    lower(&mut mixer).show(Some(1));
    assert_eq!(lower(&mut mixer).row_on_air(), Some(1));
    let mut rows = lower(&mut mixer).settings().rows.clone();
    rows.insert(
        0,
        LowerThirdRow {
            name: "Naujas Pranešėjas".into(),
            ..LowerThirdRow::default()
        },
    );
    lower(&mut mixer).set_rows(rows);
    assert_eq!(
        lower(&mut mixer).row_on_air(),
        Some(2),
        "the second of the two"
    );
}

// ── Through the engine ─────────────────────────────────────────────

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn app_in(workspace: &str) -> Option<VardaApp> {
    use clap::Parser as _;
    let gpu = headless_gpu()?;
    let config = varda::app::AppConfig::try_parse_from([
        "varda",
        "--headless",
        "--no-osc",
        "--no-ndi",
        "--no-syphon",
        "--no-camera",
        "--workspace",
        workspace,
    ])
    .unwrap();
    Some(VardaApp::new(gpu, &config).unwrap())
}

/// The lower third's layer UUID and what it shows.
fn lower_third_of(
    app: &VardaApp,
) -> Option<(
    String,
    varda::engine::value::lower_third::LowerThirdSnapshot,
)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| c.decks.clone())
        .find_map(|d| d.lower_third.map(|l| (d.uuid, l)))
}

fn add_lower_third(app: &mut VardaApp) -> String {
    let reply = send_cmd(
        app,
        EngineCommand::AddInput {
            source: InputSource::LowerThird { settings: None },
            name: None,
            audio: None,
        },
    );
    assert!(matches!(reply, CommandResult::OkWithId { .. }), "{reply:?}");
    lower_third_of(app).expect("a lower third").0
}

#[allow(clippy::needless_pass_by_value)]
fn ok(reply: CommandResult) {
    assert!(
        !matches!(reply, CommandResult::Err { .. }),
        "expected it to work: {reply:?}"
    );
}

/// An organiser's Excel list saved as CSV in Windows-1257 with `;` reads
/// with its Lithuanian letters; saved again, it is read again; saved broken,
/// the names before stay and why is said.
#[test]
fn an_organisers_list_is_read_and_read_again_when_saved() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let list = dir.path().join("pranesejai.csv");
    // "Vardas, pavardė;Pareigos" / "Justina Prunskienė;Seimo narė" in cp1257.
    std::fs::write(
        &list,
        b"Vardas, pavard\xeb;Pareigos\r\nJustina Prunskien\xeb;Seimo nar\xeb\r\n",
    )
    .unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid.clone(),
            path: list.to_string_lossy().into_owned(),
        },
    ));
    // Read on the lists' thread, within a frame or two.
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 1), "read");
    let (_, l) = lower_third_of(&app).unwrap();
    assert_eq!(l.settings.rows[0].name, "Justina Prunskienė");
    assert_eq!(l.settings.rows[0].title, "Seimo narė");
    assert_eq!(l.settings.source.as_deref(), list.to_str());

    // The organiser fixes the list and saves it.
    app.render_mixer_frame();
    std::fs::write(
        &list,
        "Vardas, pavardė;Pareigos\nJustina Prunskienė;Seimo narė\nOna Šimkūnaitė;Moderatorė\n",
    )
    .unwrap();
    let until = Instant::now() + Duration::from_secs(6);
    while lower_third_of(&app).unwrap().1.settings.rows.len() != 2 && Instant::now() < until {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, l) = lower_third_of(&app).unwrap();
    assert_eq!(l.settings.rows.len(), 2, "read again when saved");
    assert_eq!(l.settings.rows[1].name, "Ona Šimkūnaitė");

    // Saved empty by mistake: the names stay, and why is said.
    std::fs::write(&list, "").unwrap();
    let until = Instant::now() + Duration::from_secs(6);
    while lower_third_of(&app).unwrap().1.problem.is_none() && Instant::now() < until {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(50));
    }
    let (_, l) = lower_third_of(&app).unwrap();
    assert!(l.problem.is_some(), "why it was not read is said");
    assert_eq!(l.settings.rows.len(), 2, "the names before stay");
}

fn rows_now(app: &VardaApp) -> usize {
    lower_third_of(app).map_or(0, |(_, l)| l.settings.rows.len())
}

/// Frames until `wanted` holds of the app, for at most 6 s.
fn frames_until_app(app: &mut VardaApp, wanted: impl Fn(&VardaApp) -> bool) -> bool {
    let until = Instant::now() + Duration::from_secs(6);
    while Instant::now() < until {
        if wanted(app) {
            return true;
        }
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(30));
    }
    wanted(app)
}

/// Undo of a change to its look leaves the cue where Show left it: the
/// next Show is the next person, not the one just shown.
#[test]
#[allow(clippy::float_cmp)] // the colours as sent, back
fn undo_leaves_the_cue_where_show_left_it() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let mut wanted = settings(LowerThirdMotion::Cut, 0.5);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid.clone(),
            settings: wanted.clone(),
        },
    ));
    wanted.look.plate = [0.1, 0.6, 0.2];
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid.clone(),
            settings: wanted,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::LowerThirdShow {
            deck_uuid: uuid,
            row: None,
        },
    ));
    assert_eq!(lower_third_of(&app).unwrap().1.settings.cued, 1);
    ok(send_cmd(&mut app, EngineCommand::Undo));
    let (_, l) = lower_third_of(&app).unwrap();
    assert_eq!(l.settings.look.plate, [0.8, 0.2, 0.2], "the colour undone");
    assert_eq!(l.settings.cued, 1, "the cue where Show left it");
}

/// A list that could not be read is tried again, though its file's time
/// has not changed (it was locked, and is not now); Stop following clears
/// what was said of it.
#[test]
fn a_list_that_could_not_be_read_is_tried_again() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\n").unwrap();
    std::fs::set_permissions(&list, std::fs::Permissions::from_mode(0o000)).unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid.clone(),
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(
        frames_until_app(&mut app, |a| lower_third_of(a).unwrap().1.problem.is_some()),
        "why it could not be read is said"
    );
    std::fs::set_permissions(&list, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        frames_until_app(&mut app, |a| rows_now(a) == 1),
        "read when it can be"
    );
    assert!(lower_third_of(&app).unwrap().1.problem.is_none());

    // Followed somewhere it is not, then not followed: nothing said.
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid.clone(),
            path: dir.path().join("gone.csv").to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| lower_third_of(a)
        .unwrap()
        .1
        .problem
        .is_some()));
    let mut settings = lower_third_of(&app).unwrap().1.settings;
    settings.source = None;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid,
            settings,
        },
    ));
    assert!(lower_third_of(&app).unwrap().1.problem.is_none());
}

/// A list whose read never ends (a drive that stopped answering) holds up
/// only its own lower third: another reads its list, and the first, given a
/// list that answers, reads that.
#[test]
fn a_list_that_never_answers_holds_up_no_other() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let stuck = dir.path().join("stuck.csv");
    // A named pipe with no writer: a read of it waits for ever.
    let made = std::process::Command::new("mkfifo").arg(&stuck).status();
    if !made.is_ok_and(|s| s.success()) {
        return;
    }
    let first = add_lower_third(&mut app);
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: first.clone(),
            path: stuck.to_string_lossy().into_owned(),
        },
    ));
    app.render_mixer_frame();
    let reply = send_cmd(
        &mut app,
        EngineCommand::AddInput {
            source: InputSource::LowerThird { settings: None },
            name: Some("Second".into()),
            audio: None,
        },
    );
    let CommandResult::OkWithId { uuid: channel } = reply else {
        panic!("{reply:?}");
    };
    let second = app
        .build_engine_state()
        .mixer
        .channels
        .iter()
        .find(|c| c.uuid == channel)
        .and_then(|c| c.decks.first().map(|d| d.uuid.clone()))
        .expect("its layer");
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\nOna\n").unwrap();
    let import = |app: &mut VardaApp, deck: &str| {
        ok(send_cmd(
            app,
            EngineCommand::ImportLowerThirdRows {
                deck_uuid: deck.to_string(),
                path: list.to_string_lossy().into_owned(),
            },
        ));
    };
    let rows_of = |app: &VardaApp, deck: &str| {
        app.build_engine_state()
            .mixer
            .channels
            .iter()
            .flat_map(|c| c.decks.clone())
            .find(|d| d.uuid == deck)
            .and_then(|d| d.lower_third)
            .map_or(0, |l| l.settings.rows.len())
    };
    // The second reads its list while the first's read is stuck...
    import(&mut app, &second);
    assert!(
        frames_until_app(&mut app, |a| rows_of(a, &second) == 2),
        "the second read, the first still stuck"
    );
    // ...and the first, given a list that answers, reads it.
    import(&mut app, &first);
    assert!(
        frames_until_app(&mut app, |a| rows_of(a, &first) == 2),
        "the stuck read let go"
    );
}

/// While a list is followed, its file is what the names are: rows set any
/// other way (an undo, the API) are read from the file again.
#[test]
fn a_followed_list_is_read_again_over_rows_set_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\nOna\n").unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid.clone(),
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 2), "read");
    let mut settings = lower_third_of(&app).unwrap().1.settings;
    settings.rows.truncate(1);
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid,
            settings,
        },
    ));
    assert_eq!(rows_now(&app), 1);
    assert!(
        frames_until_app(&mut app, |a| rows_now(a) == 2),
        "the file's again"
    );
}

/// Undo of choosing a list that could not be read goes back to the list
/// before — and the cue stays where Show left it, not back at the first.
#[test]
fn undo_of_a_list_that_failed_keeps_the_cue() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\nOna\nPetras\n").unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid.clone(),
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 3), "read");
    ok(send_cmd(
        &mut app,
        EngineCommand::LowerThirdShow {
            deck_uuid: uuid.clone(),
            row: None,
        },
    ));
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid,
            path: dir.path().join("gone.csv").to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| lower_third_of(a)
        .unwrap()
        .1
        .problem
        .is_some()));
    ok(send_cmd(&mut app, EngineCommand::Undo));
    // The list before, read again.
    assert!(frames_until_app(&mut app, |a| {
        let l = lower_third_of(a).unwrap().1;
        l.settings.source.as_deref() == list.to_str() && l.problem.is_none()
    }));
    for _ in 0..80 {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(30));
    }
    assert_eq!(
        lower_third_of(&app).unwrap().1.settings.cued,
        1,
        "the cue kept"
    );
}

/// Undo — meant for a colour, or pressed while another sent the list over
/// the API — undoes the look but never strips a followed list; and the list
/// made no step of its own, which would undo nothing, so the one Undo is the
/// look's.
#[test]
fn undo_never_strips_a_followed_list() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let mut settings = lower_third_of(&app).unwrap().1.settings;
    settings.look.motion = LowerThirdMotion::Fade;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid.clone(),
            settings,
        },
    ));
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\nOna\nPetras\n").unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid,
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 3), "read");
    ok(send_cmd(&mut app, EngineCommand::Undo));
    for _ in 0..20 {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, l) = lower_third_of(&app).unwrap();
    assert_ne!(
        l.settings.look.motion,
        LowerThirdMotion::Fade,
        "the look undone by the one Undo"
    );
    assert_eq!(l.settings.rows.len(), 3, "the list kept");
    assert_eq!(l.settings.source.as_deref(), list.to_str());
}

/// A look changed and undone, then a first list: Redo — of a state from
/// before the list — does not take the list away.
#[test]
fn redo_after_a_first_list_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let mut settings = lower_third_of(&app).unwrap().1.settings;
    settings.look.motion = LowerThirdMotion::Fade;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid.clone(),
            settings,
        },
    ));
    ok(send_cmd(&mut app, EngineCommand::Undo));
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\nOna\nPetras\n").unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid,
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 3), "read");
    let _ = send_cmd(&mut app, EngineCommand::Redo);
    for _ in 0..20 {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(20));
    }
    let (_, l) = lower_third_of(&app).unwrap();
    assert_eq!(
        l.settings.source.as_deref(),
        list.to_str(),
        "still followed"
    );
    assert_eq!(l.settings.rows.len(), 3, "the list kept");
}

/// Stop following, undone, follows again; redone, stops again.
#[test]
fn stop_following_undone_and_redone() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name\nJonas\n").unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid.clone(),
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 1), "read");
    let mut settings = lower_third_of(&app).unwrap().1.settings;
    settings.source = None;
    ok(send_cmd(
        &mut app,
        EngineCommand::SetLowerThirdSettings {
            deck_uuid: uuid,
            settings,
        },
    ));
    let source = |a: &VardaApp| lower_third_of(a).unwrap().1.settings.source;
    assert_eq!(source(&app), None);
    ok(send_cmd(&mut app, EngineCommand::Undo));
    assert_eq!(source(&app).as_deref(), list.to_str(), "followed again");
    ok(send_cmd(&mut app, EngineCommand::Redo));
    assert_eq!(source(&app), None, "stopped again");
}

/// A list gone for a while (a drive unplugged) and back unchanged: what was
/// said of it is cleared.
#[test]
fn a_list_back_unchanged_clears_what_was_said() {
    let dir = tempfile::tempdir().unwrap();
    let Some(mut app) = app_in(dir.path().join("ws").to_str().unwrap()) else {
        return;
    };
    let uuid = add_lower_third(&mut app);
    let list = dir.path().join("names.csv");
    let away = dir.path().join("away.csv");
    std::fs::write(&list, "Name\nJonas\n").unwrap();
    ok(send_cmd(
        &mut app,
        EngineCommand::ImportLowerThirdRows {
            deck_uuid: uuid,
            path: list.to_string_lossy().into_owned(),
        },
    ));
    assert!(frames_until_app(&mut app, |a| rows_now(a) == 1), "read");
    std::fs::rename(&list, &away).unwrap();
    assert!(frames_until_app(&mut app, |a| lower_third_of(a)
        .unwrap()
        .1
        .problem
        .is_some()));
    std::fs::rename(&away, &list).unwrap();
    assert!(
        frames_until_app(&mut app, |a| lower_third_of(a).unwrap().1.problem.is_none()),
        "cleared once it is back"
    );
}

/// A list changed while the show was closed is read when it opens.
#[test]
fn a_list_changed_while_the_show_was_closed_is_read_on_opening() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws").to_string_lossy().into_owned();
    let list = dir.path().join("names.csv");
    std::fs::write(&list, "Name,Title\nJonas Basanavičius,Daktaras\n").unwrap();
    {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let uuid = add_lower_third(&mut app);
        ok(send_cmd(
            &mut app,
            EngineCommand::ImportLowerThirdRows {
                deck_uuid: uuid,
                path: list.to_string_lossy().into_owned(),
            },
        ));
        assert!(frames_until_app(&mut app, |a| rows_now(a) == 1), "read");
        ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
    }
    std::fs::write(
        &list,
        "Name,Title\nJonas Basanavičius,Daktaras\nVincas Kudirka,Gydytojas\n",
    )
    .unwrap();
    let mut app = app_in(&ws).unwrap();
    let _ = app.load_workspace();
    let until = Instant::now() + Duration::from_secs(5);
    while lower_third_of(&app).unwrap().1.settings.rows.len() != 2 && Instant::now() < until {
        app.render_mixer_frame();
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(
        lower_third_of(&app).unwrap().1.settings.rows.len(),
        2,
        "read as the show opened"
    );
}

/// Its names and look are kept with the show; it comes back off air.
#[test]
fn a_lower_third_is_kept_with_the_show_and_comes_back_off_air() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_str().unwrap().to_string();
    let wanted = settings(LowerThirdMotion::Slide, 0.8);
    {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let uuid = add_lower_third(&mut app);
        ok(send_cmd(
            &mut app,
            EngineCommand::SetLowerThirdSettings {
                deck_uuid: uuid.clone(),
                settings: wanted.clone(),
            },
        ));
        ok(send_cmd(
            &mut app,
            EngineCommand::LowerThirdShow {
                deck_uuid: uuid,
                row: Some(0),
            },
        ));
        assert_eq!(lower_third_of(&app).unwrap().1.on_air, Some(0));
        ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
    }
    let mut app = app_in(&ws).unwrap();
    let _ = app.load_workspace();
    let (_, l) = lower_third_of(&app).expect("it came back");
    let mut saved = wanted;
    saved.cued = 1;
    assert_eq!(l.settings, saved);
    assert_eq!(l.on_air, None, "back off air");
}
