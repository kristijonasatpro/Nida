//! The pass that shows an effect through its mask (see
//! [`crate::engine::value::mask`]): the picture before the effect and after
//! it, mixed by the mask.

use crate::engine::value::mask::{EffectMask, MaskShape, MAX_PATH_POINTS};

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct MaskParams {
    shape: u32,
    count: u32,
    invert: u32,
    _padding: u32,
    rect: [f32; 4],
    feather: f32,
    aspect: f32,
    pixel: f32,
    _padding2: f32,
    points: [[f32; 4]; MAX_PATH_POINTS / 2],
}

impl MaskParams {
    /// The mask for a `width` × `height` target.
    fn of(mask: &EffectMask, (width, height): (u32, u32)) -> Self {
        let mut params = Self {
            shape: 0,
            count: 0,
            invert: u32::from(mask.invert),
            _padding: 0,
            rect: [0.0; 4],
            // Feather is in 1080ths of the height; the shader measures in heights.
            feather: mask.feather / 1080.0,
            aspect: width as f32 / height.max(1) as f32,
            pixel: 1.0 / height.max(1) as f32,
            _padding2: 0.0,
            points: [[0.0; 4]; MAX_PATH_POINTS / 2],
        };
        match &mask.shape {
            MaskShape::Rectangle {
                x,
                y,
                width,
                height,
            } => params.rect = [*x, *y, *width, *height],
            MaskShape::Ellipse {
                x,
                y,
                width,
                height,
            } => {
                params.shape = 1;
                params.rect = [*x, *y, *width, *height];
            }
            MaskShape::Path { points } => {
                params.shape = 2;
                let points = &points[..points.len().min(MAX_PATH_POINTS)];
                params.count = points.len() as u32;
                for (i, p) in points.iter().enumerate() {
                    let pair = &mut params.points[i / 2];
                    pair[(i % 2) * 2] = p[0];
                    pair[(i % 2) * 2 + 1] = p[1];
                }
            }
        }
        params
    }
}

/// The mask pass for one effect: its pipeline, its settings, and the
/// texture the effect draws into before the mask mixes it in.
pub struct MaskPass {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    scratch: wgpu::Texture,
    scratch_view: wgpu::TextureView,
}

impl MaskPass {
    /// A mask pass for `width` × `height` targets in `format`.
    pub fn new(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> Self {
        let texture_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Mask Bind Group Layout"),
            entries: &[
                texture_entry(0),
                texture_entry(1),
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
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
            label: Some("Mask Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mask Vertex Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fullscreen.wgsl").into()),
        });
        let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Mask Fragment Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/mask.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Mask Pipeline"),
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
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Mask Params"),
            size: std::mem::size_of::<MaskParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let scratch = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Mask Scratch"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let scratch_view = scratch.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            pipeline,
            bind_group_layout,
            params,
            scratch,
            scratch_view,
        }
    }

    /// Whether this pass fits `width` × `height` targets in `format`.
    pub fn fits(&self, format: wgpu::TextureFormat, width: u32, height: u32) -> bool {
        self.scratch.format() == format
            && self.scratch.width() == width.max(1)
            && self.scratch.height() == height.max(1)
    }

    /// Where the effect draws, for the mask to mix in.
    pub fn scratch_view(&self) -> &wgpu::TextureView {
        &self.scratch_view
    }

    /// Mix `before` and the effect's picture (in the scratch texture) into
    /// `output` through `mask` — only through the scissor rectangles `within`,
    /// the rest of `output` left as it was, or everywhere for `None`.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        mask: &EffectMask,
        before: &wgpu::TextureView,
        output: &wgpu::TextureView,
        within: Option<&[super::region::Pixels]>,
    ) {
        let size = (self.scratch.width(), self.scratch.height());
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&MaskParams::of(mask, size)),
        );
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Mask Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(before),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&self.scratch_view),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Mask Encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Mask Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: if within.is_some() {
                            wgpu::LoadOp::Load
                        } else {
                            wgpu::LoadOp::Clear(wgpu::Color::BLACK)
                        },
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
            super::region::draw_within(&mut pass, within);
        }
        cmd_buffers.push(encoder.finish());
    }
}
