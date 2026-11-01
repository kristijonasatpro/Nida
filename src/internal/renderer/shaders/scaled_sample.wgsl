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
// their weights slight), held within what the taps saw so an edge gets no
// halo: sharper than one bilinear tap where the picture is drawn bigger.
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
    let lowest = min(min(min(top, left), min(right, bottom)), middle);
    let highest = max(max(max(top, left), max(right, bottom)), middle);
    // Where it turns transparent, one bilinear tap: held channel by
    // channel, premultiplied colour came out brighter than its alpha —
    // a bright fringe on a logo's edge.
    if (highest.a - lowest.a > 0.002) {
        return textureSampleLevel(t, s, inside(uv, part, size), 0.0);
    }
    return clamp(colour, lowest, highest);
}

// The texels each pixel covers, averaged: n × n bilinear taps spread evenly
// over them, n the whole texels it covers across and down, four at most.
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
    let step = covers / n / size;
    let first = uv - step * (n - 1.0) * 0.5;
    var sum = vec4<f32>(0.0);
    for (var j = 0; j < i32(n.y); j = j + 1) {
        for (var i = 0; i < i32(n.x); i = i + 1) {
            let tap = inside(first + step * vec2<f32>(f32(i), f32(j)), part, size);
            sum = sum + textureSampleLevel(t, s, tap, 0.0);
        }
    }
    return sum / (n.x * n.y);
}

// The picture at `uv`, each pixel covering `covers` texels of it (see
// `texels_per_pixel`), only its `part` drawn (see `drawn_part`): averaged
// over them where drawn a quarter smaller or more, bicubic where drawn
// bigger across or down, one bilinear tap about its own size — so a
// picture drawn at its own size stays exactly as it is.
fn sample_scaled(
    t: texture_2d<f32>,
    s: sampler,
    uv: vec2<f32>,
    covers: vec2<f32>,
    part: vec4<f32>,
) -> vec4<f32> {
    let size = vec2<f32>(textureDimensions(t));
    if (covers.x > 1.25 || covers.y > 1.25) {
        return sample_reduced(t, s, uv, size, covers, part);
    }
    // Bicubic is exact along a line drawn at its own size: either way
    // drawn bigger is enough.
    if (min(covers.x, covers.y) < 0.98) {
        return sample_bicubic(t, s, uv, size, part);
    }
    return textureSampleLevel(t, s, inside(uv, part, size), 0.0);
}
