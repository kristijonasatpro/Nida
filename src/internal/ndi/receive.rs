//! The receive side's frame handoff and GPU conversion. The receive thread
//! copies what the SDK captured into a buffer reused across frames; the render
//! thread uploads it and, for UYVY, expands it to RGB on the GPU.

use std::sync::Mutex;

use super::ffi;

/// The color format a receiver asks the SDK for: UYVY from a source without
/// alpha, RGBA from one with it. Neither needs a conversion in the SDK.
pub(super) const RECEIVE_COLOR_FORMAT: i32 = 3; // NDIlib_recv_color_format_UYVY_RGBA

/// The deck texture a receiver renders into.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Pixel layouts a receiver accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ReceivedFormat {
    #[default]
    Uyvy,
    Rgba,
    Bgra,
}

impl ReceivedFormat {
    fn from_fourcc(fourcc: ffi::NDIlib_FourCC_video_type_e) -> Option<Self> {
        if fourcc == ffi::NDIlib_FourCC_video_type_e::UYVY {
            Some(Self::Uyvy)
        } else if fourcc == ffi::NDIlib_FourCC_video_type_e::RGBA {
            Some(Self::Rgba)
        } else if fourcc == ffi::NDIlib_FourCC_video_type_e::BGRA
            || fourcc == ffi::NDIlib_FourCC_video_type_e::BGRX
        {
            Some(Self::Bgra)
        } else {
            None
        }
    }

    /// Width in four-byte texels: UYVY packs each pixel pair into one.
    fn texels(self, width: u32) -> u32 {
        match self {
            Self::Uyvy => width.div_ceil(2),
            Self::Rgba | Self::Bgra => width,
        }
    }
}

/// One captured frame's bytes, rows `stride` bytes apart.
#[derive(Default)]
struct ReceivedFrame {
    format: ReceivedFormat,
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    stride: u32,
}

impl ReceivedFrame {
    /// Copy `vf` in, reusing this frame's buffer. False when the SDK handed
    /// over a layout the receiver did not ask for, or a malformed frame.
    fn fill(&mut self, vf: &ffi::NDIlib_video_frame_v2_t, cpu: bool) -> bool {
        let Some(format) = ReceivedFormat::from_fourcc(vf.FourCC) else {
            return false;
        };
        let (Ok(width), Ok(height)) = (u32::try_from(vf.xres), u32::try_from(vf.yres)) else {
            return false;
        };
        let Some(row) = format.texels(width).checked_mul(4) else {
            return false;
        };
        let stride = u32::try_from(vf.line_stride_in_bytes)
            .ok()
            .filter(|&s| s > 0)
            .unwrap_or(row);
        if vf.p_data.is_null()
            || width == 0
            || height == 0
            || vf.line_stride_in_bytes < 0
            || stride < row
            || stride % 4 != 0
        {
            return false;
        }
        let Some(len) = (stride as usize)
            .checked_mul(height as usize)
            .filter(|&n| isize::try_from(n).is_ok())
        else {
            return false;
        };
        // SAFETY: the SDK guarantees `p_data` holds `yres` rows of
        // `line_stride_in_bytes` until the frame is freed, which the caller
        // does only after this returns.
        let src = unsafe { std::slice::from_raw_parts(vf.p_data, len) };
        self.bytes.clear();
        self.bytes.extend_from_slice(src);
        if vf.FourCC == ffi::NDIlib_FourCC_video_type_e::BGRX {
            for row in self.bytes.chunks_exact_mut(stride as usize) {
                for pixel in row[..width as usize * 4].as_chunks_mut::<4>().0 {
                    pixel[3] = 255;
                }
            }
        }
        if cpu && format == ReceivedFormat::Uyvy {
            let mut rgba = vec![0; width as usize * height as usize * 4];
            super::uyvy_to_rgba(&self.bytes, &mut rgba, width, height, stride as usize);
            self.bytes = rgba;
            self.format = ReceivedFormat::Rgba;
            self.width = width;
            self.height = height;
            self.stride = width * 4;
            return true;
        }
        self.format = format;
        self.width = width;
        self.height = height;
        self.stride = stride;
        true
    }
}

#[derive(Default)]
struct Exchange {
    ready: ReceivedFrame,
    fresh: bool,
    spare: ReceivedFrame,
}

/// The latest captured frame, passed from the receive thread to the render
/// thread. Two buffers take turns, so once they have grown to the frame's size
/// neither side allocates.
#[derive(Default)]
pub(super) struct FrameSlot {
    exchange: Mutex<Exchange>,
    cpu: bool,
}

impl FrameSlot {
    pub(super) fn new(cpu: bool) -> Self {
        Self {
            exchange: Mutex::default(),
            cpu,
        }
    }

    pub(super) fn clear(&self) {
        if let Ok(mut exchange) = self.exchange.lock() {
            *exchange = Exchange::default();
        }
    }
    /// Receive thread: make `vf` the latest frame. False, storing nothing,
    /// when its layout is not one the receiver asked for.
    pub(super) fn deposit(&self, vf: &ffi::NDIlib_video_frame_v2_t) -> bool {
        let Ok(mut exchange) = self.exchange.lock() else {
            return false;
        };
        let mut frame = std::mem::take(&mut exchange.spare);
        drop(exchange);
        let stored = frame.fill(vf, self.cpu);
        let Ok(mut exchange) = self.exchange.lock() else {
            return false;
        };
        if stored {
            std::mem::swap(&mut exchange.ready, &mut frame);
            exchange.fresh = true;
        }
        exchange.spare = frame;
        stored
    }
}

/// The GPU pass that expands packed UYVY into a receiver's deck texture.
pub(super) struct UyvyUnpacker {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

impl UyvyUnpacker {
    pub(super) fn try_new(device: &wgpu::Device) -> Option<Self> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let pipeline = Self::new(device);
        if let Some(error) = pollster::block_on(scope.pop()) {
            log::warn!("NDI GPU conversion unavailable; using CPU conversion: {error}");
            None
        } else {
            Some(pipeline)
        }
    }
    pub(super) fn new(device: &wgpu::Device) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("NDI receive UYVY"),
            source: wgpu::ShaderSource::Wgsl(include_str!("receive.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("NDI receive UYVY"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("NDI receive UYVY"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("NDI receive UYVY"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("fullscreen"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("uyvy"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, layout }
    }
}

/// The half-width texture a UYVY frame is uploaded into before expansion.
struct PackedUpload {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
}

/// A receiver's deck texture, and what it takes to fill it.
pub(super) struct ReceiveTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    packed: Option<PackedUpload>,
    format: wgpu::TextureFormat,
}

impl ReceiveTarget {
    pub(super) fn new(device: &wgpu::Device, label: &str, width: u32, height: u32) -> Self {
        Self::with_format(device, label, width, height, TARGET_FORMAT)
    }

    fn with_format(
        device: &wgpu::Device,
        label: &str,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: extent(width, height),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::RENDER_ATTACHMENT,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self {
            texture,
            view,
            width,
            height,
            packed: None,
            format,
        }
    }

    pub(super) fn view(&self) -> &wgpu::TextureView {
        &self.view
    }

    pub(super) fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Render thread: put the slot's frame into the deck texture if a new one
    /// has arrived, resizing the texture to match it. A UYVY frame's expansion
    /// is recorded into `encoder`, created on first need, for the caller to
    /// submit.
    pub(super) fn update(
        &mut self,
        slot: &FrameSlot,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        unpacker: &mut Option<UyvyUnpacker>,
        encoder: &mut Option<wgpu::CommandEncoder>,
    ) -> bool {
        let Ok(mut exchange) = slot.exchange.try_lock() else {
            return false;
        };
        if !exchange.fresh {
            return false;
        }
        exchange.fresh = false;
        let frame = &exchange.ready;
        let format = if frame.format == ReceivedFormat::Bgra {
            wgpu::TextureFormat::Bgra8UnormSrgb
        } else {
            TARGET_FORMAT
        };
        if frame.width > device.limits().max_texture_dimension_2d
            || frame.height > device.limits().max_texture_dimension_2d
        {
            return false;
        }
        if (frame.width, frame.height) != (self.width, self.height) || self.format != format {
            log::info!(
                "NDI receiver: resolution changed {}×{} → {}×{}",
                self.width,
                self.height,
                frame.width,
                frame.height
            );
            *self = Self::with_format(
                device,
                "NDI Receive (resized)",
                frame.width,
                frame.height,
                format,
            );
        }
        match frame.format {
            ReceivedFormat::Rgba | ReceivedFormat::Bgra => {
                write(queue, &self.texture, frame, frame.width);
            }
            ReceivedFormat::Uyvy => {
                let unpacker = unpacker.get_or_insert_with(|| UyvyUnpacker::new(device));
                let texels = ReceivedFormat::Uyvy.texels(frame.width);
                let packed = self
                    .packed
                    .get_or_insert_with(|| packed_upload(device, unpacker, texels, frame.height));
                write(queue, &packed.texture, frame, texels);
                let encoder = encoder.get_or_insert_with(|| {
                    device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("NDI receive"),
                    })
                });
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("NDI receive UYVY"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &self.view,
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                pass.set_pipeline(&unpacker.pipeline);
                pass.set_bind_group(0, &packed.bind_group, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        true
    }
}

fn packed_upload(
    device: &wgpu::Device,
    unpacker: &UyvyUnpacker,
    texels: u32,
    height: u32,
) -> PackedUpload {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("NDI receive UYVY upload"),
        size: extent(texels, height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("NDI receive UYVY"),
        layout: &unpacker.layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::TextureView(&view),
        }],
    });
    PackedUpload {
        texture,
        bind_group,
    }
}

fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, frame: &ReceivedFrame, texels: u32) {
    queue.write_texture(
        texture.as_image_copy(),
        &frame.bytes,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(frame.stride),
            rows_per_image: Some(frame.height),
        },
        extent(texels, frame.height),
    );
}

fn extent(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::GpuContext;
    fn frame(
        fourcc: ffi::NDIlib_FourCC_video_type_e,
        bytes: &mut [u8],
        width: i32,
        height: i32,
        stride: i32,
    ) -> ffi::NDIlib_video_frame_v2_t {
        ffi::NDIlib_video_frame_v2_t {
            xres: width,
            yres: height,
            FourCC: fourcc,
            frame_rate_N: 60,
            frame_rate_D: 1,
            picture_aspect_ratio: 0.0,
            frame_format_type: 1,
            timecode: 0,
            p_data: bytes.as_mut_ptr(),
            line_stride_in_bytes: stride,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        }
    }

    fn read_rgba(
        context: &GpuContext,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> Vec<u8> {
        let padded = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = context.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("NDI receive readback"),
            size: u64::from(padded * height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            super::extent(width, height),
        );
        context.submit(std::iter::once(encoder.finish()));
        buffer.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        let _ = context.device.poll(wgpu::PollType::wait_indefinitely());
        let mapped = buffer
            .slice(..)
            .get_mapped_range()
            .expect("mapped readback");
        mapped
            .chunks(padded as usize)
            .flat_map(|row| row[..(width * 4) as usize].to_vec())
            .collect()
    }

    fn gpu() -> Option<GpuContext> {
        match GpuContext::new_headless() {
            Ok(gpu) => Some(gpu),
            Err(error) => {
                assert!(
                    std::env::var_os("VARDA_REQUIRE_GPU").is_none(),
                    "GPU required: {error}"
                );
                None
            }
        }
    }

    #[test]
    fn gpu_uyvy_matches_existing_cpu_conversion_for_sd_and_hd() {
        let Some(gpu) = gpu() else { return };
        for height in [2, 720] {
            let mut bytes = [90_u8, 100, 200, 140, 160, 90, 64, 220, 0, 0, 0, 0].repeat(height);
            let vf = frame(
                ffi::NDIlib_FourCC_video_type_e::UYVY,
                &mut bytes,
                4,
                height as i32,
                12,
            );
            let mut expected = vec![0; 4 * height * 4];
            super::super::uyvy_to_rgba(&bytes, &mut expected, 4, height as u32, 12);
            for cpu in [false, true] {
                let slot = FrameSlot::new(cpu);
                assert!(slot.deposit(&vf));
                let mut target = ReceiveTarget::new(&gpu.device, "test receiver", 2, 1);
                let mut unpacker = UyvyUnpacker::try_new(&gpu.device);
                assert!(unpacker.is_some());
                let mut encoder = None;
                assert!(target.update(&slot, &gpu.device, &gpu.queue, &mut unpacker, &mut encoder));
                if let Some(encoder) = encoder {
                    gpu.submit([encoder.finish()]);
                }
                let actual = read_rgba(&gpu, &target.texture, 4, height as u32);
                assert_eq!(actual.len(), expected.len());
                for (actual, expected) in actual.iter().zip(&expected) {
                    assert!(
                        actual.abs_diff(*expected) <= 1,
                        "height {height}, CPU fallback {cpu}: {actual} vs {expected}"
                    );
                }
                assert!(!target.update(&slot, &gpu.device, &gpu.queue, &mut unpacker, &mut None));
            }
        }
    }

    #[test]
    fn rgba_and_bgra_keep_padded_rows_alpha_and_format_changes() {
        let Some(gpu) = gpu() else { return };
        let slot = FrameSlot::default();
        let mut target = ReceiveTarget::new(&gpu.device, "test receiver", 2, 1);
        let mut unpacker = None;
        for fourcc in [
            ffi::NDIlib_FourCC_video_type_e::RGBA,
            ffi::NDIlib_FourCC_video_type_e::BGRA,
            ffi::NDIlib_FourCC_video_type_e::BGRX,
        ] {
            let mut bytes = vec![12, 34, 56, 78, 90, 123, 210, 99, 44, 55, 66, 77, 0, 0, 0, 0];
            let vf = frame(fourcc, &mut bytes, 3, 1, 16);
            assert!(slot.deposit(&vf));
            assert!(target.update(&slot, &gpu.device, &gpu.queue, &mut unpacker, &mut None));
            let actual = read_rgba(&gpu, &target.texture, 3, 1);
            let mut expected = bytes[..12].to_vec();
            if fourcc == ffi::NDIlib_FourCC_video_type_e::BGRX {
                for p in expected.chunks_exact_mut(4) {
                    p[3] = 255;
                }
            }
            assert_eq!(actual, expected);
            assert_eq!(target.dimensions(), (3, 1));
        }
    }

    #[test]
    fn malformed_frames_do_not_replace_the_latest_picture() {
        let slot = FrameSlot::default();
        let mut bytes = vec![0; 16];
        let mut vf = frame(ffi::NDIlib_FourCC_video_type_e::RGBA, &mut bytes, 2, 1, 8);
        assert!(slot.deposit(&vf));
        vf.line_stride_in_bytes = -8;
        assert!(!slot.deposit(&vf));
        vf.line_stride_in_bytes = 4;
        assert!(!slot.deposit(&vf));
        vf.line_stride_in_bytes = 8;
        vf.xres = -1;
        assert!(!slot.deposit(&vf));
        vf.xres = 2;
        vf.p_data = std::ptr::null_mut();
        assert!(!slot.deposit(&vf));
        assert!(slot.exchange.lock().unwrap().fresh);
        slot.clear();
        assert!(!slot.exchange.lock().unwrap().fresh);
    }
}
