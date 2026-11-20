"""Writes Nida's Frames that are pictures (shaders/frame_*.fs + the picture
each one shows, shaders/frames/*.png): filters that hold a layer's picture.
The picture fills the frame's window — the whole layer, as it comes — and
the frame's art is laid over its edge: a rendered frame with a clear window,
its sides kept as drawn and its plain middle stretched, so it fits a window
of any shape. Its lights breathe and a light passes over its metal. A layer
gets one from Input Settings → Position → Frame
(`EngineCommand::SetLayerFrame`), where it runs before Transform, so the
layer is moved and scaled frame and all.

The art is made outside the repository — a rendered picture, its window cut
clear and the light it throws into the window put in — and kept as
shaders/frames/<name>.png; this script writes the filter that shows it (the
list is at the end: each frame's name, description, its window's box in the
picture, the picture's shape and the colour its lights were drawn in).

usage: python3 -I scripts/make_art_frames.py <varda repo>
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
head = (HERE / "make_broadcast_looks.py").read_text()
head = head[: head.index("# ---------------------------------------------------------------- Warm --")]
exec(compile(head, "make_broadcast_looks.py (head)", "exec"))

DRAW = """
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
"""
HELPERS = HELPERS.replace("// ---- end of helpers", DRAW.strip("\n") + "\n// ---- end of helpers")

BINDINGS = """
layout(set = 0, binding = 1) uniform sampler texSampler;
layout(set = 0, binding = 2) uniform texture2D inputImage;
layout(set = 0, binding = 3) uniform texture2D fr_cols;
layout(set = 0, binding = 4) uniform texture2D fr_rows;
layout(set = 0, binding = 5) uniform texture2D fr_box;
layout(set = 0, binding = 6) uniform texture2D frame_art;

layout(set = 0, binding = 7) uniform UserParams {"""

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
layout(set = 0, binding = 3) uniform texture2D frame_art;

layout(set = 0, binding = 4) uniform UserParams {"""

CROP_INPUTS = [
    {"NAME": "crop_left", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop left"},
    {"NAME": "crop_right", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop right"},
    {"NAME": "crop_bottom", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop bottom"},
    {"NAME": "crop_top", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 1.0, "LABEL": "Crop top"},
]

SHAPES = ["As the picture", "16:9", "4:3", "1:1", "9:16"]
ANIMATIONS = ["A light going round", "Two lights going round", "Two lights, opposite ways", "Three lights going round",
              "Breathing", "Still"]

INPUTS = [
    {"NAME": "inputImage", "TYPE": "image"},
    {"NAME": "speed", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 10.0, "LABEL": "Speed"},
    {"NAME": "window", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.4, "MAX": 1.0, "LABEL": "Size"},
    {"NAME": "shape", "TYPE": "long", "DEFAULT": 0, "VALUES": list(range(len(SHAPES))), "LABELS": SHAPES,
     "LABEL": "Shape"},
    {"NAME": "fill", "TYPE": "bool", "DEFAULT": False, "LABEL": "Fill the frame"},
    {"NAME": "inside", "TYPE": "bool", "DEFAULT": False, "LABEL": "Picture inside the frame"},
    {"NAME": "frame_colour", "TYPE": "color", "DEFAULT": [0.0, 0.0, 0.0, 1.0], "LABEL": "Frame colour"},  # the art's own, set per frame
    {"NAME": "weight", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Frame size"},
    {"NAME": "anim", "TYPE": "long", "DEFAULT": 0, "VALUES": list(range(len(ANIMATIONS))), "LABELS": ANIMATIONS,
     "LABEL": "Animation"},
    {"NAME": "light_colour", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0], "LABEL": "Light colour"},
    {"NAME": "light_size", "TYPE": "float", "DEFAULT": 0.35, "MIN": 0.0, "MAX": 1.0, "LABEL": "Light size"},
    {"NAME": "glow", "TYPE": "float", "DEFAULT": 0.5, "MIN": 0.0, "MAX": 1.0, "LABEL": "Glow"},
]

# The window is Size of the picture's height, as wide as its Shape (or as
# much of that as the picture is wide), centred; `q` is a point's place from
# the middle in frame heights (y down). The picture fills the window — or,
# Picture inside the frame, the art's own opening; Fill the frame covers it
# where the shapes differ. Deep inside the opening there is only the
# picture: such a pixel is left at once.
BODY = """
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
    // The art: as wide as ART_ASPECT heights; its window's box in it (left,
    // top, right, bottom); the art is one frame height high here.
    vec4 box = vec4(BOX);
    float art_aspect = ART_ASPECT;
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
    if (max(from_open.x, from_open.y) < -REACH * unit) {
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
    float kx = min(KEEP * thick, 0.46 * awin);
    float ky = min(KEEPV * thick, 0.46);
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
"""

GLSL = {"float": "float", "long": "int", "color": "vec4", "bool": "uint"}


def art_frame(name, description, box, art_aspect, lights, keep=0.52, keepv=0.30, reach=0.20):
    """lights: the colour the art's lights were drawn in (sRGB, the brightest
    part 1); box: its window (left, top,
    right, bottom, 0–1); keep: how much of each side is kept as drawn, in
    frame heights; reach: how far into the window its glow goes."""
    inputs = [dict(i) for i in INPUTS]
    for i in inputs:
        if i["NAME"] == "frame_colour":
            i["DEFAULT"] = [round(v, 4) for v in lights] + [1.0]
    header = {
        "DESCRIPTION": description,
        "CREDIT": "Nida",
        "ISFVSN": "2.0",
        "CATEGORIES": ["Filter", "Frame"],
        "INPUTS": inputs,
        "IMPORTED": {"frame_art": {"PATH": f"frames/{name}.png"}},
        "PHASE_INPUTS": [{"PARAM": "speed", "INDEX": 0, "SCALE": 0.1, "WRAP": LOOK_WRAP}],
    }
    if USE_BOX:
        header["PASSES"] = PASSES
    else:
        inputs += CROP_INPUTS
    own = [i for i in inputs if i["TYPE"] != "image"]
    params = "\n".join(f"    {GLSL[i['TYPE']]} {i['NAME']};" for i in own)
    keep_alive = " + ".join(keep_expr(i) for i in own)
    src = "/*" + json.dumps(header, indent=4, ensure_ascii=False) + "*/\n"
    uniforms = UNIFORMS.replace("USER_PARAMS", params)
    bindings = BINDINGS if USE_BOX else BINDINGS_ONE_PASS
    uniforms = uniforms.replace("layout(set = 0, binding = 1) uniform UserParams {", bindings.strip("\n"))
    assert "frame_art" in uniforms
    src += uniforms
    src += HELPERS
    body = (BODY.replace("BOX", ", ".join(f"{v:.4f}" for v in box)).replace("ART_ASPECT", f"{art_aspect:.5f}")
            .replace("KEEPV", f"{keepv:.3f}").replace("KEEP", f"{keep:.3f}").replace("REACH", f"{reach:.3f}"))
    src += "\nvoid main() {" + KEEP_ALIVE.replace("KEEP_PARAMS", keep_alive) + (BOX if USE_BOX else BOX_FROM_CROP) + body + "}\n"
    (REPO / "shaders" / f"frame_{name}.fs").write_text(src)
    assert (REPO / "shaders" / "frames" / f"{name}.png").is_file(), f"no shaders/frames/{name}.png"
    print("wrote", f"frame_{name}.fs")


art_frame("hud", "HUD frame — machined gunmetal, screws, light strips, a meter in the lower left corner",
          (0.0287, 0.0361, 0.9707, 0.9649), 1672 / 941, (0.031, 0.986, 1.0))
