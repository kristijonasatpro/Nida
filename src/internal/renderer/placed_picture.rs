//! The pass that draws a layer's picture straight into its place — cropped,
//! bordered, rounded, moved and scaled at once (see
//! `shaders/placed_picture.wgsl` and `Deck::fold`). A picture-in-picture a
//! third across draws a ninth of the frame, from the source, rather than all
//! of it three times over: the source copied, the copy cropped, the crop
//! shrunk.

use super::region::{draw_within, Pixels};

/// The crop effect's settings, as the pass takes them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Crop {
    pub left: f32,
    pub right: f32,
    /// Measured up from the bottom, as the crop's settings are.
    pub bottom: f32,
    pub top: f32,
    pub feather: f32,
    /// Its border, when it has one.
    pub border: Option<Border>,
    /// 1: drawn as set. Less, a Merge on its way from the picture uncropped
    /// — its edge, there hard, only so far soft (see `between_framing` in
    /// `mixer::main_transition`).
    pub strength: f32,
}

/// A crop's border: its colour, thickness and roundness (in pixels of a
/// 1080-line picture), or the largest circle inside the crop.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Border {
    pub color: [f32; 4],
    pub width: f32,
    pub radius: f32,
    /// How far the shape is the largest circle inside the crop: 0 the
    /// rounded rectangle, 1 the circle; between, on the way from one to the
    /// other, as a Merge draws it.
    pub circle: f32,
    /// 1: drawn as set. Less, a Merge on its way from the crop without a
    /// border: the shape kept clear of the crop's soft edge, its colours as
    /// the crop alone draws them, so far as the border is not there yet.
    pub strength: f32,
}

/// Where and how a picture is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placed {
    /// A picture point to the source's UV (the scaling mode).
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
    /// The transform: the picture's centre moved by this, and its scale
    /// (none: 0 and 1).
    pub pos: [f32; 2],
    pub scale: [f32; 2],
    /// The transform's flip: the picture mirrored left for right, top for
    /// bottom, about its own centre — the crop with it.
    pub flip: [bool; 2],
    pub crop: Option<Crop>,
    /// The source is premultiplied (another input), drawn out straight.
    pub premultiplied: bool,
    /// 1: drawn over black, as a deck not keeping its source's transparency
    /// draws it — black where the source is clear or does not reach; 0: as
    /// it comes. Between, a Merge on its way from one to the other.
    pub over_black: f32,
}

impl Placed {
    /// The part of the frame the picture can show in, in UV: its moved and
    /// scaled frame, cut down to the crop and a little room round it for a
    /// feathered or anti-aliased edge. `pixel` is one of the frame's pixels
    /// in UV.
    pub fn bounds(&self, pixel: [f32; 2]) -> [f32; 4] {
        let (mut lo, mut hi) = ([0.0_f32; 2], [1.0_f32; 2]);
        if let Some(crop) = &self.crop {
            // Picture UV runs down from the top; the crop's bottom and top
            // up from the bottom.
            let room = [
                crop.feather + 2.0 * pixel[0] / self.scale[0].max(1e-4),
                crop.feather + 2.0 * pixel[1] / self.scale[1].max(1e-4),
            ];
            lo = [crop.left - room[0], 1.0 - crop.top - room[1]];
            hi = [crop.right + room[0], 1.0 - crop.bottom + room[1]];
        }
        // Flipped, the kept part shows mirrored.
        for axis in 0..2 {
            if self.flip[axis] {
                (lo[axis], hi[axis]) = (1.0 - hi[axis], 1.0 - lo[axis]);
            }
        }
        let to_frame = |p: f32, axis: usize| {
            ((p.clamp(0.0, 1.0) - 0.5) * self.scale[axis] + 0.5 + self.pos[axis]).clamp(0.0, 1.0)
        };
        [
            to_frame(lo[0], 0),
            to_frame(lo[1], 1),
            to_frame(hi[0], 0),
            to_frame(hi[1], 1),
        ]
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
    pos: [f32; 2],
    scale: [f32; 2],
    frame: [f32; 2],
    premultiplied: u32,
    crop_mode: u32,
    crop: [f32; 4],
    border_color: [f32; 4],
    feather: f32,
    border_width: f32,
    border_radius: f32,
    circle: f32,
    flip: [f32; 2],
    cropped: f32,
    bordered: f32,
    over_black: f32,
    /// To the 16 bytes a uniform block is a multiple of.
    _pad: [f32; 3],
}

pub struct PlacedPicturePipeline {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
}

impl PlacedPicturePipeline {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Placed Picture Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
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
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Placed Picture Pipeline Layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let vertex = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Placed Picture Vertex Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/fullscreen.wgsl").into()),
        });
        let fragment = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Placed Picture Fragment Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/placed_picture.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Placed Picture Pipeline"),
            layout: Some(&pipeline_layout),
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
            label: Some("Placed Picture Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Placed Picture Params"),
            size: std::mem::size_of::<Params>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            sampler,
            params,
        }
    }

    /// Clear `output` (`frame` pixels) and draw `placed` from `source` into
    /// it, only within `within` — the parts of the frame that show, which
    /// the pass cuts down to the picture's bounds. Returns the pixels drawn.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        cmd_buffers: &mut Vec<wgpu::CommandBuffer>,
        placed: &Placed,
        source: &wgpu::TextureView,
        output: &wgpu::TextureView,
        frame: (u32, u32),
        within: Option<&[Pixels]>,
    ) -> u64 {
        let none = Border {
            color: [0.0; 4],
            width: 0.0,
            radius: 0.0,
            circle: 0.0,
            strength: 1.0,
        };
        let (crop_mode, crop, border, feather, cropped) = match &placed.crop {
            None => (0, [0.0, 1.0, 0.0, 1.0], none, 0.0, 1.0),
            Some(c) => (
                if c.border.is_some() { 2 } else { 1 },
                [c.left, c.right, c.bottom, c.top],
                c.border.unwrap_or(none),
                c.feather,
                c.strength,
            ),
        };
        #[allow(clippy::cast_precision_loss)]
        let size = [frame.0.max(1) as f32, frame.1.max(1) as f32];
        let params = Params {
            uv_scale: placed.uv_scale,
            uv_offset: placed.uv_offset,
            pos: placed.pos,
            scale: placed.scale.map(|s| s.max(1e-4)),
            frame: size,
            premultiplied: u32::from(placed.premultiplied),
            crop_mode,
            crop,
            // Linear, as the crop effect's own colour reaches its shader
            // (see `ParamValue::write_bytes`).
            border_color: {
                let [r, g, b, a] = border.color;
                let linear = crate::params::srgb_to_linear;
                [linear(r), linear(g), linear(b), a]
            },
            feather,
            border_width: border.width,
            border_radius: border.radius,
            circle: border.circle,
            flip: placed.flip.map(|on| if on { 1.0 } else { 0.0 }),
            cropped,
            bordered: border.strength,
            over_black: placed.over_black,
            _pad: [0.0; 3],
        };
        queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&params));

        let [x0, y0, x1, y1] = placed.bounds([1.0 / size[0], 1.0 / size[1]]);
        let picture = super::region::Area { x0, y0, x1, y1 }.outer_pixels(frame.0, frame.1, 0);
        let parts: Vec<Pixels> = match (picture, within) {
            (None, _) => Vec::new(),
            (Some(picture), None) => vec![picture],
            (Some(picture), Some(parts)) => parts
                .iter()
                .map(|part| part.intersect(&picture))
                .filter(|part| !part.is_empty())
                .collect(),
        };

        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("Placed Picture Bind Group"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Placed Picture Encoder"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Placed Picture Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: output,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // Outside the picture the layer is clear.
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
            draw_within(&mut pass, Some(&parts));
        }
        cmd_buffers.push(encoder.finish());
        parts.iter().map(Pixels::count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placed(pos: [f32; 2], scale: [f32; 2], crop: Option<Crop>) -> Placed {
        Placed {
            uv_scale: [1.0; 2],
            uv_offset: [0.0; 2],
            pos,
            scale,
            flip: [false; 2],
            crop,
            premultiplied: false,
            over_black: 0.0,
        }
    }

    /// Flipped, the part a crop keeps shows mirrored, and the bounds with it.
    #[test]
    fn a_flipped_crop_is_bounded_on_the_mirrored_side() {
        let crop = Crop {
            left: 0.1,
            right: 0.4,
            bottom: 0.0,
            top: 1.0,
            feather: 0.0,
            border: None,
            strength: 1.0,
        };
        let mut flipped = placed([0.0; 2], [1.0; 2], Some(crop));
        flipped.flip = [true, false];
        let b = flipped.bounds([0.0; 2]);
        let want = [0.6, 0.0, 0.9, 1.0];
        assert!(b.iter().zip(want).all(|(a, w)| (a - w).abs() < 1e-5), "{b:?}");
    }

    #[test]
    fn a_picture_is_bounded_by_its_moved_and_scaled_frame() {
        let b = placed([0.2, -0.1], [0.5, 0.5], None).bounds([0.001; 2]);
        let want = [0.45, 0.15, 0.95, 0.65];
        assert!(b.iter().zip(want).all(|(a, w)| (a - w).abs() < 1e-5), "{b:?}");
    }

    #[test]
    fn a_crop_cuts_the_bounds_down_with_room_for_its_edge() {
        let crop = Crop {
            left: 0.25,
            right: 0.75,
            bottom: 0.0,
            top: 0.5,
            feather: 0.0,
            border: None,
            strength: 1.0,
        };
        let b = placed([0.0; 2], [1.0; 2], Some(crop)).bounds([0.001; 2]);
        // Kept from the bottom up to half way: UV 0.5 to 1, running down.
        assert!((b[0] - 0.248).abs() < 1e-4 && (b[2] - 0.752).abs() < 1e-4, "{b:?}");
        assert!((b[1] - 0.498).abs() < 1e-4 && (b[3] - 1.0).abs() < 1e-4, "{b:?}");
    }
}
