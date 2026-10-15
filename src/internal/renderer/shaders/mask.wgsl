// An effect seen through a mask: the picture before the effect and after
// it, mixed by how far inside the mask's shape each pixel is — all of the
// effect inside, none outside, a fade across a feathered edge (or the other
// way round, inverted).
//
// Distances are measured in units of the frame's height, x scaled by the
// aspect ratio, so a feather is as wide across as it is down and a square on
// the screen is square here.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct MaskParams {
    // 0 rectangle, 1 ellipse, 2 a drawn outline.
    shape: u32,
    // Points in the outline.
    count: u32,
    invert: u32,
    _padding: u32,
    // The rectangle (or the ellipse's): left, top, width, height, in UV.
    rect: vec4<f32>,
    // The fade's width, in frame heights; 0 for a clean edge.
    feather: f32,
    aspect: f32,
    // One pixel, in frame heights.
    pixel: f32,
    _padding2: f32,
    // The outline's points, two to each vector, in UV.
    points: array<vec4<f32>, 32>,
};

@group(0) @binding(0) var before_tex: texture_2d<f32>;
@group(0) @binding(1) var after_tex: texture_2d<f32>;
@group(0) @binding(2) var<uniform> mask: MaskParams;

fn outline_point(i: u32) -> vec2<f32> {
    let pair = mask.points[i / 2u];
    let p = select(pair.zw, pair.xy, i % 2u == 0u);
    return vec2<f32>(p.x * mask.aspect, p.y);
}

// Signed distance from `p` to the shape: negative inside.
fn distance_to_shape(uv: vec2<f32>) -> f32 {
    let p = vec2<f32>(uv.x * mask.aspect, uv.y);
    let half_size = mask.rect.zw * 0.5 * vec2<f32>(mask.aspect, 1.0);
    let centre = (mask.rect.xy + mask.rect.zw * 0.5) * vec2<f32>(mask.aspect, 1.0);
    if (mask.shape == 0u) {
        let q = abs(p - centre) - half_size;
        return length(max(q, vec2<f32>(0.0))) + min(max(q.x, q.y), 0.0);
    }
    if (mask.shape == 1u) {
        // Close enough to the true distance for a soft edge, exact on it.
        let k = length((p - centre) / max(half_size, vec2<f32>(1e-6)));
        return (k - 1.0) * min(half_size.x, half_size.y);
    }
    var nearest = 1e9;
    var inside = false;
    let count = min(mask.count, 64u);
    for (var i = 0u; i < count; i++) {
        let a = outline_point(i);
        let b = outline_point((i + 1u) % count);
        let ab = b - a;
        let t = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-12), 0.0, 1.0);
        nearest = min(nearest, length(p - a - ab * t));
        // Crossings of a ray to the right: odd is inside.
        if ((a.y > p.y) != (b.y > p.y)) {
            let x = a.x + (p.y - a.y) / (b.y - a.y) * ab.x;
            if (p.x < x) {
                inside = !inside;
            }
        }
    }
    return select(nearest, -nearest, inside);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let at = vec2<i32>(in.position.xy);
    let before = textureLoad(before_tex, at, 0);
    let after = textureLoad(after_tex, at, 0);
    let d = distance_to_shape(in.uv);
    var amount: f32;
    if (mask.feather > 0.0) {
        amount = 1.0 - smoothstep(-0.5 * mask.feather, 0.5 * mask.feather, d);
    } else {
        // A clean edge, a pixel wide.
        amount = clamp(0.5 - d / mask.pixel, 0.0, 1.0);
    }
    if (mask.invert == 1u) {
        amount = 1.0 - amount;
    }
    return mix(before, after, amount);
}
