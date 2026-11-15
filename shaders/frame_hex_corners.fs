/*{
    "DESCRIPTION": "Hex corners — a fine border, honeycomb growing from two opposite corners",
    "CREDIT": "Nida",
    "ISFVSN": "2.0",
    "CATEGORIES": [
        "Filter",
        "Frame"
    ],
    "INPUTS": [
        {
            "NAME": "inputImage",
            "TYPE": "image"
        },
        {
            "NAME": "speed",
            "TYPE": "float",
            "DEFAULT": 1.0,
            "MIN": 0.0,
            "MAX": 10.0,
            "LABEL": "Speed"
        },
        {
            "NAME": "window",
            "TYPE": "float",
            "DEFAULT": 0.86,
            "MIN": 0.4,
            "MAX": 1.0,
            "LABEL": "Size"
        },
        {
            "NAME": "shape",
            "TYPE": "long",
            "DEFAULT": 0,
            "VALUES": [
                0,
                1,
                2,
                3,
                4
            ],
            "LABELS": [
                "As the picture",
                "16:9",
                "4:3",
                "1:1",
                "9:16"
            ],
            "LABEL": "Shape"
        },
        {
            "NAME": "fill",
            "TYPE": "bool",
            "DEFAULT": false,
            "LABEL": "Fill the frame"
        },
        {
            "NAME": "weight",
            "TYPE": "float",
            "DEFAULT": 0.5,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Line weight"
        },
        {
            "NAME": "glow",
            "TYPE": "float",
            "DEFAULT": 0.5,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Glow"
        },
        {
            "NAME": "palette",
            "TYPE": "long",
            "DEFAULT": 3,
            "VALUES": [
                0,
                1,
                2,
                3,
                4,
                5,
                6,
                7,
                8,
                9,
                10,
                11,
                12
            ],
            "LABELS": [
                "Custom (colours below)",
                "Corporate Blue",
                "Graphite Teal",
                "Violet Tech",
                "Ice",
                "Neon",
                "Terminal",
                "Gala Gold",
                "Sunset",
                "Rosé",
                "Nordic Aurora",
                "Lagoon",
                "Lavender Mist"
            ],
            "LABEL": "Palette"
        },
        {
            "NAME": "main_colour",
            "TYPE": "color",
            "DEFAULT": [
                0.3569,
                0.2471,
                0.851,
                1.0
            ],
            "LABEL": "Main"
        },
        {
            "NAME": "accent_colour",
            "TYPE": "color",
            "DEFAULT": [
                0.7137,
                0.6118,
                1.0,
                1.0
            ],
            "LABEL": "Accent"
        }
    ],
    "PHASE_INPUTS": [
        {
            "PARAM": "speed",
            "INDEX": 0,
            "SCALE": 0.1,
            "WRAP": 4096.0
        }
    ]
}*/

#version 450

layout(location = 0) out vec4 fragColor;
layout(location = 0) in vec2 uv;

layout(set = 0, binding = 0) uniform ISFUniforms {
    float TIME;
    float TIMEDELTA;
    uint FRAMEINDEX;
    int PASSINDEX;
    vec2 RENDERSIZE;
    float audio_level;
    float audio_bass;
    float audio_mid;
    float audio_treble;
    float audio_bpm;
    float audio_beat_phase;
    vec4 DATE;
    float PHASE_TIME_0;
    float PHASE_TIME_1;
    float PHASE_TIME_2;
    float PHASE_TIME_3;
};

layout(set = 0, binding = 1) uniform sampler texSampler;
layout(set = 0, binding = 2) uniform texture2D inputImage;

layout(set = 0, binding = 3) uniform UserParams {
    float speed;
    float window;
    int shape;
    uint fill;
    float weight;
    float glow;
    int palette;
    vec4 main_colour;
    vec4 accent_colour;
};

// ---- Broadcast helpers: the same block in every broadcast_*.fs ----------

// Hashes and value noise (no textures, no loops).
float bc_hash12(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * 0.1031);
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.x + p3.y) * p3.z);
}
vec2 bc_hash22(vec2 p) {
    vec3 p3 = fract(vec3(p.xyx) * vec3(0.1031, 0.1030, 0.0973));
    p3 += dot(p3, p3.yzx + 33.33);
    return fract((p3.xx + p3.yz) * p3.zy);
}
float bc_noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(bc_hash12(i), bc_hash12(i + vec2(1.0, 0.0)), u.x),
               mix(bc_hash12(i + vec2(0.0, 1.0)), bc_hash12(i + vec2(1.0, 1.0)), u.x), u.y);
}

// sRGB <-> linear light: palettes are written in sRGB, the picture is linear.
vec3 bc_lin(vec3 c) {
    return mix(c / 12.92, pow((c + 0.055) / 1.055, vec3(2.4)), step(0.04045, c));
}
vec3 bc_srgb(vec3 c) {
    c = max(c, vec3(0.0));
    return mix(c * 12.92, 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055, step(0.0031308, c));
}

// Palette presets (sRGB): background, main, accent each.
const vec3 BC_PALETTES[36] = vec3[36](
    vec3(0.0392, 0.1020, 0.1843), vec3(0.1176, 0.4353, 0.8510), vec3(0.4353, 0.8902, 1.0000),
    vec3(0.0667, 0.0824, 0.0941), vec3(0.1216, 0.5412, 0.5412), vec3(0.6078, 0.9490, 0.8824),
    vec3(0.0706, 0.0549, 0.1647), vec3(0.3569, 0.2471, 0.8510), vec3(0.7137, 0.6118, 1.0000),
    vec3(0.0118, 0.0314, 0.0510), vec3(0.0000, 0.7216, 0.8314), vec3(0.8784, 0.9686, 1.0000),
    vec3(0.0431, 0.0235, 0.0941), vec3(1.0000, 0.1804, 0.5333), vec3(0.1804, 0.9020, 1.0000),
    vec3(0.0078, 0.0392, 0.0196), vec3(0.0980, 0.7647, 0.4902), vec3(0.7216, 1.0000, 0.8510),
    vec3(0.1098, 0.0588, 0.0392), vec3(0.8510, 0.5569, 0.1686), vec3(1.0000, 0.8353, 0.5412),
    vec3(0.1647, 0.0588, 0.1216), vec3(0.9490, 0.4000, 0.2941), vec3(1.0000, 0.7529, 0.4784),
    vec3(0.1412, 0.0706, 0.1020), vec3(0.7882, 0.3882, 0.4824), vec3(0.9686, 0.7765, 0.7098),
    vec3(0.0235, 0.0706, 0.1216), vec3(0.1686, 0.7020, 0.6039), vec3(0.6118, 0.5490, 1.0000),
    vec3(0.0235, 0.1412, 0.2000), vec3(0.1647, 0.6157, 0.7098), vec3(0.7490, 0.9451, 0.9333),
    vec3(0.1176, 0.1412, 0.2000), vec3(0.6039, 0.5608, 0.7882), vec3(0.9137, 0.8941, 0.9608)
);

// The three colours, linear: a preset's, or the pickers' for Custom (0) —
// those are linear already.
void bc_colours(int preset, vec4 bg, vec4 mn, vec4 ac, out vec3 b, out vec3 m, out vec3 a) {
    if (preset < 1 || preset > 12) {
        b = bg.rgb;
        m = mn.rgb;
        a = ac.rgb;
        return;
    }
    int i = (preset - 1) * 3;
    b = bc_lin(BC_PALETTES[i]);
    m = bc_lin(BC_PALETTES[i + 1]);
    a = bc_lin(BC_PALETTES[i + 2]);
}

// Calm centre, the last step of every moving look: in the middle and the
// lower third, where slides and names go, the picture goes toward a flat,
// dim plate of its background — whatever the look, so text over it reads —
// never below the floor LED panels need (the base is a little above black).
vec3 bc_calm_all(vec3 col, vec3 base, vec2 st, float amount) {
    vec2 c = (st - vec2(0.5, 0.48)) * vec2(1.1, 1.6);
    float centre = exp(-dot(c, c) * 3.0);
    float lower = smoothstep(0.6, 0.72, st.y) * (1.0 - smoothstep(0.93, 1.0, st.y));
    float m = clamp(amount, 0.0, 1.0) * max(centre, lower * 0.85);
    // Mixed as the eye sees it (sRGB): dark looks calm as much as bright.
    return bc_lin(mix(bc_srgb(col), bc_srgb(base) * 0.5, m * 0.9));
}

// The last step of every look: dithered by about one step of 8-bit sRGB (no
// bands on an LED wall), back in linear light.
vec4 bc_finish(vec3 lin, vec2 frag) {
    vec3 s = bc_srgb(lin);
    // Never near black, whatever the colours or the calm: lifted to the
    // floor LED panels take (4% as the eye sees it), its hue kept.
    s += max(0.04 - dot(s, vec3(0.2126, 0.7152, 0.0722)), 0.0);
    float n = bc_hash12(frag) + bc_hash12(frag + vec2(17.13, 3.71)) - 1.0;
    s = clamp(s + n / 255.0, 0.0, 1.0);
    return vec4(bc_lin(s), 1.0);
}
// Distance from p to the segment a–b.
float seg(vec2 p, vec2 a, vec2 b) {
    vec2 pa = p - a;
    vec2 ba = b - a;
    return length(pa - ba * clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0));
}
// 1 when a line from p toward +x crosses the segment a–b, else 0: an odd
// count of them round an outline says p is inside it.
float xing(vec2 p, vec2 a, vec2 b) {
    return ((a.y > p.y) != (b.y > p.y)) && (p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x) ? 1.0 : 0.0;
}
// x times itself: pow(x, 2.0) is not defined for an x below 0.
float sq(float x) {
    return x * x;
}
// 1 between lo and hi, the edges `aa` soft.
float band(float lo, float hi, float x, float aa) {
    return smoothstep(lo - aa, lo + aa, x) * (1.0 - smoothstep(hi - aa, hi + aa, x));
}
// A line of half-width w at distance d from its middle, the edge `aa` soft.
float stroke(float d, float w, float aa) {
    return 1.0 - smoothstep(w - aa, w + aa, d);
}
// How far outside (inside: below 0) a box of half-size b a point is.
float sdbox(vec2 p, vec2 b) {
    vec2 dd = abs(p) - b;
    return length(max(dd, 0.0)) + min(max(dd.x, dd.y), 0.0);
}
// The same of a box of half-size b whose corners are rounded by r.
float sdround(vec2 p, vec2 b, float r) {
    vec2 dd = abs(p) - b + r;
    return length(max(dd, 0.0)) + min(max(dd.x, dd.y), 0.0) - r;
}
// ---- end of helpers ------------------------------------------------------

void main() {
    // Every uniform kept, so none is stripped from the block the engine
    // fills by position (a branch never taken).
    if (uv.x < -1.0) {
        float k = TIME + TIMEDELTA + float(FRAMEINDEX) + float(PASSINDEX) + DATE.x + DATE.y + DATE.z + DATE.w
            + audio_level + audio_bass + audio_mid + audio_treble + audio_bpm + audio_beat_phase
            + PHASE_TIME_1 + PHASE_TIME_2 + PHASE_TIME_3 + speed + window + float(shape) + float(fill) + weight + glow + float(palette) + main_colour.x + accent_colour.x;
        fragColor = vec4(k);
        return;
    }

    vec3 unused_bg;
    vec3 mainc;
    vec3 accentc;
    bc_colours(palette, vec4(0.0), main_colour, accent_colour, unused_bg, mainc, accentc);
    float aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    vec2 q = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
    float shape_w = shape == 1 ? 1.7777778 : shape == 2 ? 1.3333333 : shape == 3 ? 1.0 : shape == 4 ? 0.5625 : aspect;
    vec2 half_size = 0.5 * window * min(1.0, aspect / shape_w) * vec2(shape_w, 1.0);
    float hw = half_size.x;
    float hh = half_size.y;
    vec2 e = abs(q) - half_size;
    float line_w = mix(0.003, 0.009, weight);
    vec2 reach = half_size + 0.006;
    vec2 room = fill != 0u ? reach : half_size;
    float scale = fill != 0u ? max(room.x / (0.5 * aspect), room.y / 0.5) : min(room.x / (0.5 * aspect), room.y / 0.5);
    vec2 at = q / (max(scale, 1e-4) * vec2(aspect, 1.0)) + 0.5;
    vec4 pic = texture(sampler2D(inputImage, texSampler), clamp(at, 0.0, 1.0));
    // Nothing of it beyond its own edges (half a pixel soft).
    vec2 texel = 0.5 / max(RENDERSIZE, vec2(1.0));
    vec2 in_pic = clamp((0.5 - abs(at - 0.5)) / texel + 0.5, 0.0, 1.0);
    pic.a *= in_pic.x * in_pic.y;
    if (max(e.x, e.y) < -0.08) {
        fragColor = pic;
        return;
    }
    float t = PHASE_TIME_0;
    // About a pixel and a half, in frame heights: the soft edge of a line.
    float px = 1.5 / max(RENDERSIZE.y, 1.0);
    // How far round the window a point is (0–1, clockwise from the top
    // left), and the round's length.
    float round_len = 4.0 * (hw + hh);
    float along;
    if (e.x > e.y) {
        along = q.x > 0.0 ? 2.0 * hw + (q.y + hh) : 4.0 * hw + 2.0 * hh + (hh - q.y);
    } else {
        along = q.y < 0.0 ? q.x + hw : 2.0 * hw + 2.0 * hh + (hw - q.x);
    }
    along /= round_len;

    // Half of it drawn: the other half is this one turned half round.
    vec2 r = q.y > 0.0 ? -q : q;
    vec2 o = half_size + 0.006;
    float cut = sdbox(q, o);
    float d = abs(cut);
    float border = stroke(d, line_w * 0.6, px);
    // Honeycomb outside the window, thinning out away from the corner.
    float cell = 0.03;
    vec2 corner = vec2(-o.x, -o.y);
    vec2 hp = (r - corner) / cell;
    vec2 sz = vec2(1.0, 1.7320508);
    vec2 ha = mod(hp, sz) - sz * 0.5;
    vec2 hb = mod(hp - sz * 0.5, sz) - sz * 0.5;
    vec2 gv = dot(ha, ha) < dot(hb, hb) ? ha : hb;
    vec2 mid = hp - gv;
    float edge_d = max(abs(gv.x), dot(abs(gv), vec2(0.5, 0.8660254)));
    vec2 where = corner + mid * cell;
    float h1 = bc_hash12(mid + 3.3);
    float h2 = bc_hash12(mid + 8.8);
    float far = length(mid * cell) / 0.16;
    float outside = max(step(hw + cell * 0.55, abs(where.x)), step(hh + cell * 0.55, abs(where.y)));
    float grows = step(h1, 1.0 - far) * outside;
    float aa = px / cell;
    float ring = band(0.36, 0.47, edge_d, aa);
    float full = 1.0 - smoothstep(0.4 - aa, 0.4 + aa, edge_d);
    float lit = 0.35 + 0.65 * exp(-sq((fract(t * 0.2 + h2) - 0.5) * 3.0));
    float comb = grows * mix(ring * 0.8, full * lit, step(h2, 0.35));
    float run = exp(-sq((fract(along - t * 0.2) - 0.5) * 7.0));
    float solid = max(border * (0.75 + 0.25 * run), comb);
    float soft = exp(-d * d / (0.012 * 0.012));
    vec3 light_col = mix(mainc, accentc, clamp(0.25 + run + comb * step(h2, 0.35) * lit, 0.0, 1.0));

    float inside_a = pic.a * clamp(0.5 - cut * max(RENDERSIZE.y, 1.0), 0.0, 1.0);
    float frame_a = clamp(max(solid, soft * mix(0.0, 0.8, glow)), 0.0, 1.0);
    vec3 line_col = mix(mainc, light_col, clamp(solid, 0.0, 1.0));
    float out_a = frame_a + inside_a * (1.0 - frame_a);
    vec3 out_rgb = out_a > 0.0 ? (line_col * frame_a + pic.rgb * inside_a * (1.0 - frame_a)) / out_a : vec3(0.0);
    fragColor = vec4(out_rgb, out_a);
}
