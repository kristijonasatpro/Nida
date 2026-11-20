"""Writes Nida's line Frames (shaders/frame_*.fs): filters that hold a layer's
picture in a frame drawn of lines of light — an outline with cut corners,
marks above and below it — with a light going round. The frame's colour and
size and the light's colour and size are the user's. The picture fills the
frame's window, the lines drawn over it and what is outside the outline
dimmed; or, Picture inside the frame, it is fitted into the outline and cut
to its shape, clear outside. A layer gets one from Input Settings →
Position → Frame (`EngineCommand::SetLayerFrame`).

usage: python3 -I scripts/make_line_frames.py <varda repo>
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
head = (HERE / "make_broadcast_looks.py").read_text()
head = head[: head.index("# ---------------------------------------------------------------- Warm --")]
exec(compile(head, "make_broadcast_looks.py (head)", "exec"))

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
// A point turned by a (radians) round the middle.
vec2 fr_turn(vec2 p, float a) {
    float c = cos(a);
    float s = sin(a);
    return vec2(c * p.x - s * p.y, s * p.x + c * p.y);
}
// A portion of light going round: 1 at its head (x a whole number), fading
// over `tail` behind it; its head is round, not cut.
float fr_portion(float x, float tail) {
    float f = fract(x);
    return exp(-f / max(tail * 0.45, 1e-3)) + exp(-sq((f - 1.0) / 0.010));
}
"""
HELPERS = HELPERS.replace("// ---- end of helpers", DRAW.strip("\n") + "\n// ---- end of helpers")

CROP_INPUTS = [
        {"NAME": "crop_left", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop left"},
        {"NAME": "crop_right", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop right"},
        {"NAME": "crop_bottom", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop bottom"},
        {"NAME": "crop_top", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop top"},
]

BINDINGS = """
layout(set = 0, binding = 1) uniform sampler texSampler;
layout(set = 0, binding = 2) uniform texture2D inputImage;
layout(set = 0, binding = 3) uniform texture2D fr_cols;
layout(set = 0, binding = 4) uniform texture2D fr_rows;
layout(set = 0, binding = 5) uniform texture2D fr_box;

layout(set = 0, binding = 6) uniform UserParams {"""

# What a frame holds is what shows of the layer's picture: its box, found
# from the picture's own alpha each time, in three small passes before the
# frame is drawn — so a frame sits round what Crop keeps, round a picture
# cropped in the input it comes from, round a picture that does not fill
# its layer. A pass a column pair (is anything in it, on 64 lines down the
# picture?), a pass a row pair,
# and one texel that holds the box: how far in from the left, top, right
# and bottom what shows begins (all 1 when nothing shows).
PASSES = [
    {"TARGET": "fr_cols", "WIDTH": "$WIDTH/2", "HEIGHT": "1", "FLOAT": True},
    {"TARGET": "fr_rows", "WIDTH": "1", "HEIGHT": "$HEIGHT/2", "FLOAT": True},
    {"TARGET": "fr_box", "WIDTH": "1", "HEIGHT": "1", "FLOAT": True},
    {},
]
BOX = """
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
"""

# Two ways for a frame to know what shows of its layer's picture. The box
# passes (the default) find it from the picture itself; NIDA_FRAMES_BOX=0
# writes single-pass frames that are told the layer's own Crop instead (the
# engine sets their crop_left … crop_top from it): for an engine whose
# multi-pass effects read last frame's picture.
import os
USE_BOX = os.environ.get("NIDA_FRAMES_BOX", "1") != "0"
BOX_FROM_CROP = """
    // What Crop keeps of the layer (the engine gives a frame Crop's own
    // edges): how far in from the left, top, right and bottom it begins.
    vec4 box_in = vec4(min(crop_left, crop_right), 1.0 - max(crop_top, crop_bottom),
                       1.0 - max(crop_left, crop_right), min(crop_top, crop_bottom));
    // Nothing shows, and the frame draws nothing.
    if (box_in.x + box_in.z >= 1.0 || box_in.y + box_in.w >= 1.0) {
        fragColor = textureLod(sampler2D(inputImage, texSampler), uv, 0.0);
        return;
    }
"""

BINDINGS_ONE_PASS = """
layout(set = 0, binding = 1) uniform sampler texSampler;
layout(set = 0, binding = 2) uniform texture2D inputImage;

layout(set = 0, binding = 3) uniform UserParams {"""

SHAPES = ["As the picture", "16:9", "4:3", "1:1", "9:16"]
ANIMATIONS = ["A light going round", "Two lights going round", "Two lights, opposite ways", "Three lights going round",
              "Breathing", "Still"]


def hexrgb(h):
    return [round(int(h[i:i + 2], 16) / 255, 4) for i in (0, 2, 4)]


def line_inputs(frame_colour, light_colour, second=None, marks=False, anim=0, inside=False, fill=False, backing=1.0):
    """second: a second colour's default (two-colour frames); marks: the frame
    has marks; inside, fill, backing: what those start as."""
    inputs = [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "speed", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 10.0, "LABEL": "Speed"},
        {"NAME": "window", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.4, "MAX": 1.0, "LABEL": "Size"},
        {"NAME": "shape", "TYPE": "long", "DEFAULT": 0, "VALUES": list(range(len(SHAPES))), "LABELS": SHAPES,
         "LABEL": "Shape"},
        {"NAME": "fill", "TYPE": "bool", "DEFAULT": fill, "LABEL": "Fill the frame"},
        {"NAME": "inside", "TYPE": "bool", "DEFAULT": inside, "LABEL": "Picture inside the frame"},
        {"NAME": "frame_colour", "TYPE": "color", "DEFAULT": hexrgb(frame_colour) + [1.0], "LABEL": "Frame colour"},
    ]
    if second:
        inputs.append({"NAME": "second_colour", "TYPE": "color", "DEFAULT": hexrgb(second) + [1.0],
                       "LABEL": "Second colour"})
    inputs.append({"NAME": "weight", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Frame size"})
    if marks:
        inputs.append({"NAME": "marks", "TYPE": "float", "DEFAULT": 0.7, "MIN": 0.0, "MAX": 1.0, "LABEL": "Marks"})
    inputs += [
        {"NAME": "anim", "TYPE": "long", "DEFAULT": anim, "VALUES": list(range(len(ANIMATIONS))), "LABELS": ANIMATIONS,
         "LABEL": "Animation"},
        {"NAME": "light_colour", "TYPE": "color", "DEFAULT": hexrgb(light_colour) + [1.0], "LABEL": "Light colour"},
        {"NAME": "light_size", "TYPE": "float", "DEFAULT": 0.35, "MIN": 0.0, "MAX": 1.0, "LABEL": "Light size"},
        {"NAME": "glow", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Glow"},
        {"NAME": "backing", "TYPE": "float", "DEFAULT": backing, "MIN": 0.0, "MAX": 1.0, "LABEL": "Backing"},
    ]
    if not USE_BOX:
        inputs += CROP_INPUTS
    return inputs


# Shared start. The window is Size of the picture's height, as wide as its
# Shape, centred. `p` is a point's place from the middle in WINDOW heights
# (y down), so every measure of the frame goes with the window's size; `hn`
# the window's half-size so measured. The body leaves: `d` (how far from the
# nearest line of the outline), `inside_o` (1 inside the outline), `lines`
# (the frame's lines, 0–1), `soft` (their glow), `lit` (how much of the
# light is on each), `ob` (the outline's half-size, for fitting the picture).
START = """
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
    float unit = 2.0 * hs.y;
    vec2 p = q / unit;
    vec2 hn = hs / unit;
    // About a pixel and a half, in window heights: the soft edge of a line.
    float px = 1.5 / (res * unit);
    // Seconds, at Speed 1.
    float t = PHASE_TIME_0 * 10.0;
    float lw = mix(0.0020, 0.0075, weight);
    // How far round the window a point is (0–1, clockwise from the top left).
    vec2 edge = abs(p) - hn;
DEEP    float ul;
    if (edge.x > edge.y) {
        ul = p.x > 0.0 ? 2.0 * hn.x + (p.y + hn.y) : 4.0 * hn.x + 2.0 * hn.y + (hn.y - p.y);
    } else {
        ul = p.y < 0.0 ? p.x + hn.x : 2.0 * hn.x + 2.0 * hn.y + (hn.x - p.x);
    }
    float along = ul / (4.0 * (hn.x + hn.y));
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
    // The lines' own colour at a point, and how white-hot their core is.
    vec3 fc = frame_colour.rgb;
    float hot = 0.0;
    // How much of a line is the frame's dark plate, not light.
    float dark = 0.0;
    // The middle of the frame's opening, from the window's (window heights):
    // where a picture inside the frame is fitted round.
    vec2 open_mid = vec2(0.0);
"""

# Deep inside the window, farther in than anything the frame draws (REACH,
# in window heights), there is only the picture — when it fills the window:
# such a pixel is left at once, as FINISH would leave it.
DEEP = """    if (inside == 0u && -max(edge.x, edge.y) > REACH + 3.0 * px) {
        float deep_scale = fill != 0u ? max(hs.x / (0.5 * aspect), hs.y / 0.5) : min(hs.x / (0.5 * aspect), hs.y / 0.5);
        vec2 deep_at = q / (max(deep_scale, 1e-4) * vec2(aspect, 1.0)) + 0.5;
        vec4 deep_pic = textureLod(sampler2D(inputImage, texSampler), kept_lo + clamp(deep_at, 0.0, 1.0) * kept_size, 0.0);
        vec2 deep_in = clamp((0.5 - abs(deep_at - 0.5)) * 2.0 * max(RENDERSIZE * kept_size, vec2(1.0)) + 0.5, 0.0, 1.0);
        deep_pic.a *= deep_in.x * deep_in.y;
        fragColor = deep_pic.a > 0.0 ? deep_pic : vec4(0.0);
        return;
    }
"""

FINISH = """
    // The picture: all of the window — or, inside the frame, fitted into
    // the outline. Fill the frame covers where the shapes differ.
    vec2 room = inside != 0u ? (ob - vec2(lw)) * unit : hs;
    float scale = fill != 0u ? max(room.x / (0.5 * aspect), room.y / 0.5) : min(room.x / (0.5 * aspect), room.y / 0.5);
    vec2 at = (q - (inside != 0u ? open_mid * unit : vec2(0.0))) / (max(scale, 1e-4) * vec2(aspect, 1.0)) + 0.5;
    vec4 pic = textureLod(sampler2D(inputImage, texSampler), kept_lo + clamp(at, 0.0, 1.0) * kept_size, 0.0);
    vec2 texel = 0.5 / max(RENDERSIZE * kept_size, vec2(1.0));
    vec2 in_pic = clamp((0.5 - abs(at - 0.5)) / texel + 0.5, 0.0, 1.0);
    pic.a *= in_pic.x * in_pic.y;
    vec2 out_edge = edge * res * unit;
    float in_window = clamp(0.5 - max(out_edge.x, out_edge.y), 0.0, 1.0);
    // Inside the outline (soft at its line).
    float in_o = clamp(0.5 + (inside_o > 0.5 ? d : -d) * res * unit, 0.0, 1.0);
    if (inside != 0u) {
        pic.a *= in_o;
    } else {
        // What is outside the outline is the picture still, dimmed.
        pic.rgb *= mix(0.32, 1.0, in_o);
        pic.a *= in_window;
    }
    // The lines in the frame's colour, turning to the light's where it is;
    // their glow under them.
    vec3 lc = light_colour.rgb;
    vec3 line_col = mix(mix(fc, vec3(1.0), clamp(hot, 0.0, 1.0)), mix(lc, vec3(1.0), 0.35) * 1.25, clamp(lit, 0.0, 1.0));
    line_col = mix(line_col, vec3(0.012, 0.013, 0.016), clamp(dark, 0.0, 1.0));
    vec3 glow_col = mix(fc, lc, clamp(lit, 0.0, 1.0));
    float glow_a = clamp(soft * mix(0.0, 0.85, glow), 0.0, 1.0) * in_window;
    float line_a = clamp(lines, 0.0, 1.0) * in_window;
    // Outside the outline the frame has a backing: a dark plate behind its
    // lines and marks, so the frame is a thing that holds the picture and
    // not lines laid over it. Backing 0 takes it away: there the picture
    // shows dimmed (when it fills the window) or nothing does.
    vec3 back_col = mix(vec3(0.010, 0.011, 0.014), fc, 0.03);
    float back_a = clamp(backing, 0.0, 1.0) * (1.0 - in_o) * in_window;
    // Decks are straight alpha: the plate over the picture, the glow over
    // them, the lines over all.
    vec4 acc = vec4(pic.rgb * pic.a, pic.a);
    acc = vec4(back_col * back_a, back_a) + acc * (1.0 - back_a);
    acc = vec4(glow_col * glow_a, glow_a) + acc * (1.0 - glow_a);
    acc = vec4(line_col * line_a, line_a) + acc * (1.0 - line_a);
    fragColor = acc.a > 0.0 ? vec4(acc.rgb / acc.a, acc.a) : vec4(0.0);
"""

GLSL = {"float": "float", "long": "int", "color": "vec4", "bool": "uint"}


def line_frame(name, description, frame_colour, light_colour, body, reach=None, **more):
    """reach: how far in from the window's edge the frame draws anything (window
    heights), its glow included; None for one that draws elsewhere too."""
    inputs = line_inputs(frame_colour, light_colour, **more)
    header = {
        "DESCRIPTION": description,
        "CREDIT": "Nida",
        "ISFVSN": "2.0",
        "CATEGORIES": ["Filter", "Frame"],
        "INPUTS": inputs,
        "PHASE_INPUTS": [{"PARAM": "speed", "INDEX": 0, "SCALE": 0.1, "WRAP": LOOK_WRAP}],
    }
    if USE_BOX:
        header["PASSES"] = PASSES
    own = [i for i in inputs if i["TYPE"] != "image"]
    params = "\n".join(f"    {GLSL[i['TYPE']]} {i['NAME']};" for i in own)
    keep = " + ".join(keep_expr(i) for i in own)
    src = "/*" + json.dumps(header, indent=4, ensure_ascii=False) + "*/\n"
    uniforms = UNIFORMS.replace("USER_PARAMS", params)
    bindings = BINDINGS if USE_BOX else BINDINGS_ONE_PASS
    uniforms = uniforms.replace("layout(set = 0, binding = 1) uniform UserParams {", bindings.strip("\n"))
    assert "inputImage" in uniforms
    src += uniforms
    src += HELPERS
    start = START.replace("DEEP", DEEP.replace("REACH", f"{reach:.3f}") if reach else "")
    src += "\nvoid main() {" + KEEP_ALIVE.replace("KEEP_PARAMS", keep) + (BOX if USE_BOX else BOX_FROM_CROP) + start + body + FINISH + "}\n"
    (REPO / "shaders" / f"frame_{name}.fs").write_text(src)
    print("wrote", f"frame_{name}.fs")


line_frame("hud_lines", "HUD lines — an outline of light with cut corners and stepped sides, marks above and below, a light going round",
           "8FF8FF", "FFFFFF", """
    vec2 a = abs(p);
    // Never quite on the middle lines, where an outline's ends are.
    vec2 ac = max(a, vec2(1e-5));
    // The marks' band above and below, and the outline within what is left.
    float band_h = 0.085 * marks;
    float gap = 0.016 * step(0.02, marks);
    vec2 ob = hn - vec2(0.012 + lw, band_h + gap + lw + 0.006);
    // Picture inside the frame: the outline is as wide as the picture
    // fitted into it is, so nothing inside it is empty.
    if (inside != 0u && fill == 0u) {
        ob.x = min(ob.x, (ob.y - lw) * aspect + lw);
    }
    float cutc = 0.045;
    float dent = 0.017;
    float sdent = 0.013;
    // A quarter of the outline (the rest its mirror), from the middle of
    // the top to the middle of the side: fine and set in across the middle,
    // a step out, thick to the cut corner and down the side, a step in,
    // fine again.
    vec2 p0 = vec2(0.0, ob.y - dent);
    vec2 p1 = vec2(ob.x * 0.25, ob.y - dent);
    vec2 p2 = vec2(ob.x * 0.25 + dent, ob.y);
    vec2 p3 = vec2(ob.x * 0.37, ob.y);
    vec2 p4 = vec2(ob.x - cutc, ob.y);
    vec2 p5 = vec2(ob.x, ob.y - cutc);
    vec2 p6 = vec2(ob.x, ob.y * 0.50);
    vec2 p7 = vec2(ob.x - sdent, ob.y * 0.50 - sdent);
    vec2 p8 = vec2(ob.x - sdent, 0.0);
    float thick = min(seg(a, p3, p4), min(seg(a, p4, p5), seg(a, p5, p6)));
    float thin = min(min(seg(a, p0, p1), seg(a, p1, p2)), min(seg(a, p2, p3), min(seg(a, p6, p7), seg(a, p7, p8))));
    float d = min(thick, thin);
    float crossed = xing(ac, p1, p2) + xing(ac, p4, p5) + xing(ac, p5, p6) + xing(ac, p6, p7) + xing(ac, p7, p8);
    float inside_o = mod(crossed, 2.0) > 0.5 ? 1.0 : 0.0;
    // Where the light is the lines are a little thicker.
    float fat = 1.0 + 0.5 * light;
    float border = max(stroke(thick, lw * 1.6 * fat, px), stroke(thin, lw * 0.55 * fat, px));

    // The marks, in the band outside the outline: a fine line that goes
    // over slanted ticks and under two bars and ends in a ring; one more
    // across the middle, a ring at each end.
    vec2 o = vec2(ob.x, ob.y + gap + lw);
    float oy = (a.y - o.y) / max(band_h, 1e-3);
    float there = step(0.02, marks);
    float fine_w = max(lw * 0.38, 0.0011);
    vec2 a0 = vec2(o.x * 1.00, o.y + band_h * 0.20);
    vec2 a1 = vec2(o.x * 0.955, o.y + band_h * 0.92);
    vec2 a2 = vec2(o.x * 0.75, o.y + band_h * 0.92);
    vec2 a3 = vec2(o.x * 0.705, o.y + band_h * 0.22);
    vec2 a4 = vec2(o.x * 0.405, o.y + band_h * 0.22);
    vec2 a5 = vec2(o.x * 0.36, o.y + band_h * 0.92);
    vec2 a6 = vec2(o.x * 0.125, o.y + band_h * 0.92);
    float fine = min(min(seg(a, a0, a1), seg(a, a1, a2)), min(seg(a, a2, a3), seg(a, a3, a4)));
    fine = min(fine, min(seg(a, a4, a5), seg(a, a5, a6)));
    // The ticks' box is closed underneath.
    fine = min(fine, seg(a, a0, vec2(o.x * 0.795, a0.y)));
    vec2 m1 = vec2(o.x * 0.165, o.y + band_h * 0.08);
    fine = min(fine, seg(a, vec2(0.0, m1.y), m1));
    float ring = band_h * 0.075;
    fine = min(fine, abs(distance(a, a6 - vec2(ring, 0.0)) - ring));
    fine = min(fine, abs(distance(a, m1 + vec2(ring, 0.0)) - ring));
    float marks_l = stroke(fine, fine_w, px) * there;
    float oy_aa = px / max(band_h, 1e-3);
    // The ticks lean as the line's outer end does, and march; the bars lean
    // the other way, the long one breathing.
    float sxt = a.x + oy * 0.062 * o.x;
    float pitch = max(o.x * 0.034, 1e-4);
    float tri = abs(fract(sxt / pitch - t * 0.30) - 0.5);
    float ticks = band(o.x * 0.80, o.x * 0.985, sxt, px) * band(0.34, 0.80, oy, oy_aa)
        * (1.0 - smoothstep(0.24, 0.24 + px / pitch, tri)) * there;
    float sxb = a.x - oy * 0.058 * o.x;
    float breathe = 0.022 * sin(t * 0.8);
    float bars = band(0.36, 0.80, oy, oy_aa) * there
        * max(band(o.x * (0.455 + breathe), o.x * 0.655, sxb, px), band(o.x * 0.375, o.x * 0.432, sxb, px));
    float lines = max(max(border, marks_l * 0.95), max(ticks, bars));
    float lit = light;
    // The glow: close round every line, wider where the light is.
    float dm = min(d, fine + (1.0 - there));
    float soft = 0.75 * exp(-sq(dm / 0.008)) + 0.5 * exp(-sq(d / 0.022)) * (0.35 + 1.4 * light)
        + 0.35 * max(ticks, bars);
""", marks=True, inside=True, reach=0.21)

line_frame("neon_double", "Neon double — two neon rectangles leaning against each other, each its own colour, small rings in two corners",
           "FF4D6D", "FFFFFF", """
    // Two rectangles, one leaning each way; they sway a little.
    float lean = radians(1.5) + radians(0.45) * sin(t * 0.35);
    float most = sin(radians(1.95));
    float pad = 0.014 + lw;
    // Picture inside the frame: the frame is as wide as the picture
    // fitted into it, so nothing inside it is empty.
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, (aspect * (hn.y - pad - lw) + pad + lw + 2.0 * hn.y * most) / (1.0 + 2.0 * aspect * most));
    }
    vec2 b = hn - vec2(pad) - vec2(hn.y, hn.x) * most;
    vec2 pa = fr_turn(p, lean);
    vec2 pb = fr_turn(p, -lean);
    float sa = sdbox(pa, b);
    float sb = sdbox(pb, b);
    // Rings in the bottom left corner, and the same turned half round in the top right.
    vec2 pr = p.y > 0.0 ? p : -p;
    vec2 c1 = vec2(-b.x + 0.055, b.y - 0.060);
    float ring_b = abs(distance(pr, c1) - 0.014);
    float ring_a = abs(distance(pr, c1 + vec2(0.048, -0.006)) - 0.0075);
    float xa = min(abs(sa), ring_a);
    float xb = min(abs(sb), ring_b);
    // Now and then the second tube dips, as neon does.
    float dip = 1.0 - 0.30 * step(0.972, bc_hash12(vec2(floor(t * 11.0), 5.0)));
    float fat = 1.0 + 0.6 * light;
    float tube_a = stroke(xa, lw * 0.75 * fat, px);
    float tube_b = stroke(xb, lw * 0.75 * fat, px) * dip;
    float halo_a = exp(-sq(xa / 0.009)) + 0.45 * exp(-sq(xa / 0.034));
    float halo_b = (exp(-sq(xb / 0.009)) + 0.45 * exp(-sq(xb / 0.034))) * dip;
    float wa = tube_a + 0.4 * halo_a + 1e-4;
    float wb = tube_b + 0.4 * halo_b + 1e-4;
    fc = (frame_colour.rgb * wa + second_colour.rgb * wb) / (wa + wb);
    hot = 0.85 * max(stroke(xa, lw * 0.28, px), stroke(xb, lw * 0.28, px) * dip);
    float lines = max(tube_a, tube_b);
    float lit = light * 0.8;
    float soft = (halo_a + halo_b) * (0.75 + 0.8 * light);
    // The picture is cut, or dimmed outside, by where both rectangles are.
    float d = abs(max(sa, sb));
    float inside_o = max(sa, sb) < 0.0 ? 1.0 : 0.0;
    vec2 ob = b - vec2(hn.y, hn.x) * most;
""", second="4DA6FF", reach=0.21)

line_frame("neon_two", "Two-tone neon — a fine rounded line, half one colour and half another, a bright portion of each going round",
           "3D6BFF", "FFFFFF", """
    float pad = 0.016 + lw * 2.0;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + lw + (hn.y - pad - lw) * aspect);
    }
    vec2 ob = hn - vec2(pad);
    float sd = sdround(p, ob, 0.05);
    float d = abs(sd);
    float inside_o = sd < 0.0 ? 1.0 : 0.0;
    // Half the round in each colour; the halves turn slowly, the other way.
    float half_at = fract(along + t * 0.010);
    float first = smoothstep(0.0, 0.03, half_at) * (1.0 - smoothstep(0.5, 0.53, half_at));
    fc = mix(second_colour.rgb, frame_colour.rgb, first);
    // The line fine and dim; where the light is, thick and hot.
    float lines = stroke(d, lw * (0.32 + 0.75 * light), px) * (0.5 + 0.5 * light);
    hot = 0.9 * light * stroke(d, lw * 0.34, px);
    float lit = light * 0.25;
    float soft = (0.22 + 1.5 * light) * (exp(-sq(d / 0.010)) + 0.55 * exp(-sq(d / 0.05)));
""", second="FF5A36", anim=1, reach=0.19)

line_frame("neon_steps", "Stepped neon — two outlines with steps at their sides, a light standing above and below",
           "FF2E63", "FFFFFF", """
    // Each outline is a box with tabs added and notches taken out of its sides.
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, 0.118 + lw + (hn.y - 0.078 - 2.0 * lw) * aspect);
    }
    vec2 bo = hn - vec2(0.070, 0.030 + lw);
    float so = sdbox(p, bo);
    so = min(so, sdbox(p - vec2(-bo.x - 0.012, -hn.y * 0.44), vec2(0.034, hn.y * 0.16)));
    so = min(so, sdbox(p - vec2(-bo.x - 0.004, hn.y * 0.22), vec2(0.022, hn.y * 0.09)));
    so = min(so, sdbox(p - vec2(bo.x + 0.012, hn.y * 0.40), vec2(0.036, hn.y * 0.15)));
    so = max(so, -sdbox(p - vec2(bo.x, -hn.y * 0.30), vec2(0.022, hn.y * 0.12)));
    vec2 bi = hn - vec2(0.118, 0.078 + lw);
    float si = sdbox(p, bi);
    si = max(si, -sdbox(p - vec2(-bi.x, hn.y * 0.36), vec2(0.024, hn.y * 0.11)));
    si = min(si, sdbox(p - vec2(bi.x + 0.008, -hn.y * 0.16), vec2(0.024, hn.y * 0.10)));
    si = max(si, -sdbox(p - vec2(bi.x, hn.y * 0.30), vec2(0.018, hn.y * 0.08)));
    si = min(si, sdbox(p - vec2(-bi.x - 0.008, -hn.y * 0.22), vec2(0.020, hn.y * 0.07)));
    float xo = abs(so);
    float xi = abs(si);
    // A light standing on the inner line's top and the outer line's bottom,
    // breathing a little.
    float stand = (0.75 + 0.25 * sin(t * 0.7))
        * (exp(-sq(p.x / 0.20) - sq((p.y + bi.y) / 0.010)) + exp(-sq((p.x - 0.04) / 0.22) - sq((p.y - bo.y) / 0.010)));
    float fat = 1.0 + 0.6 * light;
    float line_o = stroke(xo, lw * 0.55 * fat, px);
    float line_i = stroke(xi, lw * 0.55 * fat, px);
    float lines = max(line_o, line_i);
    float x = min(xo, xi);
    hot = clamp(0.45 * stroke(x, lw * 0.22, px) + stand * 1.4, 0.0, 1.0);
    float lit = light * 0.8;
    float soft = (exp(-sq(x / 0.008)) + 0.45 * exp(-sq(x / 0.030))) * (0.7 + 0.9 * light)
        + 1.6 * (exp(-sq(p.x / 0.26) - sq((p.y + bi.y) / 0.035)) + exp(-sq((p.x - 0.04) / 0.28) - sq((p.y - bo.y) / 0.035)))
        * (0.75 + 0.25 * sin(t * 0.7));
    // The picture is cut, or dimmed outside, by the inner outline.
    float d = xi;
    float inside_o = si < 0.0 ? 1.0 : 0.0;
    vec2 ob = bi;
""", inside=True, reach=0.25)

line_frame("neon_arrows", "Neon arrows — a neon rectangle, an arrow pointing out of a gap in each side",
           "FF7A1E", "FFE9B0", """
    float pad = 0.020 + lw;
    float room_x = 0.050;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + room_x + lw + (hn.y - pad - lw) * aspect);
    }
    vec2 ob = hn - vec2(pad + room_x, pad);
    float sd = sdbox(p, ob);
    float d = abs(sd);
    float inside_o = sd < 0.0 ? 1.0 : 0.0;
    // No line across the gap in each side; an arrow stands in it.
    float notch = 0.085;
    float in_gap = step(abs(p.y), notch) * step(ob.x - 0.02, abs(p.x));
    vec2 a = vec2(abs(p.x) - ob.x, abs(p.y));
    float arrow = seg(a, vec2(0.042, 0.0), vec2(-0.010, notch * 0.74));
    float x = min(d + in_gap, arrow);
    float pulse = 0.85 + 0.15 * sin(t * 1.1);
    float lines = stroke(x, lw * 0.9 * (1.0 + 0.5 * light), px);
    hot = 0.85 * stroke(x, lw * 0.34, px);
    float lit = light * 0.8;
    float soft = (exp(-sq(x / 0.010)) + 0.5 * exp(-sq(x / 0.040))) * (0.8 + 0.8 * light) * pulse
        + 0.6 * exp(-sq(arrow / 0.05)) * pulse;
""", reach=0.23)

line_frame("neon_tube", "White neon — a thick white tube round the picture, its glow one colour turning to another",
           "5FE3FF", "FFFFFF", """
    float pad = 0.030 + lw * 2.0;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + lw + (hn.y - pad - lw) * aspect);
    }
    vec2 ob = hn - vec2(pad);
    float sd = sdbox(p, ob);
    float d = abs(sd);
    float inside_o = sd < 0.0 ? 1.0 : 0.0;
    float tube = lw * 1.7;
    float lines = stroke(d, tube * (1.0 + 0.2 * light), px);
    // Its glow: one colour up and to the left, the other down and to the
    // right, the two drifting.
    float side = clamp(0.5 + 0.45 * (p.x / hn.x + p.y / hn.y) + 0.14 * sin(t * 0.25), 0.0, 1.0);
    fc = mix(frame_colour.rgb, second_colour.rgb, side);
    hot = stroke(d, tube * 0.80, px);
    float lit = light * 0.6;
    float soft = (0.9 * exp(-sq(d / 0.016)) + 0.55 * exp(-sq(d / 0.060))) * (0.85 + 0.6 * light)
        * (0.92 + 0.08 * sin(t * 1.3));
""", second="C04DFF", reach=0.22)

line_frame("flare", "Flare — a fine line round the picture, a streak of light gliding along each side",
           "A56BFF", "FFFFFF", """
    float pad = 0.024 + lw;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + lw + (hn.y - pad - lw) * aspect);
    }
    vec2 ob = hn - vec2(pad);
    float sd = sdbox(p, ob);
    float d = abs(sd);
    float inside_o = sd < 0.0 ? 1.0 : 0.0;
    float lines = stroke(d, lw * 0.42, px) * 0.9;
    // A streak on each side: long above and below, short at the sides; they
    // glide to and fro and breathe.
    float glide = 0.24 * sin(t * 0.21);
    float breathe = 0.8 + 0.2 * sin(t * 0.9);
    float top = exp(-sq((p.x - glide * hn.x) / (0.42 * hn.x)));
    float bottom = exp(-sq((p.x + glide * hn.x) / (0.42 * hn.x)));
    float left = exp(-sq((p.y - glide * hn.y) / (0.24 * hn.y)));
    float right = exp(-sq((p.y + glide * hn.y) / (0.24 * hn.y)));
    float ty = p.y + ob.y;
    float by = p.y - ob.y;
    float lx = p.x + ob.x;
    float rx = p.x - ob.x;
    float core = top * exp(-sq(ty / 0.0035)) + bottom * exp(-sq(by / 0.0035))
        + 0.8 * (left * exp(-sq(lx / 0.0030)) + right * exp(-sq(rx / 0.0030)));
    float halo = top * exp(-sq(ty / 0.030)) + bottom * exp(-sq(by / 0.030))
        + 0.7 * (left * exp(-sq(lx / 0.022)) + right * exp(-sq(rx / 0.022)));
    hot = clamp(core * breathe * 1.6, 0.0, 1.0);
    lines = max(lines, clamp(core * breathe * 1.5, 0.0, 1.0));
    float lit = light * 0.8;
    float soft = 0.5 * exp(-sq(d / 0.008)) * (1.0 + light) + 1.7 * halo * breathe * (0.8 + 0.6 * light);
""", reach=0.15)

line_frame("electric", "Electric — a rounded edge that crackles, one colour running into another",
           "7CFF5A", "FFFFFF", """
    float pad = 0.030 + lw;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + lw + (hn.y - pad - lw) * aspect);
    }
    vec2 ob = hn - vec2(pad);
    float sd = sdround(p, ob, 0.06);
    float d = abs(sd);
    float inside_o = sd < 0.0 ? 1.0 : 0.0;
    // The edge crackles: two strands, each thrown about by its own noise.
    float j1 = bc_noise(vec2(along * 140.0, t * 3.1)) - 0.5 + 0.5 * (bc_noise(vec2(along * 420.0, t * 5.3)) - 0.5);
    float j2 = bc_noise(vec2(along * 90.0 + 7.0, t * 2.3)) - 0.5;
    float reach = 0.010 + 0.012 * glow;
    float s1 = abs(sd + j1 * reach - 0.004);
    float s2 = abs(sd + j2 * reach * 1.8 - 0.009);
    float flick = 0.65 + 0.35 * bc_noise(vec2(t * 9.0, along * 6.0));
    float half_at = fract(along - t * 0.02);
    fc = mix(second_colour.rgb, frame_colour.rgb, smoothstep(0.0, 0.08, half_at) * (1.0 - smoothstep(0.5, 0.58, half_at)));
    float lines = max(stroke(s1, lw * 0.36, px), 0.55 * stroke(s2, lw * 0.22, px)) * flick;
    hot = 0.7 * stroke(s1, lw * 0.14, px) * flick;
    float lit = light * 0.7;
    float soft = (exp(-sq(s1 / 0.006)) + 0.45 * exp(-sq(d / 0.030))) * flick * (0.8 + 0.9 * light);
""", second="FF3DD4", reach=0.16)

line_frame("deco_silver", "Silver deco — fine double lines, a square in each corner, arrowheads on the sides, glints that twinkle",
           "DDE3EA", "FFFFFF", """
    float pad = 0.020 + lw;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + 0.026 + lw + (hn.y - pad - 0.026 - lw) * aspect);
    }
    vec2 bo = hn - vec2(pad);
    vec2 ob = bo - vec2(0.026);
    float so = sdbox(p, bo);
    float si = sdbox(p, ob);
    float d = abs(si);
    float inside_o = si < 0.0 ? 1.0 : 0.0;
    vec2 a = abs(p);
    // The two lines; an arrowhead on the middle of each side, pointing in.
    float x = min(abs(so), abs(si));
    float head_tb = seg(vec2(a.x, a.y), vec2(0.0, ob.y - 0.034), vec2(0.060, bo.y));
    float head_lr = seg(vec2(a.y, a.x), vec2(0.0, ob.x - 0.034), vec2(0.060, bo.x));
    x = min(x, min(head_tb, head_lr));
    // Two short bars beside each corner, along the top and bottom.
    float bars = max(band(bo.x * 0.60, bo.x * 0.90, a.x, px) * stroke(abs(a.y - (bo.y - 0.009)), lw * 0.30, px),
                     band(bo.x * 0.68, bo.x * 0.90, a.x, px) * stroke(abs(a.y - (bo.y - 0.017)), lw * 0.30, px));
    // A small square set in each corner.
    float sq_d = sdbox(a - (bo - vec2(0.013)), vec2(0.010));
    float square = clamp(0.5 - sq_d / px, 0.0, 1.0);
    float lines = max(max(stroke(x, lw * 0.40, px), bars), square);
    // Polished: a shine that runs along the lines.
    float shine = 0.5 + 0.5 * sin(6.2831853 * (along * 6.0 - t * 0.05));
    fc = frame_colour.rgb * (0.50 + 0.50 * shine);
    // Glints: four-pointed stars at places on the frame, each twinkling at
    // its own time.
    float glint = 0.0;
    for (int i = 0; i < 7; i++) {
        float fi = float(i);
        float u = fract(0.137 + fi * 0.2931);
        // A place on the outer line, by how far round it is.
        float round_len = 4.0 * (bo.x + bo.y);
        float s = u * round_len;
        vec2 c = s < 2.0 * bo.x ? vec2(s - bo.x, -bo.y)
            : s < 2.0 * bo.x + 2.0 * bo.y ? vec2(bo.x, s - 2.0 * bo.x - bo.y)
            : s < 4.0 * bo.x + 2.0 * bo.y ? vec2(bo.x - (s - 2.0 * bo.x - 2.0 * bo.y), bo.y)
            : vec2(-bo.x, bo.y - (s - 4.0 * bo.x - 2.0 * bo.y));
        float tw = max(sin(t * (0.55 + 0.11 * fi) + fi * 2.4), 0.0);
        tw = tw * tw * tw;
        vec2 g = p - c;
        float star = exp(-abs(g.x) / 0.022 - sq(g.y / 0.0016)) + exp(-abs(g.y) / 0.022 - sq(g.x / 0.0016))
            + exp(-dot(g, g) / sq(0.0045));
        glint += star * tw;
    }
    glint *= mix(0.4, 1.0, glow);
    hot = clamp(0.25 * stroke(x, lw * 0.15, px) + glint, 0.0, 1.0);
    lines = max(lines, clamp(glint, 0.0, 1.0));
    float lit = light * 0.8;
    float soft = 0.35 * exp(-sq(x / 0.007)) * (1.0 + 1.5 * light) + 0.5 * glint;
""", reach=0.2)

line_frame("overlay", "Stream overlay — a bold frame with two cut corners, one colour running into another, a tab below for a name",
           "2E6BFF", "FFFFFF", """
    float pad = 0.010;
    float thin = mix(0.006, 0.016, weight);
    float thick = thin * 2.6;
    if (inside != 0u && fill == 0u) {
        hn.x = min(hn.x, pad + thin + (hn.y - pad - (thin + thick) * 0.5) * aspect);
    }
    // The outer shape: the top left and bottom right corners cut.
    vec2 bo = hn - vec2(pad);
    vec2 ao = abs(p) - bo;
    float cut = p.x * p.y > 0.0 ? 0.075 : 0.012;
    float so = max(length(max(ao, 0.0)) + min(max(ao.x, ao.y), 0.0), (ao.x + ao.y + cut) * 0.70710678);
    // The opening: thin at the top and sides, thick at the bottom; its own
    // corners cut to match.
    vec2 mid = vec2(0.0, -(thick - thin) * 0.5);
    vec2 ob = bo - vec2(thin, (thin + thick) * 0.5);
    vec2 ai = abs(p - mid) - ob;
    float cut_i = p.x * p.y > 0.0 ? 0.058 : 0.006;
    float si = max(length(max(ai, 0.0)) + min(max(ai.x, ai.y), 0.0), (ai.x + ai.y + cut_i) * 0.70710678);
    float d = abs(si);
    float inside_o = si < 0.0 ? 1.0 : 0.0;
    open_mid = mid;
    float body = clamp(0.5 - so / px, 0.0, 1.0) * clamp(0.5 + si / px, 0.0, 1.0);
    // A tab under the middle for a name: a dark plate, the frame's line round it.
    vec2 tp = vec2(abs(p.x), p.y - (bo.y - thick));
    float tab = max(max(-tp.y - 0.034, tp.y), (tp.x - 0.30 * hn.x - tp.y * 0.9 - 0.034) * 0.74);
    float plate = clamp(0.5 - tab / px, 0.0, 1.0);
    float rim = stroke(abs(tab), thin * 0.35, px);
    // Slanted ticks in the top bar's right end, marching.
    float tx = (p.x - hn.x * 0.52 + (p.y + bo.y) * 0.9) / 0.016;
    float ticks = band(0.25, 0.75, fract(tx - t * 0.4), px / 0.016) * band(0.0, 6.0, tx, 0.02)
        * band(-bo.y + thin + 0.004, -bo.y + thin + 0.016, p.y, px);
    // One colour at the top left running into the other at the bottom right.
    float run = clamp(0.5 + 0.5 * (p.x / hn.x + p.y / hn.y) + 0.10 * sin(t * 0.3), 0.0, 1.0);
    fc = mix(frame_colour.rgb, second_colour.rgb, run);
    float lines = max(max(body, plate), max(rim, ticks));
    dark = plate * (1.0 - rim) * (1.0 - body);
    hot = 0.0;
    float lit = light * (1.0 - dark) * 0.85;
    float x = min(abs(so), abs(si));
    float soft = (0.5 * exp(-sq(x / 0.010)) + 0.3 * exp(-sq(x / 0.035))) * (0.7 + 1.0 * light) * (1.0 - plate);
""", second="FF3DA6", reach=0.21)

line_frame("ring", "Neon ring — the picture in a circle, two neon rings round it, each its own colour",
           "FF4DA6", "FFFFFF", """
    float rad = min(hn.x, hn.y) - 0.030 - lw;
    // Two rings, a little apart, swaying round each other.
    vec2 off = 0.008 * vec2(cos(t * 0.4), sin(t * 0.4));
    float sa = length(p - off) - rad;
    float sb = length(p + off) - (rad + 0.004);
    float xa = abs(sa);
    float xb = abs(sb);
    // The light goes round the ring by its angle (straight up from the
    // very middle, where there is no angle).
    vec2 from_mid = dot(p, p) > 1e-12 ? p : vec2(0.0, -1.0);
    float ang = fract(atan(from_mid.x, -from_mid.y) / 6.2831853 + 1.0);
    float ring_light = anim == 0 ? fr_portion(turn - ang, tail)
        : anim == 1 ? fr_portion((turn - ang) * 2.0, tail * 1.4)
        : anim == 2 ? fr_portion(turn - ang, tail * 0.6) + fr_portion(ang + turn + 0.5, tail * 0.6)
        : anim == 3 ? fr_portion((turn - ang) * 3.0, tail) : light;
    float lit = clamp(ring_light, 0.0, 1.0) * 0.8;
    float fat = 1.0 + 0.75 * lit;
    float tube_a = stroke(xa, lw * 0.75 * fat, px);
    float tube_b = stroke(xb, lw * 0.75 * fat, px);
    float halo_a = exp(-sq(xa / 0.009)) + 0.45 * exp(-sq(xa / 0.034));
    float halo_b = exp(-sq(xb / 0.009)) + 0.45 * exp(-sq(xb / 0.034));
    float wa = tube_a + 0.4 * halo_a + 1e-4;
    float wb = tube_b + 0.4 * halo_b + 1e-4;
    fc = (frame_colour.rgb * wa + second_colour.rgb * wb) / (wa + wb);
    hot = 0.85 * max(stroke(xa, lw * 0.28, px), stroke(xb, lw * 0.28, px));
    float lines = max(tube_a, tube_b);
    float soft = (halo_a + halo_b) * (0.75 + 0.8 * lit);
    // The picture is cut to the inner of the two circles.
    float d = abs(max(sa, sb));
    float inside_o = max(sa, sb) < 0.0 ? 1.0 : 0.0;
    vec2 ob = vec2(rad - 0.008);
""", second="5F9BFF", inside=True, fill=True, backing=0.0)
