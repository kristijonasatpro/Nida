//! Submits kept clear of a surface's configure.
//!
//! wgpu waits for the GPU to finish before it configures a surface, then
//! refuses the configure if the queue holds work again — "Failed to wait for
//! GPU to come idle before reconfiguring the Surface" — which can only be
//! another thread submitting meanwhile (wgpu-core `Device::configure_surface`).
//! The surface keeps its old size. The engine submits from a thread of its own
//! every few milliseconds, so a control window resized while it ran kept its
//! old size, and egui, laid out for the new one, drew beyond it: a scissor rect
//! outside the render target, in the encoder and again at the submit — three
//! GPU errors at every step of a resize.
//!
//! Every submit holds a [`SubmitGate`] shared and a configure holds it alone:
//! the configure waits for the submits under way, wgpu waits for the GPU, and
//! no submit starts until the surface has its new size. The submits wait that
//! long — the GPU's work in flight, a frame or two — on a resize, a move to
//! another screen or a change of present mode only.

use std::sync::{Arc, PoisonError, RwLock};

/// Shared by every clone of a `GpuContext`, as its queue is.
#[derive(Clone, Default)]
pub struct SubmitGate {
    gate: Arc<RwLock<()>>,
}

impl SubmitGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Submit `command_buffers` to `queue` — while a surface is being
    /// configured, once it has been.
    #[allow(clippy::disallowed_methods)] // the submit every other one goes through
    pub fn submit(
        &self,
        queue: &wgpu::Queue,
        command_buffers: impl IntoIterator<Item = wgpu::CommandBuffer>,
    ) -> wgpu::SubmissionIndex {
        // Collected first, so an iterator that finishes encoders does so
        // without holding up a configure.
        let command_buffers: Vec<wgpu::CommandBuffer> = command_buffers.into_iter().collect();
        let _submitting = self.gate.read().unwrap_or_else(PoisonError::into_inner);
        queue.submit(command_buffers)
    }

    /// Configure `surface` with no submit from any thread meanwhile.
    #[allow(clippy::disallowed_methods)] // the configure every other one goes through
    pub fn configure(
        &self,
        device: &wgpu::Device,
        surface: &wgpu::Surface<'_>,
        config: &wgpu::SurfaceConfiguration,
    ) {
        let _alone = self.gate.write().unwrap_or_else(PoisonError::into_inner);
        surface.configure(device, config);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use crate::renderer::context::GpuContext;
    use objc2::runtime::AnyObject;
    use objc2::{Encode, Encoding};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[repr(C)]
    struct CgSize {
        width: f64,
        height: f64,
    }

    // SAFETY: laid out as CoreGraphics' `CGSize`.
    unsafe impl Encode for CgSize {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }

    /// A surface on a bare `CAMetalLayer` — a window's surface, without the
    /// window — and the layer.
    fn layer_surface(gpu: &GpuContext) -> (wgpu::Surface<'static>, *mut AnyObject) {
        // SAFETY: a new layer, never released, so it outlives the surface.
        unsafe {
            let layer: *mut AnyObject = objc2::msg_send![objc2::class!(CAMetalLayer), new];
            let surface = gpu
                .instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CoreAnimationLayer(layer.cast()))
                .expect("a surface on a CAMetalLayer");
            (surface, layer)
        }
    }

    /// The width of the pictures the layer hands out. wgpu's own surface
    /// texture reports the width asked for even when it refused the
    /// configure, so the layer is what tells.
    fn drawable_width(layer: *mut AnyObject) -> u32 {
        // SAFETY: `layer` is a live CAMetalLayer (see `layer_surface`).
        let size: CgSize = unsafe { objc2::msg_send![layer, drawableSize] };
        size.width as u32
    }

    /// The engine's frames: a few passes over a 4K target, two frames in
    /// flight, until `drawing` goes false.
    fn draw_like_the_engine(gpu: &GpuContext, drawing: &AtomicBool) {
        let target = gpu.create_compositing_texture(3840, 2160);
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut in_flight = VecDeque::new();
        while drawing.load(Ordering::Relaxed) {
            let mut encoder = gpu
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
            for _ in 0..4 {
                encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: None,
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: &view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                            store: wgpu::StoreOp::Store,
                        },
                        depth_slice: None,
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
            }
            in_flight.push_back(gpu.submit([encoder.finish()]));
            if in_flight.len() > 2 {
                let _ = gpu.device.poll(wgpu::PollType::Wait {
                    submission_index: in_flight.pop_front(),
                    timeout: None,
                });
            }
        }
    }

    /// A window resized while the engine draws takes each new size, with no
    /// GPU error: the engine submits from a thread of its own, and wgpu
    /// refuses a configure that meets a submit from another thread.
    #[test]
    fn a_surface_takes_its_new_size_while_another_thread_keeps_submitting() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let (surface, layer) = layer_surface(&gpu);
        let caps = surface.get_capabilities(&gpu.adapter);
        let mut config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: caps.formats[0],
            width: 1280,
            height: 720,
            present_mode: wgpu::PresentMode::Fifo,
            alpha_mode: caps.alpha_modes[0],
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        gpu.configure_surface(&surface, &config);
        assert_eq!(drawable_width(layer), 1280);

        let drawing = Arc::new(AtomicBool::new(true));
        let engine = std::thread::spawn({
            let gpu = gpu.clone();
            let drawing = Arc::clone(&drawing);
            move || draw_like_the_engine(&gpu, &drawing)
        });
        std::thread::sleep(std::time::Duration::from_millis(50));

        let faults = gpu.errors.fault_count();
        let mut kept = Vec::new();
        for step in 1..=30 {
            config.width = 1280 + step * 16;
            gpu.configure_surface(&surface, &config);
            let width = drawable_width(layer);
            if width != config.width {
                kept.push((config.width, width));
            }
        }
        drawing.store(false, Ordering::Relaxed);
        engine.join().expect("the drawing thread");

        assert!(kept.is_empty(), "widths asked for, and kept: {kept:?}");
        assert_eq!(
            gpu.errors.fault_count(),
            faults,
            "{:?}",
            gpu.errors.take_faults()
        );
    }
}
