// HAP YCoCg→RGB conversion + dual-plane alpha compositing shader.
// Used for HAP Q (YCoCg DXT5) and HAP Q Alpha (YCoCg DXT5 + BC4 alpha).

struct HapConvertParams {
    opacity: f32,
    /// 0.0 = no YCoCg conversion (passthrough), 1.0 = YCoCg→RGB
    do_ycocg: f32,
    /// 0.0 = single plane (alpha from color texture), 1.0 = dual plane (alpha from separate texture)
    has_alpha_plane: f32,
    _pad: f32,
    uv_scale: vec2<f32>,
    uv_offset: vec2<f32>,
    // The part of the texture the picture fills (a size not a whole number
    // of 4×4 blocks has a larger texture).
    extent: vec2<f32>,
    _pad2: vec2<f32>,
}

@group(0) @binding(0)
var tex_sampler: sampler;

@group(0) @binding(1)
var color_texture: texture_2d<f32>;

@group(0) @binding(2)
var<uniform> params: HapConvertParams;

@group(0) @binding(3)
var alpha_texture: texture_2d<f32>;

/// Scaled YCoCg → RGB conversion (matches HAP Q spec).
/// Input: BC3/DXT5 texture where RGB stores YCoCg and A stores scale.
fn ycocg_to_rgb(color: vec4<f32>) -> vec3<f32> {
    let scale = (color.b * (255.0 / 8.0)) + 1.0;
    let co = (color.r - (0.5 * 256.0 / 255.0)) / scale;
    let cg = (color.g - (0.5 * 256.0 / 255.0)) / scale;
    let y = color.a;
    return vec3<f32>(
        y + co - cg,
        y + cg,
        y - co - cg,
    );
}

fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let low = c / 12.92;
    let high = pow((c + vec3<f32>(0.055)) / 1.055, vec3<f32>(2.4));
    return select(high, low, c <= vec3<f32>(0.04045));
}

@fragment
fn fs_main(@location(0) uv: vec2<f32>) -> @location(0) vec4<f32> {
    let scaled_uv = uv * params.uv_scale + params.uv_offset;
    // The bars Fit and Center leave are transparent, as for any other clip.
    // (Sampling first keeps the texture reads in uniform control flow.)
    let color = textureSample(color_texture, tex_sampler, scaled_uv * params.extent);
    let outside = scaled_uv.x < 0.0 || scaled_uv.x > 1.0 || scaled_uv.y < 0.0 || scaled_uv.y > 1.0;

    // Sampled before any branch, keeping texture reads in uniform control flow.
    let alpha_sample = textureSample(alpha_texture, tex_sampler, scaled_uv * params.extent);

    var rgb: vec3<f32>;
    var alpha: f32;
    if (params.do_ycocg > 0.5) {
        // Gamma-encoded once converted (the texture could not decode it):
        // to the linear light Nida mixes in. Its A is luma, not alpha — a
        // Hap Q clip is opaque, unless its alpha plane says otherwise.
        rgb = srgb_to_linear(clamp(ycocg_to_rgb(color), vec3<f32>(0.0), vec3<f32>(1.0)));
        alpha = 1.0;
    } else {
        // Decoded to linear by the texture.
        rgb = color.rgb;
        alpha = color.a;
    }
    if (params.has_alpha_plane > 0.5) {
        // Dual-plane: alpha comes from separate BC4 texture (red channel)
        alpha = alpha_sample.r;
    }

    if (outside) {
        return vec4<f32>(0.0);
    }
    return vec4<f32>(rgb, alpha * params.opacity);
}
