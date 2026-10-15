//! The pass that draws a playlist's picture: the item on screen, or it
//! giving way to the next by a fade or a slide (see
//! [`crate::engine::value::playlist`]).

/// What the pass draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mix {
    /// The item on screen alone.
    Shown,
    /// The item on screen dissolving into the next, this far.
    Fade(f32),
    /// The next pushing the item on screen off, this far, coming in from
    /// this side (an offset in UV), blurred over this much of the way (the
    /// ground a frame covers while the shutter is open; 0 for none).
    Slide(f32, [f32; 2], f32),
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MixParams {
    mode: u32,
    present: u32,
    progress: f32,
    blur: f32,
    from: [f32; 2],
    taps: u32,
    _padding: f32,
}

/// The most samples a slide's blur takes along its way.
pub const MAX_BLUR_TAPS: u32 = 16;
/// Pixels between two of a blur's samples, at most: closer than this, more
/// samples look no smoother.
const TAP_SPACING_PX: f32 = 1.5;

/// How many samples a slide's `blur` (the share of the way it is blurred
/// over) needs on a `size` frame, coming in `from` a side: one every pixel
/// and a half of the ground it covers, so a slow slide — a few pixels a
/// frame — takes a few, not the most.
pub fn blur_taps(blur: f32, from: [f32; 2], size: (u32, u32)) -> u32 {
    #[allow(clippy::cast_precision_loss)]
    let span = blur.max(0.0) * (from[0].abs() * size.0 as f32 + from[1].abs() * size.1 as f32);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let taps = (span / TAP_SPACING_PX).ceil() as u32;
    taps.clamp(1, MAX_BLUR_TAPS)
}

pub struct PlaylistMixPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    /// Bound where a picture is missing.
    blank: wgpu::TextureView,
}

impl PlaylistMixPipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Playlist Mix Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                texture_entry(1),
                texture_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Playlist Mix Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Playlist Mix Vertex Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fullscreen.wgsl").into()),
        });
        let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Playlist Mix Fragment Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/playlist_mix.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Playlist Mix Pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &vertex,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &fragment,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Playlist Mix Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Playlist Mix Params"),
            size: std::mem::size_of::<MixParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("Playlist Mix Blank"),
                size: wgpu::Extent3d {
                    width: 1,
                    height: 1,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            pipeline,
            bind_group_layout,
            sampler,
            params,
            blank,
        }
    }

    /// Draw `mix` of `shown` and `next` into `output`, all of it — `size`
    /// pixels. A missing picture is clear.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        mix: Mix,
        shown: Option<&wgpu::TextureView>,
        next: Option<&wgpu::TextureView>,
        output: &wgpu::TextureView,
        size: (u32, u32),
    ) {
        let (mode, progress, from, blur) = match mix {
            Mix::Shown => (0, 0.0, [0.0; 2], 0.0),
            Mix::Fade(t) => (1, t, [0.0; 2], 0.0),
            Mix::Slide(t, from, blur) => (2, t, from, blur),
        };
        let params = MixParams {
            mode,
            present: u32::from(shown.is_some()) | (u32::from(next.is_some()) << 1),
            progress,
            blur,
            from,
            taps: blur_taps(blur, from, size),
            _padding: 0.0,
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Playlist Mix Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(shown.unwrap_or(&self.blank)),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(next.unwrap_or(&self.blank)),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Playlist Mix Encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Playlist Mix Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        cmd_buffers.push(encoder.finish());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_slide_takes_few_samples_a_fast_one_the_most() {
        let uhd = (3840, 2160);
        // 12 px of blur (a 4 s slide at its fastest, on 4K): 8 samples.
        assert_eq!(blur_taps(12.0 / 3840.0, [1.0, 0.0], uhd), 8);
        // Up and down, the height counts.
        assert_eq!(blur_taps(12.0 / 2160.0, [0.0, -1.0], uhd), 8);
        assert_eq!(blur_taps(0.0, [1.0, 0.0], uhd), 1, "no blur, one sample");
        assert_eq!(blur_taps(0.2, [1.0, 0.0], uhd), MAX_BLUR_TAPS);
    }
}
