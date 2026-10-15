// A playlist's picture: the item on screen, or it giving way to the next —
// dissolving into it, or pushed off by it. Both are straight alpha, as decks
// are, so a fade mixes them premultiplied: a picture fitted with clear bars
// fades into one of another shape without a dark fringe.
//
// A slide is blurred along its way, as a camera's shutter blurs anything
// moving: at speed, sharp frames a long step apart look to judder.

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

struct MixParams {
    // 0 the first picture alone, 1 a fade, 2 a slide.
    mode: u32,
    // Which pictures there are: bit 0 the first, bit 1 the next.
    present: u32,
    // How far the transition has got, 0 to 1.
    progress: f32,
    // How much of the way a slide is blurred over (0 for none).
    blur: f32,
    // Where a slide brings the next picture in from, in UV (y down).
    enter_from: vec2<f32>,
    // Samples along the blur: one every pixel and a half of it, at most 16.
    taps: u32,
    _padding: f32,
};

@group(0) @binding(0) var samp: sampler;
@group(0) @binding(1) var shown_tex: texture_2d<f32>;
@group(0) @binding(2) var next_tex: texture_2d<f32>;
@group(0) @binding(3) var<uniform> params: MixParams;

fn inside(uv: vec2<f32>) -> bool {
    return all(uv >= vec2<f32>(0.0)) && all(uv <= vec2<f32>(1.0));
}

fn shown(uv: vec2<f32>) -> vec4<f32> {
    if ((params.present & 1u) == 0u || !inside(uv)) {
        return vec4<f32>(0.0);
    }
    return textureSampleLevel(shown_tex, samp, uv, 0.0);
}

fn next(uv: vec2<f32>) -> vec4<f32> {
    if ((params.present & 2u) == 0u || !inside(uv)) {
        return vec4<f32>(0.0);
    }
    return textureSampleLevel(next_tex, samp, uv, 0.0);
}

@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let t = clamp(params.progress, 0.0, 1.0);
    if (params.mode == 1u) {
        let a = shown(in.uv);
        let b = next(in.uv);
        let mixed = mix(vec4<f32>(a.rgb * a.a, a.a), vec4<f32>(b.rgb * b.a, b.a), t);
        if (mixed.a <= 0.0) {
            return vec4<f32>(0.0);
        }
        return vec4<f32>(mixed.rgb / mixed.a, mixed.a);
    }
    if (params.mode == 2u) {
        if (params.blur <= 0.0) {
            return slide(in.uv, t);
        }
        // Both pictures move as one, so the slide a moment on is the slide
        // now seen a little further along its way: averaged over the blur.
        let taps = clamp(params.taps, 1u, MAX_BLUR_TAPS);
        var sum = vec4<f32>(0.0);
        for (var i = 0u; i < taps; i = i + 1u) {
            let s = (f32(i) + 0.5) / f32(taps) - 0.5;
            let c = slide(in.uv + params.enter_from * (s * params.blur), t);
            sum = sum + vec4<f32>(c.rgb * c.a, c.a);
        }
        let avg = sum / f32(taps);
        if (avg.a <= 0.0) {
            return vec4<f32>(0.0);
        }
        return vec4<f32>(avg.rgb / avg.a, avg.a);
    }
    return shown(in.uv);
}

const MAX_BLUR_TAPS: u32 = 16u;

// The next picture from `enter_from` to its place, `t` of the way; the first
// ahead of it.
fn slide(uv: vec2<f32>, t: f32) -> vec4<f32> {
    let b_uv = uv - params.enter_from * (1.0 - t);
    if (inside(b_uv)) {
        return next(b_uv);
    }
    return shown(uv + params.enter_from * t);
}
