/*{
    "DESCRIPTION": "Crop - mask/crop with adjustable edges, and a border round what is kept",
    "CREDIT": "Varda VJ",
    "ISFVSN": "2.0",
    "CATEGORIES": ["Filter", "Stylize"],
    "INPUTS": [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "crop_left", "LABEL": "Left", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0},
        {"NAME": "crop_right", "LABEL": "Right", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 1.0},
        {"NAME": "crop_bottom", "LABEL": "Bottom", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0},
        {"NAME": "crop_top", "LABEL": "Top", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.0, "MAX": 1.0},
        {"NAME": "feather", "LABEL": "Feather", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 0.2},
        {"NAME": "invert_crop", "LABEL": "Invert", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 1.0},
        {"NAME": "border_mode", "LABEL": "Border", "TYPE": "bool", "DEFAULT": false},
        {"NAME": "border_color", "LABEL": "Colour", "TYPE": "color", "DEFAULT": [1.0, 1.0, 1.0, 1.0]},
        {"NAME": "border_width", "LABEL": "Thickness", "TYPE": "float", "DEFAULT": 6.0, "MIN": 0.0, "MAX": 100.0},
        {"NAME": "border_radius", "LABEL": "Roundness", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 540.0},
        {"NAME": "border_circle", "LABEL": "Circle", "TYPE": "bool", "DEFAULT": false}
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
    float crop_left;
    float crop_right;
    float crop_bottom;
    float crop_top;
    float feather;
    float invert_crop;
    uint border_mode; // ISF bools arrive as u32 0/1
    vec4 border_color;
    float border_width;
    float border_radius;
    uint border_circle; // the largest circle inside what is kept
};

// The border's sizes are in pixels of a 1080-line picture, whatever the
// render size, so a border looks the same at 720p and 4K. They are measured
// on the picture before Transform, so they shrink with it.
const float BORDER_UNIT = 1.0 / 1080.0;

// Signed distance to a rounded rectangle: negative inside.
float rounded_rect(vec2 p, vec2 centre, vec2 half_size, float radius) {
    vec2 q = abs(p - centre) - half_size + vec2(radius);
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

// How much of this pixel lies on the kept side of one crop edge. `inside` is
// the distance from the edge into the kept area, in UV; `pixel` is one pixel.
// Unfeathered, that is the pixel's exact coverage: an edge on a pixel boundary
// cuts cleanly there, one between pixels is anti-aliased, and an edge at the
// frame border (the default) leaves the outermost pixels untouched. A fixed
// minimum feather used to darken them to ~70% on every deck.
float edge(float inside, float pixel) {
    if (feather <= 0.0) {
        return clamp(inside / pixel + 0.5, 0.0, 1.0);
    }
    return smoothstep(-feather, feather, inside);
}

void main() {
    float audioSum = audio_level + audio_bass + audio_mid + audio_treble + audio_bpm + audio_beat_phase;
    float timeSum = TIMEDELTA + float(FRAMEINDEX) + float(PASSINDEX) + DATE.x + DATE.y + DATE.z + DATE.w + PHASE_TIME_0 + PHASE_TIME_1 + PHASE_TIME_2 + PHASE_TIME_3;
    if (uv.x < -1.0) { fragColor = vec4(audioSum + timeSum, 0.0, 0.0, 1.0); return; }

    vec4 src = texture(sampler2D(inputImage, texSampler), uv);

    // Nothing kept, nothing drawn — not a hair of an edge (inverted, all).
    if (crop_right <= crop_left || crop_top <= crop_bottom) {
        fragColor = invert_crop > 0.5 ? src : vec4(0.0);
        return;
    }

    vec2 pixel = 1.0 / max(RENDERSIZE, vec2(1.0));
    // Top and Bottom are measured up from the bottom, as their defaults (1
    // and 0) say; uv.y runs down from the top.
    float up = 1.0 - uv.y;
    float maskL = edge(uv.x - crop_left, pixel.x);
    float maskR = edge(crop_right - uv.x, pixel.x);
    float maskB = edge(up - crop_bottom, pixel.y);
    float maskT = edge(crop_top - up, pixel.y);

    float mask = maskL * maskR * maskB * maskT;
    if (invert_crop > 0.5) mask = 1.0 - mask;

    vec4 kept = vec4(src.rgb * mask, src.a * mask);
    if (border_mode == 0u || invert_crop > 0.5) {
        fragColor = kept;
        return;
    }

    // Border and rounded corners round the kept rectangle, worked out in
    // units of the frame's height so both axes measure alike.
    float aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    vec2 p = vec2(uv.x * aspect, up);
    vec2 lo = vec2(crop_left * aspect, crop_bottom);
    vec2 hi = vec2(crop_right * aspect, crop_top);
    vec2 half_size = max((hi - lo) * 0.5, vec2(0.0));
    if (half_size.x <= 0.0 || half_size.y <= 0.0) {
        // Nothing kept: no border round it either.
        fragColor = vec4(0.0);
        return;
    }
    float radius = min(border_radius * BORDER_UNIT, min(half_size.x, half_size.y));
    if (border_circle != 0u) {
        // A square on the shorter side, fully rounded.
        half_size = vec2(min(half_size.x, half_size.y));
        radius = half_size.x;
    }
    float d = rounded_rect(p, (lo + hi) * 0.5, half_size, radius);
    float px = 1.0 / max(RENDERSIZE.y, 1.0);
    // Inside the rounded outline (the corners cut away), anti-aliased.
    float shape = clamp(0.5 - d / px, 0.0, 1.0);
    // The band just inside it.
    float width = border_width * BORDER_UNIT;
    float band = width > 0.0 ? clamp(0.5 + (d + width) / px, 0.0, 1.0) * shape : 0.0;

    // Decks are straight alpha: the border goes over the picture that way.
    float picture_a = src.a * mask * shape;
    float a = band * border_color.a;
    float out_a = a + picture_a * (1.0 - a);
    vec3 out_rgb = out_a > 0.0
        ? (border_color.rgb * a + src.rgb * picture_a * (1.0 - a)) / out_a
        : vec3(0.0);
    fragColor = vec4(out_rgb, out_a);
}
