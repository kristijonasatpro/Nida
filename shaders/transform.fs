/*{
    "DESCRIPTION": "Transform - move, scale and rotate the picture about its own centre",
    "CREDIT": "Varda VJ",
    "ISFVSN": "2.0",
    "CATEGORIES": ["Filter", "Stylize"],
    "INPUTS": [
        {"NAME": "inputImage", "TYPE": "image"},
        {"NAME": "pos_x", "LABEL": "Position X", "TYPE": "float", "DEFAULT": 0.0, "MIN": -1.0, "MAX": 1.0},
        {"NAME": "pos_y", "LABEL": "Position Y", "TYPE": "float", "DEFAULT": 0.0, "MIN": -1.0, "MAX": 1.0},
        {"NAME": "scale", "LABEL": "Scale", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.01, "MAX": 5.0},
        {"NAME": "rotation", "LABEL": "Rotation", "TYPE": "float", "DEFAULT": 0.0, "MIN": -6.283, "MAX": 6.283},
        {"NAME": "scale_mode", "LABEL": "Separate X / Y scale", "TYPE": "bool", "DEFAULT": false},
        {"NAME": "scale_x", "LABEL": "Scale X", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.1, "MAX": 5.0},
        {"NAME": "scale_y", "LABEL": "Scale Y", "TYPE": "float", "DEFAULT": 1.0, "MIN": 0.1, "MAX": 5.0},
        {"NAME": "wrap", "LABEL": "Wrap (0=Clamp 1=Repeat 2=Mirror)", "TYPE": "float", "DEFAULT": 0.0, "MIN": 0.0, "MAX": 2.0},
        {"NAME": "flip_x", "LABEL": "Flip horizontal", "TYPE": "bool", "DEFAULT": false},
        {"NAME": "flip_y", "LABEL": "Flip vertical", "TYPE": "bool", "DEFAULT": false}
    ]
}*/

// Placement works the way it does in a switcher's picture-in-picture:
//  - Position moves the picture's centre across the frame. One unit is the
//    frame's width (X) or height (Y), whatever the scale, so a small picture
//    reaches the corners and past them.
//  - Scale and rotation happen about the picture's own centre, wherever it
//    has been moved to.
//  - Scale keeps proportions. X and Y only stretch it when "Separate X / Y
//    scale" is on, so the picture cannot be distorted by accident.
//  - Rotation turns pixels as squares, not as the frame's 16:9 cells, so a
//    rotated picture keeps its shape.
//  - Flip mirrors the picture about its own centre — a webcam that comes in
//    mirrored, a camera hung upside down — before it is scaled, turned and
//    moved; a crop before it is mirrored with it.

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
    float pos_x;
    float pos_y;
    float scale;
    float rotation;
    uint scale_mode; // ISF bools arrive as u32 0/1
    float scale_x;
    float scale_y;
    float wrap;
    uint flip_x;
    uint flip_y;
};

void main() {
    float audioSum = audio_level + audio_bass + audio_mid + audio_treble + audio_bpm + audio_beat_phase;
    float timeSum = TIMEDELTA + float(FRAMEINDEX) + float(PASSINDEX) + DATE.x + DATE.y + DATE.z + DATE.w + PHASE_TIME_0 + PHASE_TIME_1 + PHASE_TIME_2 + PHASE_TIME_3;
    if (uv.x < -1.0) { fragColor = vec4(audioSum + timeSum, 0.0, 0.0, 1.0); return; }

    // Work backwards from this output pixel to the source pixel it shows.
    // Offset from the picture's (moved) centre, in frame units.
    vec2 d = uv - vec2(0.5) - vec2(pos_x, pos_y);

    // Undo the rotation in square units: x scaled by the frame's aspect.
    float aspect = RENDERSIZE.x / max(RENDERSIZE.y, 1.0);
    d.x *= aspect;
    float ca = cos(-rotation), sa = sin(-rotation);
    d = vec2(d.x * ca - d.y * sa, d.x * sa + d.y * ca);
    d.x /= aspect;

    // Undo the scale, about the picture's own centre.
    vec2 s = vec2(scale);
    if (scale_mode != 0u) {
        s *= vec2(scale_x, scale_y);
    }
    vec2 p = d / max(s, vec2(1e-4)) + vec2(0.5);

    // Flipped, this pixel shows the mirror of that point of the picture.
    if (flip_x != 0u) {
        p.x = 1.0 - p.x;
    }
    if (flip_y != 0u) {
        p.y = 1.0 - p.y;
    }

    // Wrap mode
    int w = int(floor(wrap + 0.5));
    if (w == 1) {
        p = fract(p);
    } else if (w == 2) {
        p = abs(mod(p, 2.0) - 1.0);
    } else {
        // Clamp — transparent outside the picture
        if (p.x < 0.0 || p.x > 1.0 || p.y < 0.0 || p.y > 1.0) {
            fragColor = vec4(0.0);
            return;
        }
    }

    fragColor = texture(sampler2D(inputImage, texSampler), p);
}
