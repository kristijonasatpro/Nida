"""Writes Nida's Frames (shaders/frame_*.fs): filters that hold a layer's
picture — scale it into their window, cut it to their own shape, and draw
a moving outline round it; clear everywhere else. A layer gets one from
Input Settings → Position → Frame (`EngineCommand::SetLayerFrame`), where it
runs before Transform, so the layer is moved and scaled frame and all.

The palettes and helpers are those of the Broadcast backgrounds: the head
of make_broadcast_looks.py is run here (its looks are not written again —
never run that script itself: four of its looks were changed by hand).

usage: python3 -I scripts/make_frames.py <varda repo>
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
head = (HERE / "make_broadcast_looks.py").read_text()
head = head[: head.index("# ---------------------------------------------------------------- Warm --")]
exec(compile(head, "make_broadcast_looks.py (head)", "exec"))

# Drawing helpers, after the Broadcast ones.
DRAW = """
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
"""
HELPERS = HELPERS.replace("// ---- end of helpers", DRAW.strip("\n") + "\n// ---- end of helpers")

BINDINGS = """
layout(set = 0, binding = 1) uniform sampler texSampler;
layout(set = 0, binding = 2) uniform texture2D inputImage;

layout(set = 0, binding = 3) uniform UserParams {"""

SHAPES = ["As the picture", "16:9", "4:3", "1:1", "9:16"]


def frame_inputs(preset):
    _, _, m, a = PALETTES[preset - 1]
    return [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "speed", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 10.0, "LABEL": "Speed"},
        {"NAME": "window", "TYPE": "float", "DEFAULT": 0.86, "MIN": 0.4, "MAX": 1.0, "LABEL": "Size"},
        {"NAME": "shape", "TYPE": "long", "DEFAULT": 0, "VALUES": list(range(len(SHAPES))), "LABELS": SHAPES,
         "LABEL": "Shape"},
        {"NAME": "fill", "TYPE": "bool", "DEFAULT": False, "LABEL": "Fill the frame"},
        {"NAME": "weight", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Line weight"},
        {"NAME": "glow", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Glow"},
        palette_input(preset),
        {"NAME": "main_colour", "TYPE": "color", "DEFAULT": rgb(m) + [1.0], "LABEL": "Main"},
        {"NAME": "accent_colour", "TYPE": "color", "DEFAULT": rgb(a) + [1.0], "LABEL": "Accent"},
    ]


# Shared start of a frame. The window is Size of the picture's height, as
# wide as its Shape (or as much of that as the picture is wide), centred;
# `q` is a point's place from the middle in frame heights. The picture is
# read once, here: all of it inside the window, or — Fill the frame —
# covering the frame's own shape, which reaches REACH from the middle.
# Deep inside the window there is only the picture: such a pixel is left
# at once.
FRAME_START = """
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
    vec2 reach = REACH;
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
"""

# A frame's body leaves: `cut` (how far outside its shape a point is; below
# 0 inside — the picture shows there), `solid` (its lines), `soft` (their
# glow) and `light_col` (the lines' colour at their brightest). Decks are
# straight alpha: the frame goes over the picture that way.
FRAME_FINISH = """
    float inside_a = pic.a * clamp(0.5 - cut * max(RENDERSIZE.y, 1.0), 0.0, 1.0);
    float frame_a = clamp(max(solid, soft * mix(0.0, 0.8, glow)), 0.0, 1.0);
    vec3 line_col = mix(mainc, light_col, clamp(solid, 0.0, 1.0));
    float out_a = frame_a + inside_a * (1.0 - frame_a);
    vec3 out_rgb = out_a > 0.0 ? (line_col * frame_a + pic.rgb * inside_a * (1.0 - frame_a)) / out_a : vec3(0.0);
    fragColor = vec4(out_rgb, out_a);
"""

# Which corner a point is nearest, in the order a light goes round:
# 0 top left, 1 top right, 2 bottom right, 3 bottom left.
TURN = """
    float corner_no = (q.x > 0.0 ? 1.0 : 0.0) + (q.y > 0.0 ? 2.0 : 0.0);
    float turn = corner_no < 1.5 ? corner_no : 5.0 - corner_no;
"""

GLSL = {"float": "float", "long": "int", "color": "vec4", "bool": "uint"}


def frame(name, description, preset, reach, body):
    inputs = frame_inputs(preset)
    header = {
        "DESCRIPTION": description,
        "CREDIT": "Nida",
        "ISFVSN": "2.0",
        "CATEGORIES": ["Filter", "Frame"],
        "INPUTS": inputs,
        "PHASE_INPUTS": [{"PARAM": "speed", "INDEX": 0, "SCALE": 0.1, "WRAP": LOOK_WRAP}],
    }
    own = [i for i in inputs if i["TYPE"] != "image"]
    params = "\n".join(f"    {GLSL[i['TYPE']]} {i['NAME']};" for i in own)
    keep = " + ".join(keep_expr(i) for i in own)
    src = "/*" + json.dumps(header, indent=4, ensure_ascii=False) + "*/\n"
    uniforms = UNIFORMS.replace("USER_PARAMS", params)
    uniforms = uniforms.replace("layout(set = 0, binding = 1) uniform UserParams {", BINDINGS.strip("\n"))
    assert "inputImage" in uniforms
    src += uniforms
    src += HELPERS
    src += ("\nvoid main() {" + KEEP_ALIVE.replace("KEEP_PARAMS", keep)
            + FRAME_START.replace("REACH", reach) + body + FRAME_FINISH + "}\n")
    path = REPO / "shaders" / f"frame_{name}.fs"
    path.write_text(src)
    print("wrote", path.name)


frame("hud", "HUD frame — cut corners, stepped sides, marks above and below, lights running round", 1,
      "half_size + 0.02", """
    vec2 a = abs(q);
    // Never quite on the middle lines, where an outline's ends are.
    vec2 ac = max(a, vec2(1e-5));
    // The outline, a quarter of it (the rest its mirror): the corners cut
    // and thick, a step outside the window; the middle of each side fine,
    // on the window's edge.
    float pad = 0.02;
    float cutc = 0.04;
    vec2 o = half_size + pad;
    vec2 p1 = vec2(o.x * 0.3, hh);
    vec2 p2 = vec2(o.x * 0.3 + pad, o.y);
    vec2 p3 = vec2(o.x - cutc, o.y);
    vec2 p4 = vec2(o.x, o.y - cutc);
    vec2 p5 = vec2(o.x, o.y * 0.42 + pad);
    vec2 p6 = vec2(hw, o.y * 0.42);
    float thick = min(seg(a, p2, p3), min(seg(a, p3, p4), seg(a, p4, p5)));
    float thin = min(min(seg(a, vec2(0.0, hh), p1), seg(a, p1, p2)),
                     min(seg(a, p5, p6), seg(a, p6, vec2(hw, 0.0))));
    float border = max(stroke(thick, line_w * 1.5, px), stroke(thin, line_w * 0.5, px));
    float d = min(thick, thin);
    float crossed = xing(ac, p1, p2) + xing(ac, p3, p4) + xing(ac, p4, p5) + xing(ac, p5, p6)
        + xing(ac, p6, vec2(hw, 0.0));
    float cut = mod(crossed, 2.0) > 0.5 ? -d : d;
    // Two lights going round it, one each way.
    float run = exp(-sq((fract(along - t * 0.25) - 0.5) * 9.0));
    float run2 = exp(-sq((fract(along + t * 0.11 + 0.37) - 0.5) * 14.0));
    // Marks above and below, in the room outside the border: a fine line
    // that steps over slanted ticks and under two bars, and ends in a ring;
    // one more across the middle, a ring at each end.
    float room_y = clamp(0.5 - o.y, 0.0, 0.09);
    float oy = (a.y - o.y) / max(room_y, 1e-3);
    float fine_w = max(line_w * 0.4, 0.0015);
    vec2 a0 = vec2(o.x * 0.98, o.y + room_y * 0.26);
    vec2 a1 = vec2(o.x * 0.94, o.y + room_y * 0.83);
    vec2 a2 = vec2(o.x * 0.66, o.y + room_y * 0.83);
    vec2 a3 = vec2(o.x * 0.62, o.y + room_y * 0.31);
    vec2 a4 = vec2(o.x * 0.36, o.y + room_y * 0.31);
    vec2 a5 = vec2(o.x * 0.33, o.y + room_y * 0.83);
    vec2 a6 = vec2(o.x * 0.14, o.y + room_y * 0.83);
    float fine = min(min(seg(a, a0, a1), seg(a, a1, a2)), min(seg(a, a2, a3), seg(a, a3, a4)));
    fine = min(fine, min(seg(a, a4, a5), seg(a, a5, a6)));
    vec2 m1 = vec2(o.x * 0.17, o.y + room_y * 0.36);
    fine = min(fine, seg(a, vec2(0.0, m1.y), m1));
    float ring = room_y * 0.09;
    fine = min(fine, abs(distance(a, a6 - vec2(ring, 0.0)) - ring));
    fine = min(fine, abs(distance(a, m1 + vec2(ring, 0.0)) - ring));
    float there = step(0.02, room_y);
    float marks = stroke(fine, fine_w, px) * there;
    float oy_aa = px / max(room_y, 1e-3);
    // The ticks lean as the line's outer end does, the bars as its step.
    float sxt = a.x + oy * 0.0702 * o.x;
    float pitch = o.x * 0.04;
    float tri = abs(fract(sxt / pitch - t * 0.25) - 0.5);
    float ticks = band(o.x * 0.725, o.x * 0.965, sxt, px) * band(0.42, 0.72, oy, oy_aa)
        * (1.0 - smoothstep(0.25, 0.25 + px / pitch, tri)) * there;
    float sxb = a.x - oy * 0.0769 * o.x;
    float breathe = 0.02 * sin(t * 0.7);
    float bars = band(0.45, 0.78, oy, oy_aa) * there
        * max(band(o.x * (0.415 + breathe), o.x * 0.565, sxb, px), band(o.x * 0.345, o.x * 0.375, sxb, px));
    float solid = max(max(border, marks * 0.85), max(ticks * 0.9, bars));
    float soft = exp(-d * d / (0.012 * 0.012)) + 0.6 * run * exp(-d * d / (0.03 * 0.03));
    vec3 light_col = mix(mainc, accentc, clamp(0.7 + run + run2, 0.0, 1.0));
""")

frame("panel", "Tech panel — two cut corners, a raised tab with blocks lighting in turn, a rail at the side", 2,
      "half_size + 0.025", """
    // Half of it drawn: the other half is this one turned half round.
    vec2 r = q.y > 0.0 ? -q : q;
    vec2 rc = vec2(r.x, min(r.y, -1e-5));
    float pad = 0.025;
    float c1 = 0.05;
    float c2 = 0.02;
    vec2 o = half_size + pad;
    // From the left side up over a wide cut corner, along a raised thick
    // tab, down a step to the window's edge, a small cut corner, the right
    // side.
    vec2 b0 = vec2(-o.x, 0.0);
    vec2 b1 = vec2(-o.x, -o.y + c1);
    vec2 b2 = vec2(-o.x + c1, -o.y);
    vec2 b3 = vec2(-hw * 0.18, -o.y);
    vec2 b4 = vec2(-hw * 0.18 + pad, -hh);
    vec2 b5 = vec2(o.x - c2, -hh);
    vec2 b6 = vec2(o.x, -hh + c2);
    vec2 b7 = vec2(o.x, 0.0);
    float thick = min(seg(r, b1, b2), seg(r, b2, b3));
    float thin = min(min(seg(r, b0, b1), seg(r, b3, b4)),
                     min(seg(r, b4, b5), min(seg(r, b5, b6), seg(r, b6, b7))));
    float border = max(stroke(thick, line_w * 1.5, px), stroke(thin, line_w * 0.6, px));
    float d = min(thick, thin);
    float crossed = xing(rc, b0, b1) + xing(rc, b1, b2) + xing(rc, b3, b4) + xing(rc, b5, b6) + xing(rc, b6, b7);
    float cut = mod(crossed, 2.0) > 0.5 ? -d : d;
    // Blocks in the step, lighting one after another.
    float bx = r.x - (b4.x + 0.012) + (r.y + o.y) * 0.8;
    float bi = floor(bx / 0.042);
    float block = band(0.004, 0.034, bx - bi * 0.042, px) * step(-0.5, bi) * step(bi, 3.5)
        * band(-o.y + 0.003, -hh - 0.008, r.y, px);
    float lit = 0.3 + 0.7 * exp(-sq((fract(t * 0.25 - bi * 0.2) - 0.5) * 5.0));
    // A rail outside the long side, a bead sliding on it.
    float rail_x = -o.x - 0.014;
    float rail = stroke(seg(r, vec2(rail_x, -o.y * 0.62), vec2(rail_x, -o.y * 0.1)), max(line_w * 0.4, 0.0015), px);
    float bead_y = mix(-o.y * 0.62, -o.y * 0.1, 0.5 + 0.5 * sin(t * 0.6));
    float bead = stroke(distance(r, vec2(rail_x, bead_y)), 0.006, px);
    float run = exp(-sq((fract(along - t * 0.2) - 0.5) * 8.0));
    float solid = max(max(border, block * lit), max(rail * 0.8, bead));
    float soft = exp(-d * d / (0.012 * 0.012)) + 0.5 * run * exp(-d * d / (0.03 * 0.03));
    vec3 light_col = mix(mainc, accentc, clamp(0.4 + run + block * lit + bead, 0.0, 1.0));
""")

frame("circuit", "Circuit frame — cut corners, bold bars, traces running out to pads, pulses along them", 4,
      "half_size + 0.014", """
    vec2 a = abs(q);
    vec2 ac = max(a, vec2(1e-5));
    float pad = 0.014;
    float cutc = 0.028;
    vec2 o = half_size + pad;
    vec2 c1 = vec2(o.x - cutc, o.y);
    vec2 c2 = vec2(o.x, o.y - cutc);
    float d = min(min(seg(a, vec2(0.0, o.y), c1), seg(a, c1, c2)), seg(a, c2, vec2(o.x, 0.0)));
    float cut = mod(xing(ac, c1, c2) + xing(ac, c2, vec2(o.x, 0.0)), 2.0) > 0.5 ? -d : d;
    float border = stroke(d, line_w * 0.6, px);
    // Bold bars on it: one each side of the middle above and below, one
    // at the middle of each side.
    float bars = max(stroke(seg(a, vec2(o.x * 0.3, o.y), vec2(o.x * 0.55, o.y)), line_w * 1.6, px),
                     stroke(seg(a, vec2(o.x, 0.0), vec2(o.x, o.y * 0.3)), line_w * 1.6, px));
    // Three traces from each cut corner: out on the slant, then level,
    // each to a pad; a pulse of light going out along each.
    float room_x = 0.5 * aspect - o.x;
    float room_y = 0.5 - o.y;
    float traces = 0.0;
    float pads = 0.0;
    float pulses = 0.0;
    float fine_w = max(line_w * 0.4, 0.0015);
    for (int k = 0; k < 3; k++) {
        float fk = float(k);
        vec2 s0 = vec2(o.x - cutc * (0.8 - 0.3 * fk), o.y - cutc * (0.2 + 0.3 * fk));
        float slant = room_y * (0.2 + 0.25 * fk);
        vec2 s1 = s0 + slant;
        vec2 s2 = s1 + vec2(max(room_x - slant, 0.0) * (0.45 + 0.2 * fk), 0.0);
        float trace = stroke(min(seg(a, s0, s1), seg(a, s1, s2)), fine_w, px);
        traces = max(traces, trace);
        pads = max(pads, stroke(distance(a, s2), 0.0055, px));
        float along_it = (a.x - s0.x) / max(s2.x - s0.x, 1e-3);
        pulses = max(pulses, trace * exp(-sq((along_it - fract(t * 0.3 + fk * 0.33)) * 5.0)));
    }
    float there = step(0.03, room_y);
    float run = exp(-sq((fract(along - t * 0.2) - 0.5) * 8.0));
    float solid = max(max(border, bars), max(traces * 0.75, pads) * there);
    float soft = exp(-d * d / (0.012 * 0.012)) + 0.5 * run * exp(-d * d / (0.03 * 0.03));
    vec3 light_col = mix(mainc, accentc, clamp(0.3 + run + bars * 0.6 + pulses * there + pads * there, 0.0, 1.0));
""")

frame("hex", "Hex frame — flat above and below, each end coming to a point, a fine line outside", 3,
      "half_size + vec2(clamp(0.5 * aspect - half_size.x - 0.04, 0.0, 0.055), 0.0)", """
    vec2 a = abs(q);
    vec2 ac = max(a, vec2(1e-5));
    // The points lie outside the window, as far as the picture has room.
    float k = reach.x - hw;
    vec2 h1 = vec2(hw, hh);
    vec2 h2 = vec2(hw + k, 0.0);
    float d = min(seg(a, vec2(0.0, hh), h1), seg(a, h1, h2));
    float cut = xing(ac, h1, h2) > 0.5 ? -d : d;
    // A finer line outside it, open in the middle of each side.
    float g = 0.016;
    vec2 i1 = vec2(hw + g * 0.4, hh + g);
    float fine = min(seg(a, vec2(hw * 0.35, hh + g), i1), seg(a, i1, vec2(hw + k * 0.6 + g, hh * 0.4)));
    float border = max(stroke(d, line_w, px), stroke(fine, max(line_w * 0.4, 0.0015), px) * 0.8);
    // A diamond beyond each point, and a light going round.
    vec2 dm = abs(a - vec2(hw + k + 0.022, 0.0));
    float diamond = stroke(dm.x + dm.y, 0.008 * (0.8 + 0.2 * sin(t * 0.9)), px) * step(0.02, k);
    float run = exp(-sq((fract(along - t * 0.22) - 0.5) * 7.0));
    float solid = max(border * (0.75 + 0.25 * run), diamond);
    float soft = exp(-d * d / (0.014 * 0.014)) + 0.5 * run * exp(-d * d / (0.035 * 0.035));
    vec3 light_col = mix(mainc, accentc, clamp(0.3 + run + diamond, 0.0, 1.0));
""")

frame("hex_corners", "Hex corners — a fine border, honeycomb growing from two opposite corners", 3,
      "half_size + 0.006", """
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
""")

frame("notched", "Notched frame — corners stepped in, a tab above and below, squares lighting in turn", 4,
      "half_size + vec2(0.022, 0.022 + min(0.026, max(0.5 - half_size.y - 0.034, 0.0)))", """
    vec2 a = abs(q);
    vec2 ac = max(a, vec2(1e-5));
    // The sides a step outside the window, each corner stepped in to the
    // window's own; a tab raised above and below.
    float pad = 0.022;
    vec2 o = half_size + pad;
    // The tab is as high as the room above allows: the shape reaches to
    // its top.
    float tab = reach.y - o.y;
    vec2 n1 = vec2(o.x * 0.26, o.y + tab);
    vec2 n2 = vec2(o.x * 0.26 + tab, o.y);
    vec2 n3 = vec2(hw, o.y);
    vec2 n4 = vec2(o.x, hh);
    float d = min(min(seg(a, vec2(0.0, o.y + tab), n1), seg(a, n1, n2)),
                  min(seg(a, n2, n3), seg(a, n3, half_size)));
    d = min(d, min(seg(a, half_size, n4), seg(a, n4, vec2(o.x, 0.0))));
    float crossed = xing(ac, n1, n2) + xing(ac, n3, half_size) + xing(ac, n4, vec2(o.x, 0.0));
    float cut = mod(crossed, 2.0) > 0.5 ? -d : d;
    float run = exp(-sq((fract(along - t * 0.2) - 0.5) * 8.0));
    float border = stroke(d, line_w, px) * (0.8 + 0.2 * run);
    // A small square in each corner's step, lighting in turn round the frame.
""" + TURN + """
    vec2 from_sq = abs(a - (half_size + pad * 0.64));
    float lit = 0.3 + 0.7 * exp(-sq((fract(t * 0.2 - turn * 0.25) - 0.5) * 4.0));
    float squares = stroke(max(from_sq.x, from_sq.y), pad * 0.2, px) * lit;
    // In each tab, a bar that fills and empties.
    float full = o.x * 0.2 * (0.5 + 0.5 * sin(t * 0.5));
    float bar = band(o.y + tab * 0.3, o.y + tab * 0.62, a.y, px) * (1.0 - smoothstep(full, full + px, a.x))
        * step(0.012, tab);
    float solid = max(max(border, squares), bar * 0.9);
    float soft = exp(-d * d / (0.012 * 0.012));
    vec3 light_col = mix(mainc, accentc, clamp(0.3 + run + squares + bar, 0.0, 1.0));
""")

frame("steps", "Pixel steps — a fine border whose corners climb in steps, blocks blinking in them", 5,
      "half_size + 0.016", """
    vec2 a = abs(q);
    vec2 ac = max(a, vec2(1e-5));
    // Two steps cut each corner; they pass outside the window's own.
    float u = 0.016;
    vec2 o = half_size + u;
    vec2 v1 = vec2(o.x - 2.0 * u, o.y);
    vec2 v2 = vec2(o.x - 2.0 * u, o.y - u);
    vec2 v3 = vec2(o.x - u, o.y - u);
    vec2 v4 = vec2(o.x - u, o.y - 2.0 * u);
    vec2 v5 = vec2(o.x, o.y - 2.0 * u);
    float d = min(min(seg(a, vec2(0.0, o.y), v1), seg(a, v1, v2)), min(seg(a, v2, v3), seg(a, v3, v4)));
    d = min(d, min(seg(a, v4, v5), seg(a, v5, vec2(o.x, 0.0))));
    float crossed = xing(ac, v1, v2) + xing(ac, v3, v4) + xing(ac, v5, vec2(o.x, 0.0));
    float cut = mod(crossed, 2.0) > 0.5 ? -d : d;
    // The line in short lengths, brighter ones walking round it.
    float count = max(floor(round_len / 0.05), 8.0);
    float walk = 0.65 + 0.35 * smoothstep(0.45, 0.55, abs(fract(along * count - t * 0.8) - 0.5) * 2.0);
    float border = stroke(d, max(line_w * 0.5, 0.0018), px) * walk;
    // A block in each step, one after the other, corner after corner.
""" + TURN + """
    vec2 s1 = abs(a - vec2(o.x - 1.5 * u, o.y - 0.5 * u));
    vec2 s2 = abs(a - vec2(o.x - 0.5 * u, o.y - 1.5 * u));
    float lit1 = 0.25 + 0.75 * exp(-sq((fract(t * 0.25 - turn * 0.25) - 0.5) * 5.0));
    float lit2 = 0.25 + 0.75 * exp(-sq((fract(t * 0.25 - turn * 0.25 - 0.07) - 0.5) * 5.0));
    float blocks = max(stroke(max(s1.x, s1.y), u * 0.24, px) * lit1, stroke(max(s2.x, s2.y), u * 0.24, px) * lit2);
    float solid = max(border, blocks);
    float soft = exp(-d * d / (0.01 * 0.01)) * 0.8;
    vec3 light_col = mix(mainc, accentc, clamp(0.25 + blocks + 0.5 * (walk - 0.65) / 0.35, 0.0, 1.0));
""")

frame("deco", "Art deco — three lines with corner blocks, fans at the corners and the middles", 7,
      "half_size + 0.034", """
    vec2 a = abs(q);
    // Round the window, a block at each corner: three lines a step apart,
    // a bold one between two fine.
    // (The shape reaches to the blocks' far sides: pad and block.)
    float pad = 0.012;
    float gap = 0.011;
    float blk = 0.022;
    vec2 o = half_size + pad;
    float cut = min(sdbox(q, o), sdbox(a - o, vec2(blk)));
    float d = cut - gap;
    float fine_w = max(line_w * 0.4, 0.0015);
    float run = exp(-sq((fract(along - t * 0.15) - 0.5) * 6.0));
    float lines = max(max(stroke(abs(cut), fine_w, px), stroke(abs(cut - 2.0 * gap), fine_w, px)),
                      stroke(abs(d), line_w * 0.9, px));
    // Fans: spokes spreading outward, at the middle above and below and
    // beyond each corner block, as far as there is room.
    float room_y = clamp(0.5 - o.y - 2.0 * gap - 0.006, 0.0, 0.05);
    vec2 v1 = a - vec2(0.0, o.y + 2.0 * gap + 0.004);
    float fan1 = step(0.0, v1.y) * stroke(length(v1), room_y, px) * step(0.012, room_y)
        * smoothstep(0.25, 0.4, cos(atan(v1.x, max(v1.y, 1e-6)) * 7.0) * 0.5 + 0.5)
        * step(abs(atan(v1.x, max(v1.y, 1e-6))), 1.35);
    float room2 = clamp(0.5 - o.y - blk - 2.0 * gap - 0.004, 0.0, 0.04);
    vec2 v2 = a - (o + blk + 2.0 * gap * 0.7);
    float out2 = (v2.x + v2.y) * 0.7071;
    float th2 = atan((v2.x - v2.y) * 0.7071, max(out2, 1e-6));
    float fan2 = step(0.0, out2) * stroke(length(v2), room2 * 1.3, px) * step(0.008, room2)
        * smoothstep(0.25, 0.4, cos(th2 * 7.0) * 0.5 + 0.5) * step(abs(th2), 1.1);
    float fans = max(fan1, fan2) * (0.7 + 0.3 * sin(t * 0.6 + length(v1) * 40.0));
    float solid = max(lines * (0.8 + 0.2 * run), fans * 0.9);
    float soft = exp(-d * d / (0.014 * 0.014));
    vec3 light_col = mix(mainc, accentc, clamp(0.35 + run + fans * 0.5, 0.0, 1.0));
""")

frame("double", "Double line — two fine lines, bold pieces at two corners, a tab under the lower left", 1,
      "half_size + 0.006", """
    // The corners' bold pieces: one at the top right; the other, at the
    // bottom left, is that one turned half round.
    vec2 r = q.y > 0.0 ? -q : q;
    float pad = 0.006;
    float gap = 0.012;
    vec2 o2 = half_size + pad + gap;
    float cut = sdbox(q, half_size + pad);
    float d1 = abs(cut);
    float d2 = abs(sdbox(q, o2));
    float fine_w = max(line_w * 0.4, 0.0015);
    float run = exp(-sq((fract(along - t * 0.2) - 0.5) * 7.0));
    float line1 = stroke(d1, fine_w, px);
    float line2 = stroke(d2, fine_w, px);
    float far = hh * (0.2 + 0.03 * sin(t * 0.8));
    float piece = stroke(d2, line_w * 1.6, px) * step(o2.x - far, r.x) * step(r.y, far - o2.y);
    // The tab: a long rounded shape under the bottom line, a dot of light
    // going along inside it.
    float room_y = 0.5 - o2.y;
    vec2 tab_mid = vec2(-hw * 0.62, o2.y + room_y * 0.5);
    vec2 tab_half = vec2(hw * 0.26, min(room_y * 0.24, 0.014));
    float tab_d = sdbox(q - tab_mid, tab_half - tab_half.y) - tab_half.y;
    float tab = stroke(abs(tab_d), fine_w, px) * step(0.03, room_y);
    float dot_x = tab_mid.x + (tab_half.x - tab_half.y) * sin(t * 0.5);
    float tab_dot = stroke(distance(q, vec2(dot_x, tab_mid.y)), tab_half.y * 0.45, px) * step(0.03, room_y);
    float solid = max(max(line1, line2 * 0.8), max(piece, max(tab * 0.8, tab_dot)));
    float soft = exp(-d2 * d2 / (0.012 * 0.012)) * (0.4 + 0.6 * run);
    vec3 light_col = mix(mainc, accentc, clamp(line1 + run * 0.5 + tab_dot, 0.0, 1.0));
""")

frame("ribbons", "Ribbons — a fine rounded border, two ribbons of light looping round outside it", 10,
      "half_size + 0.009", """
    float cut = sdround(q, half_size + 0.009, 0.03);
    float inner = stroke(abs(cut), max(line_w * 0.5, 0.0015), px);
    // The ribbons swing between the border and as far out as there is
    // room, crossing each other; four swings round the frame.
    float room_o = clamp(min(0.5 - hh, 0.5 * aspect - hw) - 0.024, 0.0, 0.06);
    // (At the very middle there is no angle: any will do there.)
    float ph = (abs(q.x) + abs(q.y) > 1e-6 ? atan(q.y / hh, q.x / hw) : 0.0) * 4.0;
    float d1 = cut - 0.006 - room_o * (0.5 + 0.45 * sin(ph - t * 0.4));
    float d2 = cut - 0.006 - room_o * (0.5 + 0.45 * sin(ph + 2.4 + t * 0.3));
    float rib1 = stroke(abs(d1), 0.0045 + 0.002 * sin(ph * 0.5 + t * 0.2), px * 2.0);
    float rib2 = stroke(abs(d2), 0.004, px * 2.0);
    float there = step(0.012, room_o);
    float solid = max(inner * 0.8, max(rib1, rib2 * 0.85) * there);
    float soft = (exp(-d1 * d1 / (0.012 * 0.012)) + exp(-d2 * d2 / (0.012 * 0.012))) * 0.6 * there;
    vec3 light_col = mix(mainc, accentc, clamp(rib2 * (1.0 - rib1) + 0.15 * inner, 0.0, 1.0));
""")

frame("braid", "Braided border — two soft lines rippling round, crossing over each other", 9,
      "half_size + 0.01 + line_w * 0.6", """
    // A rounded border a step outside the window; its corners' curve
    // passes through the window's corners.
    float amp = 0.006;
    float pad = reach.x - hw;
    float cut = sdround(q, half_size + pad, pad / 0.2929);
    // A whole number of ripples round it, so they meet where they started.
    float ripples = max(floor(round_len / 0.11), 6.0);
    float ph = along * ripples * 6.2831853 - t * 1.2;
    float l1 = stroke(abs(cut - amp * sin(ph)), line_w * 0.6, px);
    float l2 = stroke(abs(cut + amp * sin(ph)), line_w * 0.6, px);
    float run = exp(-sq((fract(along - t * 0.12) - 0.5) * 6.0));
    float solid = max(l1, l2 * 0.75);
    float soft = exp(-cut * cut / (0.016 * 0.016)) * (0.8 + 0.2 * sin(t * 0.7));
    vec3 light_col = mix(mainc, accentc, clamp(0.15 + 0.7 * l2 * (1.0 - l1) + run, 0.0, 1.0));
""")

frame("corners", "Corner brackets — four corners that breathe, a fine line between them", 10,
      "half_size + 0.004", """
    float cut = sdround(q, half_size + 0.004, 0.012);
    float d = abs(cut);
    float far = hh * (0.22 + 0.05 * sin(t * 0.9));
    // Near a corner on both axes: the bracket; elsewhere the fine line.
    float near = step(hw - far, abs(q.x)) * step(hh - far, abs(q.y));
    float bracket = stroke(d, line_w * 1.3, px) * near;
    float fine = stroke(d, max(line_w * 0.45, 0.0015), px) * 0.55 * (1.0 - near);
    float pulse = 0.75 + 0.25 * sin(t * 0.9 + along * 6.2831853 * 4.0);
    float solid = max(bracket * pulse, fine);
    float soft = exp(-d * d / (0.014 * 0.014)) * near;
    vec3 light_col = mix(mainc, accentc, bracket);
""")

frame("glow_border", "Glow border — a rounded border of soft light, one light going round", 8,
      "half_size + 0.009", """
    float cut = sdround(q, half_size + 0.009, 0.03);
    float d = abs(cut);
    float border = stroke(d, line_w * 0.8, px);
    float run = exp(-sq((fract(along - t * 0.2) - 0.5) * 6.0));
    float breathe = 0.8 + 0.2 * sin(t * 0.7);
    float solid = border * (0.7 + 0.3 * run);
    float soft = (exp(-d * d / (0.018 * 0.018)) * breathe + run * exp(-d * d / (0.028 * 0.028))) * 1.1;
    vec3 light_col = mix(mainc, accentc, clamp(0.25 + run, 0.0, 1.0));
""")

frame("dashes", "Marching dashes — dashes walking round a rounded border", 6,
      "half_size + 0.008", """
    float cut = sdround(q, half_size + 0.008, 0.025);
    float d = abs(cut);
    // A whole number of dashes round it, so they meet where they started.
    float count = max(floor(round_len / 0.09), 8.0);
    float dash = fract(along * count - t * 1.2);
    float on = smoothstep(0.0, 0.06, dash) * (1.0 - smoothstep(0.55, 0.61, dash));
    float border = stroke(d, line_w, px) * on;
    float lead = exp(-sq((fract(along - t * 0.15) - 0.5) * 8.0));
    float solid = border;
    float soft = exp(-d * d / (0.012 * 0.012)) * on * 0.8;
    vec3 light_col = mix(mainc, accentc, clamp(0.2 + lead, 0.0, 1.0));
""")
