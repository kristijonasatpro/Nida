/*{
    "DESCRIPTION": "Aurora — slow curtains of light over a night sky",
    "CREDIT": "Nida",
    "ISFVSN": "2.0",
    "CATEGORIES": [
        "Generator",
        "Broadcast",
        "Calm"
    ],
    "INPUTS": [
        {
            "NAME": "speed",
            "TYPE": "float",
            "DEFAULT": 1.0,
            "MIN": 0.0,
            "MAX": 10.0,
            "LABEL": "Speed"
        },
        {
            "NAME": "density",
            "TYPE": "float",
            "DEFAULT": 0.5,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Density"
        },
        {
            "NAME": "calm",
            "TYPE": "float",
            "DEFAULT": 0.4,
            "MIN": 0.0,
            "MAX": 1.0,
            "LABEL": "Calm centre"
        },
        {
            "NAME": "palette",
            "TYPE": "long",
            "DEFAULT": 10,
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
            "NAME": "bg_colour",
            "TYPE": "color",
            "DEFAULT": [
                0.0235,
                0.0706,
                0.1216,
                1.0
            ],
            "LABEL": "Background"
        },
        {
            "NAME": "main_colour",
            "TYPE": "color",
            "DEFAULT": [
                0.1686,
                0.702,
                0.6039,
                1.0
            ],
            "LABEL": "Main"
        },
        {
            "NAME": "accent_colour",
            "TYPE": "color",
            "DEFAULT": [
                0.6118,
                0.549,
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
            "SCALE": 0.08,
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

layout(set = 0, binding = 1) uniform UserParams {
    float speed;
    float density;
    float calm;
    int palette;
    vec4 bg_colour;
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
// ---- end of helpers ------------------------------------------------------

void main() {
    // Every uniform kept, so none is stripped from the block the engine
    // fills by position (a branch never taken).
    if (uv.x < -1.0) {
        float k = TIME + TIMEDELTA + float(FRAMEINDEX) + float(PASSINDEX) + DATE.x + DATE.y + DATE.z + DATE.w
            + audio_level + audio_bass + audio_mid + audio_treble + audio_bpm + audio_beat_phase
            + PHASE_TIME_1 + PHASE_TIME_2 + PHASE_TIME_3 + speed + density + calm + float(palette) + bg_colour.x + main_colour.x + accent_colour.x;
        fragColor = vec4(k);
        return;
    }

    vec3 bgc;
    vec3 mainc;
    vec3 accentc;
    bc_colours(palette, bg_colour, main_colour, accent_colour, bgc, mainc, accentc);
    float aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    vec2 p = vec2((uv.x - 0.5) * aspect, uv.y);
    float t = PHASE_TIME_0;
    vec2 frag = uv * RENDERSIZE;
    // A preset's background is lifted a little toward its Main; Custom's is
    // the Background as picked (a colour picked on a preset starts as the
    // lifted one, so nothing moves). Near black is lifted at the end, for
    // every look alike (`bc_finish`).
    vec3 base = palette == 0 ? bgc : mix(bgc, mainc, 0.05);

    vec3 col = mix(base, base * 0.8, uv.y);
    float curtains = mix(1.0, 3.0, density);
    vec3 light = vec3(0.0);
    for (int i = 0; i < 3; i++) {
        float fi = float(i);
        float weight = clamp(curtains - fi, 0.0, 1.0);
        float x = p.x * (0.9 + 0.2 * fi) + fi * 3.7;
        float ridge = 0.2 + 0.12 * fi + 0.07 * sin(x * 1.3 + t * 0.6) + 0.12 * (bc_noise(vec2(x * 1.1 + t * 0.15, fi)) - 0.5);
        float below = uv.y - ridge;
        float hang = below > 0.0 ? exp(-below * mix(9.0, 6.0, density)) : exp(below * 40.0);
        // Brighter and dimmer along its length, as a real curtain folds.
        float along = smoothstep(0.25, 0.75, bc_noise(vec2(x * 0.9 - t * 0.2, fi * 4.0 + 1.0)));
        float streak = 0.75 + 0.25 * bc_noise(vec2(x * 9.0, t * 0.25 + fi * 5.0));
        vec3 c = mix(accentc, mainc, smoothstep(0.0, 0.12, max(below, 0.0)));
        light += c * hang * streak * along * (0.5 - 0.1 * fi) * weight;
    }
    col += light;
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
}
