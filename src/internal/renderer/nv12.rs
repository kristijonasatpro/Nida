//! A recording's or stream's frame turned to NV12 on the GPU, read back
//! asynchronously, for the encoder to take as it is.
//!
//! The encoders store 4:2:0 YUV. Sent RGBA, FFmpeg converted every frame on
//! the CPU — at 4K and 60 fps half a billion pixels a second, more than a
//! core, and a stream fell behind. Converted here, a 4K frame is 12 MB
//! instead of 33, and FFmpeg does nothing to it.

use std::sync::mpsc::{Receiver, TryRecvError};

use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Params {
    width: u32,
    height: u32,
    encode: u32,
    _padding: u32,
}

enum SlotState {
    Free,
    Copied,
    Mapping(Receiver<Result<(), wgpu::BufferAsyncError>>),
}

/// Bytes of an NV12 frame: the Y plane and the half-height U V plane.
pub const fn nv12_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 3 / 2
}

/// Whether a frame this size can be converted: whole 4×2 blocks.
pub const fn nv12_fits(width: u32, height: u32) -> bool {
    width > 0 && height > 0 && width.is_multiple_of(4) && height.is_multiple_of(2)
}

/// Converts one output's frames: one storage buffer on the GPU, read back
/// through two staging buffers so a copy and a read never meet.
pub struct Nv12Converter {
    width: u32,
    height: u32,
    pipeline: wgpu::ComputePipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    output: wgpu::Buffer,
    staging: [wgpu::Buffer; 2],
    slots: [SlotState; 2],
    /// When the frame in each staging buffer was drawn.
    made: [std::time::Instant; 2],
}

impl Nv12Converter {
    /// A converter for `width` × `height` frames; `None` unless they come in
    /// whole 4×2 blocks.
    pub fn new(device: &wgpu::Device, width: u32, height: u32) -> Option<Self> {
        if !nv12_fits(width, height) {
            return None;
        }
        let len = nv12_len(width, height) as u64;
        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("NV12 Bind Group Layout"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Storage { read_only: false },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("NV12 Pipeline Layout"),
            bind_group_layouts: &[Some(&bind_group_layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NV12 Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("nv12.wgsl").into()),
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("NV12 Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("NV12 Params"),
            contents: bytemuck::bytes_of(&Params {
                width,
                height,
                encode: 1,
                _padding: 0,
            }),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let output = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("NV12 GPU Output"),
            size: len,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let staging = std::array::from_fn(|index| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(if index == 0 {
                    "NV12 Readback A"
                } else {
                    "NV12 Readback B"
                }),
                size: len,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        Some(Self {
            width,
            height,
            pipeline,
            bind_group_layout,
            params,
            output,
            staging,
            slots: [SlotState::Free, SlotState::Free],
            made: [std::time::Instant::now(); 2],
        })
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Convert `source` (`width` × `height`), drawn at `made`, and queue its
    /// readback. A source in an sRGB format decodes as it is read, and is
    /// encoded again; one in a plain format already holds what the screen
    /// shows. False, and nothing done, when both staging buffers are still
    /// being read.
    pub fn encode(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        source: &wgpu::Texture,
        made: std::time::Instant,
    ) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .position(|state| matches!(state, SlotState::Free))
        else {
            return false;
        };
        queue.write_buffer(
            &self.params,
            0,
            bytemuck::bytes_of(&Params {
                width: self.width,
                height: self.height,
                encode: u32::from(source.format().is_srgb()),
                _padding: 0,
            }),
        );
        let view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("NV12 Bind Group"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: self.output.as_entire_binding(),
                },
            ],
        });
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("NV12 Convert"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind_group, &[]);
            pass.dispatch_workgroups(
                (self.width / 4).div_ceil(16),
                (self.height / 2).div_ceil(8),
                1,
            );
        }
        encoder.copy_buffer_to_buffer(
            &self.output,
            0,
            &self.staging[slot],
            0,
            nv12_len(self.width, self.height) as u64,
        );
        self.slots[slot] = SlotState::Copied;
        self.made[slot] = made;
        true
    }

    /// The converted frames whose readback has finished, with when each was
    /// drawn, in the order drawn — never waiting for the GPU. Both, when both
    /// finished together: keeping only the later left a gap in a recording.
    pub fn try_read(&mut self, device: &wgpu::Device) -> Vec<(std::time::Instant, Vec<u8>)> {
        let _ = device.poll(wgpu::PollType::Poll);
        let mut frames: Vec<(std::time::Instant, Vec<u8>)> = Vec::new();
        for index in 0..self.slots.len() {
            match std::mem::replace(&mut self.slots[index], SlotState::Free) {
                SlotState::Free => {}
                SlotState::Copied => {
                    let (sender, receiver) = std::sync::mpsc::channel();
                    self.staging[index]
                        .slice(..)
                        .map_async(wgpu::MapMode::Read, move |result| {
                            let _ = sender.send(result);
                        });
                    self.slots[index] = SlotState::Mapping(receiver);
                }
                SlotState::Mapping(receiver) => match receiver.try_recv() {
                    Ok(Ok(())) => {
                        if let Ok(range) = self.staging[index].slice(..).get_mapped_range() {
                            frames.push((self.made[index], range.to_vec()));
                        }
                        self.staging[index].unmap();
                    }
                    Err(TryRecvError::Empty) => {
                        self.slots[index] = SlotState::Mapping(receiver);
                    }
                    Ok(Err(error)) => {
                        log::warn!("NV12 readback failed: {error}");
                        self.staging[index].unmap();
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.staging[index].unmap();
                    }
                },
            }
        }
        frames.sort_by_key(|(made, _)| *made);
        frames
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 64;
    const H: u32 = 32;

    /// Left half smooth gradients, right half 16×16 blocks of saturated
    /// colours with hard edges between them, as RGBA bytes.
    fn test_picture() -> Vec<u8> {
        let blocks: [[u8; 3]; 6] = [
            [255, 0, 0],
            [0, 255, 0],
            [0, 0, 255],
            [255, 255, 255],
            [20, 20, 20],
            [240, 200, 30],
        ];
        let mut rgba = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                let px = if x < W / 2 {
                    [(x * 255 / (W / 2)) as u8, (y * 255 / H) as u8, 128]
                } else {
                    blocks[((x / 16) + (y / 16) * 2) as usize % blocks.len()]
                };
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
        rgba
    }

    /// The picture through ffmpeg's own converter, as the encoder was fed
    /// before: RGBA in, BT.709 video levels out. `None` without ffmpeg.
    fn ffmpeg_nv12(rgba: &[u8]) -> Option<Vec<u8>> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-f", "rawvideo"])
            .args(["-pix_fmt", "rgba", "-s", &format!("{W}x{H}"), "-i", "-"])
            .args(["-frames:v", "1", "-pix_fmt", "nv12"])
            .args(["-colorspace", "bt709", "-color_range", "tv"])
            .args(["-f", "rawvideo", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .ok()?;
        child.stdin.take()?.write_all(rgba).ok()?;
        let out = child.wait_with_output().ok()?;
        out.status.success().then_some(out.stdout)
    }

    fn gpu_nv12(rgba: &[u8]) -> Option<Vec<u8>> {
        let context = crate::renderer::context::GpuContext::new_headless().ok()?;
        let texture = context.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("NV12 test picture"),
            size: wgpu::Extent3d {
                width: W,
                height: H,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            // What a recording's frame is drawn into.
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        context.queue.write_texture(
            texture.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(W * 4),
                rows_per_image: Some(H),
            },
            texture.size(),
        );
        let mut converter = Nv12Converter::new(&context.device, W, H)?;
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let made = std::time::Instant::now();
        assert!(converter.encode(&context.device, &context.queue, &mut encoder, &texture, made));
        context.submit(std::iter::once(encoder.finish()));
        assert!(
            converter.try_read(&context.device).is_empty(),
            "asynchronous"
        );
        let _ = context.device.poll(wgpu::PollType::wait_indefinitely());
        let (at, frame) = converter.try_read(&context.device).pop()?;
        assert_eq!(at, made, "the frame keeps when it was drawn");
        Some(frame)
    }

    /// The same bytes as ffmpeg made from the same picture — the colours a
    /// recording had before, within a code of rounding: every pixel's
    /// brightness, and the colour of gradients and of flat areas. Near a
    /// hard edge between colours the two filter the colour differently:
    /// ffmpeg's bicubic reaches four pixels across it and rings — pure red
    /// comes out past the legal 240 — where this averages each 2×2.
    #[test]
    fn the_gpu_makes_what_ffmpeg_made() {
        let rgba = test_picture();
        let Some(expected) = ffmpeg_nv12(&rgba) else {
            eprintln!("ffmpeg not available — skipping");
            return;
        };
        let Some(got) = gpu_nv12(&rgba) else {
            eprintln!("no GPU — skipping");
            return;
        };
        assert_eq!(got.len(), nv12_len(W, H));
        assert_eq!(expected.len(), got.len());
        let (width, height) = (W as usize, H as usize);
        let worst_y = (0..width * height)
            .map(|i| got[i].abs_diff(expected[i]))
            .max()
            .unwrap();
        assert!(worst_y <= 1, "luma off by {worst_y}");
        // Chroma: in the gradients, and inside each block away from its edges.
        let mut worst_flat = 0;
        for cy in 0..height / 2 {
            for cx in 0..width / 2 {
                let left = cx * 2;
                let top = cy * 2;
                let in_gradient = left >= 2 && left + 2 < width / 2;
                let in_block = left >= width / 2
                    && (6..10).contains(&(left % 16))
                    && (6..10).contains(&(top % 16));
                if !(in_gradient || in_block) {
                    continue;
                }
                for plane in 0..2 {
                    let i = width * height + cy * width + cx * 2 + plane;
                    worst_flat = worst_flat.max(got[i].abs_diff(expected[i]));
                }
            }
        }
        assert!(worst_flat <= 1, "chroma off by {worst_flat}");
    }

    #[test]
    fn frames_in_whole_blocks_fit() {
        assert!(nv12_fits(3840, 2160));
        assert!(nv12_fits(1920, 1080));
        assert!(!nv12_fits(1918, 1080), "not whole 4-column blocks");
        assert!(!nv12_fits(1920, 1079));
        assert!(!nv12_fits(0, 1080));
        assert_eq!(nv12_len(1920, 1080), 1920 * 1080 * 3 / 2);
    }
}
