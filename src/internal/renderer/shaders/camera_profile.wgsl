// A camera's log or HLG picture turned into Rec.709 video, for the show
// (see renderer/camera_profile.rs, whose reference functions this mirrors
// line for line — keep them in step).
//
// Reads the camera's code values as sent (a non-sRGB view of its picture),
// writes Rec.709 video-coded values into an Rgba8Unorm picture that decks
// read through an sRGB view: linearised exactly as any other video source.

struct Profile {
    // The camera's gamut to Rec.709 primaries, a row each.
    row0: vec4<f32>,
    row1: vec4<f32>,
    row2: vec4<f32>,
    // 1 S-Log2, 2 S-Log3, 3 Blackmagic Film Gen 5, 4 HLG.
    curve: u32,
    // Linear light multiplied before coding: HLG's reference white to SDR
    // white (ITU-R BT.2408), 1 for the log curves.
    scale: f32,
    _pad0: u32,
    _pad1: u32,
};

@group(0) @binding(0) var picture: texture_2d<f32>;
@group(0) @binding(1) var<uniform> profile: Profile;

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
};

@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32((vertex_index & 1u) << 2u);
    let y = f32((vertex_index & 2u) << 1u);
    out.position = vec4<f32>(x - 1.0, 1.0 - y, 0.0, 1.0);
    return out;
}

const LOG2_10: f32 = 3.321928094887362;

fn pow10(x: f32) -> f32 {
    return exp2(x * LOG2_10);
}

// Sony S-Log2 (Sony, "S-Log2 Technical Paper", 2012): `x` is the code value
// with video levels taken out (0 at 64, 1 at 940 of 1023), as Nida reads a
// limited-range camera. Linear reflection, 0.18 for an 18% grey card.
fn s_log2(x: f32) -> f32 {
    var lin: f32;
    if (x >= 0.030001222851889303) {
        lin = pow10((x - 0.616596 - 0.03) / 0.432699) - 0.037584;
    } else {
        lin = (x - 0.030001222851889303) / 5.0;
    }
    return lin * 0.9 * 219.0 / 155.0;
}

// Sony S-Log3 (Sony, "Technical Summary for S-Gamut3.Cine/S-Log3 and
// S-Gamut3/S-Log3"): defined on the code value over 1023, video levels in.
fn s_log3(x: f32) -> f32 {
    let cv = (876.0 * x + 64.0);
    if (cv >= 171.2102946929) {
        return pow10((cv - 420.0) / 261.5) * 0.19 - 0.01;
    }
    return (cv - 95.0) * 0.01125 / (171.2102946929 - 95.0);
}

// Blackmagic Film Generation 5 (Blackmagic Design, "Blackmagic Generation 5
// Color Science", 2021).
fn film_gen5(v: f32) -> f32 {
    let A = 0.08692876065491224;
    let B = 0.005494072432257808;
    let C = 0.5300133392291939;
    let D = 8.283605932402494;
    let E = 0.09246575342465753;
    let log_cut = D * 0.005 + E;
    if (v < log_cut) {
        return (v - E) / D;
    }
    return exp((v - C) / A) - B;
}

// HLG inverse OETF (ITU-R BT.2100 / ARIB STD-B67), scene light 0..1.
fn hlg_inverse(e: f32) -> f32 {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let ep = max(e, 0.0);
    if (ep <= 0.5) {
        return ep * ep / 3.0;
    }
    return (exp((ep - c) / a) + b) / 12.0;
}

// HLG to display light (BT.2100 OOTF, system gamma 1.2 for a 1 000 cd/m²
// display, BT.2020 luminance), peak 1.
fn hlg_display(code: vec3<f32>) -> vec3<f32> {
    let scene = vec3<f32>(hlg_inverse(code.r), hlg_inverse(code.g), hlg_inverse(code.b));
    let ys = dot(vec3<f32>(0.2627, 0.6780, 0.0593), scene);
    return pow(max(ys, 1e-9), 0.2) * scene;
}

// Rec.709 camera OETF (ITU-R BT.709-6), carried on above 1 for highlights.
fn rec709(l: f32) -> f32 {
    if (l < 0.018) {
        return 4.5 * l;
    }
    return 1.099 * pow(l, 0.45) - 0.099;
}

// sRGB coding (IEC 61966-2-1), carried on above 1: the inverse of how Nida
// reads video, so display light comes back out unchanged.
fn srgb(l: f32) -> f32 {
    if (l <= 0.0031308) {
        return 12.92 * l;
    }
    return 1.055 * pow(l, 1.0 / 2.4) - 0.055;
}

// A camera's knee: video above 85% bends smoothly towards 100% rather than
// clipping, so highlights a log or HLG camera keeps still show.
fn knee(v: f32) -> f32 {
    let k = 0.85;
    if (v <= k) {
        return v;
    }
    return k + (1.0 - k) * (1.0 - exp(-(v - k) / (1.0 - k)));
}

@fragment
fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let code = textureLoad(picture, vec2<i32>(position.xy), 0).rgb;
    var lin: vec3<f32>;
    switch profile.curve {
        case 1u: {
            lin = vec3<f32>(s_log2(code.r), s_log2(code.g), s_log2(code.b));
        }
        case 2u: {
            lin = vec3<f32>(s_log3(code.r), s_log3(code.g), s_log3(code.b));
        }
        case 3u: {
            lin = vec3<f32>(film_gen5(code.r), film_gen5(code.g), film_gen5(code.b));
        }
        case 4u: {
            lin = hlg_display(code);
        }
        default: {
            lin = code;
        }
    }
    lin = lin * profile.scale;
    let rgb = max(
        vec3<f32>(
            dot(profile.row0.xyz, lin),
            dot(profile.row1.xyz, lin),
            dot(profile.row2.xyz, lin),
        ),
        vec3<f32>(0.0),
    );
    var video: vec3<f32>;
    if (profile.curve == 4u) {
        video = vec3<f32>(srgb(rgb.r), srgb(rgb.g), srgb(rgb.b));
    } else {
        video = vec3<f32>(rec709(rgb.r), rec709(rgb.g), rec709(rgb.b));
    }
    let shown = vec3<f32>(knee(video.r), knee(video.g), knee(video.b));
    return vec4<f32>(clamp(shown, vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
