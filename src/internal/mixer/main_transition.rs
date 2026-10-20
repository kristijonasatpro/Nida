//! Main's transitions other than a fade (see [`MainTransition`]): a merge
//! of the layers both sides have, and a wipe, slide, zoom or iris drawn by
//! a transition shader. Each draws its picture of the moment into a texture
//! of its own, which Main then shows in place of the two sides (see
//! `Mixer::composite_program`); a fade, and anything a transition cannot
//! draw, is the dissolve as before.
//!
//! A merge draws each side's layers — the same camera, clip, stream or
//! input on both is one layer — the matched ones at the same in-between
//! place and size, eased; then the two pictures are mixed by how far along
//! it is. Where the two sides frame a layer alike it simply moves; nothing
//! dips or jumps at the end, since at its end the picture is the new side's.

use std::fmt::Write as _;

use super::{MainSource, MainTransition, MixMode, Mixer};
use crate::channel::BlendMode;
use crate::deck::Deck;
use crate::renderer::placed_picture::{Border, Crop, Placed, PlacedPicturePipeline};
use crate::renderer::region::Pixels;
use crate::renderer::{BlitPipeline, GpuContext, ISFUniforms};

/// The whole frame, as a rectangle: left, top, right, bottom in UV.
const FRAME: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

/// A picture drawn whole: fitted one to one, not moved, scaled or cropped —
/// a channel's own picture, premultiplied.
const WHOLE: Placed = Placed {
    uv_scale: [1.0, 1.0],
    uv_offset: [0.0, 0.0],
    pos: [0.0, 0.0],
    scale: [1.0, 1.0],
    flip: [false, false],
    crop: None,
    premultiplied: true,
    over_black: 0.0,
};

/// Between two framings of one source by `by`: where it goes and how big,
/// what of it is kept, how soft its edge, how thick, round and coloured its
/// border — a circle opening into a rectangle on the way — all from `from`
/// to `to`. A crop or border on one side only grows from nothing, so each
/// end is drawn exactly as its side draws it: an uncropped picture's edge
/// hard, a crop without a border soft to its feather's end (see
/// `Crop::strength`, `Border::strength`).
fn between_framing(from: &Placed, to: &Placed, by: f32) -> Placed {
    let mix = |x: f32, y: f32| x + (y - x) * by;
    let mix2 = |x: [f32; 2], y: [f32; 2]| [mix(x[0], y[0]), mix(x[1], y[1])];
    let none = |other: Border| Border {
        color: other.color,
        width: 0.0,
        radius: 0.0,
        circle: 0.0,
        strength: 0.0,
    };
    let border = |a: Option<Border>, b: Option<Border>| {
        let (x, y) = match (a, b) {
            (None, None) => return None,
            (Some(x), None) => (x, none(x)),
            (None, Some(y)) => (none(y), y),
            (Some(x), Some(y)) => (x, y),
        };
        Some(Border {
            color: std::array::from_fn(|i| mix(x.color[i], y.color[i])),
            width: mix(x.width, y.width),
            radius: mix(x.radius, y.radius),
            circle: mix(x.circle, y.circle),
            strength: mix(x.strength, y.strength),
        })
    };
    let whole = Crop {
        left: 0.0,
        right: 1.0,
        bottom: 0.0,
        top: 1.0,
        feather: 0.0,
        border: None,
        strength: 0.0,
    };
    let crop = match (from.crop, to.crop) {
        (None, None) => None,
        (a, b) => {
            let (x, y) = (a.unwrap_or(whole), b.unwrap_or(whole));
            Some(Crop {
                left: mix(x.left, y.left),
                right: mix(x.right, y.right),
                bottom: mix(x.bottom, y.bottom),
                top: mix(x.top, y.top),
                feather: mix(x.feather, y.feather),
                border: border(x.border, y.border),
                strength: mix(x.strength, y.strength),
            })
        }
    };
    Placed {
        uv_scale: mix2(from.uv_scale, to.uv_scale),
        uv_offset: mix2(from.uv_offset, to.uv_offset),
        pos: mix2(from.pos, to.pos),
        scale: mix2(from.scale, to.scale),
        flip: to.flip,
        crop,
        premultiplied: to.premultiplied,
        // One side flattening the source over black, the other not: the
        // flattening comes in or goes as the picture moves — it shows only
        // where the source is see-through.
        over_black: mix(from.over_black, to.over_black),
    }
}

/// One draw of a merge onto a side: the picture, its rectangle in its own
/// frame, where it is shown, through which pixels only, the blit's mode,
/// and its strength.
type Draw<'v> = (
    &'v wgpu::TextureView,
    [f32; 4],
    [f32; 4],
    Option<&'v [Pixels]>,
    u32,
    f32,
);

/// Whether a matched pair is drawn as one picture of its source at framings
/// between theirs: both live sources drawn whole by one placed draw, flipped
/// alike — a mirror is no framing to move between — of one kind, and drawn
/// at one size, so its picture is as sharp all the way as at either end.
/// Flattened over black on one side and not the other, it still is (see
/// `between_framing`). Else why not, for the operator.
fn travels(a: &Layer<'_>, b: &Layer<'_>) -> Result<(), &'static str> {
    let (x, y) = match (&a.framing, &b.framing) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(why), _) | (_, Err(why)) => return Err(why),
    };
    if x.placed.flip != y.placed.flip {
        Err("mirrored in one input only")
    } else if x.placed.premultiplied != y.placed.premultiplied {
        Err("two different kinds of input")
    } else if x.size != y.size {
        Err("a different Resolution in each input")
    } else {
        Ok(())
    }
}

impl TravelFrame {
    fn new(context: &GpuContext, (width, height): (u32, u32)) -> Self {
        let texture = context.create_compositing_texture(width, height);
        Self {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            _texture: texture,
            size: (width, height),
        }
    }
}

/// See [`crate::renderer::blit::SUM_BLENDING`].
const SUM: wgpu::BlendState = crate::renderer::blit::SUM_BLENDING;

/// Effects that change only colour, each pixel's coverage kept as it came
/// (or cut, as a layer's own mask does): a layer's place is the same after
/// them. Any other — a mirror, a blur, a key that may fill what was empty,
/// a threshold that does — leaves it to be anywhere.
const KEEPS_ITS_PLACE: &[&str] = &[
    "brightness_contrast",
    "channel_mixer",
    "color_balance",
    "color_correction",
    "colorize",
    "duotone",
    "gradient_map",
    "hue_shift",
    "invert",
    "levels",
    "posterize",
    "sepia",
    "tint",
    crate::engine::value::mask::LAYER_MASK_EFFECT,
];

/// Where a transition draws: each side's picture of the moment, for a
/// merge, and the picture Main shows.
pub(super) struct TransitionTargets {
    size: (u32, u32),
    _textures: [wgpu::Texture; 3],
    from_view: wgpu::TextureView,
    to_view: wgpu::TextureView,
    pub(super) out_view: wgpu::TextureView,
}

impl TransitionTargets {
    fn new(context: &GpuContext, (width, height): (u32, u32)) -> Self {
        let textures = [(); 3].map(|()| context.create_compositing_texture(width, height));
        let view = |t: &wgpu::Texture| t.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            size: (width, height),
            from_view: view(&textures[0]),
            to_view: view(&textures[1]),
            out_view: view(&textures[2]),
            _textures: textures,
        }
    }
}

/// The pipelines a merge draws with, and the shaders the other transitions
/// are drawn by, as compiled.
pub(super) struct MainTransitions {
    /// Layers drawn over one another, premultiplied (each its own slot).
    layers: BlitPipeline,
    /// The new side added over the old, each weighted.
    add: BlitPipeline,
    pub(super) shaders: std::collections::HashMap<MainTransition, super::TransitionEffect>,
    /// Transitions whose shader would not compile: not tried again.
    pub(super) refused: std::collections::HashSet<MainTransition>,
    pub(super) targets: Option<TransitionTargets>,
    /// One for each picture a merge draws of a pair's source at framings
    /// between theirs (see `draw_merge`): a pass with its own settings —
    /// kept, since making one compiles its shader — and the frame it draws
    /// into, dropped when no merge runs.
    travel_passes: std::cell::RefCell<Vec<PlacedPicturePipeline>>,
    travel_frames: std::cell::RefCell<Vec<TravelFrame>>,
    /// A picture of nothing, for a side that is black.
    _black_texture: wgpu::Texture,
    black: wgpu::TextureView,
}

/// What a merge does, as a take starts (see `Mixer::merge_plan`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MergePlan {
    /// For the log: what it moves, from where to where, what it cross-fades
    /// and what it fades.
    pub text: String,
    /// The layers both sides show that cross-fade all the same — drawn too
    /// differently for one picture to move — each with why.
    pub crossfaded: Vec<(String, String)>,
}

/// A frame a merge draws a travelling pair's picture into.
struct TravelFrame {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    size: (u32, u32),
}

impl MainTransitions {
    /// The frames transitions draw into, dropped when the master is resized:
    /// made again at the new size by the next transition.
    pub(super) fn resized(&mut self) {
        self.targets = None;
        self.travel_frames.get_mut().clear();
    }

    pub(super) fn new(context: &GpuContext) -> anyhow::Result<Self> {
        let layers = BlitPipeline::with_blend(
            &context.device,
            context.compositing_format,
            wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING,
        )?;
        let add = BlitPipeline::with_blend(&context.device, context.compositing_format, SUM)?;
        let black_texture = context.create_compositing_texture(1, 1);
        let black = black_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Main Transition Black"),
            });
        clear(&mut encoder, &black);
        context.submit(std::iter::once(encoder.finish()));
        Ok(Self {
            layers,
            add,
            shaders: std::collections::HashMap::new(),
            refused: std::collections::HashSet::new(),
            targets: None,
            travel_passes: std::cell::RefCell::new(Vec::new()),
            travel_frames: std::cell::RefCell::new(Vec::new()),
            _black_texture: black_texture,
            black,
        })
    }
}

impl MainTransition {
    /// The transition shader it is drawn by, if it is drawn by one.
    pub fn shader_name(self) -> Option<&'static str> {
        match self {
            Self::Fade | Self::Merge => None,
            Self::Wipe => Some("transition_wipe_left"),
            Self::Slide => Some("transition_push"),
            Self::Zoom => Some("transition_zoom"),
            Self::Iris => Some("transition_iris"),
        }
    }
}

/// One side of a take, as a merge sees it.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Black,
    Channel(usize),
    Deck(usize, usize),
}

/// A layer of one side of a merge: its picture, where in that picture the
/// layer lies, how strongly it is drawn, and what it shows.
struct Layer<'a> {
    view: &'a wgpu::TextureView,
    premultiplied: bool,
    opacity: f32,
    rect: [f32; 4],
    key: Option<String>,
    /// What the operator calls it, for the log.
    name: String,
    /// The deck it is — channel and place — when it is one.
    deck: Option<(usize, usize)>,
    /// Its whole picture as one placed draw of a live source, where it is
    /// that (see `Deck::framing`): a pair of these over one source is drawn
    /// from it at every framing between theirs.
    framing: Result<crate::deck::Framing<'a>, &'static str>,
}

/// Each old layer's partner among the new: the first not yet taken that
/// shows the same (see [`merge_key`]).
fn pair(old: &[Layer<'_>], new: &[Layer<'_>]) -> Vec<Option<usize>> {
    let mut taken = vec![false; new.len()];
    old.iter()
        .map(|layer| {
            let key = layer.key.as_deref()?;
            let at = (0..new.len()).find(|&j| !taken[j] && new[j].key.as_deref() == Some(key))?;
            taken[at] = true;
            Some(at)
        })
        .collect()
}

/// Two settings alike, a float to a millionth.
fn same_value(a: &crate::params::ParamValue, b: &crate::params::ParamValue) -> bool {
    use crate::params::ParamValue as V;
    let close = |x: f32, y: f32| (x - y).abs() < 1e-6;
    match (a, b) {
        (V::Float(x), V::Float(y)) => close(*x, *y),
        (V::Bool(x), V::Bool(y)) => x == y,
        (V::Long(x), V::Long(y)) => x == y,
        (V::Color(x), V::Color(y)) => x.iter().zip(y).all(|(p, q)| close(*p, *q)),
        (V::Point2D(x), V::Point2D(y)) => x.iter().zip(y).all(|(p, q)| close(*p, *q)),
        _ => false,
    }
}

/// Transform's settings that only say where the picture goes and how big:
/// two copies may differ in these and still be one picture.
const PLACEMENT: &[&str] = &["pos_x", "pos_y", "scale", "scale_mode", "scale_x", "scale_y"];

/// A deck's effects that are on, in order.
fn enabled_effects(deck: &Deck) -> Vec<&crate::deck::Effect> {
    deck.effects.iter().filter(|e| e.is_active()).collect()
}

/// Whether anything modulates the settings under `prefix`.
fn modulated(modulation: &crate::modulation::ModulationEngine, prefix: &str) -> bool {
    let key = format!("{prefix}:");
    modulation
        .assignments
        .iter()
        .any(|(target, mods)| !mods.is_empty() && target.starts_with(&key))
}

/// Whether a deck draws its source over black, its transparency lost: a
/// live source not flagged transparent — a camera, a stream, a tap… (see
/// [`Deck::transparent`]). Clips, images and generators keep theirs either
/// way; a depth sensor draws its own way.
fn flattens(deck: &Deck) -> bool {
    !deck.transparent()
        && deck
            .external_source_kind()
            .is_some_and(|kind| !matches!(kind, crate::deck::ExternalSourceKind::DepthSensor(_)))
}

/// How a side draws the other side's picture in its gaps — `None` where it
/// cannot: only between two that both flatten their source over black or
/// both keep its transparency (see [`flattens`]). Where the source is
/// clear, a flattened picture is black and the other shows what is under
/// it: neither is the other's.
fn borrowed(own_flattens: bool, other_flattens: bool, other_premultiplied: bool) -> Option<u32> {
    (own_flattens == other_flattens).then_some(if other_premultiplied { 1 } else { 2 })
}

/// What a layer of a merge shows in its gaps: a picture, its rectangle in
/// its own frame, the gaps, and the blit's mode to draw it with.
type Fill<'a> = (&'a wgpu::TextureView, [f32; 4], Vec<Pixels>, u32);

/// Whether a deck's crops end where they say: no soft edge or border, which
/// draw beyond the crop's rectangle — the place a merge knows of.
fn crisp_crops(deck: &Deck, modulation: &crate::modulation::ModulationEngine) -> bool {
    deck.effects
        .iter()
        .filter(|e| e.is_active() && e.shader.name() == "crop")
        .all(|e| {
            e.plain_crop(modulation)
                .is_some_and(|crop| crop.feather <= 0.0 && crop.border.is_none())
        })
}

/// Whether two decks draw their source alike, all but where they put it
/// and how big: the same effects in the same order at the same settings —
/// crop, colour, a flip — the same fit and generator settings, crisp crops,
/// nothing modulating either. Only then is one's picture the other's,
/// moved; copies cropped or coloured apart are two pictures. (Whether each
/// keeps its transparency is the fill's to settle: see `draw_merge`.)
fn drawn_alike(a: &Deck, b: &Deck, modulation: &crate::modulation::ModulationEngine) -> bool {
    let still = |deck: &Deck| {
        !modulated(modulation, deck.param_prefix())
            && deck
                .effects
                .iter()
                .all(|e| !e.is_active() || !modulated(modulation, e.param_prefix()))
    };
    if !still(a) || !still(b) || !crisp_crops(a, modulation) || !crisp_crops(b, modulation) {
        return false;
    }
    let settings_alike = |x: &crate::params::ShaderParams,
                          y: &crate::params::ShaderParams,
                          skip: &[&str]| {
        let keys = x.values.keys().chain(y.values.keys()).filter(|k| !skip.contains(&k.as_str()));
        keys.into_iter().all(|k| match (x.values.get(k), y.values.get(k)) {
            (Some(p), Some(q)) => same_value(p, q),
            _ => false,
        })
    };
    let (ea, eb) = (enabled_effects(a), enabled_effects(b));
    a.scaling_mode() == b.scaling_mode()
        && settings_alike(&a.generator_params, &b.generator_params, &[])
        && ea.len() == eb.len()
        && ea.iter().zip(&eb).enumerate().all(|(k, (x, y))| {
            let name = x.shader.name();
            // Where a Transform puts the picture changes nothing it shows
            // only when no more than colour follows it: a crop after it
            // would keep another part.
            let last_placement = ea[k + 1..]
                .iter()
                .all(|e| KEEPS_ITS_PLACE.contains(&e.shader.name().as_str()));
            let skip = if name == "transform" && last_placement {
                PLACEMENT
            } else {
                &[]
            };
            name == y.shader.name()
                && x.mask.is_none()
                && y.mask.is_none()
                && settings_alike(&x.params, &y.params, skip)
        })
}

/// One edge of a band: where it lies, in frames, and whether a pixel whose
/// centre falls exactly on it belongs to the band.
#[derive(Clone, Copy)]
struct Edge(f32, bool);

/// The pixels of a `width` × `height` frame whose centres lie between the
/// edges given across and down: the ones a picture drawn over it covers, as
/// the blit draws a pixel only where its centre falls inside its picture.
fn centred_pixels(across: [Edge; 2], down: [Edge; 2], width: u32, height: u32) -> Option<Pixels> {
    let span = |[Edge(lo, lo_in), Edge(hi, hi_in)]: [Edge; 2], size: u32| {
        #[allow(clippy::cast_precision_loss)]
        let size_f = size as f32;
        // Pixel i's centre lies at i + 0.5.
        let (lo, hi) = (lo * size_f - 0.5, hi * size_f - 0.5);
        let first = if lo_in { lo.ceil() } else { lo.floor() + 1.0 };
        let end = if hi_in { hi.floor() + 1.0 } else { hi.ceil() };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let clamp = |v: f32| v.clamp(0.0, size_f) as u32;
        let (a, b) = (clamp(first), clamp(end));
        (a < b).then_some((a, b))
    };
    let (x0, x1) = span(across, width)?;
    let (y0, y1) = span(down, height)?;
    Some(Pixels { x0, y0, x1, y1 })
}

/// The pixels of a `width` × `height` frame where a layer drawn at `at`
/// has nothing of its own picture: its picture lies at `rect` in its own
/// frame, and the part outside that frame is drawn nowhere — up to four
/// bands, wherever `at` puts them, as rectangles that do not overlap.
/// `None` where it holds all of its picture. Counted by pixel centres, as
/// the blit draws — a centre on the frame's edge is the own picture's, the
/// blit taking 0 and 1 as inside — so the own picture and these bands share
/// no pixel and leave none out.
fn gaps(rect: [f32; 4], at: [f32; 4], width: u32, height: u32) -> Option<Vec<Pixels>> {
    let size = [rect[2] - rect[0], rect[3] - rect[1]];
    if size[0] <= 0.0 || size[1] <= 0.0 {
        return None;
    }
    let place = |v: f32, axis: usize| at[axis] + (v - rect[axis]) / size[axis] * (at[axis + 2] - at[axis]);
    let edge = |v: f32, axis: usize, inside: bool| Edge(place(v, axis), inside);
    let [x0, y0, x1, y1] = rect;
    // A band ends at the frame's edge — 0 or 1, which the own picture draws
    // — or, the picture lying wholly beyond it, at the picture's own edge,
    // which is the band's.
    let below = |v: f32, axis: usize| {
        if v < 0.0 {
            edge(v, axis, true)
        } else {
            edge(0.0, axis, false)
        }
    };
    let above = |v: f32, axis: usize| {
        if v > 1.0 {
            edge(v, axis, true)
        } else {
            edge(1.0, axis, false)
        }
    };
    let full_down = [edge(y0, 1, true), edge(y1, 1, true)];
    let mut bands = Vec::new();
    if x0 < 0.0 {
        bands.push(([edge(x0, 0, true), below(x1, 0)], full_down));
    }
    if x1 > 1.0 {
        bands.push(([above(x0, 0), edge(x1, 0, true)], full_down));
    }
    // Above and below, between the side bands, their edges included.
    let (inside0, inside1) = (x0.max(0.0), x1.min(1.0));
    if inside1 > inside0 {
        let between = [edge(inside0, 0, true), edge(inside1, 0, true)];
        if y0 < 0.0 {
            bands.push((between, [edge(y0, 1, true), below(y1, 1)]));
        }
        if y1 > 1.0 {
            bands.push((between, [above(y0, 1), edge(y1, 1, true)]));
        }
    }
    let missing = crate::renderer::region::disjoint(
        bands
            .into_iter()
            .filter_map(|(across, down)| centred_pixels(across, down, width, height)),
    );
    (!missing.is_empty()).then_some(missing)
}

/// Whether a tap draws its channel as that channel's own picture does, all
/// but where it puts it and how big: no effects but a crop at rest and one
/// plain Transform, unflipped, nothing modulated. Two Transforms are not
/// plain: the first may keep only part of the picture.
fn plain_tap(deck: &Deck, modulation: &crate::modulation::ModulationEngine) -> bool {
    let transforms = deck
        .effects
        .iter()
        .filter(|e| e.is_active() && e.shader.name() == "transform")
        .count();
    deck.tap.is_some()
        && transforms <= 1
        && !modulated(modulation, deck.param_prefix())
        && deck
            .effects
            .iter()
            .filter(|e| e.is_active())
            .all(|e| match e.shader.name().as_str() {
                "crop" => e.is_neutral_framing(modulation),
                "transform" => e.plain_placement(modulation).is_some() && e.flips() == [false, false],
                _ => false,
            })
}

/// A rectangle for the log: its corner and size, in frames.
fn place(rect: [f32; 4]) -> String {
    format!(
        "{:.2},{:.2} {:.2}×{:.2}",
        rect[0],
        rect[1],
        rect[2] - rect[0],
        rect[3] - rect[1]
    )
}

/// What a layer shows, to match it with the other side's: its source —
/// the same camera, clip, image, stream, window or input on both sides is
/// one layer. A camera by its open session, which every layer of that
/// device shares — two capture cards of one model are two cameras, though
/// they may have one name. A solid colour, a timer, a playlist match
/// nothing.
pub(super) fn merge_key(deck: &Deck) -> Option<String> {
    if let Some(camera) = deck.camera_id() {
        return Some(format!("camera:{camera}"));
    }
    if let Some(tap) = &deck.tap {
        return Some(match &tap.source {
            crate::deck::TapSource::Channel(uuid) => format!("tap:{uuid}"),
            crate::deck::TapSource::MasterProgram => "tap:master".to_string(),
        });
    }
    let kind = deck.source_type();
    match kind {
        "solid_color" | "timer" | "playlist" => None,
        _ => Some(format!(
            "{kind}:{}",
            deck.source_path().unwrap_or_else(|| deck.source_name())
        )),
    }
}

/// Where a layer lies in its own picture: its crop, moved and scaled by its
/// transform, in the order its effects come — or the whole frame where they
/// do more than crop, move and scale (inverted, turned, modulated…), which
/// no rectangle here can say.
pub(super) fn layer_rect(deck: &Deck, modulation: &crate::modulation::ModulationEngine) -> [f32; 4] {
    let mut rect = FRAME;
    for effect in deck.effects.iter().filter(|e| e.is_active()) {
        match effect.shader.name().as_str() {
            "crop" => {
                let Some(crop) = effect.plain_crop(modulation) else {
                    return FRAME;
                };
                // The crop's bottom and top run up from the bottom.
                let kept = [crop.left, 1.0 - crop.top, crop.right, 1.0 - crop.bottom];
                rect = [
                    rect[0].max(kept[0]),
                    rect[1].max(kept[1]),
                    rect[2].min(kept[2]),
                    rect[3].min(kept[3]),
                ];
            }
            "transform" => {
                let Some((pos, scale)) = effect.plain_placement(modulation) else {
                    return FRAME;
                };
                // Flipped, the part the crop keeps shows mirrored.
                let [across, down] = effect.flips();
                if across {
                    rect = [1.0 - rect[2], rect[1], 1.0 - rect[0], rect[3]];
                }
                if down {
                    rect = [rect[0], 1.0 - rect[3], rect[2], 1.0 - rect[1]];
                }
                let x = |v: f32| (v - 0.5) * scale[0] + 0.5 + pos[0];
                let y = |v: f32| (v - 0.5) * scale[1] + 0.5 + pos[1];
                rect = [x(rect[0]), y(rect[1]), x(rect[2]), y(rect[3])];
            }
            name if KEEPS_ITS_PLACE.contains(&name) => {}
            _ => return FRAME,
        }
    }
    rect
}

/// A deck as a layer of a merge: its own picture, straight alpha. `at`:
/// its channel and place.
fn deck_layer<'a>(
    slot: &'a crate::channel::DeckSlot,
    modulation: &crate::modulation::ModulationEngine,
    at: (usize, usize),
) -> Layer<'a> {
    Layer {
        view: &slot.deck.texture_view,
        premultiplied: false,
        opacity: slot.opacity,
        rect: layer_rect(&slot.deck, modulation),
        key: merge_key(&slot.deck),
        name: slot.deck.source_name().to_string(),
        deck: Some(at),
        framing: slot.deck.framing(modulation),
    }
}

/// `a` to `b` by `t`, corner by corner.
fn between(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// The UV scale and offset that show a picture's `own` rectangle at
/// `shown` — moved and resized; `None` for a rectangle with no size.
fn mapping(own: [f32; 4], shown: [f32; 4]) -> Option<([f32; 2], [f32; 2])> {
    let shown_size = [shown[2] - shown[0], shown[3] - shown[1]];
    if shown_size[0] <= 1e-6 || shown_size[1] <= 1e-6 {
        return None;
    }
    let scale = [
        (own[2] - own[0]) / shown_size[0],
        (own[3] - own[1]) / shown_size[1],
    ];
    Some((scale, [own[0] - shown[0] * scale[0], own[1] - shown[1] * scale[1]]))
}

/// A pass clearing `view` to nothing.
fn clear(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("Main Transition Clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                store: wgpu::StoreOp::Store,
            },
            depth_slice: None,
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
}

impl Mixer {
    /// For each channel, whether it takes part in the merge running — one
    /// that will be drawn as a merge, not dissolved: its layers are then
    /// drawn whole, every one, every frame, for they move and uncover one
    /// another.
    pub(super) fn merging_channels(&self) -> Vec<bool> {
        let mut merging = vec![false; self.channels.len()];
        let Some(fade) = self.main.fade.as_ref() else {
            return merging;
        };
        if fade.transition != MainTransition::Merge {
            return merging;
        }
        let (Some(from), Some(to)) = (self.side(&fade.from), self.side(&self.main.program)) else {
            return merging;
        };
        if self.merge_layers(from, to).is_err() || self.merge_layers(to, from).is_err() {
            return merging;
        }
        for side in [from, to] {
            match side {
                Side::Channel(idx) | Side::Deck(idx, _) => merging[idx] = true,
                Side::Black => {}
            }
        }
        merging
    }

    /// Let go of the frames a merge draws its travelling pictures into
    /// unless one is being drawn: they go with the merge, and with Main —
    /// switched to the layered mix mid-merge, nothing draws it.
    pub(super) fn release_merge_frames(&mut self) {
        let merging = self.mix_mode == MixMode::Program
            && self
                .main
                .fade
                .as_ref()
                .is_some_and(|fade| fade.transition == MainTransition::Merge);
        if !merging {
            self.main_transitions.travel_frames.get_mut().clear();
        }
    }

    /// Draw the transition running into its texture, if it is one this
    /// draws (not a fade) and it can: the command buffers, then Main shows
    /// the texture in place of the two sides. `None`: the dissolve.
    pub(super) fn render_main_transition(
        &mut self,
        context: &GpuContext,
    ) -> Option<Vec<wgpu::CommandBuffer>> {
        let drawn = self.draw_main_transition(context);
        if drawn.is_none() {
            // Nothing travels in a dissolve: a merge that cannot take a side
            // apart now, a side gone, or a fade.
            self.main_transitions.travel_frames.get_mut().clear();
        }
        drawn
    }

    /// See [`Self::render_main_transition`].
    fn draw_main_transition(&mut self, context: &GpuContext) -> Option<Vec<wgpu::CommandBuffer>> {
        let fade = self.main.fade.clone()?;
        if fade.transition == MainTransition::Fade {
            return None;
        }
        let size = (self.composite_texture.width(), self.composite_texture.height());
        if self.main_transitions.targets.as_ref().map(|t| t.size) != Some(size) {
            self.main_transitions.targets = Some(TransitionTargets::new(context, size));
        }
        let from = self.side(&fade.from)?;
        let to = self.side(&self.main.program)?;
        let shown = fade.shown();
        if fade.transition == MainTransition::Merge {
            self.draw_merge(context, from, to, shown)
        } else {
            self.draw_shader_transition(context, fade.transition, from, to, shown)
        }
    }

    /// A take's side as it is now.
    fn side(&self, source: &MainSource) -> Option<Side> {
        match source {
            MainSource::Black => Some(Side::Black),
            MainSource::Channel(uuid) => self.find_channel_by_uuid(uuid).map(Side::Channel),
            MainSource::Deck(uuid) => self.find_deck_by_uuid(uuid).map(|(c, d)| Side::Deck(c, d)),
        }
    }

    /// A wipe, slide, zoom or iris: its shader between the two sides'
    /// pictures. `None` where a side is a layer on its own (its picture is
    /// not premultiplied as the shader needs) or the shader is not ready.
    fn draw_shader_transition(
        &mut self,
        context: &GpuContext,
        transition: MainTransition,
        from: Side,
        to: Side,
        shown: f32,
    ) -> Option<Vec<wgpu::CommandBuffer>> {
        let picture = |side: Side| match side {
            Side::Black => Some(&self.main_transitions.black),
            Side::Channel(idx) => Some(&self.channels[idx].composite_view),
            Side::Deck(..) => None,
        };
        let (from_view, to_view) = (picture(from)?, picture(to)?);
        let effect = self.main_transitions.shaders.get_mut(&transition)?;
        let targets = self.main_transitions.targets.as_ref()?;
        // Wipe and Iris take their soft edge, and Iris the frame's shape, so
        // that they start with none of the new picture and end with none of
        // the old: the edge from just before the frame to just past it.
        let softness = effect.params.get_float("softness").unwrap_or(0.0).max(0.0);
        #[allow(clippy::cast_precision_loss)]
        let aspect = self.main_transitions.targets.as_ref().map_or(16.0 / 9.0, |t| {
            t.size.0 as f32 / t.size.1.max(1) as f32
        });
        let progress = match transition {
            MainTransition::Wipe => -softness + shown * (1.0 + 2.0 * softness),
            MainTransition::Iris => {
                // The shader's circle reaches 1.2 at progress 1, measured in
                // heights from the middle; the frame's corner is further on
                // a wide one.
                let corner = (0.5 * aspect).hypot(0.5);
                (-softness + shown * (corner + 2.0 * softness)) / 1.2
            }
            _ => shown,
        };
        effect
            .params
            .set("progress", crate::params::ParamValue::Float(progress));
        if transition == MainTransition::Slide {
            // In from the right, the old picture pushed out to the left.
            effect
                .params
                .set("direction", crate::params::ParamValue::Float(1.0));
        }
        effect.params.build_buffer_data();
        if let Some(buffer) = effect.params.buffer() {
            context.queue.write_buffer(buffer, 0, effect.params.scratch());
        }
        #[allow(clippy::cast_precision_loss)]
        let uniforms = ISFUniforms {
            time: self.start_time.elapsed().as_secs_f32(),
            time_delta: 1.0 / 60.0,
            frame_index: self.frame_count,
            pass_index: 0,
            render_size: [targets.size.0 as f32, targets.size.1 as f32],
            phase_times: [0.0; 4],
            ..Default::default()
        };
        Some(vec![effect.pipeline.render_to_cmd(
            context,
            from_view,
            to_view,
            &targets.out_view,
            &uniforms,
            effect.params.buffer(),
        )])
    }

    /// One side's layers, bottom first, for a merge with `other`: the decks
    /// of its channel as the channel shows them — or, where the other side
    /// shows this whole channel as a layer of its own, the channel's picture
    /// as one layer, the full frame, to grow into or shrink from that
    /// layer's place. For a side a merge cannot take apart — one with
    /// effects of its own, or layers blended other than normally — why not,
    /// in words for the operator.
    fn merge_layers(&self, side: Side, other: Side) -> Result<Vec<Layer<'_>>, String> {
        let auto = |slot: &crate::channel::DeckSlot| {
            slot.auto_transition.as_ref().is_some_and(|a| a.enabled)
        };
        match side {
            Side::Black => Ok(Vec::new()),
            Side::Deck(ch, dk) => {
                let slot = &self.channels[ch].decks[dk];
                if auto(slot) {
                    return Err(format!(
                        "the layer '{}' transitions by itself",
                        slot.deck.source_name()
                    ));
                }
                // Alone on Main a layer shows at full strength, as Main
                // draws it once the take is done.
                let mut layer = deck_layer(slot, &self.modulation, (ch, dk));
                layer.opacity = 1.0;
                Ok(vec![layer])
            }
            Side::Channel(idx) => {
                let channel = &self.channels[idx];
                if let Some(effect) = channel.effects.iter().find(|e| e.is_active()) {
                    return Err(format!(
                        "'{}' has an effect of its own ({})",
                        channel.name,
                        effect.shader.name()
                    ));
                }
                let taps_this = |slot: &crate::channel::DeckSlot| {
                    slot.deck.tap.as_ref().is_some_and(|t| {
                        matches!(&t.source, crate::deck::TapSource::Channel(u) if u == channel.uuid())
                    })
                };
                let tapped_by_other = match other {
                    Side::Channel(o) => {
                        let decks = &self.channels[o].decks;
                        let any_solo = decks.iter().any(|slot| slot.solo);
                        decks.iter().any(|slot| {
                            taps_this(slot)
                                && !slot.mute
                                && (!any_solo || slot.solo)
                                && slot.opacity > 0.0
                        })
                    }
                    Side::Deck(c, d) => taps_this(&self.channels[c].decks[d]),
                    Side::Black => false,
                };
                if tapped_by_other {
                    return Ok(vec![Layer {
                        view: &channel.composite_view,
                        premultiplied: true,
                        opacity: 1.0,
                        rect: FRAME,
                        key: Some(format!("tap:{}", channel.uuid())),
                        name: channel.name.clone(),
                        deck: None,
                        // The channel's picture whole, as a tap of it shows
                        // it before its crop and placement.
                        framing: Ok(crate::deck::Framing {
                            placed: WHOLE,
                            source: Some(&channel.composite_view),
                            size: (
                                channel.composite_texture.width(),
                                channel.composite_texture.height(),
                            ),
                        }),
                    }]);
                }
                let any_solo = channel.decks.iter().any(|slot| slot.solo);
                let mut order: Vec<usize> = (0..channel.decks.len()).collect();
                order.sort_by_key(|&i| channel.decks[i].z_index);
                let mut layers = Vec::new();
                for i in order {
                    let slot = &channel.decks[i];
                    if slot.mute || (any_solo && !slot.solo) || slot.opacity <= 0.0 {
                        continue;
                    }
                    // Blended or auto-transitioning layers are composited by
                    // their channel in ways a merge does not draw.
                    if slot.blend_mode != BlendMode::Normal {
                        return Err(format!(
                            "in '{}' the layer '{}' is blended as {}",
                            channel.name,
                            slot.deck.source_name(),
                            slot.blend_mode.label()
                        ));
                    }
                    if auto(slot) {
                        return Err(format!(
                            "in '{}' the layer '{}' transitions by itself",
                            channel.name,
                            slot.deck.source_name()
                        ));
                    }
                    layers.push(deck_layer(slot, &self.modulation, (idx, i)));
                }
                Ok(layers)
            }
        }
    }

    /// What the merge running does, for the log when a take starts: the
    /// layers it moves, from where to where, those it cross-fades — matched,
    /// but drawn too differently for one picture to move — and those it
    /// fades; or, when it cannot take a side apart and dissolves instead,
    /// why. `None` while no merge runs.
    pub(crate) fn merge_plan(&self) -> Option<Result<MergePlan, String>> {
        let fade = self.main.fade.as_ref()?;
        if fade.transition != MainTransition::Merge {
            return None;
        }
        let from = self.side(&fade.from)?;
        let to = self.side(&self.main.program)?;
        let (old, new) = match (self.merge_layers(from, to), self.merge_layers(to, from)) {
            (Ok(old), Ok(new)) => (old, new),
            (Err(why), _) | (_, Err(why)) => return Some(Err(why)),
        };
        let partner = pair(&old, &new);
        let mut moves = Vec::new();
        let mut crossfaded = Vec::new();
        let mut fades = Vec::new();
        for (layer, j) in old.iter().zip(&partner) {
            match j {
                Some(j) if self.moves_whole(layer, &new[*j]) => moves.push(format!(
                    "'{}' {} → {}",
                    layer.name,
                    place(layer.rect),
                    place(new[*j].rect)
                )),
                Some(j) => {
                    let why = travels(layer, &new[*j]).err().unwrap_or("drawn differently");
                    crossfaded.push((layer.name.clone(), why.to_string()));
                }
                None => fades.push(format!("'{}' out", layer.name)),
            }
        }
        for (j, layer) in new.iter().enumerate() {
            if !partner.contains(&Some(j)) {
                fades.push(format!("'{}' in", layer.name));
            }
        }
        let list = |items: &[String]| {
            if items.is_empty() {
                "nothing".to_string()
            } else {
                items.join(", ")
            }
        };
        let mut text = format!("moves {}", list(&moves));
        if !crossfaded.is_empty() {
            let named: Vec<String> = crossfaded
                .iter()
                .map(|(name, why)| format!("'{name}' ({why})"))
                .collect();
            let _ = write!(text, "; cross-fades {}", named.join(", "));
        }
        let _ = write!(text, "; fades {}", list(&fades));
        Some(Ok(MergePlan { text, crossfaded }))
    }

    /// How many frames merges keep for travelling pictures (see
    /// `draw_merge`).
    #[cfg(test)]
    pub(crate) fn merge_frames_kept(&self) -> usize {
        self.main_transitions.travel_frames.borrow().len()
    }

    /// Whether a matched pair moves as one picture: drawn from their live
    /// source at framings between theirs (see `travels`), or drawn alike
    /// (see `alike`).
    fn moves_whole(&self, a: &Layer<'_>, b: &Layer<'_>) -> bool {
        travels(a, b).is_ok() || self.alike(a, b)
    }

    /// Whether a matched pair is two decks drawn alike, or a channel's own
    /// picture and a plain tap of it: each side's own picture then moves,
    /// and fills the other's gaps (see `fill`).
    fn alike(&self, a: &Layer<'_>, b: &Layer<'_>) -> bool {
        let deck_of = |layer: &Layer<'_>| {
            layer
                .deck
                .map(|(c, d)| &self.channels[c].decks[d].deck)
        };
        match (deck_of(a), deck_of(b)) {
            (Some(x), Some(y)) => drawn_alike(x, y, &self.modulation),
            (Some(tap), None) | (None, Some(tap)) => plain_tap(tap, &self.modulation),
            (None, None) => false,
        }
    }

    /// What a layer of a merge shows in its gaps — the part of its picture
    /// outside its own frame, its picture drawn at `at` — `None` where it
    /// has none or nothing can fill them. A plain tap drawing its picture one
    /// to one shows that picture — a channel's, of this frame or the one
    /// before as the tap reads it — placed: filled from it, flattened over
    /// black where the tap flattens it. Anything else is filled from
    /// `other`, the same picture on the other side, when the two flatten
    /// their source alike.
    fn fill<'a>(
        &'a self,
        own: &Layer<'a>,
        other: &Layer<'a>,
        at: [f32; 4],
        (width, height): (u32, u32),
    ) -> Option<Fill<'a>> {
        let within = gaps(own.rect, at, width, height)?;
        let deck_of = |layer: &Layer<'_>| {
            layer
                .deck
                .map(|(c, d)| &self.channels[c].decks[d].deck)
        };
        let own_deck = deck_of(own);
        let plain = |d: &&Deck| plain_tap(d, &self.modulation) && d.maps_source_one_to_one();
        if let Some(tap) = own_deck.filter(plain) {
            if let Some(view) = &tap.external_source_view {
                let mode = if flattens(tap) { 4 } else { 1 };
                return Some((view, FRAME, within, mode));
            }
        }
        let flat = |deck: Option<&Deck>| deck.is_some_and(flattens);
        let mode = borrowed(flat(own_deck), flat(deck_of(other)), other.premultiplied)?;
        Some((other.view, other.rect, within, mode))
    }

    /// The merge at `shown` (0–1, eased): each side's layers drawn, those
    /// both sides have at the same place between theirs, then the old side
    /// and the new mixed by `shown`.
    fn draw_merge(
        &self,
        context: &GpuContext,
        from: Side,
        to: Side,
        shown: f32,
    ) -> Option<Vec<wgpu::CommandBuffer>> {
        let old = self.merge_layers(from, to).ok()?;
        let new = self.merge_layers(to, from).ok()?;
        // Each old layer matched to the first new one showing the same.
        let partner = pair(&old, &new);
        let mut new_from: Vec<Option<[f32; 4]>> = vec![None; new.len()];
        for (i, j) in partner.iter().enumerate() {
            if let Some(j) = j {
                new_from[*j] = Some(old[i].rect);
            }
        }
        let old_shown: Vec<[f32; 4]> = old
            .iter()
            .zip(&partner)
            .map(|(layer, j)| j.map_or(layer.rect, |j| between(layer.rect, new[j].rect, shown)))
            .collect();
        let new_shown: Vec<[f32; 4]> = new
            .iter()
            .zip(&new_from)
            .map(|(layer, start)| start.map_or(layer.rect, |s| between(s, layer.rect, shown)))
            .collect();
        let targets = self.main_transitions.targets.as_ref()?;
        // A pair drawn alike: each side draws its own picture and, where it
        // holds none of it — the part outside its own frame — the same
        // picture from where it is whole, through just those gaps (see
        // `fill`). A layer partly outside the frame on one side has that part
        // only elsewhere, and drawn from each side's own it faded out as the
        // take went on — an operator sliding a picture off the edge saw it. Where both hold
        // the picture the two still cross over, so copies playing apart end
        // on the new one's frame; no pixel is drawn twice. A pair drawn
        // apart (cropped or coloured apart, or modulated) is each side's own.
        // A pair of one live source is drawn once, from the source, at a
        // framing between theirs: a round picture-in-picture opens into the
        // whole picture, crop, border and all, rather than the two crossing
        // over in a box changing shape (an operator's camera, 2026-10-09).
        // Each picture drawn so: its framing now, its source, its size, its
        // strength. Where the two read different frames of the source — a
        // tap showing the frame before — each side's from its own, so each
        // end is its side's picture.
        let mut pictures: Vec<(Placed, &wgpu::TextureView, (u32, u32), f32)> = Vec::new();
        let mut old_travel: Vec<Option<usize>> = vec![None; old.len()];
        let mut new_travel: Vec<Option<usize>> = vec![None; new.len()];
        for (i, j) in partner.iter().enumerate() {
            let Some(j) = *j else { continue };
            let (Ok(from), Ok(to)) = (&old[i].framing, &new[j].framing) else {
                continue;
            };
            // A source is bound with the first frame drawn after its deck
            // comes; till then each side draws its own.
            let (Some(old_source), Some(new_source)) = (from.source, to.source) else {
                continue;
            };
            if travels(&old[i], &new[j]).is_err() {
                continue;
            }
            let placed = between_framing(&from.placed, &to.placed, shown);
            old_travel[i] = Some(pictures.len());
            if new_source == old_source {
                // On both sides at the strength between theirs.
                let opacity = old[i].opacity + (new[j].opacity - old[i].opacity) * shown;
                pictures.push((placed, old_source, from.size, opacity));
            } else {
                // Each side's frame at its own strength, weighted as the
                // sides are mixed.
                pictures.push((placed, old_source, from.size, old[i].opacity));
                pictures.push((placed, new_source, to.size, new[j].opacity));
            }
            new_travel[j] = Some(pictures.len() - 1);
        }
        let mut old_fill: Vec<Option<Fill<'_>>> = (0..old.len()).map(|_| None).collect();
        let mut new_fill: Vec<Option<Fill<'_>>> = (0..new.len()).map(|_| None).collect();
        for (i, j) in partner.iter().enumerate() {
            let Some(j) = *j else { continue };
            // Two decks drawn alike, or a channel's own picture and a plain
            // tap of it (their keys already match) — and not drawn as one
            // picture of their source already.
            if old_travel[i].is_some() || !self.alike(&old[i], &new[j]) {
                continue;
            }
            let at = old_shown[i];
            old_fill[i] = self.fill(&old[i], &new[j], at, targets.size);
            new_fill[j] = self.fill(&new[j], &old[i], at, targets.size);
        }
        let draws = old.len() + new.len() + old_fill.iter().chain(&new_fill).flatten().count();

        let transitions = &self.main_transitions;
        transitions
            .layers
            .ensure_ring_slots(&context.device, draws + 1);
        transitions.add.ensure_ring_slots(&context.device, 1);
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Main Merge Clear"),
            });
        for view in [&targets.from_view, &targets.to_view, &targets.out_view] {
            clear(&mut encoder, view);
        }
        let mut cmds = vec![encoder.finish()];
        // Each travelling pair's picture, drawn into a frame of its own at
        // the size its decks draw at, then shown at Main's as they are.
        let mut passes = transitions.travel_passes.borrow_mut();
        let mut frames = transitions.travel_frames.borrow_mut();
        for (k, (placed, source, size, _)) in pictures.iter().enumerate() {
            if passes.len() <= k {
                passes.push(PlacedPicturePipeline::new(
                    &context.device,
                    context.compositing_format,
                ));
            }
            if frames.get(k).is_none_or(|frame| frame.size != *size) {
                let frame = TravelFrame::new(context, *size);
                if k < frames.len() {
                    frames[k] = frame;
                } else {
                    frames.push(frame);
                }
            }
            passes[k].draw(
                &context.device,
                &context.queue,
                &mut cmds,
                placed,
                source,
                &frames[k].view,
                *size,
                None,
            );
        }
        // No more frames kept than this merge draws.
        frames.truncate(pictures.len());
        let mut slot = 0;
        for (layers, fills, travel_of, shown_at, target) in [
            (&old, &old_fill, &old_travel, &old_shown, &targets.from_view),
            (&new, &new_fill, &new_travel, &new_shown, &targets.to_view),
        ] {
            for (((layer, fill), travel), at) in layers.iter().zip(fills).zip(travel_of).zip(shown_at) {
                // A travelling pair's picture, on both sides at the strength
                // between theirs, so it shows once whatever the mix.
                // Else its own picture everywhere — a layer's own picture is
                // straight alpha, a channel's premultiplied, both drawn
                // premultiplied over the rest — then the other side's in its
                // gaps, as it would show it.
                let draws: Vec<Draw<'_>> =
                    if let Some(k) = *travel {
                        vec![(&frames[k].view, FRAME, FRAME, None, 2, pictures[k].3)]
                    } else {
                        let own_mode = if layer.premultiplied { 1 } else { 2 };
                        std::iter::once((layer.view, layer.rect, *at, None, own_mode, layer.opacity))
                            .chain(fill.as_ref().map(|(view, rect, within, mode)| {
                                (*view, *rect, *at, Some(within.as_slice()), *mode, layer.opacity)
                            }))
                            .collect()
                    };
                for (view, rect, at, within, mode, opacity) in draws {
                    let Some((scale, offset)) = mapping(rect, at) else {
                        continue;
                    };
                    transitions.layers.write_params_slot_mode(
                        &context.queue,
                        slot,
                        opacity,
                        scale,
                        offset,
                        mode,
                    );
                    cmds.push(transitions.layers.blend_onto_within(
                        &context.device,
                        view,
                        target,
                        slot,
                        "Main Merge Layer",
                        within,
                    ));
                    slot += 1;
                }
            }
        }
        // The old side weighted down, the new one added over it weighted up.
        transitions.layers.write_params_slot_mode(
            &context.queue,
            slot,
            1.0 - shown,
            [1.0, 1.0],
            [0.0, 0.0],
            1,
        );
        cmds.push(transitions.layers.blend_onto(
            &context.device,
            &targets.from_view,
            &targets.out_view,
            slot,
            "Main Merge Old",
        ));
        transitions
            .add
            .write_params_slot_mode(&context.queue, 0, shown, [1.0, 1.0], [0.0, 0.0], 1);
        cmds.push(transitions.add.blend_onto(
            &context.device,
            &targets.to_view,
            &targets.out_view,
            0,
            "Main Merge New",
        ));
        Some(cmds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture's rectangle shown at another: moved and resized, as a
    /// UV scale and offset — none for a rectangle with no size.
    /// Only pictures that flatten their source alike fill each other's gaps.
    #[test]
    fn only_pictures_flattened_alike_fill_each_other() {
        assert_eq!(borrowed(false, false, false), Some(2));
        assert_eq!(borrowed(true, true, false), Some(2));
        assert_eq!(borrowed(false, false, true), Some(1));
        assert_eq!(borrowed(true, false, false), None);
        assert_eq!(borrowed(false, true, true), None);
    }

    /// Between framings, a crop or border on one side only grows from
    /// nothing — none at the other end, where that side draws without it —
    /// and two uncropped framings stay uncropped.
    #[test]
    fn a_crop_or_border_on_one_side_grows_from_nothing() {
        let border = Border {
            color: [1.0, 0.0, 0.0, 1.0],
            width: 12.0,
            radius: 40.0,
            circle: 1.0,
            strength: 1.0,
        };
        let crop = |border| Crop {
            left: 0.25,
            right: 1.0,
            bottom: 0.0,
            top: 1.0,
            feather: 0.1,
            border,
            strength: 1.0,
        };
        let with = |crop| Placed { crop, ..WHOLE };
        assert_eq!(between_framing(&WHOLE, &WHOLE, 0.5).crop, None);

        let (from, to) = (with(None), with(Some(crop(Some(border)))));
        let start = between_framing(&from, &to, 0.0).crop.expect("cropped on its way");
        assert_eq!((start.left, start.strength), (0.0, 0.0));
        assert_eq!(start.border.map(|b| (b.width, b.circle, b.strength)), Some((0.0, 0.0, 0.0)));
        let end = between_framing(&from, &to, 1.0).crop.expect("cropped");
        assert_eq!(end, crop(Some(border)));

        let (from, to) = (with(Some(crop(None))), with(Some(crop(Some(border)))));
        let start = between_framing(&from, &to, 0.0).crop.expect("cropped");
        assert_eq!((start.left, start.feather, start.strength), (0.25, 0.1, 1.0));
        assert_eq!(start.border.map(|b| (b.width, b.color, b.strength)), Some((0.0, border.color, 0.0)));
    }

    #[test]
    fn a_rectangle_maps_onto_another() {
        let (scale, offset) = mapping([0.5, 0.5, 1.0, 1.0], [0.0, 0.0, 1.0, 1.0]).unwrap();
        // The output's corners sample the picture's quarter's corners.
        let at = |o: [f32; 2]| [o[0] * scale[0] + offset[0], o[1] * scale[1] + offset[1]];
        assert_eq!(at([0.0, 0.0]), [0.5, 0.5]);
        assert_eq!(at([1.0, 1.0]), [1.0, 1.0]);
        assert!(mapping(FRAME, [0.3, 0.3, 0.3, 0.6]).is_none());
    }

    /// Flipped, the part a crop keeps shows mirrored — and a merge moves the
    /// picture from there, not from where it was before the flip.
    #[test]
    fn a_flipped_crop_merges_from_its_mirrored_place() {
        let ctx = crate::renderer::GpuContext::new_headless().expect("headless GPU required for tests");
        let shaders = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("shaders");
        let mut deck = Deck::new_solid_color(&ctx, [1.0; 4], 64, 36).expect("deck");
        for name in ["crop", "transform"] {
            let shader = crate::isf::ISFShader::from_file(shaders.join(format!("{name}.fs")))
                .expect("shader");
            deck.add_effect(crate::deck::Effect::new(&ctx, shader).expect("effect"));
        }
        deck.effects[0].params.set_float("crop_left", 0.5);
        let modulation = crate::modulation::ModulationEngine::default();
        assert_eq!(layer_rect(&deck, &modulation), [0.5, 0.0, 1.0, 1.0]);
        deck.effects[1]
            .params
            .set("flip_x", crate::params::ParamValue::Bool(true));
        assert_eq!(layer_rect(&deck, &modulation), [0.0, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn halfway_between_two_rectangles() {
        assert_eq!(between([0.0, 0.0, 0.5, 0.5], [0.5, 0.5, 1.0, 1.0], 0.5), [0.25, 0.25, 0.75, 0.75]);
    }
}
