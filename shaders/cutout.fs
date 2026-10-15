/*{
    "DESCRIPTION": "Cut out - a layer's own mask: the layer transparent where its mask is, so what is under it shows through (see Input Settings, Mask)",
    "CREDIT": "Nida",
    "ISFVSN": "2.0",
    "CATEGORIES": ["Filter"],
    "INPUTS": [
        {"NAME": "inputImage", "TYPE": "image"}
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

void main() {
    // Keeps the colour, so a feathered edge fades the picture out rather
    // than darkening it: layers are straight alpha.
    vec4 src = texture(sampler2D(inputImage, texSampler), uv);
    fragColor = vec4(src.rgb, 0.0);
}
