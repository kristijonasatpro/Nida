//! Text inputs: lines drawn where their spot says, edited live without a
//! gap, hidden and shown with a fade, kept with the show, undone.

use std::time::{Duration, Instant};

use varda::app::VardaApp;
use varda::engine::value::text::{TextAlign, TextSettings};
use varda::engine::value::timer::Anchor;
use varda::engine::{CommandResult, EngineCommand, InputSource};
use varda::mixer::Mixer;
use varda::renderer::context::GpuContext;
use varda::renderer::tonemap::TonemapMode;

mod common;

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    app.process_commands();
    reply_rx.blocking_recv().unwrap()
}

fn app_in(workspace: &str) -> Option<VardaApp> {
    use clap::Parser as _;
    let gpu = common::headless_gpu()?;
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

#[allow(clippy::needless_pass_by_value)]
fn ok(reply: CommandResult) {
    assert!(
        !matches!(reply, CommandResult::Err { .. }),
        "expected it to work: {reply:?}"
    );
}

/// The text layer's UUID and its settings.
fn text_of(app: &VardaApp) -> Option<(String, TextSettings)> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| c.decks.clone())
        .find_map(|d| d.text.map(|t| (d.uuid, t.settings)))
}

fn add_text(app: &mut VardaApp, text: &str) -> String {
    let reply = send_cmd(
        app,
        EngineCommand::AddInput {
            source: InputSource::Text {
                settings: Some(TextSettings {
                    text: text.into(),
                    ..TextSettings::default()
                }),
            },
            name: None,
            audio: None,
        },
    );
    assert!(matches!(reply, CommandResult::OkWithId { .. }), "{reply:?}");
    text_of(app).expect("a text input").0
}

/// Its text and look are kept with the show; an edit is undone.
#[test]
fn a_text_input_is_kept_with_the_show_and_undone() {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().to_str().unwrap().to_string();
    let wanted = {
        let Some(mut app) = app_in(&ws) else {
            return;
        };
        let uuid = add_text(&mut app, "PERTRAUKA\nTęsiame 14:00");
        let mut settings = text_of(&app).unwrap().1;
        settings.look.align = TextAlign::Left;
        settings.look.anchor = Anchor::BottomLeft;
        settings.look.box_opacity = 0.6;
        ok(send_cmd(
            &mut app,
            EngineCommand::SetTextSettings {
                deck_uuid: uuid.clone(),
                settings: settings.clone(),
            },
        ));
        let mut typo = settings.clone();
        typo.text = "PERTRAUKA\nTęsiame 15:00".into();
        ok(send_cmd(
            &mut app,
            EngineCommand::SetTextSettings {
                deck_uuid: uuid,
                settings: typo,
            },
        ));
        ok(send_cmd(&mut app, EngineCommand::Undo));
        assert_eq!(text_of(&app).unwrap().1, settings, "the edit undone");
        ok(send_cmd(&mut app, EngineCommand::SaveWorkspace));
        settings
    };
    let mut app = app_in(&ws).unwrap();
    let _ = app.load_workspace();
    assert_eq!(text_of(&app).expect("it came back").1, wanted);
}

const W: u32 = 960;
const H: u32 = 540;

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

fn read_texture(ctx: &GpuContext, tex: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (tex.width(), tex.height());
    let padded =
        (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = ctx.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("text readback"),
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
    let (tx, rx) = std::sync::mpsc::channel();
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
    let audio = varda::audio::AudioData::default();
    let audio_values = varda::modulation::AudioValues {
        sources: std::collections::HashMap::default(),
    };
    let analyzer_values = varda::modulation::AnalyzerValues::default();
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

fn text(m: &mut Mixer) -> &mut varda::deck::text::Text {
    m.channel_mut(0).expect("a channel").decks[0]
        .deck
        .text_mut()
        .expect("a text input")
}

/// The white letters' pixels: where they are, and how many.
fn letters(frame: &[[f32; 4]]) -> (usize, f32, f32) {
    let mut n = 0;
    let (mut sx, mut sy) = (0.0, 0.0);
    for (i, px) in frame.iter().enumerate() {
        if px[0] > 0.6 && px[1] > 0.6 && px[2] > 0.6 {
            n += 1;
            sx += (i as u32 % W) as f32;
            sy += (i as u32 / W) as f32;
        }
    }
    let at = |s: f32, of: u32| {
        if n == 0 {
            -1.0
        } else {
            s / n as f32 / of as f32
        }
    };
    (n, at(sx, W), at(sy, H))
}

/// Hidden and faded out, then given a longer fade time: still hidden — no
/// jump back to part shown.
#[test]
fn a_new_fade_time_never_brings_hidden_text_back() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let mut settings = TextSettings {
        text: "PERTRAUKA".into(),
        ..TextSettings::default()
    };
    settings.look.shadow_opacity = 0.0;
    settings.fade_secs = 0.2;
    let deck = varda::deck::Deck::new_text(&ctx, settings.clone(), W, H).expect("deck");
    let mut mixer = Mixer::new(&ctx, W, H).expect("mixer");
    mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
    mixer.channel_mut(0).expect("a channel").add_deck(deck);
    assert!(text(&mut mixer).wait_drawn(Duration::from_secs(5)), "drawn");
    settings.visible = false;
    text(&mut mixer).set_settings(settings.clone());
    std::thread::sleep(Duration::from_millis(400));
    settings.fade_secs = 5.0;
    text(&mut mixer).set_settings(settings);
    let frame = draw(&ctx, &mut mixer);
    let alpha = frame.iter().map(|p| p[0]).fold(0.0, f32::max);
    assert!(alpha < 0.01, "hidden text came back: {alpha}");
}

/// Drawn at its spot — white letters in the bottom-left — edited live with
/// no frame without text between, hidden with a fade that passes through
/// partly shown, and gone.
#[test]
fn text_draws_at_its_spot_edits_live_and_fades_out() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let mut settings = TextSettings {
        text: "PERTRAUKA".into(),
        ..TextSettings::default()
    };
    settings.look.anchor = Anchor::BottomLeft;
    settings.look.shadow_opacity = 0.0;
    settings.look.size = 0.12;
    settings.fade_secs = 0.6;
    let deck = varda::deck::Deck::new_text(&ctx, settings.clone(), W, H).expect("deck");
    let mut mixer = Mixer::new(&ctx, W, H).expect("mixer");
    mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
    mixer.channel_mut(0).expect("a channel").add_deck(deck);
    assert!(text(&mut mixer).wait_drawn(Duration::from_secs(5)), "drawn");
    let (n, x, y) = letters(&draw(&ctx, &mut mixer));
    assert!(n > 500, "letters drawn: {n}");
    assert!(x < 0.45 && y > 0.6, "in the bottom left: ({x:.2}, {y:.2})");

    // Edited live: every frame until the new drawing is in has letters.
    let mut edited = settings.clone();
    edited.text = "PERTRAUKA\nTęsiame 14:30".into();
    text(&mut mixer).set_settings(edited.clone());
    let until = Instant::now() + Duration::from_secs(3);
    let mut grew = false;
    while Instant::now() < until && !grew {
        let (m, _, _) = letters(&draw(&ctx, &mut mixer));
        assert!(m > 500, "a frame without text during the edit: {m}");
        grew = m > n * 3 / 2;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(grew, "the second line came in");

    // Hidden: fades through partly shown, then gone.
    let mut hidden = edited;
    hidden.visible = false;
    text(&mut mixer).set_settings(hidden);
    let until = Instant::now() + Duration::from_secs(2);
    let mut part_way = false;
    let mut gone = false;
    while Instant::now() < until && !gone {
        let frame = draw(&ctx, &mut mixer);
        let alpha = frame.iter().map(|p| p[0]).fold(0.0, f32::max);
        part_way |= alpha > 0.1 && alpha < 0.9;
        gone = alpha < 0.01;
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(part_way, "a fade, not a cut");
    assert!(gone, "gone once faded");
}
