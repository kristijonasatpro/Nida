struct Params {
    size: vec4<u32>,
    packing: vec4<u32>,
    levels: vec4<f32>,
    red: vec4<f32>,
    green: vec4<f32>,
    blue: vec4<f32>,
}
@group(0) @binding(0) var y_plane: texture_2d<u32>;
@group(0) @binding(1) var uv_plane: texture_2d<u32>;
@group(0) @binding(2) var<uniform> p: Params;

@vertex fn vertex(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let xy = array<vec2<f32>, 3>(vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    return vec4(xy[i], 0.0, 1.0);
}
fn luma(at: vec2<i32>) -> f32 {
    let crop = vec2<i32>(i32(p.packing.w & 65535u), i32(p.packing.w >> 16u));
    let pos = clamp(at, crop, crop + vec2<i32>(p.size.xy) - 1);
    return f32(textureLoad(y_plane, pos, 0).r >> p.packing.y);
}
fn chroma(at: vec2<i32>) -> vec2<f32> {
    let crop = vec2<i32>(i32(p.packing.w & 65535u), i32(p.packing.w >> 16u));
    let pos = clamp(at, crop / 2, (crop + vec2<i32>(p.size.xy) - 1) / 2);
    return vec2<f32>(textureLoad(uv_plane, pos, 0).rg >> vec2(p.packing.y));
}
fn y_sample(at: vec2<f32>) -> f32 {
    let a = vec2<i32>(floor(at)); let f = fract(at);
    return mix(mix(luma(a), luma(a + vec2(1, 0)), f.x),
               mix(luma(a + vec2(0, 1)), luma(a + vec2(1, 1)), f.x), f.y);
}
fn y_scaled_sample(at: vec2<f32>, ratio: f32) -> f32 {
    if ratio <= 1.0 { return y_sample(at); }
    let row = i32(floor(at.y));
    var sum = 0.0;
    var weights = 0.0;
    for (var x = i32(ceil(at.x - ratio)); x <= i32(floor(at.x + ratio)); x += 1) {
        let weight = max(0.0, 1.0 - abs(f32(x) - at.x) / ratio);
        sum += weight * mix(luma(vec2(x, row)), luma(vec2(x, row + 1)), fract(at.y));
        weights += weight;
    }
    return sum / weights;
}
fn uv_sample(at: vec2<f32>) -> vec2<f32> {
    let a = vec2<i32>(floor(at)); let f = fract(at);
    return mix(mix(chroma(a), chroma(a + vec2(1, 0)), f.x),
               mix(chroma(a + vec2(0, 1)), chroma(a + vec2(1, 1)), f.x), f.y);
}
// swscale's ordinary 4:2:0 RGBA path keeps horizontal chroma pairs and
// interpolates vertically. Match its quantization, including the 8-bit
// NV12 lookup path, rather than introducing a different reconstruction.
fn uv_cpu_sample(at: vec2<f32>, uv: vec2<f32>, upright: vec2<u32>) -> vec2<f32> {
    let x = (floor(uv.x * f32(upright.x) / 2.0) + 0.5) * f32(p.size.x) / f32(upright.x) - 0.5 + f32(p.packing.w & 65535u) / 2.0;
    let y = (at.y - p.green.w) / 2.0;
    let ratio = f32(p.size.x) / f32(upright.x);
    if ratio <= 1.0 { return uv_sample(vec2(x, y)); }
    let row = i32(floor(y));
    var sum = vec2(0.0);
    var weights = 0.0;
    for (var column = i32(ceil(x - ratio)); column <= i32(floor(x + ratio)); column += 1) {
        let weight = max(0.0, 1.0 - abs(f32(column) - x) / ratio);
        sum += weight * mix(chroma(vec2(column, row)), chroma(vec2(column, row + 1)), fract(y));
        weights += weight;
    }
    return sum / weights;
}
fn nv12_sdr_compat(y: f32, uv: vec2<f32>) -> vec3<f32> {
    // FFmpeg's public sws_getCoefficients values, in 16.16 units. The
    // following arithmetic reproduces its integer luma/chroma lookup indices.
    var coeff = vec4(104597.0, 132201.0, -25675.0, -53279.0);
    if abs(p.red.z - 1.5748) < 0.0001 { coeff = vec4(117489.0, 138438.0, -13975.0, -34925.0); }
    else if abs(p.red.z - 1.4746) < 0.0001 { coeff = vec4(110013.0, 140363.0, -12277.0, -42626.0); }
    else if abs(p.red.z - 1.576) < 0.0001 { coeff = vec4(117579.0, 136230.0, -16907.0, -35559.0); }
    else if abs(p.red.z - 1.4) < 0.0001 { coeff = vec4(104448.0, 132798.0, -24759.0, -53109.0); }
    let full = p.levels.x == 0.0;
    let cy = select(76309.0, 65536.0, full);
    if full { coeff = trunc(coeff * 224.0 / 255.0); }
    coeff = trunc((coeff * 65536.0 + 32768.0) / cy);
    let chroma_codes = floor(uv + 0.5);
    let shifts = floor(chroma_codes.yxxy * coeff / 65536.0) - floor(coeff / 512.0);
    let base = select(326.0, 384.0, full);
    let black = select(1048576.0, 0.0, full);
    let indices = vec3(shifts.x, shifts.z + shifts.w, shifts.y);
    return clamp(floor((cy * (vec3(y + base) + indices) - 25165824.0 - black + 32768.0) / 65536.0), vec3(0.0), vec3(255.0)) / 255.0;
}
fn pq(v: vec3<f32>) -> vec3<f32> {
    let s = pow(max(v, vec3(0.0)), vec3(1.0 / 78.84375));
    return (10000.0 / 203.0) * pow(max(s - 0.8359375, vec3(0.0)) /
        (18.8515625 - 18.6875 * s), vec3(1.0 / 0.1593017578125));
}
fn hlg(v: vec3<f32>) -> vec3<f32> {
    return select((exp((v - 0.5599107) / 0.17883277) + 0.28466892) / 12.0,
                  v * v / 3.0, v <= vec3(0.5));
}
fn to_srgb(v: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(max(v, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055,
                  12.92 * v, v <= vec3(0.0031308));
}
fn to_linear(v: vec3<f32>) -> vec3<f32> {
    return select(pow((v + 0.055) / 1.055, vec3(2.4)), v / 12.92, v <= vec3(0.04045));
}
@fragment fn fragment(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    var uv = pos.xy / vec2<f32>(p.size.zw);
    switch p.packing.x {
        case 1u: { uv = vec2(uv.y, 1.0 - uv.x); }
        case 2u: { uv = 1.0 - uv; }
        case 3u: { uv = vec2(1.0 - uv.y, uv.x); }
        default: {}
    }
    let crop = vec2<f32>(f32(p.packing.w & 65535u), f32(p.packing.w >> 16u));
    let at = uv * vec2<f32>(p.size.xy) - 0.5 + crop;
    // Respect frame chroma placement; untagged material uses centred chroma.
    var upright_size = p.size.zw;
    if (p.packing.x & 1u) != 0u { upright_size = upright_size.yx; }
    let luma_code = y_scaled_sample(at, f32(p.size.x) / f32(upright_size.x));
    let y = (luma_code - p.levels.x) / p.levels.y;
    let chroma_codes = uv_cpu_sample(at, uv, upright_size);
    let cbcr = (chroma_codes - p.levels.z) / p.levels.w;
    let yuv = vec3(y, cbcr);
    var rgb = clamp(vec3(dot(p.red.xyz, yuv), dot(p.green.xyz, yuv), dot(p.blue.xyz, yuv)), vec3(0.0), vec3(1.0));
    if p.packing.z == 0u {
        // swscale's SDR RGBA path uses 8-bit lookup indices for P010 too.
        // Normalize its 10-bit codes before matching the integer conversion;
        // HDR retains the full-precision path below.
        let divisor = select(1.0, 4.0, p.packing.y != 0u);
        rgb = nv12_sdr_compat(floor(luma_code / divisor + 0.5), chroma_codes / divisor);
    }
    if p.packing.z != 0u {
        rgb = round(rgb * 65535.0) / 65535.0;
        if p.packing.z == 1u { rgb = pq(rgb); }
        else {
            rgb = hlg(rgb);
            let ys = dot(rgb, vec3(0.2627, 0.6780, 0.0593));
            // Same 4096-entry nearest lookup as hdr.rs.
            let q = round(clamp(ys, 0.0, 1.0) * 4095.0) / 4095.0;
            rgb *= pow(q, 0.2) * (1000.0 / 203.0);
        }
        rgb = max(vec3(dot(rgb, vec3(1.660491, -0.587641, -0.072850)),
                       dot(rgb, vec3(-0.124550, 1.1329, -0.008349)),
                       dot(rgb, vec3(-0.018151, -0.100579, 1.118730))), vec3(0.0));
        let peak = max(rgb.r, max(rgb.g, rgb.b));
        if peak > 0.75 {
            let most = (1000.0 / 203.0 - 0.75) / 0.25;
            let over = min((peak - 0.75) / 0.25, most);
            rgb *= (0.75 + 0.25 * over * (1.0 + over / (most * most)) / (1.0 + over)) / peak;
        }
        rgb = to_srgb(round(clamp(rgb, vec3(0.0), vec3(1.0)) * 4095.0) / 4095.0);
    }
    // Match the existing 8-bit sRGB source texture. The sRGB render attachment
    // re-encodes this linear value; downstream sampling decodes it exactly once.
    rgb = round(clamp(rgb, vec3(0.0), vec3(1.0)) * 255.0) / 255.0;
    return vec4(to_linear(rgb), 1.0);
}
