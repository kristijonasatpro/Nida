// A layer's picture drawn straight into its place: cropped, bordered and
// rounded as the crop effect does it, moved and scaled as the transform
// does — in the one pass that draws it, instead of the whole frame drawn
// and then cropped, and the whole frame cropped and then shrunk.
//
// The crop's own sums (crop.fs), done at the point of the picture the
// transform brings to this pixel. Its edges are anti-aliased over this
// frame's pixels rather than the picture's, since the picture is not drawn
// at its own size first.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct Params {
    // A picture point to the source's UV: the scaling mode (Fit, Fill…).
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    // The transform: where the picture's centre moves, and its scale.
    pos: vec2<f32>,
    scale: vec2<f32>,
    // The layer's frame in pixels.
    frame: vec2<f32>,
    // 1: the source is premultiplied (another input), drawn out straight.
    premultiplied: u32,
    // 1: cropped; 2: cropped with a border.
    crop_mode: u32,
    // Left, right, bottom, top — bottom and top measured up from the bottom.
    crop: vec4<f32>,
    border_color: vec4<f32>,
    feather: f32,
    border_width: f32,
    border_radius: f32,
    // 1: the largest circle inside the crop, not a rounded rectangle;
    // between, on the way from one to the other (a Merge).
    circle: f32,
    // 1 on an axis: the transform's flip, the picture mirrored about its
    // own centre — the crop with it, which comes before it.
    flip: vec2<f32>,
    // 1: as set. Less, a Merge on its way from the picture drawn whole,
    // with the hard edge of its own (`cropped`), or from the crop drawn
    // without a border (`bordered`): the shape kept clear of the crop's
    // soft edge, the colours as the crop alone draws them.
    cropped: f32,
    bordered: f32,
    // 1: drawn over black, as a deck not keeping its source's transparency
    // draws it — black where the source is clear or does not reach; 0: as
    // the source comes. Between, a Merge on its way from one to the other.
    over_black: f32,
};

@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var source: texture_2d<f32>;
@group(0) @binding(2) var<uniform> params: Params;

// The border's sizes are in pixels of a 1080-line picture (see crop.fs).
const BORDER_UNIT: f32 = 1.0 / 1080.0;

fn rounded_rect(p: vec2<f32>, centre: vec2<f32>, half_size: vec2<f32>, radius: f32) -> f32 {
    let q = abs(p - centre) - half_size + vec2<f32>(radius);
    return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0) - radius;
}

fn edge(inside: f32, pixel: f32) -> f32 {
    if (params.feather <= 0.0) {
        return clamp(inside / pixel + 0.5, 0.0, 1.0);
    }
    return smoothstep(-params.feather, params.feather, inside);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    // Back from this pixel to the picture point the transform shows here —
    // its mirror, flipped.
    let placed = (in.uv - vec2<f32>(0.5) - params.pos) / params.scale + vec2<f32>(0.5);
    let p = select(placed, vec2<f32>(1.0) - placed, params.flip > vec2<f32>(0.5));
    if (any(p < vec2<f32>(0.0)) || any(p > vec2<f32>(1.0))) {
        return vec4<f32>(0.0);
    }
    let source_uv = p * params.uv_scale + params.uv_offset;
    var src = vec4<f32>(0.0);
    if (all(source_uv >= vec2<f32>(0.0)) && all(source_uv <= vec2<f32>(1.0))) {
        src = textureSampleLevel(source, samp, source_uv, 0.0);
        if (params.premultiplied == 1u && src.a > 0.0) {
            src = vec4<f32>(src.rgb / src.a, src.a);
        }
    }
    if (params.over_black > 0.0) {
        // Black comes in under the picture: its colour as it shows over
        // black, opaque so far as it is over black — a clear pixel's own
        // colour counting for nothing on the way.
        let a = select(mix(src.a, 1.0, params.over_black), 1.0, params.over_black >= 1.0);
        src = vec4<f32>(src.rgb * src.a / a, a);
    }
    if (params.crop_mode == 0u) {
        return src;
    }

    // Nothing kept, nothing drawn (see crop.fs).
    if (params.crop.y <= params.crop.x || params.crop.w <= params.crop.z) {
        return vec4<f32>(0.0);
    }

    // One of this frame's pixels, measured on the picture.
    let frame = max(params.frame, vec2<f32>(1.0));
    let pixel = 1.0 / (frame * params.scale);
    let up = 1.0 - p.y;
    let soft = edge(p.x - params.crop.x, pixel.x)
        * edge(params.crop.y - p.x, pixel.x)
        * edge(up - params.crop.z, pixel.y)
        * edge(params.crop.w - up, pixel.y);
    // The whole picture's own edge is hard: on a Merge's way from it, the
    // crop's edge only so far soft.
    let hard = step(params.crop.x, p.x) * step(p.x, params.crop.y)
        * step(params.crop.z, up) * step(up, params.crop.w);
    let mask = select(mix(hard, soft, params.cropped), soft, params.cropped >= 1.0);
    if (params.crop_mode == 1u) {
        return vec4<f32>(src.rgb * mask, src.a * mask);
    }

    let aspect = frame.x / frame.y;
    let pp = vec2<f32>(p.x * aspect, up);
    let lo = vec2<f32>(params.crop.x * aspect, params.crop.z);
    let hi = vec2<f32>(params.crop.y * aspect, params.crop.w);
    var half_size = max((hi - lo) * 0.5, vec2<f32>(0.0));
    if (half_size.x <= 0.0 || half_size.y <= 0.0) {
        // Nothing kept: no border round it either (see crop.fs).
        return vec4<f32>(0.0);
    }
    var radius = min(params.border_radius * BORDER_UNIT, min(half_size.x, half_size.y));
    if (params.circle > 0.0) {
        // A square on the shorter side, fully rounded (see crop.fs) — so
        // far as the shape is the circle.
        let side = min(half_size.x, half_size.y);
        half_size = mix(half_size, vec2<f32>(side), params.circle);
        radius = mix(radius, side, params.circle);
    }
    let px = pixel.y;
    // On a Merge's way from the crop without a border, the shape clear of
    // its soft edge — the edge reaches a feather and a pixel past the crop
    // — so far as the border is not there yet.
    let unbordered = 1.0 - params.bordered;
    half_size += unbordered * vec2<f32>(
        (params.feather + 2.0 * pixel.x) * aspect,
        params.feather + 2.0 * pixel.y,
    );
    let d = rounded_rect(pp, (lo + hi) * 0.5, half_size, radius);
    let shape = clamp(0.5 - d / px, 0.0, 1.0);
    let width = params.border_width * BORDER_UNIT;
    var band = 0.0;
    if (width > 0.0) {
        band = clamp(0.5 + (d + width) / px, 0.0, 1.0) * shape;
    }
    let picture_a = src.a * mask * shape;
    // A border on its way in shows only so far as it is there: however
    // thin, its band covers half a pixel on the shape's edge.
    let a = band * params.border_color.a * params.bordered;
    let out_a = a + picture_a * (1.0 - a);
    if (out_a <= 0.0) {
        return vec4<f32>(0.0);
    }
    // The crop alone darkens the picture's colours with its edge (above),
    // so far as the border is not there yet.
    let shade = mix(1.0, mask, unbordered);
    let out_rgb = (params.border_color.rgb * a + src.rgb * shade * picture_a * (1.0 - a)) / out_a;
    return vec4<f32>(out_rgb, out_a);
}
