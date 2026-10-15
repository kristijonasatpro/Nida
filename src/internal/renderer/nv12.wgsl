// A frame to NV12 for the video encoders: BT.709 at video levels (Y 16–235,
// U and V 16–240), eight bits — the full-size Y plane, then U and V
// interleaved at half size each way, each the average of its 2×2 pixels, as
// FFmpeg's own converter makes them (so a recording looks as it did).
//
// Each invocation does a 4×2 block: two words of Y, one word of U V U V —
// whole words, so no two invocations write the same one.

struct Params {
    width: u32,
    height: u32,
    // 1: the source decodes sRGB as it is read, so encode it again — the
    // encoders take the picture as the screen shows it.
    encode: u32,
    _padding: u32,
}

@group(0) @binding(0)
var source: texture_2d<f32>;

@group(0) @binding(1)
var<uniform> params: Params;

@group(0) @binding(2)
var<storage, read_write> out_words: array<u32>;

const KR: f32 = 0.2126;
const KG: f32 = 0.7152;
const KB: f32 = 0.0722;

fn srgb_oetf(c: vec3<f32>) -> vec3<f32> {
    let v = clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
    let lower = v * 12.92;
    let upper = 1.055 * pow(v, vec3<f32>(1.0 / 2.4)) - 0.055;
    return select(upper, lower, v <= vec3<f32>(0.0031308));
}

fn rgb_at(x: i32, y: i32) -> vec3<f32> {
    let at = vec2<i32>(clamp(x, 0, i32(params.width) - 1), y);
    var c = textureLoad(source, at, 0).rgb;
    if (params.encode == 1u) {
        c = srgb_oetf(c);
    }
    return clamp(c, vec3<f32>(0.0), vec3<f32>(1.0));
}

fn luma(c: vec3<f32>) -> f32 {
    return dot(c, vec3<f32>(KR, KG, KB));
}

// Nearest code, halves up (WGSL's round() goes to even).
fn code(value: f32) -> u32 {
    return u32(clamp(floor(value + 0.5), 0.0, 255.0));
}

// A 2×2 block's colour, averaged.
fn block(x: i32, y0: i32) -> vec3<f32> {
    return (rgb_at(x, y0) + rgb_at(x + 1, y0) + rgb_at(x, y0 + 1) + rgb_at(x + 1, y0 + 1)) * 0.25;
}

@compute @workgroup_size(16, 8, 1)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    let x0 = id.x * 4u;
    let y0 = id.y * 2u;
    if (x0 >= params.width || y0 >= params.height) {
        return;
    }
    let words_per_row = params.width / 4u;

    for (var row = 0u; row < 2u; row++) {
        var word = 0u;
        for (var i = 0u; i < 4u; i++) {
            let c = rgb_at(i32(x0 + i), i32(y0 + row));
            word = word | (code(16.0 + 219.0 * luma(c)) << (8u * i));
        }
        out_words[(y0 + row) * words_per_row + x0 / 4u] = word;
    }

    var uv = 0u;
    for (var k = 0u; k < 2u; k++) {
        let x = i32(x0 + 2u * k);
        let c = block(x, i32(y0));
        let yn = luma(c);
        let u = code(128.0 + 224.0 * (c.b - yn) / 1.8556);
        let v = code(128.0 + 224.0 * (c.r - yn) / 1.5748);
        uv = uv | (u << (16u * k)) | (v << (16u * k + 8u));
    }
    out_words[words_per_row * params.height + (y0 / 2u) * words_per_row + x0 / 4u] = uv;
}
