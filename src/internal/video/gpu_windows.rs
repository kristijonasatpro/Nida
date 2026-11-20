//! GPU-resident file frames. All native objects belong to the renderer's device.
//!
//! The decoder worker copies into an owned, bounded plane pool, waits for that
//! short copy off the engine thread, then hands read-only planes to wgpu. A
//! slot is released by the *consuming command buffer's* completion callback.
//! Dropping an unconsumed mailbox frame releases its slot immediately.
use super::gpu_colour::Params;
use anyhow::{bail, ensure, Context, Result};
use ffmpeg_next as ffmpeg;
use std::{
    mem::ManuallyDrop,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};
use wgpu::util::DeviceExt;
use windows::{
    core::Interface,
    Win32::Graphics::{Direct3D12 as dx12, Dxgi::Common as dxgi},
};

#[cfg(test)]
mod qualification;
#[cfg(test)]
mod tests;

struct Slot {
    planes: [dx12::ID3D12Resource; 2],
    width: u32,
    height: u32,
    ten_bit: bool,
}

struct Converter {
    device: wgpu::Device,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
}

/// One decoder-owned event, moved with an outstanding native submission.
/// A timed-out submission must retain the registered handle until retirement.
struct CopyCompletion(OwnedHandle);

impl CopyCompletion {
    fn new() -> Option<Self> {
        // SAFETY: unnamed, auto-reset, non-inherited event; uniquely owned below.
        let raw = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
            )
        };
        (!raw.is_null()).then(|| Self(unsafe { OwnedHandle::from_raw_handle(raw) }))
    }

    fn arm(&self, fence: &dx12::ID3D12Fence, serial: u64) -> bool {
        let raw = self.0.as_raw_handle();
        // SAFETY: the prior submission completed before this event was returned
        // to the decoder. The current submission retains it until completion.
        unsafe {
            windows_sys::Win32::System::Threading::ResetEvent(raw) != 0
                && fence
                    .SetEventOnCompletion(serial, windows::Win32::Foundation::HANDLE(raw))
                    .is_ok()
        }
    }

    fn wait(&self, left: Duration) -> bool {
        // Timeout is a safety bound, not a polling interval. Fence notification
        // wakes immediately; the caller always rechecks the fence and deadline.
        let millis = u32::try_from(left.as_nanos().div_ceil(1_000_000))
            .unwrap_or(u32::MAX - 1)
            .min(u32::MAX - 1);
        // SAFETY: the submission owns this live event throughout the wait.
        let result = unsafe {
            windows_sys::Win32::System::Threading::WaitForSingleObject(
                self.0.as_raw_handle(),
                millis,
            )
        };
        result == windows_sys::Win32::Foundation::WAIT_OBJECT_0
            || result == windows_sys::Win32::Foundation::WAIT_TIMEOUT
    }
}

/// Native submissions must retain their allocator and decoder frame even when
/// a driver error prevents normal completion. This holder is retired off-thread.
struct InFlightCopy {
    allocator: dx12::ID3D12CommandAllocator,
    list: dx12::ID3D12GraphicsCommandList,
    lease: super::gpu_pool::Lease<Slot>,
    _frame: ffmpeg::util::frame::video::Video,
    _queue: dx12::ID3D12CommandQueue,
    device: dx12::ID3D12Device,
    fence: dx12::ID3D12Fence,
    serial: u64,
    completion_event: Option<CopyCompletion>,
}
impl InFlightCopy {
    fn retire(self) {
        std::thread::spawn(move || {
            while unsafe { self.fence.GetCompletedValue() } < self.serial
                && unsafe { self.device.GetDeviceRemovedReason() }.is_ok()
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            drop(self);
        });
    }
}

/// A GPU payload owns its pool slot; it contains no uncompressed CPU pixels.
pub struct GpuFrame {
    lease: super::gpu_pool::Lease<Slot>,
    converter: Arc<Converter>,
    params: Params,
    counters: Arc<super::delivery::DeliveryCounters>,
    encoded: bool,
    pub timestamp: f64,
}

impl Drop for GpuFrame {
    fn drop(&mut self) {
        super::pacing_trace::emit("payload_drop", self.timestamp, f64::from(self.encoded));
        if !self.encoded {
            self.counters.dropped_frames.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl GpuFrame {
    /// Consume the frame and release it only after this encoder completes.
    pub fn encode(mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::Texture) {
        super::pacing_trace::emit("encode", self.timestamp, 0.0);
        self.encoded = true;
        self.counters.gpu_frames.fetch_add(1, Ordering::Relaxed);
        log::trace!("Encoding GPU video picture at {:.6}s", self.timestamp);
        let slot = self.lease.resource();
        let mut textures = Vec::with_capacity(2);
        for plane in 0..2 {
            let (width, height) = if plane == 0 {
                (slot.width, slot.height)
            } else {
                (slot.width.div_ceil(2), slot.height.div_ceil(2))
            };
            let format = plane_format(slot.ten_bit, plane);
            let size = wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            };
            // SAFETY: same device, owned resource, copy completed on worker.
            // This wrapper exclusively owns wgpu's tracking of this lease.
            // The resource is COPY_DEST after the copy and is never written
            // natively while the wrapper/consuming submission is alive.
            let hal = unsafe {
                wgpu::hal::dx12::Device::texture_from_raw(
                    slot.planes[plane].clone(),
                    format,
                    wgpu::TextureDimension::D2,
                    size,
                    1,
                    1,
                )
            };
            let texture = unsafe {
                self.converter
                    .device
                    .create_texture_from_hal::<wgpu::hal::api::Dx12>(
                        hal,
                        &wgpu::TextureDescriptor {
                            label: Some("GPU video plane"),
                            size,
                            mip_level_count: 1,
                            sample_count: 1,
                            dimension: wgpu::TextureDimension::D2,
                            format,
                            usage: wgpu::TextureUsages::TEXTURE_BINDING
                                | wgpu::TextureUsages::COPY_DST,
                            view_formats: &[],
                        },
                        wgpu::TextureUses::COPY_DST,
                    )
            };
            textures.push(texture);
        }
        let views: Vec<_> = textures
            .iter()
            .map(|t| t.create_view(&wgpu::TextureViewDescriptor::default()))
            .collect();
        let uniform = self
            .converter
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("GPU video colour parameters"),
                contents: bytemuck::bytes_of(&self.params),
                usage: wgpu::BufferUsages::UNIFORM,
            });
        let bind = self
            .converter
            .device
            .create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("GPU video planes"),
                layout: &self.converter.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&views[0]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&views[1]),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: uniform.as_entire_binding(),
                    },
                ],
            });
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("GPU video colour conversion"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.converter.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        // Restore native ownership in a known state, using wgpu's tracker.
        encoder.transition_resources(
            std::iter::empty(),
            textures.iter().map(|texture| wgpu::TextureTransition {
                texture,
                selector: None,
                state: wgpu::TextureUses::COPY_DST,
            }),
        );
        encoder.on_submitted_work_done(move || {
            super::pacing_trace::emit("gpu_complete", self.timestamp, 0.0);
            drop(textures);
            drop(self);
        });
    }
}

pub(crate) struct GpuDecoder {
    device: dx12::ID3D12Device,
    queue: dx12::ID3D12CommandQueue,
    fence: dx12::ID3D12Fence,
    serial: u64,
    completion_event: Option<CopyCompletion>,
    commands: Option<(
        dx12::ID3D12CommandAllocator,
        dx12::ID3D12GraphicsCommandList,
    )>,
    pool: super::gpu_pool::Pool<Slot>,
    converter: Arc<Converter>,
    gate: crate::renderer::submit_gate::SubmitGate,
    counters: Arc<super::delivery::DeliveryCounters>,
}

impl GpuDecoder {
    pub fn new(
        context: &crate::renderer::GpuContext,
        counters: Arc<super::delivery::DeliveryCounters>,
    ) -> Result<Self> {
        // SAFETY: only clone the COM device, never retain the HAL guard.
        let device = unsafe { context.device.as_hal::<wgpu::hal::api::Dx12>() }
            .context("GPU-resident video needs the DX12 renderer")?
            .raw_device()
            .clone();
        let queue = unsafe {
            device.CreateCommandQueue(&dx12::D3D12_COMMAND_QUEUE_DESC {
                Type: dx12::D3D12_COMMAND_LIST_TYPE_DIRECT,
                ..Default::default()
            })
        }?;
        let fence = unsafe { device.CreateFence(0, dx12::D3D12_FENCE_FLAG_NONE) }?;
        let completion_event = CopyCompletion::new();
        #[cfg(test)]
        let completion_event =
            if std::env::var("NIDA_VIDEO_BENCH_COPY_POLL").is_ok_and(|v| v == "1") {
                None
            } else {
                completion_event
            };
        let shader_source: std::borrow::Cow<'_, str> = include_str!("gpu_video.wgsl").into();
        // Hardware qualification can iterate on the production WGSL without
        // relinking the complete application's test binary. Never a release-app override.
        #[cfg(test)]
        let shader_source = if let Some(path) = std::env::var_os("NIDA_TEST_GPU_VIDEO_SHADER") {
            std::borrow::Cow::Owned(std::fs::read_to_string(path)?)
        } else {
            shader_source
        };
        let shader = context
            .device
            .create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("GPU video conversion"),
                source: wgpu::ShaderSource::Wgsl(shader_source),
            });
        let layout = context
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("GPU video conversion"),
                entries: &[
                    plane_binding(0),
                    plane_binding(1),
                    wgpu::BindGroupLayoutEntry {
                        binding: 2,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: false,
                            min_binding_size: wgpu::BufferSize::new(
                                std::mem::size_of::<Params>() as u64
                            ),
                        },
                        count: None,
                    },
                ],
            });
        let pipeline_layout =
            context
                .device
                .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("GPU video conversion"),
                    bind_group_layouts: &[Some(&layout)],
                    immediate_size: 0,
                });
        let pipeline = context
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("GPU video conversion"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vertex"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fragment"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8UnormSrgb,
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
        Ok(Self {
            device,
            queue,
            fence,
            serial: 0,
            completion_event,
            commands: None,
            pool: super::gpu_pool::Pool::new(counters.clone()),
            counters,
            converter: Arc::new(Converter {
                device: context.device.clone(),
                pipeline,
                layout,
            }),
            gate: context.submit_gate.clone(),
        })
    }

    /// Attach FFmpeg to the exact renderer device, including on hybrid laptops.
    pub fn open(&self, parameters: ffmpeg::codec::Parameters) -> Result<ffmpeg::decoder::Video> {
        let mut context = ffmpeg::codec::context::Context::from_parameters(parameters)?;
        let codec = if context.id() == ffmpeg::codec::Id::AV1 {
            ffmpeg::decoder::find_by_name("av1")
        } else {
            ffmpeg::decoder::find(context.id())
        }
        .context("No video decoder")?;
        unsafe {
            let mut index = 0;
            loop {
                let config = ffmpeg::ffi::avcodec_get_hw_config(codec.as_ptr(), index);
                ensure!(!config.is_null(), "Codec has no D3D12VA configuration");
                if (*config).device_type == ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA
                    && (*config).methods
                        & ffmpeg::ffi::AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32
                        != 0
                {
                    break;
                }
                index += 1;
            }
            let mut reference = ffmpeg::ffi::av_hwdevice_ctx_alloc(
                ffmpeg::ffi::AVHWDeviceType::AV_HWDEVICE_TYPE_D3D12VA,
            );
            ensure!(!reference.is_null(), "Cannot allocate D3D12VA context");
            #[allow(
                clippy::cast_ptr_alignment,
                reason = "av_hwdevice_ctx_alloc returns aligned AVHWDeviceContext storage"
            )]
            let hw = &mut *((*reference).data.cast::<ffmpeg::ffi::AVHWDeviceContext>());
            let dx = &mut *hw.hwctx.cast::<ffmpeg::ffi::AVD3D12VADeviceContext>();
            // FFmpeg owns this additional COM reference and releases it.
            dx.device = self.device.clone().into_raw().cast();
            let status = ffmpeg::ffi::av_hwdevice_ctx_init(reference);
            if status < 0 {
                ffmpeg::ffi::av_buffer_unref(&raw mut reference);
                bail!("D3D12VA initialization failed ({status})");
            }
            (*context.as_mut_ptr()).hw_device_ctx = reference;
            (*context.as_mut_ptr()).get_format = Some(prefer_d3d12);
        }
        Ok(context.decoder().open_as(codec)?.video()?)
    }

    /// None means backpressure: preserve the displayed frame, with no CPU work.
    pub fn copy(
        &mut self,
        frame: &ffmpeg::util::frame::video::Video,
        output: (u32, u32),
        turns: u8,
        timestamp: f64,
    ) -> Result<Option<GpuFrame>> {
        super::pacing_trace::emit("copy_begin", timestamp, 0.0);
        ensure!(
            frame.format() == ffmpeg::format::Pixel::D3D12,
            "Decoder did not produce a D3D12 frame"
        );
        ensure!(
            !frame.is_interlaced(),
            "Interlaced video uses the CPU field processor"
        );
        let raw = unsafe { &*frame.as_ptr() };
        ensure!(
            raw.crop_left.saturating_add(raw.crop_right) < frame.width() as usize
                && raw.crop_top.saturating_add(raw.crop_bottom) < frame.height() as usize,
            "Invalid frame cropping"
        );
        ensure!(
            !raw.hw_frames_ctx.is_null() && !raw.data[0].is_null(),
            "Missing native frame"
        );
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "FFmpeg's hw_frames_ctx contains aligned AVHWFramesContext storage"
        )]
        let frames = unsafe {
            &*((*raw.hw_frames_ctx)
                .data
                .cast::<ffmpeg::ffi::AVHWFramesContext>())
        };
        let ten_bit = match frames.sw_format {
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NV12 => false,
            ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_P010LE => true,
            other => bail!("Unsupported GPU video format {other:?}"),
        };
        #[allow(
            clippy::cast_ptr_alignment,
            reason = "D3D12 AVFrames store an aligned AVD3D12VAFrame descriptor in data[0]"
        )]
        let native = unsafe { &*raw.data[0].cast::<ffmpeg::ffi::AVD3D12VAFrame>() };
        ensure!(
            !native.texture.is_null() && !native.sync_ctx.fence.is_null(),
            "Missing decoder texture/fence"
        );
        let source_ptr = native.texture.cast();
        let ready_ptr = native.sync_ctx.fence.cast();
        let source = unsafe { dx12::ID3D12Resource::from_raw_borrowed(&source_ptr) }
            .context("Missing decoder resource")?;
        let ready = unsafe { dx12::ID3D12Fence::from_raw_borrowed(&ready_ptr) }
            .context("Missing decoder fence")?;
        let desc = unsafe { source.GetDesc() };
        let width = u32::try_from(desc.Width)?;
        let height = desc.Height;
        ensure!(
            desc.Format
                == if ten_bit {
                    dxgi::DXGI_FORMAT_P010
                } else {
                    dxgi::DXGI_FORMAT_NV12
                },
            "Unexpected native surface format"
        );
        ensure!(
            native.subresource_index >= 0
                && (native.subresource_index as u32) < u32::from(desc.DepthOrArraySize),
            "Invalid decoder array slice"
        );
        let device = &self.device;
        let Some(lease) = self.pool.acquire((width, height, ten_bit), || {
            Ok(Slot {
                planes: [
                    Self::make_plane(device, width, height, ten_bit, 0)?,
                    Self::make_plane(device, width.div_ceil(2), height.div_ceil(2), ten_bit, 1)?,
                ],
                width,
                height,
                ten_bit,
            })
        })?
        else {
            super::pacing_trace::emit("pool_full", timestamp, 0.0);
            self.counters
                .backpressure_events
                .fetch_add(1, Ordering::Relaxed);
            return Ok(None);
        };
        // Match FFmpeg's hwcontext_d3d12va.c transfer contract: wait on the
        // frame fence, copy only its planes, restore COMMON, then complete the
        // copy before allowing the decoder worker to continue. No private ABI.
        let (allocator, list) = if let Some((allocator, list)) = self.commands.take() {
            // The preceding copy fence completed before these were returned.
            unsafe {
                allocator.Reset()?;
                list.Reset(&allocator, None)?;
            }
            (allocator, list)
        } else {
            let allocator: dx12::ID3D12CommandAllocator = unsafe {
                self.device
                    .CreateCommandAllocator(dx12::D3D12_COMMAND_LIST_TYPE_DIRECT)
            }?;
            let list: dx12::ID3D12GraphicsCommandList = unsafe {
                self.device.CreateCommandList(
                    0,
                    dx12::D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None,
                )
            }?;
            (allocator, list)
        };
        let slices = u32::from(desc.DepthOrArraySize);
        let mip_levels = u32::from(desc.MipLevels);
        for plane in 0..2 {
            let index =
                native.subresource_index as u32 * mip_levels + plane as u32 * mip_levels * slices;
            let before = barrier(
                source,
                index,
                dx12::D3D12_RESOURCE_STATE_COMMON,
                dx12::D3D12_RESOURCE_STATE_COPY_SOURCE,
            );
            unsafe {
                list.ResourceBarrier(std::slice::from_ref(&before));
            }
            let src = location(source, index);
            let dst = location(&lease.resource().planes[plane], 0);
            unsafe {
                list.CopyTextureRegion(&raw const dst, 0, 0, 0, &raw const src, None);
            }
            drop_location(src);
            drop_location(dst);
            let after = barrier(
                source,
                index,
                dx12::D3D12_RESOURCE_STATE_COPY_SOURCE,
                dx12::D3D12_RESOURCE_STATE_COMMON,
            );
            unsafe {
                list.ResourceBarrier(std::slice::from_ref(&after));
            }
            drop_barrier(before);
            drop_barrier(after);
        }
        unsafe {
            list.Close()?;
        }
        self.serial += 1;
        let serial = self.serial;
        let copy = InFlightCopy {
            allocator,
            list,
            lease,
            _frame: frame.clone(),
            _queue: self.queue.clone(),
            device: self.device.clone(),
            fence: self.fence.clone(),
            serial,
            completion_event: self.completion_event.take(),
        };
        // Configure must not overlap a native submission either. This queue is
        // private to the worker, so no other writer can interleave its commands.
        let mut submitted = false;
        let trace_fences = super::pacing_trace::enabled();
        let mut decode_ready = false;
        if trace_fences {
            decode_ready = unsafe { ready.GetCompletedValue() } >= native.sync_ctx.fence_value;
            super::pacing_trace::emit(
                "decoder_fence_at_submit",
                timestamp,
                f64::from(decode_ready),
            );
        }
        super::pacing_trace::emit("native_submit", timestamp, serial as f64);
        let status = self.gate.native_submit(|| unsafe {
            self.queue.Wait(ready, native.sync_ctx.fence_value)?;
            self.queue.ExecuteCommandLists(&[Some(copy.list.cast()?)]);
            submitted = true;
            self.queue.Signal(&self.fence, serial)
        });
        if let Err(error) = status {
            if submitted {
                copy.retire();
            }
            return Err(error.into());
        }
        let started = Instant::now();
        let mut polls = 0_u32;
        let mut event_wait = copy
            .completion_event
            .as_ref()
            .filter(|event| event.arm(&self.fence, serial));
        if event_wait.is_none() {
            super::pacing_trace::emit("copy_wait_fallback", timestamp, 0.0);
        }
        loop {
            if trace_fences
                && !decode_ready
                && unsafe { ready.GetCompletedValue() } >= native.sync_ctx.fence_value
            {
                decode_ready = true;
                super::pacing_trace::emit("decoder_fence_observed", timestamp, 0.0);
            }
            let completed = unsafe { self.fence.GetCompletedValue() };
            if completed >= serial && completed != u64::MAX {
                break;
            }
            if completed == u64::MAX || started.elapsed() >= Duration::from_secs(2) {
                // Keep every native dependency alive if the driver has not
                // finished. Reap off-thread; no engine/UI wait or unsafe reuse.
                copy.retire();
                bail!("GPU video copy timed out or the device was removed");
            }
            if let Some(event) = event_wait {
                let waiting_at = Instant::now();
                let succeeded =
                    event.wait(Duration::from_secs(2).saturating_sub(started.elapsed()));
                super::pacing_trace::emit(
                    "copy_event_wait",
                    timestamp,
                    waiting_at.elapsed().as_secs_f64(),
                );
                if !succeeded {
                    // Keep the registered event in copy even on wait failure.
                    // Safe legacy polling remains available for OS/API failures.
                    event_wait = None;
                    super::pacing_trace::emit("copy_wait_fallback", timestamp, 1.0);
                }
                continue;
            }
            let sleep_started = Instant::now();
            std::thread::sleep(Duration::from_millis(1));
            super::pacing_trace::emit(
                "copy_sleep",
                timestamp,
                sleep_started.elapsed().as_secs_f64(),
            );
            polls += 1;
        }
        super::pacing_trace::emit("copy_complete", timestamp, f64::from(polls));
        self.counters.gpu_copies.fetch_add(2, Ordering::Relaxed);
        self.counters.copy_wait_ns.fetch_add(
            started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64,
            Ordering::Relaxed,
        );
        self.commands = Some((copy.allocator, copy.list));
        self.completion_event = copy.completion_event;
        Ok(Some(GpuFrame {
            lease: copy.lease,
            converter: self.converter.clone(),
            params: Params::new(frame, ten_bit, output, turns),
            timestamp,
            counters: self.counters.clone(),
            encoded: false,
        }))
    }

    fn make_plane(
        device: &dx12::ID3D12Device,
        width: u32,
        height: u32,
        ten_bit: bool,
        plane: usize,
    ) -> Result<dx12::ID3D12Resource> {
        let format = match (ten_bit, plane) {
            (false, 0) => dxgi::DXGI_FORMAT_R8_UINT,
            (false, _) => dxgi::DXGI_FORMAT_R8G8_UINT,
            (true, 0) => dxgi::DXGI_FORMAT_R16_UINT,
            (true, _) => dxgi::DXGI_FORMAT_R16G16_UINT,
        };
        let mut resource = None;
        unsafe {
            device.CreateCommittedResource(
                &dx12::D3D12_HEAP_PROPERTIES {
                    Type: dx12::D3D12_HEAP_TYPE_DEFAULT,
                    ..Default::default()
                },
                dx12::D3D12_HEAP_FLAG_NONE,
                &dx12::D3D12_RESOURCE_DESC {
                    Dimension: dx12::D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                    Width: u64::from(width),
                    Height: height,
                    DepthOrArraySize: 1,
                    MipLevels: 1,
                    Format: format,
                    SampleDesc: dxgi::DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Layout: dx12::D3D12_TEXTURE_LAYOUT_UNKNOWN,
                    ..Default::default()
                },
                dx12::D3D12_RESOURCE_STATE_COPY_DEST,
                None,
                &raw mut resource,
            )
        }?;
        resource.context("Missing GPU plane")
    }
}

fn plane_format(ten: bool, plane: usize) -> wgpu::TextureFormat {
    match (ten, plane) {
        (false, 0) => wgpu::TextureFormat::R8Uint,
        (false, _) => wgpu::TextureFormat::Rg8Uint,
        (true, 0) => wgpu::TextureFormat::R16Uint,
        (true, _) => wgpu::TextureFormat::Rg16Uint,
    }
}
fn plane_binding(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::FRAGMENT,
        ty: wgpu::BindingType::Texture {
            sample_type: wgpu::TextureSampleType::Uint,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}
unsafe extern "C" fn prefer_d3d12(
    context: *mut ffmpeg::ffi::AVCodecContext,
    formats: *const ffmpeg::ffi::AVPixelFormat,
) -> ffmpeg::ffi::AVPixelFormat {
    unsafe {
        let mut next = formats;
        while *next != ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            if *next == ffmpeg::ffi::AVPixelFormat::AV_PIX_FMT_D3D12 {
                return *next;
            }
            next = next.add(1);
        }
        ffmpeg::ffi::avcodec_default_get_format(context, formats)
    }
}
fn barrier(
    resource: &dx12::ID3D12Resource,
    subresource: u32,
    before: dx12::D3D12_RESOURCE_STATES,
    after: dx12::D3D12_RESOURCE_STATES,
) -> dx12::D3D12_RESOURCE_BARRIER {
    dx12::D3D12_RESOURCE_BARRIER {
        Type: dx12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
        Flags: dx12::D3D12_RESOURCE_BARRIER_FLAG_NONE,
        Anonymous: dx12::D3D12_RESOURCE_BARRIER_0 {
            Transition: ManuallyDrop::new(dx12::D3D12_RESOURCE_TRANSITION_BARRIER {
                pResource: ManuallyDrop::new(Some(resource.clone())),
                Subresource: subresource,
                StateBefore: before,
                StateAfter: after,
            }),
        },
    }
}
fn drop_barrier(mut value: dx12::D3D12_RESOURCE_BARRIER) {
    unsafe {
        ManuallyDrop::drop(&mut (*value.Anonymous.Transition).pResource);
    }
}
fn location(resource: &dx12::ID3D12Resource, index: u32) -> dx12::D3D12_TEXTURE_COPY_LOCATION {
    dx12::D3D12_TEXTURE_COPY_LOCATION {
        pResource: ManuallyDrop::new(Some(resource.clone())),
        Type: dx12::D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
        Anonymous: dx12::D3D12_TEXTURE_COPY_LOCATION_0 {
            SubresourceIndex: index,
        },
    }
}
fn drop_location(mut value: dx12::D3D12_TEXTURE_COPY_LOCATION) {
    unsafe {
        ManuallyDrop::drop(&mut value.pResource);
    }
}
