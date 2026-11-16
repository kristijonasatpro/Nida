/*{
    "DESCRIPTION": "HUD frame — machined gunmetal, screws, light strips, a meter in the lower left corner",
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
            "DEFAULT": 1.0,
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
            "NAME": "inside",
            "TYPE": "bool",
            "DEFAULT": false,
            "LABEL": "Picture inside the frame"
        },
        {
            "NAME": "frame_colour",
            "TYPE": "color",
            "DEFAULT": [
                0.031,
                0.986,
                1.0,
                1.0
            ],
            "LABEL": "Frame colour"
        },
        {
            "NAME": "weight",
            "TYPE": "float",
            "DEFAULT": 0.5,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Frame size"
        },
        {
            "NAME": "anim",
            "TYPE": "long",
            "DEFAULT": 0,
            "VALUES": [
                0,
                1,
                2,
                3,
                4,
                5
            ],
            "LABELS": [
                "A light going round",
                "Two lights going round",
                "Two lights, opposite ways",
                "Three lights going round",
                "Breathing",
                "Still"
            ],
            "LABEL": "Animation"
        },
        {
            "NAME": "light_colour",
            "TYPE": "color",
            "DEFAULT": [
                1.0,
                1.0,
                1.0,
                1.0
            ],
            "LABEL": "Light colour"
        },
        {
            "NAME": "light_size",
            "TYPE": "float",
            "DEFAULT": 0.35,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Light size"
        },
        {
            "NAME": "glow",
            "TYPE": "float",
            "DEFAULT": 0.5,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Glow"
        }
    ],
    "IMPORTED": {
        "frame_art": {
            "PATH": "frames/hud.png"
        }
    },
    "PHASE_INPUTS": [
        {
            "PARAM": "speed",
            "INDEX": 0,
            "SCALE": 0.1,
            "WRAP": 4096.0
        }
    ],
    "PASSES": [
        {
            "TARGET": "fr_cols",
            "WIDTH": "$WIDTH/2",
            "HEIGHT": "1",
            "FLOAT": true
        },
        {
            "TARGET": "fr_rows",
            "WIDTH": "1",
            "HEIGHT": "$HEIGHT/2",
            "FLOAT": true
        },
        {
            "TARGET": "fr_box",
            "WIDTH": "1",
            "HEIGHT": "1",
            "FLOAT": true
        },
        {}
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
layout(set = 0, binding = 3) uniform texture2D fr_cols;
layout(set = 0, binding = 4) uniform texture2D fr_rows;
layout(set = 0, binding = 5) uniform texture2D fr_box;
layout(set = 0, binding = 6) uniform texture2D frame_art;

layout(set = 0, binding = 7) uniform UserParams {
    float speed;
    float window;
    int shape;
    uint fill;
    uint inside;
    vec4 frame_colour;
    float weight;
    int anim;
    vec4 light_colour;
    float light_size;
    float glow;
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

// Calm centre, the last step of every moving look: over the middle of the
// picture — all of it but a border, so where slides, frames and names go —
// the picture goes toward a flat, dim plate of its background, whatever
// the look, so text over it reads; never below the floor LED panels need
// (the base is a little above black). Calm inside a rounded box in the
// middle, the look coming back by degrees toward the picture's edges.
vec3 bc_calm_all(vec3 col, vec3 base, vec2 st, float amount) {
    vec2 from_box = abs(st - 0.5) - vec2(0.22, 0.22) + 0.15;
    float outside = length(max(from_box, 0.0)) + min(max(from_box.x, from_box.y), 0.0) - 0.15;
    float m = clamp(amount, 0.0, 1.0) * (1.0 - smoothstep(0.0, 0.26, outside));
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
// x times itself: pow(x, 2.0) is not defined for an x below 0.
float sq(float x) {
    return x * x;
}
// A portion of light going round: 1 at its head (x a whole number), fading
// over `tail` behind it; its head is round, not cut.
float fr_portion(float x, float tail) {
    float f = fract(x);
    return exp(-f / max(tail * 0.45, 1e-3)) + exp(-sq((f - 1.0) / 0.010));
}
// ---- end of helpers ------------------------------------------------------

void main() {
    // Every uniform kept, so none is stripped from the block the engine
    // fills by position (a branch never taken).
    if (uv.x < -1.0) {
        float k = TIME + TIMEDELTA + float(FRAMEINDEX) + float(PASSINDEX) + DATE.x + DATE.y + DATE.z + DATE.w
            + audio_level + audio_bass + audio_mid + audio_treble + audio_bpm + audio_beat_phase
            + PHASE_TIME_1 + PHASE_TIME_2 + PHASE_TIME_3 + speed + window + float(shape) + float(fill) + float(inside) + frame_colour.x + weight + float(anim) + light_colour.x + light_size + glow;
        fragColor = vec4(k);
        return;
    }

    if (PASSINDEX == 0) {
        // One column pair, looked at on 64 lines spread down the picture
        // (every row would cost a whole read of the picture each time: a
        // box's sides are found as well this way, and only something thinner
        // than a 64th of the picture can slip between the lines). Each tap
        // is on the line between two rows and between the pair's two
        // columns, so it sees four pixels.
        ivec2 size = textureSize(sampler2D(inputImage, texSampler), 0);
        float seen = 0.0;
        for (int i = 0; i < 64; i++) {
            float y = (floor((float(i) + 0.5) / 64.0 * float(size.y) * 0.5) * 2.0 + 1.0) / float(size.y);
            seen = max(seen, textureLod(sampler2D(inputImage, texSampler), vec2(uv.x, y), 0.0).a);
            if (seen >= 0.004) {
                break;
            }
        }
        fragColor = vec4(step(0.004, seen));
        return;
    }
    if (PASSINDEX == 1) {
        ivec2 size = textureSize(sampler2D(inputImage, texSampler), 0);
        float seen = 0.0;
        for (int i = 0; i < 64; i++) {
            float x = (floor((float(i) + 0.5) / 64.0 * float(size.x) * 0.5) * 2.0 + 1.0) / float(size.x);
            seen = max(seen, textureLod(sampler2D(inputImage, texSampler), vec2(x, uv.y), 0.0).a);
            if (seen >= 0.004) {
                break;
            }
        }
        fragColor = vec4(step(0.004, seen));
        return;
    }
    if (PASSINDEX == 2) {
        ivec2 pairs = max(textureSize(sampler2D(inputImage, texSampler), 0) / 2, ivec2(1));
        float first_x = -1.0;
        float last_x = -1.0;
        for (int i = 0; i < 4096; i++) {
            if (i >= pairs.x) {
                break;
            }
            if (textureLod(sampler2D(fr_cols, texSampler), vec2((float(i) + 0.5) / float(pairs.x), 0.5), 0.0).r > 0.5) {
                first_x = first_x < 0.0 ? float(i) : first_x;
                last_x = float(i);
            }
        }
        float first_y = -1.0;
        float last_y = -1.0;
        for (int i = 0; i < 4096; i++) {
            if (i >= pairs.y) {
                break;
            }
            if (textureLod(sampler2D(fr_rows, texSampler), vec2(0.5, (float(i) + 0.5) / float(pairs.y)), 0.0).r > 0.5) {
                first_y = first_y < 0.0 ? float(i) : first_y;
                last_y = float(i);
            }
        }
        vec2 n = vec2(pairs);
        fragColor = last_x < 0.0 || last_y < 0.0 ? vec4(1.0)
            : vec4(first_x / n.x, first_y / n.y, 1.0 - (last_x + 1.0) / n.x, 1.0 - (last_y + 1.0) / n.y);
        return;
    }
    // What shows of the layer's picture (see PASSES): nothing, and the
    // frame draws nothing.
    vec4 box_in = textureLod(sampler2D(fr_box, texSampler), vec2(0.5), 0.0);
    if (box_in.x + box_in.z >= 1.0 || box_in.y + box_in.w >= 1.0) {
        fragColor = textureLod(sampler2D(inputImage, texSampler), uv, 0.0);
        return;
    }

    // `aspect` is the shape of what shows, `q` a point's place from its
    // middle in its heights (y down), `res` its height in pixels.
    vec2 kept_lo = box_in.xy;
    vec2 kept_size = max(1.0 - box_in.xy - box_in.zw, vec2(1e-3));
    vec2 kept_mid = kept_lo + 0.5 * kept_size;
    float layer_aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    float aspect = layer_aspect * kept_size.x / kept_size.y;
    vec2 q = vec2((uv.x - kept_mid.x) * layer_aspect, uv.y - kept_mid.y) / kept_size.y;
    float res = max(RENDERSIZE.y * kept_size.y, 1.0);
    float shape_w = shape == 1 ? 1.7777778 : shape == 2 ? 1.3333333 : shape == 3 ? 1.0 : shape == 4 ? 0.5625 : aspect;
    vec2 hs = 0.5 * window * min(1.0, aspect / shape_w) * vec2(shape_w, 1.0);
    // The art: as wide as 1.77683 heights; its window's box in it (left,
    // top, right, bottom); the art is one frame height high here.
    vec4 box = vec4(0.0287, 0.0361, 0.9707, 0.9649);
    float art_aspect = 1.77683;
    float unit = 2.0 * hs.y;
    // Frame size: the art's corners and sides drawn larger or smaller.
    float thick = weight < 0.5 ? mix(0.55, 1.0, weight * 2.0) : mix(1.0, 1.9, weight * 2.0 - 1.0);
    vec2 inset = vec2(max(box.x, 1.0 - box.z) * art_aspect, max(box.y, 1.0 - box.w)) * unit * thick;
    vec2 room = inside != 0u ? hs - inset : hs;
    float scale = fill != 0u ? max(room.x / (0.5 * aspect), room.y / 0.5) : min(room.x / (0.5 * aspect), room.y / 0.5);
    vec2 at = q / (max(scale, 1e-4) * vec2(aspect, 1.0)) + 0.5;
    vec4 pic = texture(sampler2D(inputImage, texSampler), kept_lo + clamp(at, 0.0, 1.0) * kept_size);
    // Nothing of it beyond its own edges (half a pixel soft).
    vec2 texel = 0.5 / max(RENDERSIZE * kept_size, vec2(1.0));
    vec2 in_pic = clamp((0.5 - abs(at - 0.5)) / texel + 0.5, 0.0, 1.0);
    float within = in_pic.x * in_pic.y;
    vec2 from_open = abs(q) - (hs - inset);
    if (max(from_open.x, from_open.y) < -0.200 * unit) {
        fragColor = vec4(pic.rgb * within, mix(1.0, pic.a, within));
        return;
    }
    // Where in the art a point is: the art's sides as they are drawn, its
    // plain middle stretched (or left out) to the window's width.
    float awin = hs.x / hs.y;
    float xl = (q.x + hs.x) / unit;
    float yl = (q.y + hs.y) / unit;
    // Always a middle left to stretch: with none the art's two halves
    // would meet where they do not match.
    float kx = min(0.520 * thick, 0.46 * awin);
    float ky = min(0.300 * thick, 0.46);
    float ax = kx / thick / art_aspect;
    float ay = ky / thick;
    float ua = xl < kx ? xl / thick / art_aspect
        : xl > awin - kx ? 1.0 - (awin - xl) / thick / art_aspect
        : mix(ax, 1.0 - ax, (xl - kx) / max(awin - 2.0 * kx, 1e-5));
    float va = yl < ky ? yl / thick
        : yl > 1.0 - ky ? 1.0 - (1.0 - yl) / thick
        : mix(ay, 1.0 - ay, (yl - ky) / max(1.0 - 2.0 * ky, 1e-5));
    vec4 art = textureLod(sampler2D(frame_art, texSampler), clamp(vec2(ua, va), 0.0, 1.0), 0.0);
    // Nothing of the art or the picture outside the window's box. Inside
    // it, where the picture does not reach (its shape another than the
    // window's), the frame is backed with black, as a screen is.
    vec2 out_edge = (abs(q) - hs) * res;
    float in_window = clamp(0.5 - max(out_edge.x, out_edge.y), 0.0, 1.0);
    art.a *= in_window;
    float pic_a = mix(1.0, pic.a, within) * in_window;
    pic = vec4(pic.rgb * within, pic_a);

    // Seconds, at Speed 1; how far round the window a point is (0–1,
    // clockwise from the top left).
    float t = PHASE_TIME_0 * 10.0;
    vec2 edge = abs(q) - hs;
    float ul;
    if (edge.x > edge.y) {
        ul = q.x > 0.0 ? 2.0 * hs.x + (q.y + hs.y) : 4.0 * hs.x + 2.0 * hs.y + (hs.y - q.y);
    } else {
        ul = q.y < 0.0 ? q.x + hs.x : 2.0 * hs.x + 2.0 * hs.y + (hs.x - q.x);
    }
    float along = ul / (4.0 * (hs.x + hs.y));
    // The light: a portion of it going round, two going opposite ways,
    // three, all of it breathing, or none.
    float tail = mix(0.04, 0.55, light_size);
    float turn = t * 0.085;
    float light = anim == 0 ? fr_portion(turn - along, tail)
        : anim == 1 ? fr_portion((turn - along) * 2.0, tail * 1.4)
        : anim == 2 ? fr_portion(turn - along, tail * 0.6) + fr_portion(along + turn + 0.5, tail * 0.6)
        : anim == 3 ? fr_portion((turn - along) * 3.0, tail)
        : anim == 4 ? 0.5 + 0.5 * sin(t * 1.3) : 0.0;
    light = clamp(light, 0.0, 1.0);
    vec3 lc = light_colour.rgb;
    // The art's own lights are what is bright and coloured in it. Frame
    // colour (it starts as the colour they were drawn in) is the colour
    // they take: the grey in each pixel kept, the rest turned to it.
    vec3 c = art.rgb;
    float mx = max(c.r, max(c.g, c.b));
    float mn = min(c.r, min(c.g, c.b));
    float emis = smoothstep(0.25, 0.60, mx) * smoothstep(0.10, 0.30, mx - mn);
    float fmax = max(frame_colour.r, max(frame_colour.g, frame_colour.b));
    c = vec3(mn) + (mx - mn) * frame_colour.rgb / max(fmax, 0.02);
    float solid = step(0.999, art.a);
    // Where the light is: the art's lights turn to its colour and brighten,
    // the metal beside them catches it, and it glows into the window.
    float lum = dot(c, vec3(0.2126, 0.7152, 0.0722));
    c = mix(c, mix(lc, vec3(1.0), 0.3) * (0.8 + 0.6 * mx), emis * light * 0.85);
    c += lc * light * (0.05 + 0.9 * lum) * 0.55 * (1.0 - emis) * solid;
    float into = max(-max(from_open.x, from_open.y), 0.0) / unit;
    vec3 spill = lc * light * mix(0.0, 0.55, glow) * exp(-into / 0.030) * step(max(from_open.x, from_open.y), 0.0);
    pic.rgb += spill * step(0.001, pic.a);
    // The art's own glow in the window (its see-through part) breathes a little.
    art.a = clamp(art.a * (1.0 + (1.0 - solid) * emis * (0.5 * light + 0.12 * sin(t * 0.9)) * mix(0.3, 1.0, glow)), 0.0, 1.0);
    // A light passes over the metal now and then.
    float sweep = exp(-sq((q.x * 0.75 + q.y * 0.66 - mix(-1.6, 1.6, fract(t * 0.05))) * 6.0));
    c += vec3(0.9, 0.95, 1.0) * sweep * mix(0.1, 0.5, glow) * (0.06 + lum) * (1.0 - emis) * solid;
    c = max(c, vec3(0.0));
    // Decks are straight alpha: the art goes over the picture that way.
    float out_a = art.a + pic.a * (1.0 - art.a);
    fragColor = out_a > 0.0 ? vec4((c * art.a + pic.rgb * pic.a * (1.0 - art.a)) / out_a, out_a) : vec4(0.0);
}
