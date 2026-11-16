//! A layer's frame (Input Settings → Position → Frame): a `frame_*` filter
//! that holds the layer's picture — what shows of it, which the frame finds
//! itself each time it is drawn: what the layer's Crop keeps, a picture
//! cropped in the input it is repeated from. The picture fills the frame's
//! window — all that shows, as it comes — the frame is drawn over its edge,
//! and outside the window all is clear; or, Picture inside the frame, the
//! picture is fitted into the frame. It runs before the layer's Transform,
//! so the layer moves and scales frame and all.
//!
//! `NIDA_SAVE_FRAMES=<dir>` also writes the pictures looked at as PNGs.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use varda::app::VardaApp;
use varda::audio::AudioData;
use varda::deck::{Deck, Effect};
use varda::engine::value::mask::{EffectMask, MaskShape, LAYER_MASK_EFFECT};
use varda::engine::{CommandResult, EngineCommand};
use varda::isf::ISFShader;
use varda::mixer::{MainSource, Mixer};
use varda::modulation::{AnalyzerValues, AudioValues};
use varda::params::{ParamValue, ShaderParams};
use varda::renderer::context::GpuContext;

mod common;

/// The frame that is a picture (`shaders/frames/hud.png`).
const ART_FRAME: &str = "frame_hud";

/// The frames drawn of lines of light.
const LINE_FRAMES: [&str; 11] = [
    "frame_deco_silver",
    "frame_electric",
    "frame_flare",
    "frame_hud_lines",
    "frame_neon_arrows",
    "frame_neon_double",
    "frame_neon_steps",
    "frame_neon_tube",
    "frame_neon_two",
    "frame_overlay",
    "frame_ring",
];

const GREEN: [f32; 4] = [0.0, 1.0, 0.0, 1.0];
const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn send_cmd(app: &mut VardaApp, cmd: EngineCommand) -> CommandResult {
    let tx = app.command_sender();
    let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
    tx.send((cmd, Some(reply_tx))).unwrap();
    varda::testing::reply_of(app, reply_rx)
}

fn id(result: CommandResult) -> String {
    match result {
        CommandResult::OkWithId { uuid } => uuid,
        other => panic!("expected an id, got {other:?}"),
    }
}

fn f16(bits: u16) -> f32 {
    let exp = i32::from((bits >> 10) & 0x1f);
    let frac = f32::from(bits & 0x3ff);
    let mag = if exp == 0 {
        frac * 2f32.powi(-24)
    } else {
        (1.0 + frac / 1024.0) * 2f32.powi(exp - 15)
    };
    if bits >> 15 == 1 {
        -mag
    } else {
        mag
    }
}

/// Every pixel of an `Rgba16Float` texture, row by row.
fn frame_pixels(gpu: &GpuContext, texture: &wgpu::Texture) -> Vec<[f32; 4]> {
    let (w, h) = (texture.width(), texture.height());
    let row = (w * 8).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: u64::from(row * h),
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let mut encoder = gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(row),
                rows_per_image: Some(h),
            },
        },
        texture.size(),
    );
    gpu.submit(std::iter::once(encoder.finish()));
    let (tx, rx) = mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    gpu.device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        })
        .ok();
    rx.recv().unwrap().unwrap();
    let data = buffer.slice(..).get_mapped_range().unwrap();
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let at = (y * row + x * 8) as usize;
            out.push(std::array::from_fn(|c| {
                f16(u16::from_le_bytes([data[at + c * 2], data[at + c * 2 + 1]]))
            }));
        }
    }
    out
}

/// A show on Main with one input: blue under red. Its channel and the red
/// layer.
fn blue_under_red(app: &mut VardaApp) -> (String, String) {
    send_cmd(
        app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    for deck in state.mixer.channels.iter().flat_map(|c| &c.decks) {
        send_cmd(
            app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    let channel = app.build_engine_state().mixer.channels[0].uuid.clone();
    for color in [[0.0, 0.0, 1.0, 1.0], [1.0, 0.0, 0.0, 1.0]] {
        id(send_cmd(
            app,
            EngineCommand::AddSolidColorDeck {
                channel_uuid: channel.clone(),
                color,
            },
        ));
    }
    let red = app.build_engine_state().mixer.channels[0].decks[1].uuid.clone();
    send_cmd(
        app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(channel.clone()),
            fade: false,
        },
    );
    (channel, red)
}

/// Pixel assertions observe completed framing, including asynchronous defaults.
/// Fail explicitly on a build error rather than comparing an unfinished source.
fn wait_for_effects(app: &mut VardaApp) {
    let until = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        app.process_inputs();
        let state = app.build_engine_state();
        let mut building = false;
        for effect in state
            .mixer
            .channels
            .iter()
            .flat_map(|channel| &channel.decks)
            .flat_map(|deck| &deck.effects)
        {
            match &effect.load_state {
                varda::engine::EffectLoadState::Ready => {}
                varda::engine::EffectLoadState::Building => building = true,
                varda::engine::EffectLoadState::Failed { message } => {
                    panic!("effect failed: {message}")
                }
            }
        }
        if !building {
            return;
        }
        assert!(
            std::time::Instant::now() < until,
            "source effects did not finish loading"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The first channel's picture — the red layer over the blue one — drawn
/// twice so every pass has run.
fn picture(app: &mut VardaApp) -> (Vec<[f32; 4]>, u32, u32) {
    picture_after(app, 0, 2)
}

/// The picture of the channel at `index`, after so many frames more are
/// drawn.
fn picture_after(app: &mut VardaApp, index: usize, frames: u32) -> (Vec<[f32; 4]>, u32, u32) {
    wait_for_effects(app);
    for _ in 0..frames {
        app.render_mixer_frame();
    }
    let texture = &app.mixer_ref().channels()[index].composite_texture;
    let (w, h) = (texture.width(), texture.height());
    (frame_pixels(app.gpu_context(), texture), w, h)
}

/// The layer's own picture, after its effects and before it goes over what
/// is under it — so what is clear in it reads as clear — drawn twice so
/// every pass has run.
fn own(app: &mut VardaApp, layer: &str) -> (Vec<[f32; 4]>, u32, u32) {
    own_after(app, layer, 2)
}

/// The same, after so many frames more are drawn.
fn own_after(app: &mut VardaApp, layer: &str, frames: u32) -> (Vec<[f32; 4]>, u32, u32) {
    wait_for_effects(app);
    for _ in 0..frames {
        app.render_mixer_frame();
    }
    let slot = app
        .mixer_ref()
        .channels()
        .iter()
        .flat_map(|c| &c.decks)
        .find(|slot| slot.deck.uuid() == layer)
        .expect("the layer");
    let texture = &slot.deck.texture;
    let (w, h) = (texture.width(), texture.height());
    (frame_pixels(app.gpu_context(), texture), w, h)
}

fn srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// With `NIDA_SAVE_FRAMES=<dir>`, the picture as `<name>.png` there, to be
/// looked at: what is clear in it shows as grey squares.
fn save(pixels: &[[f32; 4]], wide: u32, high: u32, name: &str) {
    let Some(dir) = std::env::var_os("NIDA_SAVE_FRAMES") else {
        return;
    };
    let mut img = image::RgbImage::new(wide, high);
    for (i, px) in pixels.iter().enumerate() {
        let (x, y) = (i as u32 % wide, i as u32 / wide);
        let under = if (x / 24 + y / 24) % 2 == 0 { 0.18 } else { 0.10 };
        let alpha = px[3].clamp(0.0, 1.0);
        let to8 = |c: f32| (srgb(c * alpha + under * (1.0 - alpha)) * 255.0).round() as u8;
        img.put_pixel(x, y, image::Rgb([to8(px[0]), to8(px[1]), to8(px[2])]));
    }
    img.save(Path::new(&dir).join(format!("{name}.png")))
        .expect("save png");
}

/// The names of the layer's effects, in order.
fn chain(app: &VardaApp, deck: &str) -> Vec<String> {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .unwrap()
        .effects
        .iter()
        .map(|e| e.name.clone())
        .collect()
}

/// The layer's effect of that name.
fn effect_named(app: &VardaApp, deck: &str, name: &str) -> String {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .find(|d| d.uuid == deck)
        .unwrap()
        .effects
        .iter()
        .find(|e| e.name == name)
        .unwrap_or_else(|| panic!("the layer's {name}"))
        .uuid
        .clone()
}

/// The colour at (x, y), each 0 to 1 across and down the picture.
fn at(pixels: &[[f32; 4]], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    pixels[((y * h as f32) as u32 * w + (x * w as f32) as u32) as usize]
}

/// How wide the picture is for its height. These tests' places are worked
/// out for a 16:9 one.
fn aspect_of(wide: u32, high: u32) -> f32 {
    let aspect = wide as f32 / high as f32;
    assert!((1.7..1.8).contains(&aspect), "a 16:9 picture: {wide}x{high}");
    aspect
}

/// The colour at a place given as the frames' shaders measure: from the
/// middle of the picture in picture heights, x to the right and y down.
fn from_middle(pixels: &[[f32; 4]], w: u32, h: u32, x: f32, y: f32) -> [f32; 4] {
    at(pixels, w, h, 0.5 + x / aspect_of(w, h), 0.5 + y)
}

/// The layer's own picture: red.
fn red(px: [f32; 4]) -> bool {
    px[0] > 0.9 && px[1] < 0.1 && px[2] < 0.1
}

/// The red picture to a hair, all there.
fn just_red(px: [f32; 4]) -> bool {
    (px[0] - 1.0).abs() < 0.004 && px[1] < 0.004 && px[2] < 0.004 && solid(px)
}

/// The red picture dimmed, as a line frame leaves it outside its outline.
fn dimmed(px: [f32; 4]) -> bool {
    (0.25..0.4).contains(&px[0]) && px[1] < 0.05 && px[2] < 0.05 && solid(px)
}

/// What is under the layer.
fn blue(px: [f32; 4]) -> bool {
    px[2] > 0.9 && px[0] < 0.1 && px[1] < 0.1
}

/// One of a frame's lines, or the picture's own border, where a test
/// makes them green.
fn green(px: [f32; 4]) -> bool {
    px[1] > 0.5 && px[0] < 0.4 && px[2] < 0.4
}

/// One of a frame's lines where a test makes them blue.
fn bluish(px: [f32; 4]) -> bool {
    px[2] > 0.5 && px[0] < 0.4 && px[1] < 0.4
}

/// Dark: the plate of the Stream overlay's tab, a line frame's backing.
fn dark(px: [f32; 4]) -> bool {
    px[0] < 0.08 && px[1] < 0.08 && px[2] < 0.08
}

/// Black: what the art frame backs its window with.
fn black(px: [f32; 4]) -> bool {
    px[0] < 0.02 && px[1] < 0.02 && px[2] < 0.02
}

/// Nothing there: what is under the layer shows.
fn clear(px: [f32; 4]) -> bool {
    px[3] < 0.01
}

/// All there: nothing of what is under the layer shows.
fn solid(px: [f32; 4]) -> bool {
    px[3] > 0.99
}

/// How much of the picture `is`, 0 to 1.
fn share(pixels: &[[f32; 4]], is: impl Fn([f32; 4]) -> bool) -> f32 {
    pixels.iter().filter(|px| is(**px)).count() as f32 / pixels.len() as f32
}

/// Every frame's name, as the registry lists them.
fn frame_names(app: &VardaApp) -> Vec<String> {
    app.build_engine_state()
        .registry
        .frames
        .iter()
        .map(|frame| frame.name.clone())
        .collect()
}

/// A setting of one of the show's effects, as the state shows it.
fn setting(app: &VardaApp, effect: &str, name: &str) -> ParamValue {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .flat_map(|d| &d.effects)
        .find(|e| e.uuid == effect)
        .expect("the effect")
        .params
        .params
        .iter()
        .find(|p| p.name == name)
        .unwrap_or_else(|| panic!("no setting {name}"))
        .value
}

/// Whether the effect has a setting of that name.
fn has(app: &VardaApp, effect: &str, name: &str) -> bool {
    app.build_engine_state()
        .mixer
        .channels
        .iter()
        .flat_map(|c| &c.decks)
        .flat_map(|d| &d.effects)
        .find(|e| e.uuid == effect)
        .expect("the effect")
        .params
        .params
        .iter()
        .any(|p| p.name == name)
}

/// Set one of an effect's settings — one it has.
fn set(app: &mut VardaApp, effect: &str, name: &str, value: ParamValue) {
    assert!(has(app, effect, name), "no setting {name}");
    send_cmd(
        app,
        EngineCommand::SetEffectParam {
            effect_uuid: effect.to_string(),
            name: name.to_string(),
            value,
        },
    );
}

/// Ask for `name` on `layer` — or, with None, for no frame.
fn frame(app: &mut VardaApp, layer: &str, name: Option<&str>) -> CommandResult {
    send_cmd(
        app,
        EngineCommand::SetLayerFrame {
            deck_uuid: layer.to_string(),
            frame: name.map(str::to_string),
        },
    )
}

/// Put `layer` in the frame `name` as it comes — out of the frame it was
/// in first, or what was set on that one would come along: the frame's
/// effect.
fn framed(app: &mut VardaApp, layer: &str, name: &str) -> String {
    frame(app, layer, None);
    id(frame(app, layer, Some(name)))
}

/// Put `layer` in the frame `name` without its light and its glow — only
/// the picture and the frame itself are left to tell apart — and, whatever
/// the frame starts with, the picture not inside it and not filling it,
/// and a line frame without its backing: the frame's effect.
fn plain(app: &mut VardaApp, layer: &str, name: &str) -> String {
    let effect = framed(app, layer, name);
    set(app, &effect, "anim", ParamValue::Long(5));
    set(app, &effect, "glow", ParamValue::Float(0.0));
    set(app, &effect, "inside", ParamValue::Bool(false));
    set(app, &effect, "fill", ParamValue::Bool(false));
    if has(app, &effect, "backing") {
        set(app, &effect, "backing", ParamValue::Float(0.0));
    }
    effect
}

/// The same, a line frame's lines all of one `colour` — told apart so from
/// the red picture.
fn plain_lines(app: &mut VardaApp, layer: &str, name: &str, colour: [f32; 4]) -> String {
    let effect = plain(app, layer, name);
    set(app, &effect, "frame_colour", ParamValue::Color(colour));
    if has(app, &effect, "second_colour") {
        set(app, &effect, "second_colour", ParamValue::Color(colour));
    }
    effect
}

/// Give the layer's picture a green border `width` thick (in pixels of a
/// 1080-line picture) just inside its edge — the layer's Crop draws it,
/// before the frame — so where the picture's edge is can be seen.
fn green_border(app: &mut VardaApp, layer: &str, width: f32) {
    let crop = effect_named(app, layer, "crop");
    set(app, &crop, "border_mode", ParamValue::Bool(true));
    set(app, &crop, "border_color", ParamValue::Color(GREEN));
    set(app, &crop, "border_width", ParamValue::Float(width));
}

/// The HUD lines' outline as it comes (Frame size 0.5, Marks 0.7), from
/// the middle in picture heights; `scripts/make_line_frames.py` has the
/// sums.
mod hud_lines {
    /// Half the width of its lines' measure (`lw`): its thick lines are 1.6
    /// of it each side, its fine ones 0.55.
    pub const LINE: f32 = 0.004_75;

    /// Its top and bottom lines (`ob.y`) — but for the middle quarter,
    /// which is set in.
    pub const TOP: f32 = 0.413_75;

    /// The middle of the band of marks beyond them: empty right above and
    /// below the picture's middle.
    pub const MARKS: f32 = 0.464_25;

    /// Its sides (`ob.x`) in a picture `aspect` wide — thick from 0.21 to
    /// 0.36 above and below the middle.
    pub fn side(aspect: f32) -> f32 {
        aspect / 2.0 - 0.016_75
    }

    /// Its sides with the picture inside the frame, not filling it: as far
    /// out as the picture fitted between its top and bottom lines.
    pub fn side_inside(aspect: f32) -> f32 {
        (TOP - LINE) * aspect + LINE
    }
}

/// The frames are there: the HUD frame, which is a picture, and eleven of
/// lines.
#[test]
fn the_library_has_the_frames() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let names = frame_names(&app);
    for name in std::iter::once(&ART_FRAME).chain(&LINE_FRAMES) {
        assert!(names.iter().any(|n| n == *name), "{name} among {names:?}");
    }
    // One more is to be named above, to be tested with its kind.
    assert_eq!(names.len(), LINE_FRAMES.len() + 1, "these and no other: {names:?}");
}

/// Every frame, as it comes, has the picture in the middle of the layer to
/// a hair, and nothing of the layer clear — the frame's window is the whole
/// layer, the frame over the picture's edge, or round the picture inside
/// it on its backing. A frame that starts with the picture inside it and
/// no backing (the Neon ring) cuts the picture to its shape instead: clear
/// in the layer's corners.
#[test]
fn every_frame_as_it_comes_keeps_the_picture_whole() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let names = frame_names(&app);
    assert!(names.len() >= 12, "the frames: {names:?}");
    for name in &names {
        let effect = framed(&mut app, &layer, name);
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-as-it-comes"));
        for (x, y) in [(0.5, 0.5), (0.4, 0.4), (0.6, 0.6)] {
            let middle = at(&pixels, wide, high, x, y);
            assert!(just_red(middle), "{name}: the picture at {x}, {y}: {middle:?}");
        }
        let cut_out = has(&app, &effect, "backing")
            && matches!(setting(&app, &effect, "inside"), ParamValue::Bool(true))
            && matches!(setting(&app, &effect, "backing"), ParamValue::Float(v) if v < 0.5);
        if cut_out {
            for (x, y) in [(0.02, 0.03), (0.98, 0.03), (0.02, 0.97), (0.98, 0.97)] {
                let corner = at(&pixels, wide, high, x, y);
                assert!(clear(corner), "{name}: cut to its shape, clear at {x}, {y}: {corner:?}");
            }
            continue;
        }
        if matches!(setting(&app, &effect, "inside"), ParamValue::Bool(true)) {
            // Round the picture inside it, on its backing: little is
            // clear — a hair's width between the picture and a fine line
            // of the frame, the two tabs of the Stepped neon's outline.
            let thin = share(&pixels, |px| !solid(px));
            assert!(thin < 0.02, "{name}: {thin} of the layer is not all there");
            continue;
        }
        let thin = pixels.iter().position(|px| !solid(*px));
        assert!(
            thin.is_none(),
            "{name}: not all there at pixel {thin:?} of a picture {wide} wide"
        );
        // And what is under the layer shows nowhere.
        let (pixels, ..) = picture(&mut app);
        let under = share(&pixels, blue);
        assert!(under <= 0.0, "{name}: what is under shows through {under} of it");
    }
    assert_eq!(app.gpu_context().errors.fault_count(), 0, "no GPU error drawing them");
}

/// The picture under a frame — not inside it — is drawn exactly as it
/// comes: a picture with a hole in its middle (cut by the layer's Crop,
/// inverted; it still shows to all four edges, so the frame's window is
/// the whole layer) is, away from the frame's edge, pixel for pixel what
/// it is without a frame.
#[test]
fn the_picture_under_a_frame_is_as_without_one() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let crop = effect_named(&app, &layer, "crop");
    for (name, edge) in [("crop_left", 0.4), ("crop_right", 0.6), ("crop_bottom", 0.4), ("crop_top", 0.6)] {
        set(&mut app, &crop, name, ParamValue::Float(edge));
    }
    set(&mut app, &crop, "invert_crop", ParamValue::Float(1.0));
    let (bare, wide, high) = own(&mut app, &layer);
    let hole = at(&bare, wide, high, 0.41, 0.41);
    assert!(clear(hole), "cut away: {hole:?}");
    let kept = at(&bare, wide, high, 0.39, 0.39);
    assert!(just_red(kept), "kept: {kept:?}");
    for name in &frame_names(&app) {
        plain(&mut app, &layer, name);
        let (pixels, ..) = own(&mut app, &layer);
        let mut differ = 0_usize;
        let mut looked = 0_usize;
        for (i, (with, without)) in pixels.iter().zip(&bare).enumerate() {
            let (x, y) = ((i as u32 % wide) as f32 / wide as f32, (i as u32 / wide) as f32 / high as f32);
            if !((0.3..0.7).contains(&x) && (0.3..0.7).contains(&y)) {
                continue;
            }
            looked += 1;
            // As they show: what is clear has no colour to compare.
            let shown = |px: &[f32; 4]| [px[0] * px[3], px[1] * px[3], px[2] * px[3], px[3]];
            if shown(with).iter().zip(shown(without)).any(|(now, was)| (now - was).abs() > 0.004) {
                differ += 1;
            }
        }
        assert!(looked > 1000, "{name}: looked at {looked} pixels");
        assert_eq!(differ, 0, "{name}: {differ} of {looked} pixels are not as without a frame");
    }
}

/// Where the frame is in a picture: the middle, across and down (0 to 1),
/// of all that is neither clear, nor the red picture, nor that dimmed.
fn frame_middle(pixels: &[[f32; 4]], wide: u32, high: u32) -> (f32, f32) {
    let (mut left, mut right, mut top, mut bottom) = (wide, 0, high, 0);
    for (i, px) in pixels.iter().enumerate() {
        if px[3] < 0.5 || red(*px) || dimmed(*px) {
            continue;
        }
        let (x, y) = (i as u32 % wide, i as u32 / wide);
        left = left.min(x);
        right = right.max(x);
        top = top.min(y);
        bottom = bottom.max(y);
    }
    assert!(left <= right, "nothing of a frame in the picture");
    (
        (left + right + 1) as f32 / 2.0 / wide as f32,
        (top + bottom + 1) as f32 / 2.0 / high as f32,
    )
}

/// How many pixels left of `x` (0 to 1 across) are not clear.
fn left_of(pixels: &[[f32; 4]], wide: u32, x: f32) -> usize {
    pixels
        .iter()
        .enumerate()
        .filter(|(i, px)| ((*i as u32 % wide) as f32) < x * wide as f32 && !clear(**px))
        .count()
}

/// A frame sits round what shows of the layer's picture — here, what the
/// layer's Crop keeps: on a layer cropped at the left, nothing of the
/// frame or of the picture is left of that, the frame's own middle is the
/// kept part's, and the picture is in the kept part's middle.
#[test]
fn a_frame_sits_round_what_crop_keeps() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let crop = effect_named(&app, &layer, "crop");
    set(&mut app, &crop, "crop_left", ParamValue::Float(0.3));
    for name in &frame_names(&app) {
        plain_lines(&mut app, &layer, name, GREEN);
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-cropped"));
        let stray = left_of(&pixels, wide, 0.29);
        assert_eq!(stray, 0, "{name}: {stray} pixels left of what Crop keeps are not clear");
        // What is kept is from 0.3 to 1 across: its middle is at 0.65.
        let (across, down) = frame_middle(&pixels, wide, high);
        assert!(
            (across - 0.65).abs() < 0.02 && (down - 0.5).abs() < 0.02,
            "{name}: the frame's middle is at {across}, {down}"
        );
        let middle = at(&pixels, wide, high, 0.65, 0.5);
        assert!(just_red(middle), "{name}: the picture in the kept part's middle: {middle:?}");
    }

    // The art frame's band and the HUD lines' side are at the kept part's
    // left edge.
    plain(&mut app, &layer, ART_FRAME);
    let (pixels, wide, high) = own(&mut app, &layer);
    let band = at(&pixels, wide, high, 0.312, 0.5);
    assert!(solid(band) && !red(band), "the art's band at the kept part's edge: {band:?}");
    plain_lines(&mut app, &layer, "frame_hud_lines", GREEN);
    let (pixels, wide, high) = own(&mut app, &layer);
    let layer_aspect = aspect_of(wide, high);
    let flank = hud_lines::side(layer_aspect * 0.7);
    let on_line = at(&pixels, wide, high, 0.65 - flank / layer_aspect, 0.5 + 0.28);
    assert!(green(on_line) && solid(on_line), "the HUD lines' side at the kept part's edge: {on_line:?}");
}

/// A show on Main with two inputs: the first a red layer; the second a
/// blue one and, over it, a layer that repeats the first input. The first
/// input's red layer, and the layer repeating that input.
fn a_repeated_input(app: &mut VardaApp) -> (String, String) {
    send_cmd(
        app,
        EngineCommand::SetMixMode {
            mode: varda::mixer::MixMode::Program,
        },
    );
    let state = app.build_engine_state();
    let (first, second) = (
        state.mixer.channels[0].uuid.clone(),
        state.mixer.channels[1].uuid.clone(),
    );
    for deck in state.mixer.channels[..2].iter().flat_map(|c| &c.decks) {
        send_cmd(
            app,
            EngineCommand::RemoveDeck {
                deck_uuid: deck.uuid.clone(),
            },
        );
    }
    let shown = id(send_cmd(
        app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: first.clone(),
            color: [1.0, 0.0, 0.0, 1.0],
        },
    ));
    id(send_cmd(
        app,
        EngineCommand::AddSolidColorDeck {
            channel_uuid: second.clone(),
            color: [0.0, 0.0, 1.0, 1.0],
        },
    ));
    let repeating = id(send_cmd(
        app,
        EngineCommand::AddTapDeck {
            channel_uuid: second.clone(),
            source: varda::scene::TapSourceConfig::Channel { uuid: first },
        },
    ));
    send_cmd(
        app,
        EngineCommand::TakeToMain {
            source: MainSource::Channel(second),
            fade: false,
        },
    );
    (shown, repeating)
}

/// What shows need not be of the layer's own cropping: a layer that
/// repeats another input, whose own layer is cropped there, is clear
/// where that input is — and its frame sits round what shows of it.
#[test]
fn a_frame_sits_round_what_shows_of_a_repeated_input() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (shown, layer) = a_repeated_input(&mut app);
    // Cropped in the input it is repeated from; the layer's own Crop is
    // left as it came.
    let crop = effect_named(&app, &shown, "crop");
    set(&mut app, &crop, "crop_left", ParamValue::Float(0.3));
    // Over the second input's blue (a layer that only repeats an input is
    // drawn straight from it, with no picture of its own to look at).
    let (bare, wide, high) = picture_after(&mut app, 1, 2);
    let (cut_away, kept) = (at(&bare, wide, high, 0.15, 0.5), at(&bare, wide, high, 0.65, 0.5));
    assert!(blue(cut_away) && red(kept), "the repeated input: {cut_away:?}, {kept:?}");
    for name in &frame_names(&app) {
        plain_lines(&mut app, &layer, name, GREEN);
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-repeated"));
        let stray = left_of(&pixels, wide, 0.29);
        assert_eq!(stray, 0, "{name}: {stray} pixels left of what shows are not clear");
        let (across, down) = frame_middle(&pixels, wide, high);
        assert!(
            (across - 0.65).abs() < 0.02 && (down - 0.5).abs() < 0.02,
            "{name}: the frame's middle is at {across}, {down}"
        );
        let middle = at(&pixels, wide, high, 0.65, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle of what shows: {middle:?}");
    }
}

/// Cropping a layer already in a frame moves the frame, the very frame
/// the picture is cropped: the frame finds what shows each time it is
/// drawn.
#[test]
fn cropping_after_the_frame_was_set_moves_it_the_same_frame() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let crop = effect_named(&app, &layer, "crop");
    for name in &frame_names(&app) {
        for edge in ["crop_left", "crop_bottom"] {
            set(&mut app, &crop, edge, ParamValue::Float(0.0));
        }
        plain_lines(&mut app, &layer, name, GREEN);
        let (pixels, wide, high) = own(&mut app, &layer);
        let (across, down) = frame_middle(&pixels, wide, high);
        assert!(
            (across - 0.5).abs() < 0.02 && (down - 0.5).abs() < 0.02,
            "{name}: round the whole layer, the frame's middle is at {across}, {down}"
        );
        // A third off the left, a fifth off the bottom: what is kept is
        // from 0.3 to 1 across and from 0 to 0.8 down. One frame drawn.
        set(&mut app, &crop, "crop_left", ParamValue::Float(0.3));
        set(&mut app, &crop, "crop_bottom", ParamValue::Float(0.2));
        let (pixels, wide, high) = own_after(&mut app, &layer, 1);
        let (across, down) = frame_middle(&pixels, wide, high);
        assert!(
            (across - 0.65).abs() < 0.02 && (down - 0.4).abs() < 0.02,
            "{name}: round what Crop keeps, the frame's middle is at {across}, {down}"
        );
        let stray = left_of(&pixels, wide, 0.29);
        assert_eq!(stray, 0, "{name}: {stray} pixels left of what Crop keeps are not clear");
        let below = at(&pixels, wide, high, 0.65, 0.9);
        assert!(clear(below), "{name}: clear below what Crop keeps: {below:?}");
        let middle = at(&pixels, wide, high, 0.65, 0.4);
        assert!(just_red(middle), "{name}: the picture in the kept part's middle: {middle:?}");
    }
}

/// Crop turned off, all of the picture shows again, and the frame has the
/// whole layer back; on again, what Crop keeps.
#[test]
fn crop_turned_off_gives_the_frame_the_whole_layer() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let crop = effect_named(&app, &layer, "crop");
    set(&mut app, &crop, "crop_left", ParamValue::Float(0.3));
    for name in [ART_FRAME, "frame_hud_lines"] {
        plain_lines(&mut app, &layer, name, GREEN);
        let (pixels, wide, high) = own(&mut app, &layer);
        let (across, _) = frame_middle(&pixels, wide, high);
        assert!((across - 0.65).abs() < 0.02, "{name}: round what Crop keeps: {across}");

        send_cmd(
            &mut app,
            EngineCommand::ToggleEffect {
                effect_uuid: crop.clone(),
            },
        );
        let (pixels, wide, high) = own(&mut app, &layer);
        let (across, _) = frame_middle(&pixels, wide, high);
        assert!((across - 0.5).abs() < 0.02, "{name}: Crop off, round the whole layer: {across}");
        let thin = share(&pixels, |px| !solid(px));
        assert!(thin <= 0.0, "{name}: Crop off, {thin} of the layer is not all there");

        send_cmd(
            &mut app,
            EngineCommand::ToggleEffect {
                effect_uuid: crop.clone(),
            },
        );
        let (pixels, wide, high) = own(&mut app, &layer);
        let (across, _) = frame_middle(&pixels, wide, high);
        assert!((across - 0.65).abs() < 0.02, "{name}: Crop on again, round what it keeps: {across}");
    }
}

/// A picture with nothing showing — a crop that keeps nothing — has no
/// frame either: every frame, as it comes, draws nothing.
#[test]
fn a_frame_round_a_clear_picture_draws_nothing() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let crop = effect_named(&app, &layer, "crop");
    set(&mut app, &crop, "crop_left", ParamValue::Float(0.6));
    set(&mut app, &crop, "crop_right", ParamValue::Float(0.4));
    let (bare, ..) = own(&mut app, &layer);
    let showing = share(&bare, |px| !clear(px));
    assert!(showing <= 0.0, "nothing kept, and {showing} of the picture shows");
    for name in &frame_names(&app) {
        framed(&mut app, &layer, name);
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-nothing"));
        let drawn = share(&pixels, |px| !clear(px));
        assert!(drawn <= 0.0, "{name}: round nothing, {drawn} of the layer is drawn on");
    }
}

/// Backing is a line frame's dark plate outside its outline: with it,
/// what was the picture dimmed there, or — the picture inside the frame —
/// clear, is the plate; never outside the frame's window.
#[test]
fn backing_is_a_dark_plate_outside_a_line_frames_outline() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let plate = |px: [f32; 4]| dark(px) && solid(px);
    for name in LINE_FRAMES {
        // Without it (some of the Stream overlay is dark as it is).
        let effect = plain_lines(&mut app, &layer, name, GREEN);
        let (pixels, ..) = own(&mut app, &layer);
        let bare = share(&pixels, plate);
        set(&mut app, &effect, "inside", ParamValue::Bool(true));
        let (pixels, ..) = own(&mut app, &layer);
        let gone = share(&pixels, clear);

        set(&mut app, &effect, "backing", ParamValue::Float(1.0));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-inside-backed"));
        let (backed, left_clear) = (share(&pixels, plate), share(&pixels, clear));
        assert!(
            backed > bare + 0.01 && left_clear < gone - 0.01,
            "{name}: inside the frame, {backed} of the layer is the plate ({bare} without), {left_clear} clear ({gone} without)"
        );
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");

        set(&mut app, &effect, "inside", ParamValue::Bool(false));
        let (pixels, wide, high) = own(&mut app, &layer);
        let (backed, dim) = (share(&pixels, plate), share(&pixels, dimmed));
        // (The soft edge between the plate and the picture is as dim, a
        // pixel wide.)
        assert!(
            backed > bare + 0.01 && dim < 0.005,
            "{name}: not inside, {backed} of the layer is the plate ({bare} without), {dim} the picture dimmed"
        );
        let thin = share(&pixels, |px| !solid(px));
        assert!(thin <= 0.0, "{name}: {thin} of the layer is not all there");
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");

        set(&mut app, &effect, "window", ParamValue::Float(0.6));
        let (pixels, wide, high) = own(&mut app, &layer);
        for (x, y) in [(0.08, 0.5), (0.5, 0.92)] {
            let beyond = at(&pixels, wide, high, x, y);
            assert!(clear(beyond), "{name}: no plate outside the window at {x}, {y}: {beyond:?}");
        }
    }
}

/// Size makes the frame's window smaller, round the middle: at 0.6 the
/// layer is clear well outside it, the picture still in the middle and
/// nothing clear well inside.
#[test]
fn size_makes_the_window_smaller_and_clear_outside() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    for name in &frame_names(&app) {
        let effect = plain(&mut app, &layer, name);
        set(&mut app, &effect, "window", ParamValue::Float(0.6));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-size-0.6"));
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");
        // The window is 0.6 of the layer each way: from 0.2 to 0.8.
        for (x, y) in [(0.08, 0.5), (0.92, 0.5), (0.5, 0.08), (0.5, 0.92), (0.1, 0.1), (0.9, 0.9)] {
            let outside = at(&pixels, wide, high, x, y);
            assert!(clear(outside), "{name}: clear outside the window at {x}, {y}: {outside:?}");
        }
        for (x, y) in [(0.24, 0.5), (0.76, 0.5), (0.5, 0.24), (0.5, 0.76)] {
            let inside = at(&pixels, wide, high, x, y);
            assert!(solid(inside), "{name}: all there inside the window at {x}, {y}: {inside:?}");
        }
        // What is under the layer shows outside the window.
        let (pixels, wide, high) = picture(&mut app);
        let under = at(&pixels, wide, high, 0.08, 0.5);
        assert!(blue(under), "{name}: what is under, outside the window: {under:?}");
    }
}

/// The frame is drawn over the picture's edge: nothing of the picture
/// shows on the art frame's band, nor on a line frame's lines — which are
/// round the layer's edge, none of them in its middle.
#[test]
fn a_frame_is_drawn_over_the_pictures_edge() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);

    // The art is solid for 3.5% of the frame's height in from its edge.
    plain(&mut app, &layer, ART_FRAME);
    let (pixels, wide, high) = own(&mut app, &layer);
    save(&pixels, wide, high, "frame_hud-plain");
    for (x, y) in [(0.5, 0.015), (0.5, 0.985), (0.012, 0.5), (0.988, 0.5)] {
        let band = at(&pixels, wide, high, x, y);
        assert!(solid(band) && !red(band), "the art over the picture at {x}, {y}: {band:?}");
    }
    let opening = at(&pixels, wide, high, 0.5, 0.07);
    assert!(just_red(opening), "the picture in the art's opening: {opening:?}");

    for name in LINE_FRAMES {
        plain_lines(&mut app, &layer, name, GREEN);
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-green"));
        let mut lines = 0_usize;
        for (i, px) in pixels.iter().enumerate() {
            // Anything of the frame's colour: its lines, soft edges too.
            if px[1] < 0.25 {
                continue;
            }
            lines += 1;
            let (x, y) = ((i as u32 % wide) as f32 / wide as f32, (i as u32 / wide) as f32 / high as f32);
            assert!(
                !((0.3..0.7).contains(&x) && (0.3..0.7).contains(&y)),
                "{name}: a line in the middle at {x}, {y}"
            );
        }
        let of_all = lines as f32 / pixels.len() as f32;
        assert!(
            (0.0005..0.2).contains(&of_all),
            "{name}: {:.3}% of the picture is its lines",
            of_all * 100.0
        );
    }

    // On one of the HUD lines' own: the thick part of its top and bottom
    // lines, which is the frame's colour and nothing of the picture.
    plain_lines(&mut app, &layer, "frame_hud_lines", GREEN);
    let (pixels, wide, high) = own(&mut app, &layer);
    let along = 0.6 * hud_lines::side(aspect_of(wide, high));
    for (x, y) in [(along, -hud_lines::TOP), (-along, hud_lines::TOP)] {
        let on_line = from_middle(&pixels, wide, high, x, y);
        assert!(green(on_line) && solid(on_line), "the HUD lines' line at {x}, {y}: {on_line:?}");
        let beside = from_middle(&pixels, wide, high, x, y * 0.9);
        assert!(just_red(beside), "the picture inside it at {x}, {}: {beside:?}", y * 0.9);
    }
}

/// Shape makes the window square whatever the picture: clear to the left
/// and right of it, the frame and the picture within.
#[test]
fn shape_makes_the_window_square() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    for name in &frame_names(&app) {
        let effect = plain(&mut app, &layer, name);
        set(&mut app, &effect, "shape", ParamValue::Long(3));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-square"));
        // The window is as wide as the layer is high, in the middle.
        let left = 0.5 - 0.5 / aspect_of(wide, high);
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");
        for (x, y) in [(left / 2.0, 0.5), (1.0 - left / 2.0, 0.5), (left - 0.03, 0.1), (1.03 - left, 0.9)] {
            let beside = at(&pixels, wide, high, x, y);
            assert!(clear(beside), "{name}: clear beside a square window at {x}, {y}: {beside:?}");
        }
        for x in [left + 0.02, 0.98 - left] {
            let within = at(&pixels, wide, high, x, 0.5);
            assert!(solid(within), "{name}: the window at {x}, 0.5: {within:?}");
        }
    }
}

/// Fill the frame: off, the whole picture shows inside a window of another
/// shape — a wide picture in a square one leaves room above and below it,
/// which the art frame backs with black and a line frame leaves clear; on,
/// the picture covers the window, and is still cut to it.
#[test]
fn fill_has_the_picture_cover_the_window() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    for name in &frame_names(&app) {
        let effect = plain(&mut app, &layer, name);
        set(&mut app, &effect, "shape", ParamValue::Long(3));
        let (pixels, wide, high) = own(&mut app, &layer);
        // The whole picture, as wide as the square window, is 9/16 of its
        // height: from 0.219 to 0.781 down the layer. (Looked at above and
        // below it both near the frame and farther in: a line frame draws
        // the pixels deep inside its window by a shorter way.)
        let left = 0.5 - 0.5 / aspect_of(wide, high);
        for y in [0.25, 0.75] {
            let whole = at(&pixels, wide, high, 0.5, y);
            assert!(just_red(whole), "{name}: the whole picture at 0.5, {y}: {whole:?}");
        }
        for y in [0.12, 0.16, 0.84, 0.88] {
            let room = at(&pixels, wide, high, 0.5, y);
            if name == ART_FRAME {
                assert!(black(room) && solid(room), "{name}: backed with black at 0.5, {y}: {room:?}");
            } else {
                assert!(clear(room), "{name}: clear above and below the picture at 0.5, {y}: {room:?}");
            }
        }

        set(&mut app, &effect, "fill", ParamValue::Bool(true));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-square-filled"));
        for y in [0.12, 0.16, 0.5, 0.84, 0.88] {
            let filled = at(&pixels, wide, high, 0.5, y);
            assert!(just_red(filled), "{name}: the picture fills the window at 0.5, {y}: {filled:?}");
        }
        for x in [left / 2.0, 1.0 - left / 2.0] {
            let beside = at(&pixels, wide, high, x, 0.5);
            assert!(clear(beside), "{name}: and is cut to it at {x}, 0.5: {beside:?}");
        }
    }
}

/// Picture inside the frame, the art frame: off, the picture's edge is
/// under the art's band — a border round the picture does not show; on,
/// the picture is scaled into the art's opening, its border showing just
/// inside the band.
#[test]
fn picture_inside_the_art_frame_is_scaled_into_its_opening() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    // 30 of 1080: 2.8% of the picture's height, under a band solid for 3.5%.
    green_border(&mut app, &layer, 30.0);
    let (pixels, wide, high) = own(&mut app, &layer);
    let border = at(&pixels, wide, high, 0.5, 0.014);
    assert!(green(border), "the picture's own border: {border:?}");

    let effect = plain(&mut app, &layer, ART_FRAME);
    let (pixels, ..) = own(&mut app, &layer);
    let showing = share(&pixels, green);
    assert!(showing <= 0.0, "the picture's border under the band: {showing} of it shows");

    set(&mut app, &effect, "inside", ParamValue::Bool(true));
    let (pixels, wide, high) = own(&mut app, &layer);
    save(&pixels, wide, high, "frame_hud-inside");
    // The opening starts 3.6% down; the picture, scaled to 0.928 into it,
    // has its border from there to 6.2%, and the art's glow ends at 5.8%.
    for y in [0.057, 0.943] {
        let inside = at(&pixels, wide, high, 0.5, y);
        assert!(green(inside) && solid(inside), "the picture's border in the opening at 0.5, {y}: {inside:?}");
    }
    for (x, y) in [(0.5, 0.015), (0.5, 0.985), (0.012, 0.5), (0.988, 0.5)] {
        let band = at(&pixels, wide, high, x, y);
        assert!(solid(band) && !green(band) && !red(band), "the band at {x}, {y}: {band:?}");
    }
    let middle = at(&pixels, wide, high, 0.5, 0.5);
    assert!(just_red(middle), "the picture in the middle: {middle:?}");
}

/// A line frame, the picture not inside it: the picture fills the window,
/// dimmed outside the frame's outline, nothing clear. Picture inside the
/// frame: it is cut to the outline — clear outside, nothing dimmed — and,
/// filling the frame too, shows nowhere outside the outline.
#[test]
fn a_line_frame_dims_the_picture_outside_its_outline_or_cuts_it() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    for name in LINE_FRAMES {
        let effect = plain_lines(&mut app, &layer, name, GREEN);
        let (outside, wide, high) = own(&mut app, &layer);
        let dim = share(&outside, dimmed);
        assert!(dim > 0.01, "{name}: {dim} of the picture is dimmed, outside the outline");
        let thin = share(&outside, |px| !solid(px));
        assert!(thin <= 0.0, "{name}: {thin} of the layer is not all there");
        let middle = at(&outside, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");

        set(&mut app, &effect, "inside", ParamValue::Bool(true));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-inside"));
        let dim = share(&pixels, dimmed);
        assert!(dim < 0.001, "{name}: {dim} of the picture is dimmed with it inside the frame");
        let gone = share(&pixels, clear);
        assert!(gone > 0.01, "{name}: {gone} of the layer is clear, outside the outline");
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");

        // Filling the frame the picture reaches past the outline, which is
        // where it was before: none of it shows where it was dimmed.
        set(&mut app, &effect, "fill", ParamValue::Bool(true));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-inside-filled"));
        let past = pixels
            .iter()
            .zip(&outside)
            .filter(|(now, before)| dimmed(**before) && red(**now) && now[3] > 0.5)
            .count();
        assert_eq!(past, 0, "{name}: {past} pixels of the picture show outside the outline");
        let middle = at(&pixels, wide, high, 0.5, 0.5);
        assert!(just_red(middle), "{name}: the picture in the middle: {middle:?}");
    }

    // The HUD lines, in the band of marks above and below its outline: the
    // picture dimmed; and, inside the frame and filling it — it reaches
    // past the outline there — cut away.
    let effect = plain_lines(&mut app, &layer, "frame_hud_lines", GREEN);
    let (pixels, wide, high) = own(&mut app, &layer);
    for y in [-hud_lines::MARKS, hud_lines::MARKS] {
        let beyond = from_middle(&pixels, wide, high, 0.0, y);
        assert!(dimmed(beyond), "dimmed outside the outline at 0, {y}: {beyond:?}");
    }
    set(&mut app, &effect, "inside", ParamValue::Bool(true));
    set(&mut app, &effect, "fill", ParamValue::Bool(true));
    let (pixels, wide, high) = own(&mut app, &layer);
    for y in [-hud_lines::MARKS, hud_lines::MARKS] {
        let beyond = from_middle(&pixels, wide, high, 0.0, y);
        assert!(clear(beyond), "cut to the outline at 0, {y}: {beyond:?}");
    }
    let middle = at(&pixels, wide, high, 0.5, 0.5);
    assert!(just_red(middle), "the picture in the middle: {middle:?}");
}

/// How many rows of the picture's green border show on the way down from
/// the layer's top to its middle, two fifths of the way across — clear of
/// what the frames have in the middle of their top.
fn border_rows(pixels: &[[f32; 4]], wide: u32, high: u32) -> f32 {
    let columns = (wide * 39 / 100)..(wide * 41 / 100);
    let seen = (0..high / 2)
        .flat_map(|y| columns.clone().map(move |x| (x, y)))
        .filter(|(x, y)| {
            let px = pixels[(y * wide + x) as usize];
            green(px) && solid(px)
        })
        .count();
    seen as f32 / columns.len() as f32
}

/// Picture inside the frame, a line frame: the whole picture is fitted
/// into the outline — a border round the picture shows inside the frame's
/// lines, where not inside it is under them or dimmed outside them.
#[test]
fn picture_inside_a_line_frame_shows_the_whole_picture() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    // 60 of 1080: 5.6% of the picture's height.
    green_border(&mut app, &layer, 60.0);
    for name in LINE_FRAMES {
        let effect = plain_lines(&mut app, &layer, name, BLUE);
        let (pixels, wide, high) = own(&mut app, &layer);
        let not_inside = border_rows(&pixels, wide, high);
        set(&mut app, &effect, "inside", ParamValue::Bool(true));
        let (pixels, wide, high) = own(&mut app, &layer);
        save(&pixels, wide, high, &format!("{name}-inside-border"));
        let inside = border_rows(&pixels, wide, high);
        assert!(
            inside > 0.02 * high as f32 && inside > 1.3 * not_inside,
            "{name}: {inside} rows of the picture's border show with it inside the frame, {not_inside} without"
        );
    }
}

/// Picture inside the HUD lines: the picture's border shows just inside
/// the outline's lines, and the frame is built as wide as the picture so
/// fitted, its sides at the picture's.
#[test]
fn picture_inside_the_hud_lines_is_fitted_into_its_outline() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    // 60 of 1080: 5.6% of the picture's height.
    green_border(&mut app, &layer, 60.0);
    let effect = plain_lines(&mut app, &layer, "frame_hud_lines", BLUE);
    let (pixels, wide, high) = own(&mut app, &layer);
    let aspect = aspect_of(wide, high);
    let flank = hud_lines::side_inside(aspect);
    // Just inside the outline's top and bottom lines: where the fitted
    // picture's border is, from 0.364 to the line's own edge at 0.406.
    let under_line = [(0.6 * flank, -0.385), (-0.6 * flank, 0.385)];
    // On the outline's sides once it is as wide as the fitted picture.
    let on_side = [(flank, 0.28), (-flank, -0.28)];
    for (x, y) in under_line.into_iter().chain(on_side) {
        let whole = from_middle(&pixels, wide, high, x, y);
        assert!(just_red(whole), "not inside, the picture's middle at {x}, {y}: {whole:?}");
    }

    set(&mut app, &effect, "inside", ParamValue::Bool(true));
    let (pixels, wide, high) = own(&mut app, &layer);
    for (x, y) in under_line {
        let border = from_middle(&pixels, wide, high, x, y);
        assert!(green(border) && solid(border), "the picture's border inside the outline at {x}, {y}: {border:?}");
    }
    for (x, y) in on_side {
        let on_line = from_middle(&pixels, wide, high, x, y);
        assert!(bluish(on_line) && solid(on_line), "the outline's side at the picture's, {x}, {y}: {on_line:?}");
        let beyond = from_middle(&pixels, wide, high, x * 1.08, y);
        assert!(clear(beyond), "clear beyond it at {}, {y}: {beyond:?}", x * 1.08);
        let within = from_middle(&pixels, wide, high, x * 0.9, y);
        assert!(red(within) && solid(within), "the picture within it at {}, {y}: {within:?}", x * 0.9);
    }
}

/// The Stream overlay is a solid frame, not lines: a band over the
/// picture's edge — thin at the top and sides, some 2.6 times as thick at
/// the bottom — and a dark tab above the bottom's middle, for a name.
#[test]
fn the_stream_overlay_is_a_band_with_a_tab() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    plain_lines(&mut app, &layer, "frame_overlay", GREEN);
    let (pixels, wide, high) = own(&mut app, &layer);
    save(&pixels, wide, high, "frame_overlay-green");
    // From the middle in picture heights (`scripts/make_line_frames.py`):
    // the band is from 0.479 to 0.49 at the top, 0.461 to 0.49 at the bottom.
    for (x, y) in [(-0.5, -0.4845), (-0.5, 0.47), (0.5, 0.4757)] {
        let band = from_middle(&pixels, wide, high, x, y);
        assert!(green(band) && solid(band), "the band at {x}, {y}: {band:?}");
    }
    let under_top = from_middle(&pixels, wide, high, -0.5, -0.47);
    assert!(just_red(under_top), "as far in from the top, the picture: {under_top:?}");
    // The tab: from 0.427 to the band, 0.28 to each side of the middle.
    let tab = from_middle(&pixels, wide, high, 0.0, 0.4444);
    assert!(dark(tab) && solid(tab), "the tab's dark plate: {tab:?}");
    let beside = from_middle(&pixels, wide, high, 0.5, 0.4444);
    assert!(just_red(beside), "beside the tab, the picture: {beside:?}");
}

/// The Neon ring starts with the picture inside it and filling it: the
/// picture is cut to a circle nearly as high as the layer, clear outside.
#[test]
fn the_neon_ring_cuts_the_picture_to_a_circle() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    // As it comes, but for its light and its glow.
    let effect = framed(&mut app, &layer, "frame_ring");
    for name in ["inside", "fill"] {
        let starts = setting(&app, &effect, name);
        assert!(matches!(starts, ParamValue::Bool(true)), "it starts with {name} on: {starts:?}");
    }
    set(&mut app, &effect, "anim", ParamValue::Long(5));
    set(&mut app, &effect, "glow", ParamValue::Float(0.0));
    let (pixels, wide, high) = own(&mut app, &layer);
    save(&pixels, wide, high, "frame_ring-plain");
    // The rings are 0.465 of the layer's height from the middle.
    for (x, y) in [(0.0, 0.0), (0.42, 0.0), (-0.42, 0.0), (0.0, -0.42), (0.0, 0.42), (0.3, 0.3), (-0.3, -0.3)] {
        let within = from_middle(&pixels, wide, high, x, y);
        assert!(just_red(within), "the picture in the circle at {x}, {y}: {within:?}");
    }
    for (x, y) in [(0.51, 0.0), (-0.51, 0.0), (0.37, 0.37), (-0.37, 0.37), (0.37, -0.37), (0.8, 0.45)] {
        let beyond = from_middle(&pixels, wide, high, x, y);
        assert!(clear(beyond), "clear outside the circle at {x}, {y}: {beyond:?}");
    }
}

const W: u32 = 1280;
const H: u32 = 720;

/// A red picture in one frame, alone on a mixer and drawn a frame at a
/// time: for what a frame does as time goes, and at other sizes.
struct Framed<'a> {
    ctx: &'a GpuContext,
    mixer: Mixer,
    drawn: u32,
}

impl<'a> Framed<'a> {
    fn new(ctx: &'a GpuContext, path: &Path) -> Self {
        Self::sized(ctx, path, W, H, |_| ())
    }

    /// In a layer `wide` × `high`, the picture cut first by a Crop set by
    /// `crop` — as a layer's own is, before its frame.
    fn sized(
        ctx: &'a GpuContext,
        path: &Path,
        wide: u32,
        high: u32,
        crop: impl FnOnce(&mut ShaderParams),
    ) -> Self {
        let load = |path: &Path| {
            let shader = ISFShader::from_file(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
            Effect::new(ctx, shader).unwrap_or_else(|e| panic!("{}: {e:#}", path.display()))
        };
        let mut deck = Deck::new_solid_color(ctx, [1.0, 0.0, 0.0, 1.0], wide, high).expect("deck");
        let mut cut = load(&shader_file("crop"));
        crop(&mut cut.params);
        deck.add_effect(cut);
        deck.add_effect(load(path));
        let mut mixer = Mixer::new(ctx, wide, high).expect("mixer");
        mixer.channel_mut(0).expect("a channel").add_deck(deck);
        Self { ctx, mixer, drawn: 0 }
    }

    /// The frame's effect: the second, after Crop.
    fn frame(&mut self) -> &mut Effect {
        &mut self.mixer.channel_mut(0).expect("a channel").decks[0]
            .deck
            .effects[1]
    }

    /// The frame's settings.
    fn settings(&mut self) -> &mut ShaderParams {
        &mut self.frame().params
    }

    /// Draw the next frame, a sixtieth of a second on.
    fn step(&mut self) {
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
            free_run_time: Some(self.drawn as f32 / 60.0),
        };
        self.mixer.render(self.ctx, &inputs, 60, &[]).expect("render");
        self.drawn += 1;
    }

    /// Draw the next frame and read the layer's own picture back.
    fn draw(&mut self) -> Vec<[f32; 4]> {
        self.step();
        let deck = &self.mixer.channel(0).expect("a channel").decks[0].deck;
        frame_pixels(self.ctx, &deck.texture)
    }
}

/// Every frame's shader file, by name.
fn frame_files() -> Vec<PathBuf> {
    frames_in(&Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders"))
}

/// The frames' shader files in the folder `dir`, by name.
fn frames_in(dir: &Path) -> Vec<PathBuf> {
    let mut frames: Vec<PathBuf> = std::fs::read_dir(dir)
        .expect("shaders dir")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension().is_some_and(|ext| ext == "fs")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("frame_"))
        })
        .collect();
    frames.sort();
    frames
}

/// The shader file of the effect `name`.
fn shader_file(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("shaders")
        .join(format!("{name}.fs"))
}

fn stem(path: &Path) -> &str {
    path.file_stem().and_then(|name| name.to_str()).unwrap_or_default()
}

/// How many pixels of two pictures are not the same.
fn changed(one: &[[f32; 4]], other: &[[f32; 4]]) -> usize {
    differ_by(one, other, 0.02)
}

/// How many pixels of two pictures differ by more than `by`, as they show.
fn differ_by(one: &[[f32; 4]], other: &[[f32; 4]], by: f32) -> usize {
    let shown = |px: &[f32; 4]| [px[0] * px[3], px[1] * px[3], px[2] * px[3], px[3]];
    one.iter()
        .zip(other)
        .filter(|(was, now)| shown(was).iter().zip(shown(now)).any(|(p, q)| (p - q).abs() > by))
        .count()
}

/// The frame finds what shows in a layer of any size — smaller and larger
/// than the size its passes are made for, and one of odd numbers: round a
/// picture cropped at the left and the bottom, nothing left of it, its
/// middle the kept part's, its left edge within a few pixels of the crop's.
#[test]
fn a_frame_finds_what_shows_in_a_layer_of_any_size() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    for (wide, high) in [(1280_u32, 720_u32), (1281, 721), (3840, 2160)] {
        for name in [ART_FRAME, "frame_hud_lines"] {
            let mut framed = Framed::sized(&ctx, &shader_file(name), wide, high, |crop| {
                crop.set_float("crop_left", 0.3);
                crop.set_float("crop_bottom", 0.2);
            });
            let settings = framed.settings();
            settings.set_color("frame_colour", GREEN);
            settings.set_long("anim", 5);
            settings.set_float("glow", 0.0);
            settings.set_bool("inside", false);
            settings.set_float("backing", 0.0);
            settings.set_float("speed", 0.0);
            let pixels = framed.draw();
            let stray = left_of(&pixels, wide, 0.29);
            assert_eq!(stray, 0, "{name} at {wide}x{high}: {stray} pixels left of what Crop keeps are not clear");
            let (across, down) = frame_middle(&pixels, wide, high);
            assert!(
                (across - 0.65).abs() < 0.02 && (down - 0.4).abs() < 0.02,
                "{name} at {wide}x{high}: the frame's middle is at {across}, {down}"
            );
            let middle = at(&pixels, wide, high, 0.65, 0.4);
            assert!(just_red(middle), "{name} at {wide}x{high}: the picture in the kept part's middle: {middle:?}");
            // The first column with anything in it, half way down what is kept.
            let row = (0.4 * high as f32) as u32 * wide;
            let first = (0..wide)
                .find(|x| !clear(pixels[(row + x) as usize]))
                .expect("something in the row");
            let off = first as f32 - 0.3 * wide as f32;
            assert!(
                off.abs() <= 4.0,
                "{name} at {wide}x{high}: the frame begins {off} pixels from the crop's edge"
            );
        }
    }
}

/// The frame's shader at `path` with its way out for the pixels deep inside
/// the picture taken away — every pixel drawn the long way — written into
/// `dir` (the art frame's picture beside it): `None` for a frame that has
/// no such way out.
fn the_long_way(path: &Path, dir: &Path) -> Option<PathBuf> {
    const END: &str = "        return;\n    }\n";
    let source = std::fs::read_to_string(path).expect("the shader");
    let begins = source
        .find("    if (inside == 0u && -max(edge.x, edge.y) > ")
        .or_else(|| source.find("    if (max(from_open.x, from_open.y) < -"))?;
    let ends = begins + source[begins..].find(END).expect("the way out's end") + END.len();
    let long = dir.join(path.file_name()?);
    std::fs::write(&long, format!("{}{}", &source[..begins], &source[ends..])).expect("written");
    if source.contains("\"IMPORTED\"") {
        let art = dir.join("frames");
        std::fs::create_dir_all(&art).expect("a folder");
        for entry in std::fs::read_dir(path.parent()?.join("frames")).expect("the art") {
            let from = entry.expect("a file").path();
            std::fs::copy(&from, art.join(from.file_name()?)).expect("copied");
        }
    }
    Some(long)
}

/// A frame leaves at once the pixels deep inside the picture, where
/// nothing of it is drawn: that changes nothing of the picture — with a
/// hole in its middle here, whole in the window and filling a square one —
/// from what the same shader draws the long way.
#[test]
fn the_way_out_deep_inside_the_picture_changes_nothing() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let dir = tempfile::TempDir::new().expect("a folder");
    let mut compared = 0;
    for path in &frame_files() {
        let name = stem(path);
        let Some(long) = the_long_way(path, dir.path()) else {
            continue;
        };
        compared += 1;
        for filled in [false, true] {
            let draw = |file: &Path| {
                let mut framed = Framed::sized(&ctx, file, W, H, |crop| {
                    for (edge, to) in [("crop_left", 0.4), ("crop_right", 0.6), ("crop_bottom", 0.4), ("crop_top", 0.6)] {
                        crop.set_float(edge, to);
                    }
                    crop.set_float("invert_crop", 1.0);
                });
                let settings = framed.settings();
                settings.set_bool("inside", false);
                settings.set_bool("fill", filled);
                settings.set_long("shape", if filled { 3 } else { 0 });
                settings.set_float("speed", 0.0);
                framed.draw()
            };
            let (short_way, long_way) = (draw(path), draw(&long));
            let hole = at(&short_way, W, H, 0.5, 0.5);
            assert!(clear(hole), "{name}: the hole in the picture's middle: {hole:?}");
            let differ = differ_by(&short_way, &long_way, 0.002);
            assert_eq!(differ, 0, "{name}, filled {filled}: {differ} pixels are not as drawn the long way");
        }
    }
    assert!(compared >= 11, "the frames with a way out: {compared}");
}

/// Every frame moves as time goes at Speed 1, and at Speed 0 stands still
/// where it got to.
#[test]
fn every_frame_moves_and_at_speed_zero_stands_still() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let files = frame_files();
    assert!(files.len() >= 5, "the frames: {files:?}");
    for path in &files {
        let name = stem(path);
        let mut framed = Framed::new(&ctx, path);
        let first = framed.draw();
        // Half a second on.
        for _ in 0..29 {
            framed.step();
        }
        let later = framed.draw();
        let moved = changed(&first, &later);
        assert!(moved > 200, "{name}: hardly moved in half a second ({moved} pixels)");
        framed.settings().set_float("speed", 0.0);
        for _ in 0..29 {
            framed.step();
        }
        let stopped = framed.draw();
        let still = changed(&later, &stopped);
        assert_eq!(still, 0, "{name}: moved at Speed 0 ({still} pixels)");
    }
}

/// One frame of the frame `name`, its light `turn` of the way round the
/// window (0 the top left corner, clockwise), its settings set by `setup`.
fn with_light_at(
    ctx: &GpuContext,
    name: &str,
    turn: f32,
    setup: impl FnOnce(&mut ShaderParams),
) -> Vec<[f32; 4]> {
    let mut framed = Framed::new(ctx, &shader_file(name));
    setup(framed.settings());
    // Standing still, its time put there: the light goes 0.085 of the way
    // round a second, and the frame's phase counts tenths of seconds.
    framed.settings().set_float("speed", 0.0);
    framed.frame().phase_accumulators[0] = turn / 0.085 / 10.0;
    framed.draw()
}

/// Where the light is, a line is the light's colour, not the frame's: the
/// HUD lines' top line with the light there, and its bottom line the
/// frame's colour; then the other way round.
#[test]
fn where_the_light_is_a_line_is_the_lights_colour() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let aspect = aspect_of(W, H);
    let along = 0.6 * hud_lines::side(aspect);
    // How far round the window those two places are: along the top from the
    // left, and — half way round from there — back along the bottom.
    let round = 2.0 * aspect + 2.0;
    let top = (along + aspect / 2.0) / round;
    let bottom = top + 0.5;
    let draw = |turn: f32| {
        with_light_at(&ctx, "frame_hud_lines", turn, |settings| {
            settings.set_color("frame_colour", GREEN);
            settings.set_float("glow", 0.0);
            settings.set_bool("inside", false);
        })
    };
    // The light's head just past each place in turn, its tail on it.
    for (turn, lit, unlit) in [
        (top + 0.01, (along, -hud_lines::TOP), (-along, hud_lines::TOP)),
        (bottom + 0.01, (-along, hud_lines::TOP), (along, -hud_lines::TOP)),
    ] {
        let pixels = draw(turn);
        save(&pixels, W, H, &format!("frame_hud_lines-light-{turn:.2}"));
        let bright = from_middle(&pixels, W, H, lit.0, lit.1);
        assert!(
            bright[0] > 0.8 && bright[2] > 0.8,
            "the light {turn} round, on the line at {lit:?}: {bright:?}"
        );
        let calm = from_middle(&pixels, W, H, unlit.0, unlit.1);
        assert!(green(calm), "the light {turn} round, the line at {unlit:?} the frame's colour: {calm:?}");
    }
}

/// The light goes round every frame: what the light's colour changes in
/// the picture — where the light is — is on the right of the layer with
/// the light put half way down its right side, and on the left with it
/// half way up its left.
#[test]
fn the_light_goes_round_every_frame() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    // How far across the layer (0 to 1) the light is, `turn` round.
    let light_is_at = |name: &str, turn: f32| {
        let draw = |colour: [f32; 4]| {
            with_light_at(&ctx, name, turn, |settings| {
                // One light, and the frame round the whole layer, whatever
                // the frame starts with.
                settings.set_long("anim", 0);
                settings.set_bool("inside", false);
                settings.set_color("light_colour", colour);
            })
        };
        let (white, coloured) = (draw([1.0, 1.0, 1.0, 1.0]), draw(BLUE));
        let (mut sum, mut across) = (0.0_f64, 0.0_f64);
        for (i, (one, other)) in white.iter().zip(&coloured).enumerate() {
            let differs: f32 = one.iter().zip(other).map(|(p, q)| (p - q).abs()).sum();
            sum += f64::from(differs);
            across += f64::from(differs) * f64::from(i as u32 % W) / f64::from(W);
        }
        assert!(sum > 100.0, "{name}: the light {turn} round shows its colour ({sum})");
        across / sum
    };
    for path in &frame_files() {
        let name = stem(path);
        // Just past half way down the right side; just past half way up
        // the left.
        let on_right = light_is_at(name, 0.42);
        assert!(on_right > 0.6, "{name}: the light on the right side is {on_right} across");
        let on_left = light_is_at(name, 0.92);
        assert!(on_left < 0.4, "{name}: the light on the left side is {on_left} across");
    }
}

/// Still takes the light away: the light's colour then changes nothing of
/// the picture, where with a light going round it does.
#[test]
fn still_takes_the_light_away() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    for path in &frame_files() {
        let name = stem(path);
        let draw = |anim: i32, light: [f32; 4]| {
            let mut framed = Framed::new(&ctx, path);
            framed.settings().set_long("anim", anim);
            framed.settings().set_color("light_colour", light);
            // At its start: the light's head at the top left corner.
            framed.settings().set_float("speed", 0.0);
            framed.draw()
        };
        let lit = changed(&draw(0, [1.0, 1.0, 1.0, 1.0]), &draw(0, BLUE));
        assert!(lit > 200, "{name}: a light going round shows its colour ({lit} pixels)");
        let still = changed(&draw(5, [1.0, 1.0, 1.0, 1.0]), &draw(5, BLUE));
        assert_eq!(still, 0, "{name}: Still, and the light's colour shows ({still} pixels)");
    }
}

/// Frame colour (and Second colour, where a frame has one) is the colour
/// of a line frame's lines and their glow: made green, parts of the picture
/// are greener than blue and none bluer than green; made blue, the other
/// way round. (The red picture has neither; a white-hot line has both.)
#[test]
fn frame_colour_colours_a_line_frames_lines() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let greener = |px: [f32; 4]| px[1] > px[2] + 0.25;
    let bluer = |px: [f32; 4]| px[2] > px[1] + 0.25;
    for name in LINE_FRAMES {
        let draw = |colour: [f32; 4]| {
            let mut framed = Framed::new(&ctx, &shader_file(name));
            let settings = framed.settings();
            settings.set_color("frame_colour", colour);
            settings.set_color("second_colour", colour);
            settings.set_long("anim", 5);
            settings.set_float("glow", 1.0);
            settings.set_float("speed", 0.0);
            framed.draw()
        };
        let pixels = draw(GREEN);
        save(&pixels, W, H, &format!("{name}-glow-green"));
        let (of_green, of_blue) = (share(&pixels, greener), share(&pixels, bluer));
        assert!(of_green > 0.002, "{name}: green lines: {of_green} of the picture");
        assert!(of_blue <= 0.0, "{name}: green lines, and {of_blue} of the picture is bluer");
        let pixels = draw(BLUE);
        let (of_green, of_blue) = (share(&pixels, greener), share(&pixels, bluer));
        assert!(of_blue > 0.002, "{name}: blue lines: {of_blue} of the picture");
        assert!(of_green <= 0.0, "{name}: blue lines, and {of_green} of the picture is greener");
    }
}

/// Frame colour is the colour of the art frame's lights: as it comes they
/// are the cyan they were drawn in; made magenta, none is cyan.
#[test]
fn frame_colour_colours_the_art_frames_lights() {
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let cyan = |px: [f32; 4]| px[1] > 0.5 && px[2] > 0.5 && px[0] < 0.3;
    let magenta = |px: [f32; 4]| px[0] > 0.5 && px[2] > 0.5 && px[1] < 0.3;
    let draw = |colour: Option<[f32; 4]>| {
        let mut framed = Framed::new(&ctx, &shader_file(ART_FRAME));
        let settings = framed.settings();
        if let Some(colour) = colour {
            settings.set_color("frame_colour", colour);
        }
        settings.set_long("anim", 5);
        settings.set_float("glow", 0.0);
        settings.set_float("speed", 0.0);
        framed.draw()
    };
    let pixels = draw(None);
    save(&pixels, W, H, "frame_hud-lights");
    let (of_cyan, of_magenta) = (share(&pixels, cyan), share(&pixels, magenta));
    assert!(of_cyan > 0.001, "as it comes, cyan lights: {of_cyan} of the picture");
    assert!(of_magenta <= 0.0, "as it comes, {of_magenta} of the picture is magenta");
    let pixels = draw(Some([1.0, 0.0, 1.0, 1.0]));
    save(&pixels, W, H, "frame_hud-lights-magenta");
    let (of_cyan, of_magenta) = (share(&pixels, cyan), share(&pixels, magenta));
    assert!(of_magenta > 0.001, "magenta lights: {of_magenta} of the picture");
    assert!(of_cyan <= 0.0, "magenta lights, and {of_cyan} of the picture is cyan");
}

/// Where the layer shows over the blue under it, in the channel's
/// picture: how far across it begins and ends, and how far down (0 to 1).
fn layer_reaches(pixels: &[[f32; 4]], wide: u32, high: u32) -> [f32; 4] {
    let (mut left, mut right, mut top, mut bottom) = (wide, 0, high, 0);
    for (i, px) in pixels.iter().enumerate() {
        if !blue(*px) {
            let (x, y) = (i as u32 % wide, i as u32 / wide);
            left = left.min(x);
            right = right.max(x + 1);
            top = top.min(y);
            bottom = bottom.max(y + 1);
        }
    }
    assert!(left < right, "nothing of the layer in the picture");
    [
        left as f32 / wide as f32,
        right as f32 / wide as f32,
        top as f32 / high as f32,
        bottom as f32 / high as f32,
    ]
}

/// The frame runs before the layer's Transform: the layer scaled to half
/// is half the size frame and all — its lines within the layer's place,
/// what is under it showing beyond them.
#[test]
fn a_frame_goes_where_its_layer_goes() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    plain_lines(&mut app, &layer, "frame_hud_lines", GREEN);
    assert_eq!(chain(&app, &layer), ["crop", "frame_hud_lines", "transform"]);
    let transform = effect_named(&app, &layer, "transform");
    set(&mut app, &transform, "scale", ParamValue::Float(0.5));
    let (pixels, wide, high) = picture(&mut app);
    let middle = at(&pixels, wide, high, 0.5, 0.5);
    assert!(red(middle), "the picture in the middle: {middle:?}");
    let beyond = at(&pixels, wide, high, 0.15, 0.15);
    assert!(blue(beyond), "what is under, beyond the layer: {beyond:?}");
    let mut lines = 0_usize;
    for (i, px) in pixels.iter().enumerate() {
        if !green(*px) {
            continue;
        }
        lines += 1;
        let (x, y) = ((i as u32 % wide) as f32 / wide as f32, (i as u32 / wide) as f32 / high as f32);
        assert!(
            (0.24..0.76).contains(&x) && (0.24..0.76).contains(&y),
            "a line outside the layer's place, at {x}, {y}"
        );
    }
    assert!(lines > 100, "the frame's lines: {lines}");
}

/// A framed layer keeps its frame, where it is, from the first frame it is
/// drawn and frame after frame — scaled and moved too: the frame looks at
/// the layer's picture as it comes this frame, before the layer's
/// Transform (and not at what the frame before left in the layer's
/// textures, which is the picture after it).
#[test]
fn a_frame_stays_where_its_layer_is_frame_after_frame() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let transform = effect_named(&app, &layer, "transform");
    // Neither what is under the layer, nor the layer's picture, nor that
    // dimmed: the frame itself.
    let of_frame = |px: [f32; 4]| !blue(px) && !red(px) && !dimmed(px);
    let near = |reach: [f32; 4], place: [f32; 4]| reach.iter().zip(place).all(|(at, to)| (at - to).abs() < 0.01);
    for name in &frame_names(&app) {
        set(&mut app, &transform, "scale", ParamValue::Float(1.0));
        set(&mut app, &transform, "pos_x", ParamValue::Float(0.0));
        plain_lines(&mut app, &layer, name, GREEN);
        // The first frame it is drawn: the frame is there, round the layer.
        let (pixels, wide, high) = picture_after(&mut app, 0, 1);
        let reach = layer_reaches(&pixels, wide, high);
        let drawn = share(&pixels, of_frame);
        assert!(
            near(reach, [0.0, 1.0, 0.0, 1.0]) && drawn > 0.0005,
            "{name}: on its first frame the layer reaches {reach:?}, {drawn} of it the frame"
        );
        // Half the size, a tenth to the right: from 0.35 to 0.85 across and
        // 0.25 to 0.75 down. One frame drawn, then twelve more.
        set(&mut app, &transform, "scale", ParamValue::Float(0.5));
        set(&mut app, &transform, "pos_x", ParamValue::Float(0.1));
        for frames in [1, 12] {
            let (pixels, wide, high) = picture_after(&mut app, 0, frames);
            save(&pixels, wide, high, &format!("{name}-scaled-and-moved-{frames}"));
            let reach = layer_reaches(&pixels, wide, high);
            let drawn = share(&pixels, of_frame);
            assert!(
                near(reach, [0.35, 0.85, 0.25, 0.75]) && drawn > 0.0001,
                "{name}: scaled and moved, {frames} more drawn, the layer reaches {reach:?}, {drawn} of the picture the frame"
            );
            let middle = at(&pixels, wide, high, 0.6, 0.5);
            assert!(red(middle), "{name}: the picture in the layer's middle: {middle:?}");
        }
    }
}

/// Asking for a frame puts it before Transform, and the layer's own mask
/// stays last; None takes the frame away, and Undo brings it back.
#[test]
fn a_frame_is_set_before_transform_and_taken_away() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    assert_eq!(chain(&app, &layer), ["crop", "transform"]);
    id(frame(&mut app, &layer, Some("frame_hud")));
    assert_eq!(chain(&app, &layer), ["crop", "frame_hud", "transform"]);

    id(send_cmd(
        &mut app,
        EngineCommand::SetLayerMask {
            deck_uuid: layer.clone(),
            mask: Some(EffectMask {
                shape: MaskShape::Ellipse {
                    x: 0.3,
                    y: 0.3,
                    width: 0.4,
                    height: 0.4,
                },
                feather: 0.0,
                invert: false,
            }),
        },
    ));
    id(frame(&mut app, &layer, Some("frame_neon_two")));
    assert_eq!(
        chain(&app, &layer),
        ["crop", "frame_neon_two", "transform", LAYER_MASK_EFFECT],
        "one frame, before Transform, the mask last"
    );

    let gone = id(frame(&mut app, &layer, None));
    assert!(gone.is_empty(), "no frame, no effect to name: {gone}");
    assert_eq!(chain(&app, &layer), ["crop", "transform", LAYER_MASK_EFFECT]);
    send_cmd(&mut app, EngineCommand::Undo);
    assert_eq!(
        chain(&app, &layer),
        ["crop", "frame_neon_two", "transform", LAYER_MASK_EFFECT],
        "Undo brings the frame back"
    );
}

/// A frame put in another's place keeps what was changed on the first:
/// its size here.
#[test]
fn another_frame_keeps_what_was_changed_on_the_first() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let hud = id(frame(&mut app, &layer, Some("frame_hud")));
    set(&mut app, &hud, "window", ParamValue::Float(0.7));
    let neon = id(frame(&mut app, &layer, Some("frame_neon_two")));
    assert_ne!(neon, hud, "another effect");
    let size = setting(&app, &neon, "window");
    assert!(
        matches!(size, ParamValue::Float(v) if (v - 0.7).abs() < 1e-6),
        "the size set on the first frame: {size:?}"
    );
}

/// What was left as it came on the first frame is the new frame's own: its
/// animation (the HUD frame's is a light going round, the Two-tone neon's
/// two) and its colour (the HUD frame's cyan, the neon's blue).
#[test]
fn another_frame_takes_its_own_for_what_was_left_as_it_came() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    let hud = id(frame(&mut app, &layer, Some("frame_hud")));
    let first = setting(&app, &hud, "anim");
    assert!(matches!(first, ParamValue::Long(0)), "the HUD frame's animation: {first:?}");
    let cyan = setting(&app, &hud, "frame_colour");
    assert!(
        matches!(cyan, ParamValue::Color(c) if c[1] > 0.9 && c[0] < 0.1),
        "the HUD frame's colour: {cyan:?}"
    );
    let neon = id(frame(&mut app, &layer, Some("frame_neon_two")));
    let animation = setting(&app, &neon, "anim");
    assert!(matches!(animation, ParamValue::Long(1)), "the neon's own animation: {animation:?}");
    let colour = setting(&app, &neon, "frame_colour");
    assert!(
        matches!(colour, ParamValue::Color(c) if c[1] < 0.5 && c[2] > 0.9),
        "the neon's own colour: {colour:?}"
    );
}

/// A name that is no frame — another effect's — is refused, and the layer
/// left as it was.
#[test]
fn a_name_that_is_no_frame_is_refused() {
    let Some(gpu) = common::headless_gpu() else {
        return;
    };
    let mut app = VardaApp::new(gpu, &varda::testing::headless_config()).unwrap();
    let (_, layer) = blue_under_red(&mut app);
    id(frame(&mut app, &layer, Some("frame_hud")));
    let before = chain(&app, &layer);
    for name in ["transform", "no_such_frame"] {
        let refused = frame(&mut app, &layer, Some(name));
        assert!(matches!(refused, CommandResult::Err { .. }), "{name}: {refused:?}");
        assert_eq!(chain(&app, &layer), before, "{name}: left as it was");
    }
}

/// What each frame costs the GPU at 3840×2160: a layer's picture drawn and
/// waited for with the frame on it, less the same without one — a
/// measurement, run by hand
/// (`cargo test --release --test frames -- --ignored --nocapture`). The
/// picture is a still one, kept from frame to frame without a frame on it:
/// a frame, which moves, has it drawn every frame — that is in the figure.
/// And how long each takes to make — its shader compiled, the art frame's
/// picture read — which the show waits for when one is picked.
/// `NIDA_COST_FRAMES=<folder>` times the frames of that folder instead:
/// another make of them, to compare.
#[test]
#[ignore = "a measurement"]
fn cost_of_each_frame_at_4k() {
    use varda::renderer::tonemap::TonemapMode;
    const W4: u32 = 3840;
    const H4: u32 = 2160;
    const FRAMES: u32 = 30;
    let Some(ctx) = common::headless_gpu() else {
        return;
    };
    let time = |frame: Option<&Path>| -> f64 {
        let mut mixer = Mixer::new(&ctx, W4, H4).expect("mixer");
        mixer.set_tonemap_mode(&ctx.queue, TonemapMode::Bypass);
        let mut deck = Deck::new_solid_color(&ctx, [0.8, 0.1, 0.1, 1.0], W4, H4).expect("deck");
        if let Some(path) = frame {
            let shader = ISFShader::from_file(path).expect("shader");
            deck.add_effect(Effect::new(&ctx, shader).expect("effect"));
        }
        mixer.channel_mut(0).expect("a channel").add_deck(deck);
        let audio = AudioData::default();
        let audio_values = AudioValues {
            sources: std::collections::HashMap::default(),
        };
        let analyzer_values = AnalyzerValues::default();
        let mut draw = |n: u32| {
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
            draw(n);
        }
        let started = std::time::Instant::now();
        for n in 5..5 + FRAMES {
            draw(n);
        }
        started.elapsed().as_secs_f64() * 1000.0 / f64::from(FRAMES)
    };
    // The first timing of all is the GPU warming up: taken, and let go.
    time(None);
    let alone = time(None);
    println!("the picture alone: {alone:.2} ms a frame");
    let frames = std::env::var_os("NIDA_COST_FRAMES").map_or_else(frame_files, |dir| frames_in(Path::new(&dir)));
    for path in &frames {
        let started = std::time::Instant::now();
        let shader = ISFShader::from_file(path).expect("shader");
        drop(Effect::new(&ctx, shader).expect("effect"));
        let making = started.elapsed().as_secs_f64() * 1000.0;
        let ms = time(Some(path));
        println!(
            "{}: {ms:.2} ms a frame ({:+.2} over the picture alone), made in {making:.0} ms",
            stem(path),
            ms - alone
        );
    }
}
