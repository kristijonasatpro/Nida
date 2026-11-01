"""Writes Nida's Broadcast backgrounds (shaders/broadcast_*.fs) from one
shared header and helper block, so every look keeps the same controls,
palettes, calm centre and dither.

usage: python3 -I make_broadcast.py <varda repo>
"""
import json
import sys
from pathlib import Path

REPO = Path(sys.argv[1])

PALETTES = [
    ("Corporate Blue", "0A1A2F", "1E6FD9", "6FE3FF"),
    ("Graphite Teal", "111518", "1F8A8A", "9BF2E1"),
    ("Violet Tech", "120E2A", "5B3FD9", "B69CFF"),
    ("Ice", "03080D", "00B8D4", "E0F7FF"),
    ("Neon", "0B0618", "FF2E88", "2EE6FF"),
    ("Terminal", "020A05", "19C37D", "B8FFD9"),
    ("Gala Gold", "1C0F0A", "D98E2B", "FFD58A"),
    ("Sunset", "2A0F1F", "F2664B", "FFC07A"),
    ("Rosé", "24121A", "C9637B", "F7C6B5"),
    ("Nordic Aurora", "06121F", "2BB39A", "9C8CFF"),
    ("Lagoon", "062433", "2A9DB5", "BFF1EE"),
    ("Lavender Mist", "1E2433", "9A8FC9", "E9E4F5"),
]


def rgb(hexs):
    return [round(int(hexs[i:i + 2], 16) / 255, 4) for i in (0, 2, 4)]


def vec3(hexs):
    r, g, b = rgb(hexs)
    return f"vec3({r:.4f}, {g:.4f}, {b:.4f})"


PALETTE_GLSL = ",\n    ".join(
    f"{vec3(b)}, {vec3(m)}, {vec3(a)}" for _, b, m, a in PALETTES
)

HELPERS = """
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
    PALETTE_GLSL
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
""".replace("PALETTE_GLSL", PALETTE_GLSL)

UNIFORMS = """
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
USER_PARAMS
};
"""

KEEP_ALIVE = """
    // Every uniform kept, so none is stripped from the block the engine
    // fills by position (a branch never taken).
    if (uv.x < -1.0) {
        float k = TIME + TIMEDELTA + float(FRAMEINDEX) + float(PASSINDEX) + DATE.x + DATE.y + DATE.z + DATE.w
            + audio_level + audio_bass + audio_mid + audio_treble + audio_bpm + audio_beat_phase
            + PHASE_TIME_1 + PHASE_TIME_2 + PHASE_TIME_3 + KEEP_PARAMS;
        fragColor = vec4(k);
        return;
    }
"""

COMMON_INPUTS = [
    {"NAME": "speed", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 10.0, "LABEL": "Speed"},
    {"NAME": "density", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Density"},
    {"NAME": "calm", "TYPE": "float", "DEFAULT": 0.4, "MIN": 0.0, "MAX": 1.0, "LABEL": "Calm centre"},
]


def palette_input(default):
    return {
        "NAME": "palette", "TYPE": "long", "DEFAULT": default,
        "VALUES": list(range(13)),
        "LABELS": ["Custom (colours below)"] + [p[0] for p in PALETTES],
        "LABEL": "Palette",
    }


def colour_inputs(preset):
    _, b, m, a = PALETTES[preset - 1]
    return [
        {"NAME": "bg_colour", "TYPE": "color", "DEFAULT": rgb(b) + [1.0], "LABEL": "Background"},
        {"NAME": "main_colour", "TYPE": "color", "DEFAULT": rgb(m) + [1.0], "LABEL": "Main"},
        {"NAME": "accent_colour", "TYPE": "color", "DEFAULT": rgb(a) + [1.0], "LABEL": "Accent"},
    ]


GLSL_TYPE = {"float": "float", "long": "int", "color": "vec4", "bool": "uint"}


def keep_expr(inp):
    n = inp["NAME"]
    t = inp["TYPE"]
    if t == "color":
        return f"{n}.x"
    if t in ("long", "bool"):
        return f"float({n})"
    return n


SOFT_BINDINGS = """
layout(set = 0, binding = 1) uniform sampler texSampler;
layout(set = 0, binding = 2) uniform texture2D soft;

layout(set = 0, binding = 3) uniform UserParams {"""


# The phase starts again from 0 past this: a float growing all day loses the
# precision slow motion needs. A look cuts to a fresh picture there once —
# 4096 is about 11 hours at Speed 1, an hour at 10 — while Colours loop
# exactly at 1.
LOOK_WRAP = 4096.0


def write(name, description, mood, inputs, phase_scale, body, soft=False, wrap=LOOK_WRAP):
    header = {
        "DESCRIPTION": description,
        "CREDIT": "Nida",
        "ISFVSN": "2.0",
        "CATEGORIES": ["Generator", "Broadcast", mood],
        "INPUTS": inputs,
        "PHASE_INPUTS": [{"PARAM": "speed", "INDEX": 0, "SCALE": phase_scale, "WRAP": wrap}],
    }
    if soft:
        # Its light drawn at half size — soft by nature, a quarter the cost —
        # then brought up to full size, with the calm centre and dither.
        header["PASSES"] = [
            {"TARGET": "soft", "WIDTH": "$WIDTH/2", "HEIGHT": "$HEIGHT/2", "FLOAT": True},
            {},
        ]
    params = "\n".join(f"    {GLSL_TYPE[i['TYPE']]} {i['NAME']};" for i in inputs)
    keep = " + ".join(keep_expr(i) for i in inputs)
    src = "/*" + json.dumps(header, indent=4, ensure_ascii=False) + "*/\n"
    uniforms = UNIFORMS.replace("USER_PARAMS", params)
    if soft:
        uniforms = uniforms.replace("layout(set = 0, binding = 1) uniform UserParams {", SOFT_BINDINGS.strip("\n"))
    src += uniforms
    src += HELPERS
    src += "\nvoid main() {" + KEEP_ALIVE.replace("KEEP_PARAMS", keep) + body + "}\n"
    path = REPO / "shaders" / f"broadcast_{name}.fs"
    path.write_text(src)
    print("wrote", path.name)


def look(name, description, mood, preset, phase_scale, body, soft=False):
    inputs = COMMON_INPUTS + [palette_input(preset)] + colour_inputs(preset)
    write(name, description, mood, inputs, phase_scale, body, soft)


# The full-size pass of a look drawn soft: its light from the half-size
# pass — colour, and how much of the base it covers, so a disc or a node
# shows its own colour where it is whole — the calm centre, dithered.
SOFT_FINISH = """
    if (PASSINDEX == 0) {
        // Its highlights eased here, before the picture is scaled up, so
        // the soft edges between pixels keep their share.
        float peak = max(light.r, max(light.g, light.b));
        fragColor = vec4(light / (1.0 + peak * 0.8), clamp(cover, 0.0, 1.0));
        return;
    }
    vec4 soft_light = texture(sampler2D(soft, texSampler), uv);
    vec3 col = base * (1.0 - soft_light.a) + soft_light.rgb;
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
"""


# Shared start of every look's body: colours, aspect, position in frame
# heights (x centred), and the phase.
START = """
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
"""

# ---------------------------------------------------------------- Warm --
look("sunset_waves", "Sunset waves — broad bands of warm light drifting slowly", "Warm", 8, 0.12, START + """
    float bands = mix(2.0, 4.0, density);
    float glow = 0.0;
    for (int i = 0; i < 4; i++) {
        float fi = float(i);
        // Each band fades in as Density brings it.
        float weight = clamp(bands - fi, 0.0, 1.0);
        float centre = 0.22 + 0.56 * (fi + 0.5) / bands;
        float warp = bc_noise(vec2(p.x * 0.7 + t * 0.25 + fi * 7.1, fi * 3.3 + t * 0.08)) - 0.5;
        float y = centre + 0.10 * sin(p.x * (1.1 + 0.35 * fi) + t * (0.55 + 0.12 * fi) + fi * 2.1) + 0.16 * warp;
        float width = mix(0.11, 0.2, density) * (1.0 + 0.25 * sin(t * 0.35 + fi * 1.7));
        float d = (uv.y - y) / width;
        glow += exp(-d * d) * (0.75 - 0.12 * fi) * weight;
    }
    glow = clamp(glow, 0.0, 1.0);
    vec3 col = mix(base, mainc * 0.55, smoothstep(0.0, 0.7, glow));
    col = mix(col, accentc * 0.6, smoothstep(0.6, 1.0, glow) * 0.5);
    col = mix(col, base, smoothstep(0.55, 1.0, uv.y) * 0.25);
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
""")

look("bokeh", "Drifting bokeh — soft discs of light in three depths", "Warm", 7, 0.1, START + """
    vec3 light = vec3(0.0);
    float cover = 0.0;
    for (int layer = 0; layer < 3; layer++) {
        if (PASSINDEX != 0) {
            break;
        }
        float fl = float(layer);
        float cells = mix(2.6, 6.0, fl / 2.0);
        vec2 drift = vec2(0.12, -0.05) * t * (0.6 + 0.25 * fl);
        vec2 q = p * cells + drift + vec2(fl * 13.7, fl * 5.3);
        vec2 id = floor(q);
        vec2 f = fract(q);
        float soft = mix(0.35, 0.12, fl / 2.0);
        float alpha = mix(0.35, 0.55, fl / 2.0);
        for (int dy = -1; dy <= 1; dy++) {
            for (int dx = -1; dx <= 1; dx++) {
                vec2 cell = id + vec2(float(dx), float(dy));
                float h = bc_hash12(cell + fl * 31.1);
                if (h > density * 0.85 + 0.05) {
                    continue;
                }
                vec2 o = bc_hash22(cell + 7.3);
                vec2 at = vec2(float(dx), float(dy)) + 0.2 + 0.6 * o
                    + 0.12 * vec2(sin(t * 0.6 + h * 6.28), cos(t * 0.5 + h * 5.1)) - f;
                float r = mix(0.22, 0.42, bc_hash12(cell + 3.1)) * (1.0 + 0.06 * sin(t * 0.8 + h * 12.0));
                float d = length(at);
                float disc = 1.0 - smoothstep(r * (1.0 - soft), r, d);
                float rim = smoothstep(r * 0.75, r * 0.95, d) * disc;
                vec3 c = mix(mainc, accentc, bc_hash12(cell + 9.9));
                light += c * (disc * 0.28 + rim * 0.12) * alpha;
                cover = max(cover, disc * clamp(alpha * 1.8, 0.0, 1.0));
            }
        }
    }
""" + SOFT_FINISH, soft=True)

# ---------------------------------------------------------------- Tech --
look("hex_glow", "Hex field — a slow wave of glowing cells", "Tech", 1, 0.1, START + """
    vec2 q = p * 7.0;
    vec2 r = vec2(1.0, 1.7320508);
    vec2 h = r * 0.5;
    vec2 a = mod(q, r) - h;
    vec2 b = mod(q - h, r) - h;
    vec2 gv = dot(a, a) < dot(b, b) ? a : b;
    vec2 id = q - gv;
    vec2 g = abs(gv);
    float hexd = max(dot(g, normalize(vec2(1.0, 1.7320508))), g.x);
    float edge = 0.5 - hexd;
    float n = bc_noise(id * 0.32 + vec2(t * 0.22, t * 0.09)) * 0.65 + bc_noise(id * 0.85 - t * 0.12) * 0.35;
    float threshold = mix(0.72, 0.38, density);
    float lit = smoothstep(threshold, threshold + 0.18, n);
    float line = 1.0 - smoothstep(0.015, 0.07, edge);
    // Each part over what is under it, in its own colour where whole.
    vec3 col = mix(base, mainc * 0.08, lit);
    col = mix(col, accentc * 0.42, lit * smoothstep(0.0, 0.3, edge));
    col = mix(col, mainc * 0.22, line);
    // Softer and dimmer toward the top: a little depth.
    col = mix(base, col, 0.55 + 0.45 * uv.y);
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
""")

look("node_network", "Node network — drifting points joined by fading lines", "Tech", 2, 0.1, """
    // Points: one per cell, present by Density, drifting on small loops.
""" + START + """
    vec3 light = vec3(0.0);
    float cover = 0.0;
    // Drawn in the half-size pass only.
    if (PASSINDEX == 0) {
    float cells = 5.0;
    vec2 q = p * cells;
    vec2 id = floor(q);
    vec2 f = fract(q);
    vec2 pts[9];
    float on[9];
    int k = 0;
    for (int dy = -1; dy <= 1; dy++) {
        for (int dx = -1; dx <= 1; dx++) {
            vec2 cell = id + vec2(float(dx), float(dy));
            vec2 o = bc_hash22(cell);
            float h = bc_hash12(cell + 4.7);
            vec2 wobble = 0.3 * vec2(sin(t * (0.5 + o.x) + h * 6.28), cos(t * (0.4 + o.y) + h * 4.1));
            pts[k] = vec2(float(dx), float(dy)) + 0.5 + 0.25 * (o - 0.5) + wobble;
            on[k] = step(h, 0.45 + 0.5 * density);
            k++;
        }
    }
    float width = 0.012 * cells;
    float lines = 0.0;
    float nodes = 0.0;
    // From the middle point to the others, and round the middle.
    for (int i = 0; i < 9; i++) {
        if (i == 4) {
            continue;
        }
        vec2 a2 = pts[4];
        vec2 b2 = pts[i];
        vec2 pa = f - a2;
        vec2 ba = b2 - a2;
        float hh = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
        float d = length(pa - ba * hh);
        float len = length(ba);
        float fade = clamp(1.0 - len / 1.6, 0.0, 1.0);
        lines += (1.0 - smoothstep(width * 0.4, width, d)) * fade * on[4] * on[i];
    }
    int ring[8] = int[8](1, 5, 7, 3, 1, 5, 7, 3);
    int next[4] = int[4](5, 7, 3, 1);
    for (int i = 0; i < 4; i++) {
        vec2 a2 = pts[ring[i]];
        vec2 b2 = pts[next[i]];
        vec2 pa = f - a2;
        vec2 ba = b2 - a2;
        float hh = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);
        float d = length(pa - ba * hh);
        float fade = clamp(1.0 - length(ba) / 1.6, 0.0, 1.0);
        lines += (1.0 - smoothstep(width * 0.4, width, d)) * fade * on[ring[i]] * on[next[i]];
    }
    for (int i = 0; i < 9; i++) {
        float d = length(f - pts[i]);
        nodes += (1.0 - smoothstep(0.035, 0.07, d)) * on[i];
    }
    // The nodes over the lines, each over what is under it.
    float lc = min(lines, 1.0);
    float nc = min(nodes, 1.0);
    light = accentc * 0.6 * nc + mainc * 0.45 * lc * (1.0 - nc);
    cover = nc + lc * (1.0 - nc);
    }
""" + SOFT_FINISH, soft=True)

look("circuit_grid", "Circuit grid — traces with pulses travelling along them", "Tech", 3, 0.1, START + """
    float cells = 10.0;
    vec2 q = p * cells;
    vec2 id = floor(q);
    vec2 f = fract(q) - 0.5;
    float chance = mix(0.25, 0.6, density);
    // A connection on each edge, shared by the cells either side of it.
    float east = step(bc_hash12(id + vec2(0.5, 0.0)), chance);
    float west = step(bc_hash12(id + vec2(-0.5, 0.0)), chance);
    float south = step(bc_hash12(id + vec2(0.0, 0.5) + 71.0), chance);
    float north = step(bc_hash12(id + vec2(0.0, -0.5) + 71.0), chance);
    float w = 0.07;
    float trace = 0.0;
    trace = max(trace, east * (1.0 - smoothstep(w * 0.6, w, abs(f.y))) * step(-w, f.x));
    trace = max(trace, west * (1.0 - smoothstep(w * 0.6, w, abs(f.y))) * step(f.x, w));
    trace = max(trace, south * (1.0 - smoothstep(w * 0.6, w, abs(f.x))) * step(-w, f.y));
    trace = max(trace, north * (1.0 - smoothstep(w * 0.6, w, abs(f.x))) * step(f.y, w));
    float links = east + west + south + north;
    float pad = (1.0 - smoothstep(0.14, 0.2, length(f))) * step(0.5, links) * step(links, 1.5);
    // Pulses: along each row and column, a few soft heads at their own pace.
    float row = bc_hash12(vec2(id.y, 3.3));
    float column = bc_hash12(vec2(id.x, 8.1));
    float span = 12.0;
    float headx = (fract(t * (0.25 + 0.3 * row) + row * 7.0) - 0.5) * span;
    float heady = (fract(t * (0.2 + 0.25 * column) + column * 5.0) - 0.5) * span;
    float dxh = abs(mod(q.x - headx + span * 0.5, span) - span * 0.5);
    float dyh = abs(mod(q.y - heady + span * 0.5, span) - span * 0.5);
    float along = (east + west > 0.5 ? 1.0 : 0.0) * (1.0 - smoothstep(w * 0.6, w, abs(f.y)));
    float down = (south + north > 0.5 ? 1.0 : 0.0) * (1.0 - smoothstep(w * 0.6, w, abs(f.x)));
    float pulse = along * exp(-dxh * 1.6) * step(row, density * 0.7 + 0.2)
        + down * exp(-dyh * 1.6) * step(column, density * 0.7 + 0.2);
    vec3 col = mix(base, mainc * 0.28, max(trace, pad));
    col = mix(col, accentc * 0.6, min(pulse, 1.0));
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
""")

# --------------------------------------------------------------- Cyber --
look("data_streaks", "Data streaks — dashed lines falling slowly in three depths", "Cyber", 4, 0.1, START + """
    vec3 col = base;
    for (int layer = 0; layer < 3; layer++) {
        float fl = float(layer);
        float width = mix(0.034, 0.014, fl / 2.0);
        float column = floor(p.x / width);
        float inside = fract(p.x / width);
        float h = bc_hash12(vec2(column, fl * 17.0));
        if (h > density * 0.6 + 0.1) {
            continue;
        }
        float h2 = bc_hash12(vec2(column + 5.0, fl * 3.0));
        float h3 = bc_hash12(vec2(column + 9.0, fl * 7.0));
        float len = mix(0.25, 0.6, h2);
        float fall = mix(0.18, 0.35, h3) * (1.0 - 0.25 * fl);
        float head = fract(t * fall + h2 * 3.0) * (1.0 + len) - 0.0;
        float rel = (head - uv.y) / len;
        if (rel < -0.05 || rel > 1.0) {
            continue;
        }
        // Its head fades in over a little of its length: no bright point
        // switching on as it passes.
        float lead = smoothstep(0.0, 0.08, rel);
        float along = uv.y / (width * 1.8) + h3 * 3.0;
        // A pixel's worth of `along`, from the frame's height: no derivative
        // after the branches above.
        float aa = 1.0 / max(RENDERSIZE.y * width * 1.8, 1.0);
        float f = fract(along);
        float dash = smoothstep(0.0, aa, f) * (1.0 - smoothstep(0.6 - aa, 0.6, f));
        float body = (1.0 - smoothstep(0.32, 0.45, abs(inside - 0.5))) * dash;
        float tail = pow(clamp(1.0 - rel, 0.0, 1.0), 1.6) * lead;
        float bright = mix(0.75, 0.35, fl / 2.0);
        col = mix(col, mainc * bright * 0.45, body * tail);
        col = mix(col, accentc * bright * 0.5, body * smoothstep(0.92, 1.0, 1.0 - rel) * lead);
    }
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
""")

# ---------------------------------------------------------------- Calm --
look("aurora", "Aurora — slow curtains of light over a night sky", "Calm", 10, 0.08, START + """
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
""")

look("silk_ribbons", "Silk ribbons — soft folds of satin flowing slowly, with drifting motes", "Calm", 12, 0.08, START + """
    vec3 col = base;
    float ribbons = mix(1.0, 3.0, density);
    for (int i = 0; i < 3; i++) {
        float fi = float(i);
        float weight = clamp(ribbons - fi, 0.0, 1.0);
        float k = 1.1 + 0.4 * fi;
        float phase = t * (0.5 + 0.1 * fi) + fi * 2.4;
        float y = 0.3 + 0.2 * fi + 0.09 * sin(p.x * k + phase) + 0.04 * (bc_noise(vec2(p.x * 0.8 + t * 0.1, fi * 9.0)) - 0.5);
        float slope = 0.09 * k * cos(p.x * k + phase);
        float thick = 0.05 + 0.025 * sin(p.x * 0.7 + phase * 0.5 + fi);
        float d = (uv.y - y) / thick;
        float body = exp(-d * d);
        float sheen = pow(clamp(0.5 + 0.5 * sin(d * 2.2 + slope * 6.0), 0.0, 1.0), 6.0);
        float off = (d - 0.35 * sin(phase)) * 5.0;
        float fold = exp(-off * off) * 0.3;
        col += (mainc * body * 0.35 + accentc * body * (sheen * 0.3 + fold) * 0.5) * weight;
    }
    // Dust motes: few, small, drifting up.
    vec2 q = p * 14.0 + vec2(0.0, t * 0.6);
    vec2 id = floor(q);
    vec2 f = fract(q) - 0.5;
    float h = bc_hash12(id);
    vec2 o = bc_hash22(id) - 0.5;
    float mote = (1.0 - smoothstep(0.03, 0.07, length(f - o * 0.6))) * step(h, density * 0.12);
    col += accentc * mote * 0.35;
    fragColor = bc_finish(bc_calm_all(col, base, uv, calm), frag);
""")

# ------------------------------------------------------------- Colours --
gradient_inputs = [
    {"NAME": "speed", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 3.0, "LABEL": "Drift"},
    {"NAME": "angle", "TYPE": "float", "DEFAULT": 90.0, "MIN": 0.0, "MAX": 360.0, "LABEL": "Angle"},
    {"NAME": "style", "TYPE": "long", "DEFAULT": 0, "VALUES": [0, 1], "LABELS": ["Linear", "Radial"], "LABEL": "Style"},
    {"NAME": "spread", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Accent amount"},
    palette_input(1),
] + colour_inputs(1)
write("gradient", "Gradient — two or three colours, still or drifting very slowly", "Colours", gradient_inputs, 0.05, wrap=1.0, body="""
    vec3 bgc;
    vec3 mainc;
    vec3 accentc;
    bc_colours(palette, bg_colour, main_colour, accent_colour, bgc, mainc, accentc);
    float aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    vec2 c = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
    float s;
    if (style == 1) {
        s = clamp(length(c) / (0.5 * sqrt(aspect * aspect + 1.0)), 0.0, 1.0);
    } else {
        float a = radians(angle);
        vec2 dir = vec2(cos(a), sin(a));
        float reach = 0.5 * (abs(dir.x) * aspect + abs(dir.y));
        s = clamp(dot(c, dir) / max(reach, 1e-3) * 0.5 + 0.5, 0.0, 1.0);
    }
    s = clamp(s + 0.08 * sin(PHASE_TIME_0 * 6.2831), 0.0, 1.0);
    vec3 col = mix(mainc, bgc, smoothstep(0.0, 1.0, s));
    col = mix(col, accentc, spread * 0.6 * (1.0 - smoothstep(0.0, 0.45, s)));
    fragColor = bc_finish(col, uv * RENDERSIZE);
""")

vignette_inputs = [
    {"NAME": "speed", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 3.0, "LABEL": "Breathing"},
    {"NAME": "edge", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Edge darkening"},
    palette_input(1),
] + colour_inputs(1)
write("vignette", "Vignette plate — one colour with softly darkened edges", "Colours", vignette_inputs, 0.05, wrap=1.0, body="""
    vec3 bgc;
    vec3 mainc;
    vec3 accentc;
    bc_colours(palette, bg_colour, main_colour, accent_colour, bgc, mainc, accentc);
    float aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    vec2 c = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
    float r = length(c) / (0.5 * sqrt(aspect * aspect + 1.0));
    float breathe = 1.0 + 0.04 * sin(PHASE_TIME_0 * 6.2831);
    // A preset's plate is lifted toward its Main; Custom's is the
    // Background as picked.
    vec3 plate = palette == 0 ? bgc : mix(bgc, mainc, 0.35);
    vec3 col = plate * mix(1.0, 0.35, edge * smoothstep(0.35 * breathe, 1.1, r));
    fragColor = bc_finish(col, uv * RENDERSIZE);
""")
