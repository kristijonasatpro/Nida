// Expands received 8-bit UYVY into RGB, the inverse of the send path's
// conversion: limited-range BT.601 for SD / BT.709 for HD. Preserve Nida's
// existing display-encoded RGB storage; the sRGB attachment encodes on write.

// One `U Y0 V Y1` quad per texel, so the texture is half the frame's width.
@group(0) @binding(0)
var packed: texture_2d<f32>;

@vertex
fn fullscreen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn srgb_inverse(encoded: vec3<f32>) -> vec3<f32> {
    let lower = encoded / 12.92;
    let upper = pow((encoded + 0.055) / 1.055, vec3<f32>(2.4));
    return select(upper, lower, encoded <= vec3<f32>(0.04045));
}

@fragment
fn uyvy(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let x = u32(position.x);
    let quad = textureLoad(packed, vec2<i32>(i32(x / 2u), i32(position.y)), 0) * 255.0;
    let luma_code = select(quad.y, quad.w, (x & 1u) == 1u);

    let luma = (luma_code - 16.0) / 219.0;
    let cb = (quad.x - 128.0) / 224.0;
    let cr = (quad.z - 128.0) / 224.0;
    let hd = textureDimensions(packed).y >= 720u;
    let kr = select(0.299, 0.2126, hd);
    let kb = select(0.114, 0.0722, hd);
    let r = luma + 2.0 * (1.0 - kr) * cr;
    let b = luma + 2.0 * (1.0 - kb) * cb;
    let g = (luma - kr * r - kb * b) / (1.0 - kr - kb);

    // The target is sRGB-encoded: write linear light and let the store encode.
    let encoded = clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0));
    return vec4<f32>(srgb_inverse(encoded), 1.0);
}
