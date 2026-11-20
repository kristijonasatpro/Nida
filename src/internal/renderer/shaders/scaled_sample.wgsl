// A picture sampled where it is drawn bigger or smaller than it is. One
// bilinear tap blurs a picture drawn bigger — an HD source in a 4K show,
// sharper still when the show goes back out to an HD screen — and skips
// texels of one drawn smaller by more than half, its fine detail flickering
// as moiré (a 4K show on a screen not exactly half its size, a preview a
// quarter of it). Prepended to the shaders that draw pictures scaled.

// Texels of the picture each pixel covers, across and down: below one it is
// drawn bigger, above one smaller. Taken where every pixel of the draw takes
// it alike — before any early return.
// Within a hair of a whole number it is that number: the GPU's derivatives
// round, and an exact half size read as a little over two took three taps
// a pixel, softer, where its neighbours took two.
fn texels_per_pixel(uv: vec2<f32>, size: vec2<f32>) -> vec2<f32> {
    let covers = fwidth(uv) * size;
    let whole = round(covers);
    return select(covers, whole, abs(covers - whole) < vec2<f32>(0.02));
}

// The part of the picture drawn — `uv_offset` to `uv_offset + uv_scale`,
// within it — as (left, top, right, bottom): a bus is a part of the
// master, and taps reaching past it took in the bus beside it.
fn drawn_part(scale: vec2<f32>, offset: vec2<f32>) -> vec4<f32> {
    let far = offset + scale;
    return clamp(
        vec4<f32>(min(offset, far), max(offset, far)),
        vec4<f32>(0.0),
        vec4<f32>(1.0),
    );
}

// `uv` held between the first and last texel centres inside `part` (see
// `drawn_part`), so a bilinear tap takes no texel from outside it — or, a
// part with no centre inside it, to the texel its middle is in.
fn inside(uv: vec2<f32>, part: vec4<f32>, size: vec2<f32>) -> vec2<f32> {
    let low = part.xy * size;
    let high = part.zw * size;
    let middle = floor((low + high) * 0.5) + 0.5;
    let none = ceil(low - 0.5) > floor(high - 0.5);
    let first = select(ceil(low - 0.5) + 0.5, middle, none);
    let last = select(floor(high - 0.5) + 0.5, middle, none);
    return clamp(uv * size, first, last) / size;
}

// Catmull-Rom bicubic from five bilinear taps (the 4×4's corners left out,
// their weights slight), held within what the taps saw — each as far as it
// weighs — so an edge gets no halo: sharper than one bilinear tap where the
// picture is drawn bigger.
fn sample_bicubic(
    t: texture_2d<f32>,
    s: sampler,
    uv: vec2<f32>,
    size: vec2<f32>,
    part: vec4<f32>,
) -> vec4<f32> {
    let pos = uv * size;
    let centre = floor(pos - 0.5) + 0.5;
    let f = pos - centre;
    let f2 = f * f;
    let f3 = f2 * f;
    let w0 = -0.5 * f3 + f2 - 0.5 * f;
    let w1 = 1.5 * f3 - 2.5 * f2 + 1.0;
    let w2 = -1.5 * f3 + 2.0 * f2 + 0.5 * f;
    let w3 = 0.5 * f3 - 0.5 * f2;
    let w12 = w1 + w2;
    let mid = (centre + w2 / w12) / size;
    let before = (centre - 1.0) / size;
    let after = (centre + 2.0) / size;
    let top = textureSampleLevel(t, s, inside(vec2<f32>(mid.x, before.y), part, size), 0.0);
    let left = textureSampleLevel(t, s, inside(vec2<f32>(before.x, mid.y), part, size), 0.0);
    let middle = textureSampleLevel(t, s, inside(mid, part, size), 0.0);
    let right = textureSampleLevel(t, s, inside(vec2<f32>(after.x, mid.y), part, size), 0.0);
    let bottom = textureSampleLevel(t, s, inside(vec2<f32>(mid.x, after.y), part, size), 0.0);
    let wt = w12.x * w0.y;
    let wl = w0.x * w12.y;
    let wm = w12.x * w12.y;
    let wr = w3.x * w12.y;
    let wb = w12.x * w3.y;
    let colour = (top * wt + left * wl + middle * wm + right * wr + bottom * wb)
        / (wt + wl + wm + wr + wb);
    // Each outer tap holds the result in as far as it weighs: a tap whose
    // weight goes to nothing as the picture moves a hair (from one texel
    // to the next) moved the bounds at once, and the picture jumped.
    let pt = mix(middle, top, clamp(abs(wt) * 20.0, 0.0, 1.0));
    let pl = mix(middle, left, clamp(abs(wl) * 20.0, 0.0, 1.0));
    let pr = mix(middle, right, clamp(abs(wr) * 20.0, 0.0, 1.0));
    let pb = mix(middle, bottom, clamp(abs(wb) * 20.0, 0.0, 1.0));
    let lowest = min(min(min(pt, pl), min(pr, pb)), middle);
    let highest = max(max(max(pt, pl), max(pr, pb)), middle);
    // Where it turns transparent — any tap, whatever it weighs — one
    // bilinear tap: held channel by channel, premultiplied colour came out
    // brighter than its alpha, a bright fringe on a logo's edge (and so it
    // did taken by degrees).
    let clearest = min(min(min(top.a, left.a), min(right.a, bottom.a)), middle.a);
    let densest = max(max(max(top.a, left.a), max(right.a, bottom.a)), middle.a);
    if (densest - clearest > 0.002) {
        return textureSampleLevel(t, s, inside(uv, part, size), 0.0);
    }
    return clamp(colour, lowest, highest);
}

// A picture's code values (sRGB) as light: what the GPU does of itself
// reading an sRGB texture, done here for a picture read as it was coded —
// a screen or window captured (see `ScreenCaptureManager::coded_view`) —
// after it is filtered. Averaged as light, dark text on a light page drawn
// smaller came out pale and thin, the light around each stroke counting
// for more than the stroke: a window half as tall again as the show, fitted
// into it, 6 dB further from the same shrink done by anything else (which
// all average code values), its ink a third lighter.
fn light_of_coded(c: vec4<f32>) -> vec4<f32> {
    let v = clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0));
    let low = v / 12.92;
    let high = pow((v + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return vec4<f32>(select(high, low, v < vec3<f32>(0.04045)), c.a);
}

// The texels each pixel covers, averaged: n × n bilinear taps, n the whole
// texels it covers across and down, four at most — each tap the average of
// its share of them exactly (see below).
// Where a pixel covers an even whole number of texels — a 4K show on an HD
// screen, a bus's slice of it on an LED wall, a preview at a quarter — a
// tap between two texels is their average already: half as many taps, two
// texels apart, take in every texel alike (one a pixel at half size, four
// at a quarter, where it took four and sixteen).
fn sample_reduced(
    t: texture_2d<f32>,
    s: sampler,
    uv: vec2<f32>,
    size: vec2<f32>,
    covers: vec2<f32>,
    part: vec4<f32>,
) -> vec4<f32> {
    let pairs = covers * 0.5;
    // ...and only where the pixel's middle falls on a texel edge: offset by
    // part of a texel (a bus's slice, a warp), a tap between two texels is
    // no longer their average, and half-way stripes would come out solid.
    let at = uv * size;
    let even = abs(pairs - round(pairs)) < vec2<f32>(0.01)
        & abs(at - round(at)) < vec2<f32>(0.01);
    let n = select(
        clamp(ceil(covers), vec2<f32>(1.0), vec2<f32>(4.0)),
        clamp(round(pairs), vec2<f32>(1.0), vec2<f32>(4.0)),
        even,
    );
    // The texels each tap stands for, and where the pixel's begin.
    let span = covers / n;
    let low = at - covers * 0.5;
    // A tap in the middle of its share took in the texels beside it too —
    // each pixel as much as a texel more than it covers, softer than the
    // average of what it covers (a window drawn at 0.7 of its size: a
    // seventeenth of its edges' contrast gone). A share of one texel or
    // less lies in one texel or across two: the tap goes between their
    // centres, as far towards the second as its part of the share, and is
    // their average by those parts exactly. The middle still where a tap
    // stands for more than a texel (two at a time, above; past four taps),
    // and along a side not drawn smaller, which one tap draws smoothly.
    let plain = even | (span > vec2<f32>(1.0)) | (covers < vec2<f32>(1.0));
    var sum = vec4<f32>(0.0);
    for (var j = 0; j < i32(n.y); j = j + 1) {
        for (var i = 0; i < i32(n.x); i = i + 1) {
            let begins = low + span * vec2<f32>(f32(i), f32(j));
            let texel = floor(begins);
            let over = max(begins + span - texel - vec2<f32>(1.0), vec2<f32>(0.0));
            let exact = texel + vec2<f32>(0.5) + over / max(span, vec2<f32>(1e-6));
            let middle = begins + span * 0.5;
            let tap = inside(select(exact, middle, plain) / size, part, size);
            sum = sum + textureSampleLevel(t, s, tap, 0.0);
        }
    }
    return sum / (n.x * n.y);
}

// The picture at `uv`, each pixel covering `covers` texels of it (see
// `texels_per_pixel`), only its `part` drawn (see `drawn_part`): averaged
// over them where drawn a quarter smaller or more, bicubic where drawn
// bigger or a little smaller, one bilinear tap at its own size — so a
// picture drawn at its own size stays exactly as it is — each turning into
// the next by degrees: a layer growing or shrinking in a Merge took a jump
// in sharpness where one met the other.
fn sample_scaled(
    t: texture_2d<f32>,
    s: sampler,
    uv: vec2<f32>,
    covers: vec2<f32>,
    part: vec4<f32>,
) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(t));
    // From bicubic to averaging over a quarter smaller.
    let reduce = smoothstep(1.15, 1.35, max(covers.x, covers.y));
    if (reduce >= 1.0) {
        return sample_reduced(t, s, uv, size, covers, part);
    }
    // From one tap at its own size to bicubic a few hundredths away from
    // it (`texels_per_pixel` takes within 0.02 as its own size exactly).
    let off = abs(covers - vec2<f32>(1.0));
    let own = 1.0 - smoothstep(0.02, 0.06, max(off.x, off.y));
    let tap = textureSampleLevel(t, s, inside(uv, part, size), 0.0);
    if (own >= 1.0) {
        return tap;
    }
    // Bicubic where drawn other than its own size, a little smaller too:
    // one bilinear tap there blurred text as much as averaging it (a 4K
    // screen at 0.89 of the show, measured on an M5 Max: a fifth of its
    // edges' contrast gone, as sharp as ffmpeg's bilinear, where its bicubic
    // kept them).
    let sharp = mix(sample_bicubic(t, s, uv, size, part), tap, own);
    if (reduce <= 0.0) {
        return sharp;
    }
    return mix(sharp, sample_reduced(t, s, uv, size, covers, part), reduce);
}
