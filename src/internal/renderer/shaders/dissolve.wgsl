// Dissolve shader - blends two pictures by a mix factor on a fullscreen triangle
// DissolveParams is 16 bytes (1 x f32 + padding)

struct DissolveParams {
    mix: f32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@group(0) @binding(0)
var texture_sampler: sampler;

@group(0) @binding(1)
var from_texture: texture_2d<f32>;

@group(0) @binding(2)
var to_texture: texture_2d<f32>;

@group(0) @binding(3)
var<uniform> params: DissolveParams;

// Fullscreen triangle vertex shader (no vertex buffers needed)
@vertex
fn vs_main(@builtin(vertex_index) vertex_index: u32) -> VertexOutput {
    var out: VertexOutput;

    let x = f32((vertex_index & 1u) << 2u);
    let y = f32((vertex_index & 2u) << 1u);

    out.position = vec4<f32>(x - 1.0, 1.0 - y, 0.0, 1.0);
    out.uv = vec2<f32>(x * 0.5, y * 0.5);

    return out;
}

// Sampled by normalised UV, so the two pictures need not share a size: a 1x1
// black source dissolves against a full-resolution channel like any other.
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let from_color = textureSample(from_texture, texture_sampler, in.uv);
    let to_color = textureSample(to_texture, texture_sampler, in.uv);
    return mix(from_color, to_color, clamp(params.mix, 0.0, 1.0));
}
