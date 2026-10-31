use anyhow::{Context, Result};
use wgpu::util::DeviceExt;
use winit::window::Window;

// Plain, framework-free output value types live in `config` so the engine
// contract layer can name them without importing this wgpu/winit file. Kept
// re-exported here so existing `crate::renderer::context::…` paths still work;
// window-lifecycle inherent impls (e.g. `OutputWindow::set_target`) stay below.
pub use super::config::{
    AlphaMode, CalibrationMode, OutputRotation, OutputSource, OutputTarget,
    PresentationCapabilities, PresentationColorProfile, PresentationDepth, PresentationFormat,
    PresentationPixelFormat, PresentationRequest, RecordingCodec, ResolvedPresentation,
    RtmpCodecContract, SrtCodec, StreamingCodec,
};

/// Linear-light format used by the entire color path: deck render targets, all
/// three effect tiers, ISF pass buffers, compute output, channel/mixer
/// composites, and the dome/edge-blend intermediates.
///
/// Single source of truth — see spec/unified-color-pipeline.md. 8-bit appears
/// only at the two boundaries where it is correct: sRGB-tagged source ingest
/// (hardware EOTF on sample) and sRGB output encode (hardware OETF on write).
/// Non-color data textures (analyzer, audio, calibration, MSDF atlases) keep
/// their own formats and are deliberately excluded.
pub const COLOR_PATH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

fn resolve_eight_bit_presentation(
    request: PresentationRequest,
    pixel_format: PresentationPixelFormat,
    color_profile: PresentationColorProfile,
    alpha_mode: AlphaMode,
    fallback_reason: impl Into<String>,
) -> ResolvedPresentation {
    PresentationCapabilities::new(
        vec![PresentationFormat {
            depth: PresentationDepth::Sdr8,
            pixel_format,
            color_profile,
            alpha_mode,
        }],
        Some(fallback_reason.into()),
    )
    .resolve(request)
    .expect("every output adapter provides an eight-bit presentation format")
}

fn surface_pixel_format(format: wgpu::TextureFormat) -> PresentationPixelFormat {
    match format {
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb => {
            PresentationPixelFormat::Bgra8
        }
        _ => PresentationPixelFormat::Rgba8,
    }
}

struct SurfacePresentationSelection {
    format: wgpu::TextureFormat,
    color_space: wgpu::SurfaceColorSpace,
    resolved: ResolvedPresentation,
}

fn select_surface_presentation(
    request: PresentationRequest,
    capabilities: &wgpu::SurfaceCapabilities,
) -> Result<SurfacePresentationSelection> {
    let sdr8_format = capabilities
        .formats
        .iter()
        .copied()
        .find(wgpu::TextureFormat::is_srgb)
        .or_else(|| capabilities.formats.first().copied())
        .context("output surface exposes no SDR presentation format")?;
    let supports_rgb10_srgb = capabilities
        .color_spaces(wgpu::TextureFormat::Rgb10a2Unorm)
        .contains(wgpu::SurfaceColorSpaces::SRGB);

    let mut formats = Vec::with_capacity(2);
    if supports_rgb10_srgb {
        formats.push(PresentationFormat {
            depth: PresentationDepth::Sdr10,
            pixel_format: PresentationPixelFormat::Rgb10A2,
            color_profile: PresentationColorProfile::SrgbFull,
            alpha_mode: AlphaMode::Opaque,
        });
    }
    formats.push(PresentationFormat {
        depth: PresentationDepth::Sdr8,
        pixel_format: surface_pixel_format(sdr8_format),
        color_profile: PresentationColorProfile::SrgbFull,
        alpha_mode: AlphaMode::Opaque,
    });
    let reason = (!supports_rgb10_srgb).then(|| {
        if capabilities
            .format_capabilities
            .iter()
            .any(|candidate| candidate.format == wgpu::TextureFormat::Rgb10a2Unorm)
        {
            "RGB10A2 is available, but not with an sRGB color-space contract".to_string()
        } else {
            "the output surface does not expose RGB10A2".to_string()
        }
    });
    let resolved = PresentationCapabilities::new(formats, reason).resolve(request)?;
    let (format, color_space) = if resolved.resolved == PresentationDepth::Sdr10 {
        (
            wgpu::TextureFormat::Rgb10a2Unorm,
            wgpu::SurfaceColorSpace::Srgb,
        )
    } else {
        (sdr8_format, wgpu::SurfaceColorSpace::Auto)
    };

    Ok(SurfacePresentationSelection {
        format,
        color_space,
        resolved,
    })
}

fn resolve_headless_presentation(
    request: PresentationRequest,
    target: &OutputTarget,
) -> ResolvedPresentation {
    if let Some(plan) = super::subprocess::StreamingPlan::for_target(target, request) {
        debug_assert_eq!(
            plan.expected_readback(),
            if plan.resolved.resolved == PresentationDepth::Sdr10 {
                super::ReadbackFormat::Rgb10A2
            } else {
                super::ReadbackFormat::Rgba8
            }
        );
        return plan.resolved;
    }
    let (pixel_format, color_profile, alpha_mode, fallback_reason) = match target {
        OutputTarget::NdiSend { .. } => (
            PresentationPixelFormat::Uyvy,
            PresentationColorProfile::Rec709Limited,
            AlphaMode::Opaque,
            "the active NDI sender supports UYVY only",
        ),
        OutputTarget::SyphonServer { .. } => (
            PresentationPixelFormat::Bgra8,
            PresentationColorProfile::SrgbFull,
            AlphaMode::Premultiplied,
            "Syphon interoperability is limited to BGRA8",
        ),
        OutputTarget::DeckLink { .. } => (
            PresentationPixelFormat::Uyvy,
            PresentationColorProfile::Rec709Limited,
            AlphaMode::Opaque,
            "a Blackmagic output takes 8-bit YUV",
        ),
        OutputTarget::Recording { .. } => (
            PresentationPixelFormat::EncoderNative("yuv420p".to_string()),
            PresentationColorProfile::Rec709Limited,
            AlphaMode::Opaque,
            "the active FFmpeg path is configured for eight-bit video",
        ),
        OutputTarget::SrtStream { .. }
        | OutputTarget::HlsStream { .. }
        | OutputTarget::DashStream { .. }
        | OutputTarget::RtmpStream { .. }
        | OutputTarget::Stream
        | OutputTarget::Record => unreachable!("streaming targets resolve above"),
        OutputTarget::Windowed | OutputTarget::Display { .. } => (
            PresentationPixelFormat::Rgba8,
            PresentationColorProfile::SrgbFull,
            AlphaMode::Opaque,
            "the output surface is configured for eight-bit SDR",
        ),
    };

    resolve_eight_bit_presentation(
        request,
        pixel_format,
        color_profile,
        alpha_mode,
        fallback_reason,
    )
}

/// Hold each frame presented to `surface` on screen at least `seconds`, or
/// present each as soon as possible (`None`). macOS only: Metal's
/// `presentDrawable:afterMinimumDuration:`, through Nida's wgpu-hal patch
/// (vendor/wgpu-hal/NIDA-PATCH.md); elsewhere nothing.
pub fn set_frame_hold(surface: &wgpu::Surface<'_>, seconds: Option<f64>) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: only a setting read at present time is changed; the
        // surface itself is not touched.
        if let Some(hal) = unsafe { surface.as_hal::<wgpu::hal::api::Metal>() } {
            hal.set_minimum_present_duration(seconds);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (surface, seconds);
}

/// Whether a screen refreshing every `refresh` (unknown: no) refreshes
/// faster than frames come, every `period` seconds — a 120 Hz screen at
/// 60 fps, not a 60 Hz one (see `OutputWindow::holds_frames`).
fn refreshes_faster(refresh: Option<std::time::Duration>, period: f64) -> bool {
    refresh.is_some_and(|refresh| refresh.as_secs_f64() < period * 0.8)
}

/// When the frames presented on `surface` since the last call reached the
/// screen — host seconds, oldest first, 0 for one never shown — as Metal
/// reports it. Empty off macOS.
pub fn take_presented_times(surface: &wgpu::Surface<'_>) -> Vec<f64> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: reads a record the present path keeps; the surface itself
        // is not touched.
        if let Some(hal) = unsafe { surface.as_hal::<wgpu::hal::api::Metal>() } {
            return hal.take_presented_times();
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = surface;
    Vec::new()
}

/// Tell `surface` whether its window is hidden (`None`: not known), as the
/// main thread hears it. Drawn from the engine thread, a surface may not ask
/// its window (macOS windows are main-thread only), and a hidden window's surface
/// has no frame to draw into. macOS only, through Nida's wgpu-hal patch.
pub fn set_occluded(surface: &wgpu::Surface<'_>, occluded: Option<bool>) {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: only a flag read when a frame is asked for is changed.
        if let Some(hal) = unsafe { surface.as_hal::<wgpu::hal::api::Metal>() } {
            hal.set_occluded(occluded);
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (surface, occluded);
}

/// GPU rendering context — device, queue, and adapter.
///
/// Owns the GPU resources needed for rendering (mixer, deck, channel, effects).
/// Does NOT own any window surface — that's a presentation concern owned by
/// the UI consumer (`WindowSurface`) or output windows (`OutputWindow`).
///
/// Can be created with a window hint (for adapter compatibility) or headless.
///
/// `Clone` is cheap — wgpu types are internally `Arc`-wrapped.
/// Cloning produces a handle to the same GPU resources, useful for
/// background thread deck creation.
#[derive(Clone)]
pub struct GpuContext {
    /// Construction lifetimes shared with deck/effect workers.
    pub builds: super::builds::Builds,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub texture_format: wgpu::TextureFormat,
    /// Linear-light color-path format (`COLOR_PATH_FORMAT`). Distinct from
    /// `texture_format`, which is the surface/presentation format.
    pub compositing_format: wgpu::TextureFormat,
    pub timestamp_supported: bool,
    /// Catches GPU errors that would otherwise abort the process, and attributes
    /// them to whatever was being drawn. See spec/error-handling.md.
    pub errors: super::gpu_guard::GpuErrorGuard,
    /// Counts command buffer commits. Read once per frame by the frame loop.
    pub submits: super::submit_stats::SubmitCounter,
    /// Keeps every submit clear of a surface's configure, which wgpu refuses
    /// when another thread submits meanwhile. See `submit_gate`.
    pub submit_gate: super::submit_gate::SubmitGate,
}

/// Window surface for presentation — surface, swapchain config, and size.
///
/// Owned by the UI consumer. Handles surface acquisition, resize, and present.
/// The engine never touches this directly.
pub struct WindowSurface {
    pub surface: wgpu::Surface<'static>,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub size: winit::dpi::PhysicalSize<u32>,
    /// The present mode chosen at start (see `GpuContext::new`), which
    /// `set_vsync(false)` goes back to.
    pub default_present_mode: wgpu::PresentMode,
}

impl GpuContext {
    /// Create a GPU context + window surface from a window.
    ///
    /// The adapter is selected for compatibility with the window's surface.
    /// Returns both the GPU context (for the engine) and the window surface (for the UI).
    ///
    /// # Errors
    ///
    /// Returns an error if the surface cannot be created for the window, if no
    /// suitable adapter is found, or if device creation fails.
    pub async fn new_for_window(window: &'static Window) -> Result<(Self, WindowSurface)> {
        let (instance, surface, size) = Self::create_surface_for_window(window)?;
        Self::new_with_surface(instance, surface, size).await
    }

    /// Create the wgpu instance and surface on the current (main) thread.
    /// On macOS, `create_surface` accesses `NSView`/`CAMetalLayer` which must
    /// happen on the main thread.  The returned objects are `Send` and can be
    /// passed to a background thread for adapter/device creation.
    ///
    /// # Errors
    ///
    /// Returns an error if `wgpu` cannot create a surface for the window
    /// (unsupported window handle or no compatible backend).
    pub fn create_surface_for_window(
        window: &'static Window,
    ) -> Result<(
        wgpu::Instance,
        wgpu::Surface<'static>,
        winit::dpi::PhysicalSize<u32>,
    )> {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        });
        let surface = instance
            .create_surface(window)
            .context("Failed to create surface")?;
        Ok((instance, surface, size))
    }

    /// Complete GPU initialization given a pre-created instance and surface.
    /// Safe to call from a background thread — all Metal dispatch work is
    /// resolved through the pre-created surface.
    ///
    /// # Errors
    ///
    /// Returns an error if no adapter compatible with the surface is found, or
    /// if the device request fails (e.g. the requested limits are unsupported).
    pub async fn new_with_surface(
        instance: wgpu::Instance,
        surface: wgpu::Surface<'static>,
        size: winit::dpi::PhysicalSize<u32>,
    ) -> Result<(Self, WindowSurface)> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
                apply_limit_buckets: false,
            })
            .await
            .context("Failed to find suitable GPU adapter")?;

        log::info!("Using GPU: {}", adapter.get_info().name);
        log::info!("Backend: {:?}", adapter.get_info().backend);

        let (required_features, timestamp_supported) = Self::select_optional_features(&adapter);

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Varda Device"),
                required_features,
                required_limits: wgpu::Limits {
                    max_texture_dimension_2d: 16384,
                    ..wgpu::Limits::default()
                },
                memory_hints: wgpu::MemoryHints::default(),
                experimental_features: wgpu::ExperimentalFeatures::default(),
                trace: wgpu::Trace::default(),
            })
            .await
            .context("Failed to create device")?;

        let surface_caps = surface.get_capabilities(&adapter);
        let surface_format = surface_caps
            .formats
            .iter()
            .copied()
            .find(wgpu::TextureFormat::is_srgb)
            .unwrap_or(surface_caps.formats[0]);

        // Prefer Immediate to avoid macOS ProMotion throttling the render loop.
        // The UI event loop drives frame pacing via request_redraw().
        // Fallback: Mailbox (non-blocking vsync) > Fifo (blocking vsync, last resort).
        let present_mode = if surface_caps
            .present_modes
            .contains(&wgpu::PresentMode::Immediate)
        {
            wgpu::PresentMode::Immediate
        } else if surface_caps
            .present_modes
            .contains(&wgpu::PresentMode::Mailbox)
        {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        log::info!(
            "Present mode: {:?} (available: {:?})",
            present_mode,
            surface_caps.present_modes
        );

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: surface_format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        // Installed before anything renders: wgpu's default handler panics, and
        // a panic on the render thread ends the performance.
        let errors = super::gpu_guard::GpuErrorGuard::new();
        errors.install(&device);

        let gpu = GpuContext {
            builds: super::builds::Builds::default(),
            instance,
            adapter,
            device,
            queue,
            texture_format: surface_format,
            compositing_format: COLOR_PATH_FORMAT,
            timestamp_supported,
            errors,
            submits: super::submit_stats::SubmitCounter::new(),
            submit_gate: super::submit_gate::SubmitGate::new(),
        };
        gpu.configure_surface(&surface, &surface_config);
        let win_surface = WindowSurface {
            default_present_mode: surface_config.present_mode,
            surface,
            surface_config,
            size,
        };

        Ok((gpu, win_surface))
    }

    /// Select the optional device features to request from an adapter.
    ///
    /// Shared by the windowed and headless paths so HAP (BC texture
    /// compression) and GPU timing behave identically regardless of whether a
    /// window surface exists. Returns the feature set to request and whether
    /// timestamp queries are usable for GPU timing.
    fn select_optional_features(adapter: &wgpu::Adapter) -> (wgpu::Features, bool) {
        let mut required_features = wgpu::Features::empty();
        if adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC)
        {
            required_features |= wgpu::Features::TEXTURE_COMPRESSION_BC;
            log::info!("GPU supports BC texture compression (HAP video enabled)");
        } else {
            log::warn!("GPU does not support BC texture compression — HAP video will fall back to ffmpeg CPU decode");
        }

        let rgba16_features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let rgba16_renderable = adapter.features().contains(rgba16_features)
            && adapter
                .get_texture_format_features(wgpu::TextureFormat::Rgba16Unorm)
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT);
        if rgba16_renderable {
            required_features |= rgba16_features;
            log::info!("GPU supports normalized RGBA16 recording targets");
        } else {
            log::warn!(
                "GPU does not support normalized RGBA16 targets — 10-bit ProRes 4444 will \
                 fall back to eight-bit SDR"
            );
        }

        let mut timestamp_supported = false;
        if adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            required_features |= wgpu::Features::TIMESTAMP_QUERY;
            if !adapter
                .features()
                .contains(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS)
            {
                log::warn!("GPU supports TIMESTAMP_QUERY but not TIMESTAMP_QUERY_INSIDE_ENCODERS — GPU timing disabled");
            } else if encoder_timestamps_are_trustworthy(adapter) {
                required_features |= wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
                timestamp_supported = true;
                log::info!("GPU supports timestamp queries inside encoders (GPU timing enabled)");
            } else {
                log::warn!("Apple GPU: encoder timestamps are not implementable on this hardware — GPU timing disabled");
            }
        }

        (required_features, timestamp_supported)
    }

    /// Create a headless GPU context (no window surface).
    ///
    /// Requests the same optional features as the windowed path (notably
    /// `TEXTURE_COMPRESSION_BC`) so HAP video uses the GPU-native `BCn` path in
    /// headless installations. Falls back to software adapter if no hardware
    /// GPU is available. Used for headless mode and tests.
    ///
    /// # Errors
    ///
    /// Returns an error if no GPU adapter is available at all, or if the
    /// headless device request fails.
    pub fn new_headless() -> Result<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::all(),
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            display: None,
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
        });

        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .context("Failed to find GPU adapter for headless context")?;

        log::info!("Using GPU: {}", adapter.get_info().name);
        log::info!("Backend: {:?}", adapter.get_info().backend);

        let (required_features, timestamp_supported) = Self::select_optional_features(&adapter);

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("Varda Headless Device"),
            required_features,
            required_limits: wgpu::Limits {
                max_texture_dimension_2d: 16384,
                ..wgpu::Limits::default()
            },
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::default(),
        }))
        .context("Failed to create headless device")?;

        let errors = super::gpu_guard::GpuErrorGuard::new();
        errors.install(&device);

        Ok(GpuContext {
            builds: super::builds::Builds::default(),
            instance,
            adapter,
            device,
            queue,
            texture_format: wgpu::TextureFormat::Rgba8UnormSrgb,
            compositing_format: COLOR_PATH_FORMAT,
            timestamp_supported,
            errors,
            submits: super::submit_stats::SubmitCounter::new(),
            submit_gate: super::submit_gate::SubmitGate::new(),
        })
    }

    /// Submit command buffers, counting the commit.
    ///
    /// Prefer this over `context.queue.submit()` anywhere on the per-frame path
    /// so the submit tally stays accurate. See `submit_stats`.
    pub fn submit<I>(&self, command_buffers: I) -> wgpu::SubmissionIndex
    where
        I: IntoIterator<Item = wgpu::CommandBuffer>,
    {
        self.submits.record();
        self.submit_gate.submit(&self.queue, command_buffers)
    }

    /// Configure `surface` as `config` says, with no submit from any thread
    /// meanwhile — which wgpu would refuse the configure for, leaving the
    /// surface its old size. See `submit_gate`.
    pub fn configure_surface(
        &self,
        surface: &wgpu::Surface<'_>,
        config: &wgpu::SurfaceConfiguration,
    ) {
        self.submit_gate.configure(&self.device, surface, config);
    }

    /// Create a texture for rendering
    pub fn create_render_texture(&self, width: u32, height: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Render Texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.texture_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Create a texture for compositing in linear-light space (`Rgba16Float`).
    /// Used for channel composites, mixer composites, effect ping-pong, and sub-mixes.
    pub fn create_compositing_texture(&self, width: u32, height: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Compositing Texture (Rgba16Float)"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: self.compositing_format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        })
    }

    /// Create a uniform buffer
    pub fn create_uniform_buffer<T: bytemuck::Pod>(&self, data: &T) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("Uniform Buffer"),
                contents: bytemuck::cast_slice(&[*data]),
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            })
    }

    /// Update a uniform buffer
    pub fn update_uniform_buffer<T: bytemuck::Pod>(&self, buffer: &wgpu::Buffer, data: &T) {
        self.queue
            .write_buffer(buffer, 0, bytemuck::cast_slice(&[*data]));
    }
}

/// Whether `CommandEncoder::write_timestamp` produces usable results here.
///
/// Apple GPUs are tile-based deferred renderers: Metal only permits counter
/// sampling at stage boundaries (via a pass descriptor's
/// `sampleBufferAttachments`), so there is no `sampleCountersInBuffer` for an
/// encoder-level timestamp to lower to. wgpu 29 still advertises
/// `TIMESTAMP_QUERY_INSIDE_ENCODERS` on this hardware and emulates it with a
/// dummy blit pass, but that emulation drops the render work submitted
/// alongside it — decks composite from never-written textures and the frame
/// fills with NaN. Upstream has since stopped advertising the feature here
/// (gfx-rs/wgpu ef9974f); until we take that release, refuse it ourselves.
fn encoder_timestamps_are_trustworthy(adapter: &wgpu::Adapter) -> bool {
    /// Apple's PCI vendor ID, as reported for Metal adapters.
    const APPLE_VENDOR: u32 = 0x106B;

    let info = adapter.get_info();
    !(info.backend == wgpu::Backend::Metal
        && (info.vendor == APPLE_VENDOR || info.name.starts_with("Apple")))
}

impl WindowSurface {
    /// Present in step with the screen's refresh (`Fifo`), or as chosen at
    /// start (`Immediate` where there is one). On a variable-refresh screen,
    /// presents not in step with it can pull its refresh about — for every
    /// app on it, the display link that starts frames included.
    pub fn set_vsync(&mut self, gpu: &GpuContext, on: bool) {
        let mode = if on {
            wgpu::PresentMode::Fifo
        } else {
            self.default_present_mode
        };
        if self.surface_config.present_mode != mode {
            self.surface_config.present_mode = mode;
            gpu.configure_surface(&self.surface, &self.surface_config);
            log::info!("Control window presents: {mode:?}");
        }
    }

    /// Resize the window surface
    pub fn resize(&mut self, gpu: &GpuContext, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.surface_config.width = new_size.width;
            self.surface_config.height = new_size.height;
            gpu.configure_surface(&self.surface, &self.surface_config);
        }
    }
}

/// Info for rendering one surface into an output window.
pub struct SurfaceRenderInfo<'a> {
    /// Surface uuid — cache key for its baked hole mask.
    pub uuid: &'a str,
    /// The content texture to sample from
    pub content_view: &'a wgpu::TextureView,
    /// Polygon vertices in normalized canvas coords [0..1] (primary contour)
    pub vertices: &'a [[f32; 2]],
    /// Additional disjoint contours for combined surfaces. Empty for simple
    /// surfaces. When non-empty, the surface renders every contour (no warp).
    pub extra_contours: &'a [Vec<[f32; 2]>],
    /// Bounding box: [x, y, width, height] in [0..1]
    pub bounding_box: [f32; 4],
    /// UV scale for content sampling (Fill=[1,1], Mapped=[`bb_w`, `bb_h`])
    pub uv_scale: [f32; 2],
    /// UV offset for content sampling (Fill=[0,0], Mapped=[`bb_x`, `bb_y`])
    pub uv_offset: [f32; 2],
    /// Warp mode: `CornerPin` or Mesh. None = no warp (render at polygon's native position).
    pub warp_mode: Option<super::warp::WarpMode>,
    /// Per-surface overlap zones (Auto mode). Default = no zones.
    pub overlap_zones: super::edge_blend::SurfaceOverlapZones,
    /// Flattened subtractive hole contours in surface uv space (8i.7). Empty =
    /// no holes.
    pub hole_uv_contours: Vec<Vec<[f32; 2]>>,
}

/// Membership of a surface in an output. Warp now lives on the `Surface`
/// itself (`Surface.warp`); an assignment only records inclusion and the
/// per-output overlap zones used for edge blending.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SurfaceAssignment {
    /// UUID of the assigned surface
    pub surface_uuid: String,
    /// Whether this assignment is enabled
    pub enabled: bool,
    /// Per-surface overlap zones (set by Auto mode detection).
    #[serde(default)]
    pub overlap_zones: super::edge_blend::SurfaceOverlapZones,
}

/// A display target an output window has not reached yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitedDisplay {
    /// Monitor name, as the platform reports it.
    pub monitor: String,
    /// The screen itself, for an output that lost it while running: it goes
    /// back to that screen only — not to another of the same model that has
    /// taken its name meanwhile. None at a show opened without it: then by
    /// name.
    pub screen: Option<winit::monitor::MonitorHandle>,
    /// The monitor was not connected, so the operator is told when the output
    /// lands on it. False when only the window's mapping was being waited on.
    pub was_missing: bool,
}

/// What an output window is configured to show on: the monitor it is waiting
/// for when there is one, otherwise its live target. The index is a placeholder
/// — a display target is persisted and restored by monitor name.
fn configured_target(target: &OutputTarget, awaiting_display: Option<&str>) -> OutputTarget {
    match awaiting_display {
        Some(name) => OutputTarget::Display {
            name: name.to_string(),
            monitor_index: 0,
        },
        None => target.clone(),
    }
}

/// The largest centred `[x, y, w, h]` of `content_aspect` (width ÷ height) that
/// fits a `canvas_width` × `canvas_height` canvas, in normalised coordinates.
///
/// Content narrower than the canvas gets bars on the left and right, wider gets
/// them above and below, and an exact match returns the full unit square so the
/// usual case costs nothing. Degenerate inputs — a zero-sized canvas, or an
/// aspect that is zero, negative, infinite or NaN — also return the full square,
/// which stretches rather than producing a NaN-sized quad.
pub fn aspect_fit_rect(content_aspect: f32, canvas_width: u32, canvas_height: u32) -> [f32; 4] {
    if canvas_width == 0
        || canvas_height == 0
        || !content_aspect.is_finite()
        || content_aspect <= 0.0
    {
        return [0.0, 0.0, 1.0, 1.0];
    }
    let canvas_aspect = canvas_width as f32 / canvas_height as f32;
    let (w, h) = if content_aspect > canvas_aspect {
        (1.0, canvas_aspect / content_aspect)
    } else {
        (content_aspect / canvas_aspect, 1.0)
    };
    [(1.0 - w) * 0.5, (1.0 - h) * 0.5, w, h]
}

/// Where a Blackmagic output draws its picture in its format's frame
/// (`aspect_fit_rect`): the picture — the master, or the share of it a bus's
/// `region_scale` gives — whole, its shape kept, turned by `rotation`, black
/// around; as a window shows the master. Its frame is the format's, not the
/// picture's (see `crate::decklink`).
pub fn fitted_in_format(
    master: [u32; 2],
    region_scale: [f32; 2],
    rotation: OutputRotation,
    width: u32,
    height: u32,
) -> [f32; 4] {
    let wide = master[0] as f32 * region_scale[0];
    let high = master[1] as f32 * region_scale[1];
    let aspect = if rotation.swaps_dimensions() {
        high / wide
    } else {
        wide / high
    };
    aspect_fit_rect(aspect, width, height)
}

/// What became of an output window's presents, kept where the engine thread
/// presents them after handing the engine back (see [`PresentJob`]).
#[derive(Debug, Default)]
pub struct PresentStats {
    presents: std::sync::atomic::AtomicU64,
    unavailable: std::sync::atomic::AtomicU64,
    /// How long the last present waited for a frame to draw into, ns.
    last_acquire_ns: std::sync::atomic::AtomicU64,
    presented: std::sync::atomic::AtomicBool,
    /// The surface said it was out of date: reconfigured before the next.
    outdated: std::sync::atomic::AtomicBool,
    /// The window is closing: nothing more is presented to it.
    closed: std::sync::atomic::AtomicBool,
    /// Frames the presenter let go to catch up (see `presenter::Presenter`).
    let_go: std::sync::atomic::AtomicU64,
}

/// A picture of a frame's own, waiting with it to go on the window's screen:
/// the engine draws the next frames elsewhere meanwhile (see [`PresentJob`]).
pub struct PresentPicture {
    texture: wgpu::Texture,
    /// The window's blit reading it.
    bind_group: wgpu::BindGroup,
}

/// An output window's pictures for frames on their way to its screen: a
/// picture no frame holds is drawn into again, and there are never more than
/// a frame waiting, one presented and one drawn need.
#[derive(Default)]
struct PresentPictures {
    pictures: Vec<std::sync::Arc<PresentPicture>>,
    /// The size and format they are, as the window's picture was.
    made_as: Option<(u32, u32, wgpu::TextureFormat)>,
}

impl PresentPictures {
    /// Pictures at most: [`super::presenter::WAITING_MOST`] waiting, one being
    /// presented and one being drawn.
    const MOST: usize = super::presenter::WAITING_MOST + 2;

    /// A picture no frame holds, the size and format of `like`, read by
    /// `blit`; `None` only if all are held, which the waiting limit rules out.
    fn free(
        &mut self,
        device: &wgpu::Device,
        like: &wgpu::Texture,
        blit: &BlitPipeline,
    ) -> Option<std::sync::Arc<PresentPicture>> {
        let made_as = (like.width(), like.height(), like.format());
        if self.made_as != Some(made_as) {
            self.pictures.clear();
            self.made_as = Some(made_as);
        }
        if let Some(free) = self
            .pictures
            .iter()
            .find(|picture| std::sync::Arc::strong_count(picture) == 1)
        {
            return Some(std::sync::Arc::clone(free));
        }
        if self.pictures.len() >= Self::MOST {
            return None;
        }
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Output Picture"),
            size: like.size(),
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: like.format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let picture = std::sync::Arc::new(PresentPicture {
            bind_group: blit.create_bind_group(device, &view),
            texture,
        });
        self.pictures.push(std::sync::Arc::clone(&picture));
        Some(picture)
    }

    /// Made afresh at the next frame: the blit reading them changed.
    fn clear(&mut self) {
        self.pictures.clear();
        self.made_as = None;
    }
}

/// A frame drawn for an output window, waiting to go on its screen: the wait
/// for a frame of the window's to draw into, the copy and the present.
///
/// With every frame held on screen a frame's time, a window's frames queue up
/// and taking the next means waiting for its screen to free one — ~11 ms of
/// every frame, measured. Done while holding the engine, that kept the control
/// window waiting for it twice a frame (it fell to 30 fps). So the engine
/// draws each window's picture and the window's presenter presents it, on a
/// thread of its own (see [`super::presenter::Presenter`]).
pub struct PresentJob {
    name: String,
    /// The window's number for the frame (see `present_trace`).
    frame: u64,
    surface: std::sync::Arc<wgpu::Surface<'static>>,
    /// Held while presenting; reconfiguring the surface waits for it.
    lock: std::sync::Arc<std::sync::Mutex<()>>,
    pipeline: wgpu::RenderPipeline,
    /// The frame's own picture, given back to the window's pictures when
    /// the job is done.
    picture: std::sync::Arc<PresentPicture>,
    /// How often the window's screen shows a new frame: its refresh, or
    /// the show's frame time when that is longer.
    refresh: std::time::Duration,
    /// Each frame held on screen a frame's time (see `set_frame_hold`).
    held: bool,
    stats: std::sync::Arc<PresentStats>,
}

impl PresentJob {
    /// The output window's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How often the window's screen shows a new frame.
    pub fn refresh(&self) -> std::time::Duration {
        self.refresh
    }

    /// Whether each frame is held on screen a frame's time.
    pub fn held(&self) -> bool {
        self.held
    }

    /// Take a frame of the window's, copy the picture into it, and present
    /// it. Returns how long it waited for the frame — none when it could not
    /// be presented: the window closing, or no frame to draw into.
    pub fn present(self, context: &GpuContext) -> Option<std::time::Duration> {
        use super::present_trace::{Step, TRACE};
        use std::sync::atomic::Ordering::Relaxed;
        TRACE.note(&self.name, self.frame, Step::Taken);
        let _surface = self
            .lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.stats.closed.load(Relaxed) {
            return None;
        }
        let started = std::time::Instant::now();
        let acquired = self.surface.get_current_texture();
        let waited = started.elapsed();
        self.stats.last_acquire_ns.store(
            u64::try_from(waited.as_nanos()).unwrap_or(u64::MAX),
            Relaxed,
        );
        let had = matches!(
            acquired,
            wgpu::CurrentSurfaceTexture::Success(_) | wgpu::CurrentSurfaceTexture::Suboptimal(_)
        );
        TRACE.note(
            &self.name,
            self.frame,
            if had { Step::Acquired } else { Step::NoFrame },
        );
        let output = match acquired {
            wgpu::CurrentSurfaceTexture::Success(output) => output,
            wgpu::CurrentSurfaceTexture::Suboptimal(output) => {
                self.stats.outdated.store(true, Relaxed);
                output
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                log::warn!("Output '{}': surface out of date, reconfiguring", self.name);
                self.stats.outdated.store(true, Relaxed);
                self.stats.unavailable.fetch_add(1, Relaxed);
                return None;
            }
            other => {
                log::debug!("Output '{}': surface unavailable: {other:?}", self.name);
                self.stats.unavailable.fetch_add(1, Relaxed);
                return None;
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&format!("Output '{}' Present", self.name)),
            });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(&format!("Output '{}' Swap Blit", self.name)),
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
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.picture.bind_group, &[]);
            super::region::draw_within(&mut pass, None);
        }
        context.submit(std::iter::once(encoder.finish()));
        TRACE.note(&self.name, self.frame, Step::Submitted);
        context.queue.present(output);
        TRACE.note(&self.name, self.frame, Step::Presented);
        self.stats.presents.fetch_add(1, Relaxed);
        self.stats.presented.store(true, Relaxed);
        Some(waited)
    }

    /// Let the frame go unpresented: its presenter is catching up (see
    /// `presenter::Presenter`).
    pub fn let_go_to_catch_up(self) {
        use super::present_trace::{Step, TRACE};
        self.stats
            .let_go
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        TRACE.note(&self.name, self.frame, Step::LetGo);
    }
}

/// Whether this is the main thread: on macOS, the only one that may use
/// windows. Elsewhere every thread may.
pub fn on_main_thread() -> bool {
    #[cfg(target_os = "macos")]
    return objc2::MainThreadMarker::new().is_some();
    #[cfg(not(target_os = "macos"))]
    return true;
}

/// An output's OS window, usable on the main thread only.
///
/// On macOS a window may only be used on the main thread, and winit, asked
/// from another, waits for the main thread to do it — which stalls the engine
/// thread for as long as the main thread is busy, and never returns while the
/// main thread waits for the engine. So the window is handed out only on the
/// main thread (`get`); what the engine thread needs to know — the id, the
/// screen, size and position — is kept from the main thread's reading, and
/// what it wants done to the window is sent to the main thread (`on_main`).
pub struct OsWindow {
    window: &'static Window,
    id: winit::window::WindowId,
}

impl OsWindow {
    /// On the main thread, where the window was just made.
    pub fn new(window: &'static Window) -> Self {
        Self {
            id: window.id(),
            window,
        }
    }

    pub fn id(&self) -> winit::window::WindowId {
        self.id
    }

    /// The window, on the main thread; `None` on any other (macOS).
    pub fn get(&self) -> Option<&'static Window> {
        on_main_thread().then_some(self.window)
    }

    /// Do `f` with the window on the main thread: at once when on it, else
    /// as soon as the main thread is free — not waited for, so the engine
    /// thread never waits on the main thread. Calls run in the order made.
    pub fn on_main(&self, f: impl FnOnce(&'static Window) + Send + 'static) {
        let window = self.window;
        #[cfg(target_os = "macos")]
        if !on_main_thread() {
            dispatch2::DispatchQueue::main().exec_async(move || f(window));
            return;
        }
        f(window);
    }
}

/// The title of the output window named `name`.
pub fn output_title(name: &str) -> String {
    format!("Nida - {name}")
}

/// An output window that displays content on a separate display/projector.
///
/// Each output window has its own OS window and wgpu surface, but shares
/// the device and queue from the `GpuContext`.
pub struct OutputWindow {
    pub uuid: String,
    pub name: String,
    pub window: OsWindow,
    pub surface: std::sync::Arc<wgpu::Surface<'static>>,
    /// Held while a frame is presented to `surface` (see [`PresentJob`]).
    present_lock: std::sync::Arc<std::sync::Mutex<()>>,
    present_stats: std::sync::Arc<PresentStats>,
    /// Puts its frames on its screen, on a thread of its own.
    presenter: super::presenter::Presenter,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub size: winit::dpi::PhysicalSize<u32>,
    pub blit_pipeline: BlitPipeline,
    pub polygon_pipeline: PolygonBlitPipeline,
    /// Where this output is displayed (windowed or on a specific monitor)
    pub target: OutputTarget,
    /// The monitor this output is configured for but not yet showing on: it is
    /// not connected, or (on Linux) this window has not been mapped yet. Moved
    /// there once both hold, and saved as the target meanwhile so a restart does
    /// not forget it. Any explicit target change clears it.
    pub awaiting_display: Option<AwaitedDisplay>,
    /// The screen this window was put on to cover — the screen itself, not
    /// its name, which changes as screens come and go (two of a model are
    /// told apart by place) — and since when it has been missing from the
    /// screens listed, if it is (see `VardaApp::release_lost_displays`).
    pub covered_monitor: Option<winit::monitor::MonitorHandle>,
    pub display_missing_since: Option<std::time::Instant>,
    /// Whether this window has presented a frame — the compositor has mapped it.
    pub presented: bool,
    /// macOS: whether this window is currently covering a screen (see
    /// [`Self::cover_screen`]) rather than being an ordinary window.
    #[cfg(target_os = "macos")]
    covering_screen: bool,
    /// Surface assignments — which surfaces this output renders, with per-surface warp.
    /// Empty = render all surfaces (fallback behavior).
    pub surface_assignments: Vec<SurfaceAssignment>,
    /// LED screens this output's feed carries; when set, it replaces the
    /// surfaces (see [`crate::engine::value::led`]).
    pub led: Option<crate::engine::value::led::LedLayout>,
    /// Calibration display mode (Off / Projector test card / per-Surface cards).
    pub calibration_mode: CalibrationMode,
    /// Whether edge blend is auto-computed or manually configured.
    pub edge_blend_mode: super::edge_blend::EdgeBlendMode,
    /// Edge blending configuration for multi-projector overlap zones.
    pub edge_blend: super::edge_blend::EdgeBlendConfig,
    /// GPU pipeline for applying edge blend post-process.
    pub edge_blend_pipeline: super::edge_blend::EdgeBlendPipeline,
    /// Pre-blend intermediate: surfaces render here when edge blending is active.
    pub surface_texture: wgpu::Texture,
    pub surface_texture_view: wgpu::TextureView,
    /// Post-blend result texture. UI preview reads from this.
    /// When edge blend is off, surfaces render directly here.
    /// When edge blend is on, edge blend shader writes here from `surface_texture`.
    pub preview_texture: wgpu::Texture,
    pub preview_texture_view: wgpu::TextureView,
    /// Per-output rotation applied at the final blit stage.
    pub rotation: OutputRotation,
    /// Persisted precision and dithering request.
    pub presentation_request: PresentationRequest,
    /// Runtime format selected for the active surface.
    pub resolved_presentation: ResolvedPresentation,
    /// Presents at the display's own rate: see [`PresentPace`].
    pace: PresentPace,
    /// The display's refresh interval, and when the main thread last read
    /// it (see [`Self::note_screen`]).
    refresh: Option<(Option<std::time::Duration>, std::time::Instant)>,
    /// The display the window is on (a `CGDirectDisplayID`), read with the
    /// refresh interval.
    display_id: Option<u32>,
    /// The screen the window is on, read with the refresh interval.
    screen: Option<winit::monitor::MonitorHandle>,
    /// Where the window is on the desktop, as the main thread last saw it.
    pub position: Option<winit::dpi::PhysicalPosition<i32>>,
    /// The display whose refresh starts frames, when one does (set each
    /// frame). On it, every frame is presented: the frames already come at
    /// its rate.
    pub clock_display: Option<u32>,
    /// Frames presented.
    pub presents: u64,
    /// Frames drawn but not presented, to keep to the display's rate.
    pub pace_skipped: u64,
    /// Frames drawn for the window: each one's number (see `present_trace`).
    frames_drawn: u64,
    /// Pictures of the frames on their way to the screen (see [`PresentJob`]).
    pictures: PresentPictures,
    /// Frames its presenter let go to catch up, as last counted into
    /// `pace_skipped`.
    let_go_counted: u64,
    /// Frames the window had nowhere to present (no drawable).
    pub unavailable: u64,
    /// How long this frame waited for a frame to draw into.
    pub last_acquire: std::time::Duration,
    /// How long each frame is held on screen, as last set on the surface.
    pub frame_hold: Option<f64>,
    /// The show's frame time, seconds, as last set by the frame loop.
    pub frame_period: f64,
    /// How evenly this window's frames reached its screen.
    pub on_screen: super::on_screen::OnScreen,
    /// Whether its screen has stopped showing its frames (Metal says when a
    /// frame was shown; nothing else does).
    #[cfg(target_os = "macos")]
    unshown: super::on_screen::Unshown,
    /// Whether its screen says what it shows, as found since it was made or
    /// put on that screen.
    #[cfg(target_os = "macos")]
    screen_says: ScreenSays,
    /// The window is to be made again, and why: the main thread does it
    /// (`VardaApp::reopen_unshown_outputs`).
    pub reopen: Option<Reopen>,
}

/// Whether an output's screen says which of its frames it showed — Metal
/// tells when a frame was shown — as found since the window was made or put
/// on that screen. A `DisplayLink`, `AirPlay` or Sidecar screen shows the
/// frames but never says so.
#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenSays {
    /// Frames said shown so far, fewer than [`SAYS_FRAMES`]: watched for
    /// going unshown, not yet known to say.
    Counting(u32),
    /// A second's frames said shown, or more: it says. Watched for going
    /// unshown.
    Yes,
    /// Made again, it still said too few shown: it never says (a screen at
    /// another rate than the show's said none either, 2026-10-26). Not
    /// watched — made again for that, each time would only blink it — till
    /// it is put on another screen.
    Never,
}

/// Frames said shown that tell a screen says what it shows: a second's at
/// 60 a second. A few said shown as a window starts, then none, is not.
#[cfg(target_os = "macos")]
const SAYS_FRAMES: u32 = 60;

#[cfg(target_os = "macos")]
impl ScreenSays {
    /// As it stands once Metal's report of when frames were shown (`shown`,
    /// 0 for one never shown) is taken.
    fn with_shown(self, shown: &[f64]) -> Self {
        let said = u32::try_from(shown.iter().filter(|&&at| at > 0.0).count()).unwrap_or(u32::MAX);
        match self {
            Self::Counting(n) if n.saturating_add(said) >= SAYS_FRAMES => Self::Yes,
            Self::Counting(n) => Self::Counting(n.saturating_add(said)),
            other => other,
        }
    }
}

/// Why an output's window is to be made again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reopen {
    /// Its screen has shown none of its frames for a while (see
    /// `on_screen::Unshown`).
    Unshown,
    /// Asked for (`ReopenOutput`).
    Asked,
}

/// Paces an output's presents to the rate its display shows them.
///
/// Outputs present in step with the display (vsync): shown between two of its
/// refreshes, a frame tore — the top of the screen one frame, the bottom the
/// next — visible as a line across anything moving. In step, a frame waits
/// for the display's next refresh when the one before has not been shown
/// yet, and that wait would hold up the whole render loop — every output,
/// recording and stream — for a display slower than the show (a TV at 30 or
/// 50 Hz, one at 59.94 under a 60 fps show). So an output presents only as
/// often as its display refreshes: it earns a present with each refresh's
/// worth of time, keeps up to two in hand for uneven frames, and a frame
/// with none left is shown in the preview but not sent.
#[derive(Debug, Default)]
struct PresentPace {
    credit: f64,
    last: Option<std::time::Instant>,
}

impl PresentPace {
    /// Presents kept in hand at most.
    const IN_HAND: f64 = 2.0;

    /// Whether to present the frame rendered `now` to a display refreshing
    /// every `refresh` (`None` when unknown: every frame).
    fn due(&mut self, now: std::time::Instant, refresh: Option<std::time::Duration>) -> bool {
        let Some(refresh) = refresh.filter(|r| !r.is_zero()) else {
            return true;
        };
        let earned = self.last.map_or(Self::IN_HAND, |at| {
            now.saturating_duration_since(at).as_secs_f64() / refresh.as_secs_f64()
        });
        self.last = Some(now);
        self.credit = (self.credit + earned).min(Self::IN_HAND);
        if self.credit >= 0.9 {
            self.credit -= 1.0;
            true
        } else {
            false
        }
    }
}

impl OutputWindow {
    /// Name the window `name`, its title too — set on the main thread, not
    /// waited for.
    pub fn rename(&mut self, name: &str) {
        self.name = name.to_string();
        let title = output_title(name);
        self.window.on_main(move |window| window.set_title(&title));
    }

    /// Create a new output window with its own surface, sharing the given device/queue.
    ///
    /// # Errors
    ///
    /// Returns an error if the output surface cannot be created for the window,
    /// or if any of the blit/polygon/edge-blend pipelines fail to build.
    pub fn new(context: &GpuContext, window: &'static Window, name: String) -> Result<Self> {
        let size = window.inner_size();

        let surface = context
            .instance
            .create_surface(window)
            .context("Failed to create output surface")?;

        let surface_caps = surface.get_capabilities(&context.adapter);
        let presentation_request = PresentationRequest::default();
        let selection = select_surface_presentation(presentation_request, &surface_caps)?;

        // In step with the display, so a moving picture does not tear; paced
        // (see `PresentPace`) so a slow display never holds up the render loop.
        let present_mode = wgpu::PresentMode::Fifo;

        let surface_config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: selection.format,
            width: size.width,
            height: size.height,
            present_mode,
            alpha_mode: surface_caps.alpha_modes[0],
            color_space: selection.color_space,
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };

        context.configure_surface(&surface, &surface_config);

        let blit_pipeline = BlitPipeline::new(&context.device, surface_config.format)?;
        let polygon_pipeline = PolygonBlitPipeline::new(&context.device, surface_config.format)?;
        let edge_blend_pipeline =
            super::edge_blend::EdgeBlendPipeline::new(&context.device, surface_config.format)?;
        let (surface_texture, surface_texture_view) = Self::create_intermediate_texture(
            &context.device,
            size.width,
            size.height,
            surface_config.format,
            "Surface Intermediate",
        );
        let (preview_texture, preview_texture_view) = Self::create_intermediate_texture(
            &context.device,
            size.width,
            size.height,
            surface_config.format,
            "Preview",
        );
        let presenter = super::presenter::Presenter::start(&name, context.clone());

        Ok(Self {
            uuid: crate::deck::generate_short_uuid(),
            name,
            position: window.outer_position().ok(),
            window: OsWindow::new(window),
            surface: std::sync::Arc::new(surface),
            present_lock: std::sync::Arc::default(),
            present_stats: std::sync::Arc::default(),
            presenter,
            surface_config,
            size,
            blit_pipeline,
            polygon_pipeline,
            target: OutputTarget::Windowed,
            awaiting_display: None,
            covered_monitor: None,
            display_missing_since: None,
            presented: false,
            #[cfg(target_os = "macos")]
            covering_screen: false,
            surface_assignments: Vec::new(),
            led: None,
            calibration_mode: CalibrationMode::Off,
            edge_blend_mode: super::edge_blend::EdgeBlendMode::default(),
            edge_blend: super::edge_blend::EdgeBlendConfig::default(),
            edge_blend_pipeline,
            surface_texture,
            surface_texture_view,
            preview_texture,
            preview_texture_view,
            rotation: OutputRotation::default(),
            presentation_request,
            resolved_presentation: selection.resolved,
            pace: PresentPace::default(),
            display_id: None,
            clock_display: None,
            presents: 0,
            pace_skipped: 0,
            frames_drawn: 0,
            pictures: PresentPictures::default(),
            let_go_counted: 0,
            unavailable: 0,
            last_acquire: std::time::Duration::ZERO,
            frame_hold: None,
            frame_period: 1.0 / 60.0,
            on_screen: super::on_screen::OnScreen::default(),
            #[cfg(target_os = "macos")]
            unshown: super::on_screen::Unshown::new(std::time::Instant::now(), 0),
            #[cfg(target_os = "macos")]
            screen_says: ScreenSays::Counting(0),
            reopen: None,
            refresh: None,
            screen: None,
        })
    }

    /// Read which screen the window is on, and its refresh — on the main
    /// thread, every two seconds at most (a window moved to another screen).
    pub fn note_screen(&mut self) {
        let now = std::time::Instant::now();
        if self
            .refresh
            .is_some_and(|(_, read)| now.duration_since(read) < std::time::Duration::from_secs(2))
        {
            return;
        }
        let Some(window) = self.window.get() else {
            return;
        };
        // Whether the window is hidden, for the engine thread's surface
        // (kept current between readings by winit's `Occluded` events).
        #[cfg(target_os = "macos")]
        set_occluded(&self.surface, macos_window::occluded(window));
        let monitor = window.current_monitor();
        let interval = monitor
            .as_ref()
            .and_then(winit::monitor::MonitorHandle::refresh_rate_millihertz)
            .filter(|mhz| *mhz > 0)
            .map(|mhz| std::time::Duration::from_secs_f64(1000.0 / f64::from(mhz)));
        #[cfg(target_os = "macos")]
        {
            use winit::platform::macos::MonitorHandleExtMacOS as _;
            self.display_id = monitor
                .as_ref()
                .map(winit::monitor::MonitorHandle::native_id);
        }
        self.screen = monitor;
        self.refresh = Some((interval, now));
    }

    /// Ask for the window to be redrawn. Not on macOS: there the presents
    /// alone keep the window drawn, and the request — from the engine
    /// thread — would wait for the main thread.
    pub fn request_redraw(&self) {
        #[cfg(not(target_os = "macos"))]
        self.window.on_main(|window| window.request_redraw());
    }

    /// The refresh interval of the display the window is on, as last read.
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        self.refresh.and_then(|(interval, _)| interval)
    }

    /// Whether this window's frames are held on screen a frame's time (see
    /// `set_frame_hold`), at the show's frame time `period`: only on a screen
    /// refreshing faster than the show — a 120 Hz screen at 60 fps, where a
    /// frame put up as soon as it is drawn may stay one refresh or three. On
    /// a screen at the show's rate each frame is up one refresh anyway, and
    /// holding them cost an LED wall 4–13 hitches a second under load against
    /// 0–2 without (a laptop, 2026-10-02): the window's own thread, waiting
    /// for its screen to free each held frame, missed refreshes.
    pub fn holds_frames(&self, period: f64) -> bool {
        refreshes_faster(self.refresh_interval(), period)
    }

    /// The screen the window is on, as last read.
    pub fn screen(&self) -> Option<&winit::monitor::MonitorHandle> {
        self.screen.as_ref()
    }

    /// The display the window is on (a `CGDirectDisplayID`), as last read.
    pub fn display_id(&self) -> Option<u32> {
        self.display_id
    }

    /// What the window is, for reading its steps in a present trace.
    pub fn trace_info(&self) -> super::present_trace::OutputInfo {
        super::present_trace::OutputInfo {
            name: self.name.clone(),
            display: self.display_id,
            refresh_ms: self.refresh_interval().map(|r| r.as_secs_f64() * 1000.0),
            follows_clock: self.follows_clock(),
            frame_hold: self.frame_hold,
        }
    }

    /// Whether the window is on the display whose refresh starts frames.
    pub fn follows_clock(&self) -> bool {
        self.clock_display.is_some() && self.clock_display == self.display_id
    }

    /// Create an intermediate GPU texture for the render pipeline.
    fn create_intermediate_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
        label: &str,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            // Copied from: each frame's picture goes to the screen as a copy
            // of its own (see `PresentPictures`).
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&wgpu::TextureViewDescriptor::default());
        (tex, view)
    }

    fn intermediate_format_for(
        depth: PresentationDepth,
        surface_format: wgpu::TextureFormat,
    ) -> wgpu::TextureFormat {
        if depth == PresentationDepth::Sdr10 {
            COLOR_PATH_FORMAT
        } else {
            surface_format
        }
    }

    fn intermediate_format(&self) -> wgpu::TextureFormat {
        Self::intermediate_format_for(
            self.resolved_presentation.resolved,
            self.surface_config.format,
        )
    }

    fn rebuild_intermediate_textures(&mut self, device: &wgpu::Device) {
        let format = self.intermediate_format();
        let (width, height) = self
            .rotation
            .effective_dimensions(self.size.width, self.size.height);
        let (texture, view) = Self::create_intermediate_texture(
            device,
            width,
            height,
            format,
            "Surface Intermediate",
        );
        self.surface_texture = texture;
        self.surface_texture_view = view;
        let (texture, view) =
            Self::create_intermediate_texture(device, width, height, format, "Preview");
        self.preview_texture = texture;
        self.preview_texture_view = view;
    }

    /// Resize this output window's surface
    pub fn resize(&mut self, context: &GpuContext, new_size: winit::dpi::PhysicalSize<u32>) {
        if new_size.width > 0 && new_size.height > 0 {
            self.size = new_size;
            self.surface_config.width = new_size.width;
            self.surface_config.height = new_size.height;
            self.configure_surface(context);
            self.rebuild_intermediate_textures(&context.device);
        }
    }

    /// Configure the surface as `surface_config` says — once no frame is
    /// being presented to it (see [`PresentJob`]).
    fn configure_surface(&self, context: &GpuContext) {
        let _presenting = self
            .present_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        context.configure_surface(&self.surface, &self.surface_config);
    }

    /// Set output rotation and rebuild intermediate textures at effective dimensions.
    pub fn set_rotation(&mut self, device: &wgpu::Device, rotation: OutputRotation) {
        self.rotation = rotation;
        self.rebuild_intermediate_textures(device);
    }

    /// Update the request, reconfigure the surface, and rebuild format-bound resources.
    ///
    /// # Errors
    ///
    /// Returns an error if the active surface has no SDR format or a replacement
    /// presentation pipeline cannot be created.
    pub fn set_presentation_request(
        &mut self,
        context: &GpuContext,
        request: PresentationRequest,
    ) -> Result<()> {
        let capabilities = self.surface.get_capabilities(&context.adapter);
        let selection = select_surface_presentation(request, &capabilities)?;
        let intermediate_format =
            Self::intermediate_format_for(selection.resolved.resolved, selection.format);
        let surface_changed = self.surface_config.format != selection.format
            || self.surface_config.color_space != selection.color_space;
        let intermediate_changed = self.preview_texture.format() != intermediate_format;

        let blit_pipeline = surface_changed
            .then(|| BlitPipeline::new(&context.device, selection.format))
            .transpose()?;
        let polygon_pipeline = intermediate_changed
            .then(|| PolygonBlitPipeline::new(&context.device, intermediate_format))
            .transpose()?;
        let edge_blend_pipeline = intermediate_changed
            .then(|| {
                super::edge_blend::EdgeBlendPipeline::new(&context.device, intermediate_format)
            })
            .transpose()?;

        self.presentation_request = request;
        self.resolved_presentation = selection.resolved;
        if surface_changed {
            self.surface_config.format = selection.format;
            self.surface_config.color_space = selection.color_space;
            self.configure_surface(context);
            if let Some(pipeline) = blit_pipeline {
                self.blit_pipeline = pipeline;
                self.pictures.clear();
            }
        }
        if intermediate_changed {
            if let Some(pipeline) = polygon_pipeline {
                self.polygon_pipeline = pipeline;
            }
            if let Some(pipeline) = edge_blend_pipeline {
                self.edge_blend_pipeline = pipeline;
            }
            self.rebuild_intermediate_textures(&context.device);
        }
        Ok(())
    }

    /// Render the routed content stretched over the whole window.
    ///
    /// Used for the projector calibration card, which has to reach the physical
    /// edges of the output for alignment against it to mean anything. Content
    /// should go through [`render_fit`](Self::render_fit) instead.
    pub fn render(&mut self, context: &GpuContext, content_view: &wgpu::TextureView) {
        self.render_quad(context, content_view, [0.0, 0.0, 1.0, 1.0]);
    }

    /// Render the routed content letterboxed to `content_aspect` (width ÷ height).
    ///
    /// With no surfaces defined the window is the whole canvas, so content used
    /// to be stretched to fill it: a 1080×1920 composite arrived correctly
    /// proportioned and was squashed into a 16:9 window. The pass clears to
    /// black, so insetting the quad puts bars around the content rather than
    /// distorting it. A window that already matches the content aspect insets by
    /// nothing and renders exactly as before.
    pub fn render_fit(
        &mut self,
        context: &GpuContext,
        content_view: &wgpu::TextureView,
        content_aspect: f32,
    ) {
        // Measured against the rotated dimensions: the surface pass draws into
        // an intermediate at those and rotation is applied on the way to the
        // swap chain, so a 90° output turns a landscape window into a portrait
        // canvas.
        let (ew, eh) = self
            .rotation
            .effective_dimensions(self.size.width, self.size.height);
        self.render_quad(
            context,
            content_view,
            aspect_fit_rect(content_aspect, ew, eh),
        );
    }

    /// Blit `content_view` into one normalised `[x, y, w, h]` rectangle of the
    /// window, with the content's full extent mapped across it.
    fn render_quad(
        &mut self,
        context: &GpuContext,
        content_view: &wgpu::TextureView,
        rect: [f32; 4],
    ) {
        let [x, y, w, h] = rect;
        let quad: [[f32; 2]; 4] = [[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
        self.render_surfaces(
            context,
            &[SurfaceRenderInfo {
                uuid: "",
                content_view,
                vertices: &quad,
                extra_contours: &[],
                bounding_box: rect,
                uv_scale: [1.0, 1.0],
                uv_offset: [0.0, 0.0],
                warp_mode: None,
                overlap_zones: super::edge_blend::SurfaceOverlapZones::default(),
                hole_uv_contours: Vec::new(),
            }],
        );
    }

    /// Render multiple surfaces composited at their canvas positions.
    /// Each surface is rendered as a textured polygon using fan triangulation.
    /// Warp is applied per the `WarpMode`: `CornerPin` uses homography in the vertex shader,
    /// Mesh mode bakes warp into triangle vertices directly.
    pub fn render_surfaces(&mut self, context: &GpuContext, surfaces: &[SurfaceRenderInfo<'_>]) {
        use super::present_trace::{Step, TRACE};
        self.sync_present_stats();
        if self
            .present_stats
            .outdated
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            self.configure_surface(context);
        }
        // What the screen did with the frames presented before: each should
        // stay up a frame, or a refresh of a slower screen.
        let shown = take_presented_times(&self.surface);
        if !shown.is_empty() {
            let refresh = self.refresh_interval().map_or(0.0, |r| r.as_secs_f64());
            self.on_screen.record(&shown, self.frame_period.max(refresh));
            if TRACE.recording() {
                let times = shown.clone();
                TRACE.note(&self.name, 0, Step::Shown { times });
            }
        }
        // Only a window covering its screen should always be seen: one that
        // shows none of its frames has lost its screen (Stage Manager, an app
        // switch). A plain window — one waiting for its monitor on the
        // control window's screen, say — may just be behind another, and
        // macOS can still call it visible; made again, it only took the
        // keyboard from the control window (a laptop, 2026-10-06).
        #[cfg(target_os = "macos")]
        {
            self.screen_says = self.screen_says.with_shown(&shown);
        }
        #[cfg(target_os = "macos")]
        if self.covering_screen
            && self.screen_says != ScreenSays::Never
            && self
                .unshown
                .note(std::time::Instant::now(), self.presents, &shown)
            && self.reopen.is_none()
        {
            log::warn!(
                "Output '{}': its screen has shown none of its frames for {:.0?}",
                self.name,
                super::on_screen::UNSHOWN_TIME
            );
            self.reopen = Some(Reopen::Unshown);
        }
        // Post-process edge blend only applies in Manual mode.
        // Auto mode uses per-surface blend in the polygon shader.
        let use_edge_blend = self.edge_blend_mode == super::edge_blend::EdgeBlendMode::Manual
            && self.edge_blend.any_enabled();

        // Pipeline:
        //   No edge blend:  surfaces → preview_texture → swap chain  (2 passes)
        //   Edge blend:     surfaces → surface_texture → edge blend → preview_texture → swap chain  (3 passes)
        // The UI preview always reads preview_texture_view.
        let surface_render_target = if use_edge_blend {
            &self.surface_texture_view
        } else {
            &self.preview_texture_view
        };

        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some(&format!("Output '{}' Encoder", self.name)),
            });

        // Pass 1: Render surfaces into the surface render target.
        // Triangulate on the CPU, then prepare draws from the pipeline's
        // persistent param/vertex pools (no per-frame GPU buffer allocation).
        let draws: Vec<super::blit::PolygonDrawDesc<'_>> = surfaces
            .iter()
            .map(|surf| {
                let bb = surf.bounding_box;

                // Combined (multi-contour) surface: a single warp mesh can't
                // represent disjoint contours, so render every contour as a
                // bounding-box UV fill. Matches the stage editor and fixes only
                // the primary contour rendering (see combine_surfaces).
                let (homography, vertices) = if surf.extra_contours.is_empty() {
                    // Dispatch warp mode: CornerPin → homography, Mesh → vertex-baked, None → identity
                    match &surf.warp_mode {
                        Some(super::warp::WarpMode::CornerPin { corners }) => {
                            let src_corners = [
                                [bb[0], bb[1]],
                                [bb[0] + bb[2], bb[1]],
                                [bb[0] + bb[2], bb[1] + bb[3]],
                                [bb[0], bb[1] + bb[3]],
                            ];
                            let h = super::warp::compute_forward_homography(&src_corners, corners);
                            let verts = PolygonBlitPipeline::triangulate_verts(
                                surf.vertices,
                                bb[0],
                                bb[1],
                                bb[2],
                                bb[3],
                            );
                            (Some(h), verts)
                        }
                        Some(super::warp::WarpMode::Mesh(mesh)) => {
                            // Mesh mode: warp baked into vertices, identity homography
                            (None, PolygonBlitPipeline::mesh_verts(mesh))
                        }
                        Some(super::warp::WarpMode::Bezier(b)) => {
                            // Bezier: tessellate the control cage into a mesh, then bake.
                            (None, PolygonBlitPipeline::mesh_verts(&b.tessellate()))
                        }
                        None => {
                            let verts = PolygonBlitPipeline::triangulate_verts(
                                surf.vertices,
                                bb[0],
                                bb[1],
                                bb[2],
                                bb[3],
                            );
                            (None, verts)
                        }
                    }
                } else {
                    (
                        None,
                        PolygonBlitPipeline::triangulate_multi(
                            surf.vertices,
                            surf.extra_contours,
                            bb[0],
                            bb[1],
                            bb[2],
                            bb[3],
                        ),
                    )
                };

                super::blit::PolygonDrawDesc {
                    content_view: surf.content_view,
                    uv_scale: surf.uv_scale,
                    uv_offset: surf.uv_offset,
                    homography,
                    overlap_zones: &surf.overlap_zones,
                    vertices,
                    mask_uuid: surf.uuid,
                    mask_uv_contours: surf.hole_uv_contours.clone(),
                }
            })
            .collect();

        let (prepared, vertex_pool) =
            self.polygon_pipeline
                .prepare(&context.device, &context.queue, &draws);

        {
            let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some(&format!("Output '{}' Surface Pass", self.name)),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: surface_render_target,
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

            self.polygon_pipeline
                .draw(&mut render_pass, &prepared, &vertex_pool);
        }

        // Pass 2 (edge blend only): surface_texture → edge blend → preview_texture
        if use_edge_blend {
            self.edge_blend_pipeline.render(
                &context.device,
                &context.queue,
                &mut encoder,
                &self.surface_texture_view,
                &self.preview_texture_view,
                &self.edge_blend,
            );
        }

        // Final pass: blit the picture → swap chain (with rotation) — when
        // the display is ready for another frame (see `PresentPace`). Handed
        // to the window's presenter (see [`super::presenter::Presenter`]): the
        // wait for the window's next frame to draw into is not made here. The
        // frame goes with a copy of its picture of its own: the next frames
        // are drawn into `preview_texture` while it waits, and a frame copied
        // from that once it was the window's turn showed whichever frame had
        // been drawn last — the next one, for 60% of an LED wall's frames
        // while a screen was captured (a laptop, 2026-10-02).
        self.frames_drawn += 1;
        let frame = self.frames_drawn;
        let refresh = self.refresh_interval();
        let on_clock = self.clock_display.is_some() && self.clock_display == self.display_id;
        let due = on_clock || self.pace.due(std::time::Instant::now(), refresh);
        let picture = due
            .then(|| {
                self.pictures
                    .free(&context.device, &self.preview_texture, &self.blit_pipeline)
            })
            .flatten();
        if let Some(picture) = &picture {
            encoder.copy_texture_to_texture(
                self.preview_texture.as_image_copy(),
                picture.texture.as_image_copy(),
                self.preview_texture.size(),
            );
        }
        context.submit(std::iter::once(encoder.finish()));
        let Some(picture) = picture else {
            self.pace_skipped += 1;
            TRACE.note(&self.name, frame, Step::KeptBack);
            return;
        };
        self.blit_pipeline.set_screen_presentation(
            &context.queue,
            self.rotation.index(),
            &self.resolved_presentation,
            self.presentation_request.video_levels,
        );
        let job = PresentJob {
            name: self.name.clone(),
            frame,
            surface: std::sync::Arc::clone(&self.surface),
            lock: std::sync::Arc::clone(&self.present_lock),
            pipeline: self.blit_pipeline.render_pipeline().clone(),
            picture,
            refresh: std::time::Duration::from_secs_f64(
                refresh.map_or(self.frame_period, |r| r.as_secs_f64().max(self.frame_period)),
            ),
            held: self.frame_hold.is_some(),
            stats: std::sync::Arc::clone(&self.present_stats),
        };
        TRACE.note(&self.name, frame, Step::Handed);
        // More frames waiting than may: the oldest let go for this one.
        let let_go = self.presenter.hand(job);
        if let_go > 0 {
            self.pace_skipped += let_go as u64;
            TRACE.note(&self.name, frame, Step::Replaced);
        }
    }

    /// Whether its frames went unshown because the window is hidden after
    /// all — covered, minimised, on a sleeping screen — as macOS says now
    /// (main thread). Then its surface is told so, and the window is watched
    /// afresh rather than made again.
    /// Whether a second's frames of it have been said shown since it was made
    /// or put on its screen: its screen says what it shows (see
    /// [`ScreenSays`]).
    #[cfg(target_os = "macos")]
    pub fn shown_here(&self) -> bool {
        self.screen_says == ScreenSays::Yes
    }

    /// Stop watching its frames for going unshown, and making it again for
    /// that: its screen never says what it shows. Watched again once it is
    /// put on another screen.
    #[cfg(target_os = "macos")]
    pub fn stop_watching_unshown(&mut self) {
        self.screen_says = ScreenSays::Never;
        self.reopen = None;
    }

    #[cfg(target_os = "macos")]
    pub fn unshown_because_hidden(&mut self) -> bool {
        let Some(window) = self.window.get() else {
            return false;
        };
        if macos_window::occluded(window) != Some(true) {
            return false;
        }
        set_occluded(&self.surface, Some(true));
        self.unshown = super::on_screen::Unshown::new(std::time::Instant::now(), self.presents);
        self.reopen = None;
        true
    }

    /// Bring the present figures up to date from the jobs presented since.
    fn sync_present_stats(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let stats = &self.present_stats;
        self.presents = stats.presents.load(Relaxed);
        self.unavailable = stats.unavailable.load(Relaxed);
        self.last_acquire = std::time::Duration::from_nanos(stats.last_acquire_ns.load(Relaxed));
        self.presented |= stats.presented.load(Relaxed);
        let let_go = stats.let_go.load(Relaxed);
        self.pace_skipped += let_go.saturating_sub(self.let_go_counted);
        self.let_go_counted = let_go;
    }

    /// Set the display target for this output window.
    /// `monitor` should be the `MonitorHandle` for Display targets.
    pub fn set_target(
        &mut self,
        target: OutputTarget,
        monitor: Option<winit::monitor::MonitorHandle>,
    ) {
        let bound = monitor.clone();
        match &target {
            OutputTarget::Windowed => {
                #[cfg(target_os = "macos")]
                self.uncover_screen();
                #[cfg(not(target_os = "macos"))]
                self.window.on_main(|window| {
                    window.set_fullscreen(None);
                    window.set_cursor_visible(true);
                });
            }
            OutputTarget::Display { .. } => {
                #[cfg(target_os = "macos")]
                self.cover_screen(monitor);
                // No pointer on the picture: hidden within the window.
                #[cfg(not(target_os = "macos"))]
                self.window.on_main(move |window| {
                    window.set_fullscreen(Some(winit::window::Fullscreen::Borderless(monitor)));
                    window.set_cursor_visible(false);
                });
            }
            _ => {
                log::warn!("Cannot set headless target on a windowed output");
                return;
            }
        }
        self.covered_monitor = match &target {
            OutputTarget::Display { .. } => bound,
            _ => None,
        };
        self.display_missing_since = None;
        self.awaiting_display = None;
        self.target = target;
    }

    /// macOS: turn this window into a borderless one exactly covering
    /// `monitor` (or the screen it is on), above the menu bar and Dock.
    ///
    /// Not native fullscreen: that gives each window its own animated Space,
    /// and winit cannot move a fullscreen window to another screen at all —
    /// the request only updated its bookkeeping, and leaving fullscreen first
    /// raced macOS restoring the window's old frame, so outputs stayed on
    /// whichever screen they started on. A covering window moves instantly
    /// and deterministically, the way Resolume and `MadMapper` do outputs.
    ///
    /// Done on the main thread (see [`OsWindow::on_main`]).
    #[cfg(target_os = "macos")]
    fn cover_screen(&mut self, monitor: Option<winit::monitor::MonitorHandle>) {
        self.covering_screen = true;
        // Watched afresh: what went unshown while it was a plain window says
        // nothing about its screen — nor about this one whether another said
        // what it showed.
        self.unshown = super::on_screen::Unshown::new(std::time::Instant::now(), self.presents);
        self.screen_says = ScreenSays::Counting(0);
        let name = self.name.clone();
        self.window.on_main(move |window| {
            let Some(monitor) = monitor.or_else(|| window.current_monitor()) else {
                log::warn!("Output '{name}': no screen to cover");
                return;
            };
            if window.fullscreen().is_some() {
                window.set_fullscreen(None);
            }
            window.set_decorations(false);
            macos_window::cover(window, &monitor);
            macos_window::hide_pointer_over(window);
        });
    }

    /// macOS: back to an ordinary, decorated window on the same screen.
    #[cfg(target_os = "macos")]
    fn uncover_screen(&mut self) {
        let covering = std::mem::take(&mut self.covering_screen);
        // A plain window again: not watched, and not made again for what it
        // missed while covering (one asked for still is).
        self.unshown = super::on_screen::Unshown::new(std::time::Instant::now(), self.presents);
        if self.reopen == Some(Reopen::Unshown) {
            self.reopen = None;
        }
        self.window.on_main(move |window| {
            if window.fullscreen().is_some() {
                window.set_fullscreen(None);
            }
            if !covering {
                return;
            }
            macos_window::uncover(window);
            macos_window::show_pointer_over(window.id());
            window.set_decorations(true);
            let _ = window.request_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        });
    }

    /// Operator-facing target: the configured display, marked while this
    /// window is still standing in for it.
    pub fn target_label(&self) -> String {
        match &self.awaiting_display {
            Some(awaited) => format!("{} (waiting, windowed)", awaited.monitor),
            None => self.target.to_string(),
        }
    }

    /// The target to persist: the monitor this output is waiting for, if any,
    /// rather than the windowed stand-in it shows until that monitor connects.
    pub fn configured_target(&self) -> OutputTarget {
        configured_target(
            &self.target,
            self.awaiting_display.as_ref().map(|a| a.monitor.as_str()),
        )
    }

    /// Destroy this output window, closing the OS window and reclaiming leaked memory.
    /// Must be called instead of just dropping the struct if you want the window to close.
    /// The window itself closes on the main thread (see [`OsWindow::on_main`]).
    pub fn destroy(self) {
        // Nothing more presented to it: a job in flight lets go, and one
        // waiting for its presenter is dropped with it.
        self.present_stats
            .closed
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.presenter.stop();
        // Drop surface first (it references the window)
        drop(self.surface);
        self.window.on_main(|window| {
            #[cfg(target_os = "macos")]
            macos_window::show_pointer_over(window.id());
            // Reclaim the leaked Box<Window> so it gets dropped, which closes the OS window.
            // Safety: the pointer was created by Box::leak in create_pending_outputs,
            // and we are the sole owner (no other references exist after removal from the vec).
            let window_ptr = std::ptr::from_ref::<Window>(window).cast_mut();
            unsafe {
                let _ = Box::from_raw(window_ptr);
            }
        });
    }
}

use super::blit::{BlitPipeline, PolygonBlitPipeline};

/// Calibration card colors for distinct surface identification.
/// Each surface gets a different accent color for its test card.
const CALIBRATION_COLORS: [[u8; 3]; 8] = [
    [255, 80, 80],   // Red
    [80, 200, 120],  // Green
    [80, 140, 255],  // Blue
    [255, 200, 60],  // Yellow
    [200, 80, 255],  // Purple
    [80, 220, 220],  // Cyan
    [255, 140, 60],  // Orange
    [255, 100, 180], // Pink
];

/// Gradient bars along a calibration card's foot.
const CALIBRATION_BARS: u32 = 6;

/// The levels strip's codes: full black, video black, video white, full white.
const LEVEL_PATCHES: [u8; 4] = [0, 16, 235, 255];

/// Generate a calibration test card as RGBA pixel data, in display codes
/// (sRGB): as they are, they are the codes a screen at full levels gets.
///
/// Everything lives inside the border/corner brackets:
/// - **Grid + crosshair + circle** (upper ~70% of interior)
/// - **Gradient bars** (lower ~30% of interior): grayscale, R, G, B, stepped
///   gray, and a levels strip — codes 0, 16, 235 and 255 side by side. On a
///   screen that reads video levels (a switcher's input) an output at full
///   levels shows 0 and 16 alike, black, and 235 and 255 alike, white; in
///   video levels (`PresentationRequest::video_levels`) all four apart.
///
/// Each surface gets a distinct accent color border for identification.
// gx/gy, grid_h/grad_h and at_tl/at_tr/at_bl/at_br are the clearest names for this 2D geometry.
#[allow(clippy::similar_names)]
pub fn generate_calibration_card(width: u32, height: u32, color_index: usize) -> Vec<u8> {
    let [cr, cg, cb] = CALIBRATION_COLORS[color_index % CALIBRATION_COLORS.len()];
    let mut pixels = vec![0u8; (width * height * 4) as usize];

    let bg = [20u8, 20, 30, 255];
    let border_color = [cr, cg, cb, 255];
    let grid_color = [cr / 3, cg / 3, cb / 3, 255];
    let grid_bright = [cr / 2, cg / 2, cb / 2, 255];
    let center_color = [255u8, 255, 255, 200];
    let corner_color = [255u8, 255, 255, 255];

    let border_w = (width.min(height) / 40).max(2);
    let _corner_size = (width.min(height) / 8).max(8);

    // Interior content region (inside border)
    let inset = border_w + 1;
    let inner_w = width.saturating_sub(inset * 2);
    let inner_h = height.saturating_sub(inset * 2);

    // Split interior: top 70% = grid zone, bottom 30% = gradient bars
    let grid_h = (inner_h as f32 * 0.70) as u32;
    let grad_h = inner_h - grid_h;
    let bar_h = grad_h / CALIBRATION_BARS;
    let grid_zone_bottom = inset + grid_h;

    // Crosshair centered on FULL card (not just grid zone)
    let cx = width / 2;
    let cy = height / 2;
    let cross_len = height.min(inner_w) / 4;
    let cross_thick = (width.min(height) / 200).max(1);

    // Corner brackets sit at the very edge of the output (pixel 0)
    let bracket_len = (width.min(height) / 6).max(10);
    let bracket_thick = (width.min(height) / 80).max(2);

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let mut color = bg;

            let inside = x >= inset && x < width - inset && y >= inset && y < height - inset;

            if inside {
                // === Gradient bars (bottom 30% of interior) ===
                if y >= grid_zone_bottom {
                    let bar_idx = (y - grid_zone_bottom) / bar_h.max(1);
                    let t = (x - inset) as f32 / inner_w.max(1) as f32;
                    let v = (t * 255.0) as u8;

                    color = match bar_idx {
                        0 => [v, v, v, 255], // Grayscale
                        1 => [v, 0, 0, 255], // Red
                        2 => [0, v, 0, 255], // Green
                        3 => [0, 0, v, 255], // Blue
                        4 => {
                            // 16-step gray
                            let step = (t * 16.0).floor().min(15.0) as u8;
                            let sv = step * 17;
                            [sv, sv, sv, 255]
                        }
                        _ => {
                            // Levels: 0, 16, 235, 255.
                            let code = LEVEL_PATCHES[((t * 4.0) as usize).min(3)];
                            [code, code, code, 255]
                        }
                    };
                }
                // === Grid zone (top 70% of interior) ===
                else {
                    let gx_norm = (x - inset) as f32 / inner_w.max(1) as f32;
                    let gy_norm = (y - inset) as f32 / grid_h.max(1) as f32;

                    // 8×8 grid
                    let gx_frac = (gx_norm * 8.0).fract();
                    let gy_frac = (gy_norm * 8.0).fract();
                    if !(0.02..=0.98).contains(&gx_frac) || !(0.02..=0.98).contains(&gy_frac) {
                        color = grid_color;
                    }

                    // Sub-grid
                    if (gx_frac - 0.5).abs() < 0.01 || (gy_frac - 0.5).abs() < 0.01 {
                        color = [grid_color[0] / 2, grid_color[1] / 2, grid_color[2] / 2, 180];
                    }
                }
            }

            // Center crosshair — spans full card, drawn on top of everything except corners
            if (x.abs_diff(cx) <= cross_thick && y.abs_diff(cy) <= cross_len)
                || (y.abs_diff(cy) <= cross_thick && x.abs_diff(cx) <= cross_len)
            {
                color = center_color;
            }

            // Center circle
            let dx = x as f32 - cx as f32;
            let dy = y as f32 - cy as f32;
            let dist = (dx * dx + dy * dy).sqrt();
            if (dist - cross_len as f32 * 0.6).abs() < 1.5 {
                color = border_color;
            }

            // Edge midpoint markers (on the border itself)
            let edge_pts = [
                (cx, 0u32),       // top center
                (cx, height - 1), // bottom center
                (0u32, cy),       // left center
                (width - 1, cy),  // right center
            ];
            for (ex, ey) in edge_pts {
                if (x.abs_diff(ex) <= cross_thick * 3 && y.abs_diff(ey) <= border_w + 4)
                    || (y.abs_diff(ey) <= cross_thick * 3 && x.abs_diff(ex) <= border_w + 4)
                {
                    color = grid_bright;
                }
            }

            // Border
            if x < border_w || x >= width - border_w || y < border_w || y >= height - border_w {
                color = border_color;
            }

            // Corner brackets at the very edge (pixel 0) — drawn LAST, on top of border
            let at_tl = x < bracket_len && y < bracket_len;
            let at_tr = x >= width - bracket_len && y < bracket_len;
            let at_br = x >= width - bracket_len && y >= height - bracket_len;
            let at_bl = x < bracket_len && y >= height - bracket_len;
            if at_tl || at_tr || at_br || at_bl {
                let on_h = y < bracket_thick || y >= height - bracket_thick;
                let on_v = x < bracket_thick || x >= width - bracket_thick;
                if on_h || on_v {
                    color = corner_color;
                }
            }

            pixels[idx..idx + 4].copy_from_slice(&color);
        }
    }
    pixels
}

/// Create calibration card textures for N colors, returning (texture, view) pairs.
pub fn create_calibration_textures(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    count: usize,
) -> Vec<(wgpu::Texture, wgpu::TextureView)> {
    let card_w = 512u32;
    let card_h = 512u32;

    (0..count)
        .map(|i| {
            let pixels = generate_calibration_card(card_w, card_h, i);
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some(&format!("Calibration Card {i}")),
                size: wgpu::Extent3d {
                    width: card_w,
                    height: card_h,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                // Display codes, read as such: a code on the card is that code on
                // a screen at full levels.
                format: wgpu::TextureFormat::Rgba8UnormSrgb,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &pixels,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(4 * card_w),
                    rows_per_image: Some(card_h),
                },
                wgpu::Extent3d {
                    width: card_w,
                    height: card_h,
                    depth_or_array_layers: 1,
                },
            );
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            (texture, view)
        })
        .collect()
}

// ── Headless Output ─────────────────────────────────────────────────
// Codec value types (`RecordingCodec`, `SrtCodec`, `StreamingCodec`) and the
// unified `OutputTarget` are defined in the framework-free `config` module and
// re-exported at the top of this file.

/// A live audio passthrough subscription held by an active output, used to
/// unsubscribe on stop and to report passthrough health (dropped chunks).
/// See spec/audio-passthrough.md.
pub struct AudioPassthrough {
    /// The capture device this output is tee'd from, or `None` when it
    /// carries an audio-mixer bus instead.
    pub source_id: Option<crate::audio::AudioSourceId>,
    /// Subscription token, for unsubscribe on stop.
    pub token: crate::audio::PcmToken,
    /// PCM chunks dropped on backpressure (producer side health stat).
    pub dropped: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// What the output is carrying, as shown: a device name or `Bus: <name>`.
    pub label: String,
}

impl AudioPassthrough {
    /// Give the subscription back. A capture device is released so it can
    /// close; a mixer bus tap ends by itself once the encoder's end drops.
    pub fn release(self, capture: &mut crate::audio::AudioManager) {
        if let Some(id) = self.source_id {
            capture.unsubscribe_pcm(id, self.token);
        }
    }
}

/// A headless output renders content to a GPU texture, reads it back to CPU,
/// and sends it to an external target (NDI, Syphon, recording, SRT).
///
/// Unlike `OutputWindow`, this has no OS window or surface — it renders
/// offscreen via `ReadbackBuffer`.
pub struct HeadlessOutput {
    /// Stable UUID (8-char hex)
    pub uuid: String,
    /// Human-readable name for this output
    pub name: String,
    /// What content to render (Master, Channel, Deck, etc.)
    pub source: OutputSource,
    /// The audio-mixer bus a recording or stream carries. Wins over the
    /// target's own capture device when set.
    pub audio_bus: Option<String>,
    /// GPU readback infrastructure (double-buffered staging)
    pub readback: super::ReadbackBuffer,
    /// Where to send the readback frames (unified target)
    pub target: OutputTarget,
    /// Offscreen render texture (`COPY_SRC` for readback)
    pub texture: wgpu::Texture,
    /// View into the offscreen render texture
    pub texture_view: wgpu::TextureView,
    /// Blit pipeline for copying source content into the offscreen texture
    pub blit_pipeline: BlitPipeline,
    /// Polygon pipeline for rendering assigned surfaces with warp
    pub polygon_pipeline: PolygonBlitPipeline,
    /// Width of the output
    pub width: u32,
    /// Height of the output
    pub height: u32,
    /// Active ffmpeg subprocess (for Recording/SRT targets). Boxed for the same
    /// reason as `audio_pcm`, and because its inline size is platform-dependent:
    /// unboxed it made `HeadlessOutput` 224 bytes larger than `OutputWindow` on
    /// Windows, tripping `clippy::large_enum_variant` on that platform only.
    pub subprocess: Option<Box<super::FfmpegSubprocess>>,
    /// A Blackmagic output's device while it runs (see `crate::decklink`).
    pub decklink: Option<Box<crate::decklink::DeckLinkSender>>,
    /// A Blackmagic output the operator started and has not stopped: saved
    /// running, and started again as the stage opens — and, stopped by
    /// itself meanwhile (its device unplugged, or missing as the stage
    /// opened), when its device appears again.
    pub keep_running: bool,
    /// Since when a Blackmagic output saved running has waited for its
    /// device as the stage opened: said only if it waits on — discovery
    /// tells of a device a moment after the stage is open.
    pub waiting_since: Option<std::time::Instant>,
    /// The test card in place of its picture (`Projector`), as an output
    /// window's — a Blackmagic output's, to check its levels on the device.
    pub calibration_mode: CalibrationMode,
    /// Active audio passthrough subscription (None = video-only). Boxed to keep
    /// the rarely-set field off the hot `UnifiedOutput` enum's size.
    pub audio_pcm: Option<Box<AudioPassthrough>>,
    /// Whether this output is actively streaming/recording
    pub active: bool,
    /// When this output was started (for duration tracking on non-subprocess outputs)
    pub started_at: Option<std::time::Instant>,
    /// Surface assignments — which surfaces this output renders, with per-surface warp.
    /// Empty = render source directly (fallback behavior).
    pub surface_assignments: Vec<SurfaceAssignment>,
    /// LED screens this output's feed carries; when set, it replaces the
    /// source and surfaces.
    pub led: Option<crate::engine::value::led::LedLayout>,
    /// Whether edge blend is auto-computed or manually configured.
    pub edge_blend_mode: super::edge_blend::EdgeBlendMode,
    /// Edge blending configuration for multi-projector overlap zones.
    pub edge_blend: super::edge_blend::EdgeBlendConfig,
    /// GPU pipeline for applying edge blend post-process.
    pub edge_blend_pipeline: super::edge_blend::EdgeBlendPipeline,
    /// Intermediate texture used when edge blending is active.
    pub edge_blend_texture: wgpu::Texture,
    /// View into the intermediate edge blend texture.
    pub edge_blend_texture_view: wgpu::TextureView,
    /// Per-output rotation applied at the final blit stage.
    pub rotation: OutputRotation,
    /// Persisted precision and dithering request.
    pub presentation_request: PresentationRequest,
    /// Runtime format selected for the active adapter.
    pub resolved_presentation: ResolvedPresentation,
    /// Turns each frame to NV12 on the GPU for an encoder that takes it (a
    /// recording or the stream), in place of the RGBA readback.
    pub nv12: Option<super::nv12::Nv12Converter>,
}

/// Result of delivering a frame to an output target.
pub enum DeliveryResult {
    /// Frame delivered successfully (or no-op for unhandled targets).
    Ok,
    /// Subprocess write failed — output should be deactivated.
    Failed(String),
    /// SRT client disconnected: the old subprocess has been stopped and the
    /// caller must respawn the listener. The caller owns the respawn (rather
    /// than this method) so it can re-subscribe audio passthrough — `deliver_frame`
    /// has no `AudioManager` handle.
    SrtNeedsRestart,
}

impl HeadlessOutput {
    fn storage_formats(
        presentation: &ResolvedPresentation,
    ) -> (wgpu::TextureFormat, super::ReadbackFormat) {
        match presentation.pixel_format {
            PresentationPixelFormat::Rgba16 => (
                wgpu::TextureFormat::Rgba16Unorm,
                super::ReadbackFormat::Rgba16Unorm,
            ),
            PresentationPixelFormat::P216 => (
                wgpu::TextureFormat::Rgba16Float,
                super::ReadbackFormat::P216,
            ),
            _ if presentation.resolved == PresentationDepth::Sdr10 => (
                wgpu::TextureFormat::Rgb10a2Unorm,
                super::ReadbackFormat::Rgb10A2,
            ),
            _ => (
                wgpu::TextureFormat::Rgba8UnormSrgb,
                super::ReadbackFormat::Rgba8,
            ),
        }
    }

    fn readback_alpha_mode(presentation: &ResolvedPresentation) -> AlphaMode {
        if presentation.pixel_format == PresentationPixelFormat::Rgba16 {
            AlphaMode::Premultiplied
        } else {
            presentation.alpha_mode
        }
    }

    /// Deliver readback frame data to the configured output target.
    ///
    /// For subprocess targets (Recording, SRT, HLS, DASH, RTMP), feeds the frame to ffmpeg.
    /// For NDI/Syphon, publishes directly through the respective manager.
    /// Returns a `DeliveryResult` indicating what happened.
    ///
    /// `fps` is the master render rate, used by NDI to declare its frame rate.
    /// ffmpeg targets already fixed theirs at spawn.
    pub fn deliver_frame(
        &mut self,
        frame_data: &[u8],
        ndi_manager: &mut crate::ndi::NdiManager,
        fps: u32,
    ) -> DeliveryResult {
        match &mut self.target {
            OutputTarget::Recording { .. }
            | OutputTarget::HlsStream { .. }
            | OutputTarget::DashStream { .. }
            | OutputTarget::RtmpStream { .. } => {
                if let Some(sub) = &mut self.subprocess {
                    if !sub.feed_frame(frame_data) {
                        if let Some(mut sub) = self.subprocess.take() {
                            sub.stop();
                        }
                        return DeliveryResult::Failed(format!(
                            "Subprocess write failed for '{}'",
                            self.name
                        ));
                    }
                }
                DeliveryResult::Ok
            }
            OutputTarget::SrtStream { .. } => {
                if let Some(sub) = &mut self.subprocess {
                    if !sub.feed_frame(frame_data) {
                        // Client disconnected. Tear down the dead listener and
                        // hand the respawn back to the caller, which re-subscribes
                        // audio passthrough (this method has no AudioManager).
                        if let Some(mut sub) = self.subprocess.take() {
                            sub.stop();
                        }
                        return DeliveryResult::SrtNeedsRestart;
                    }
                }
                DeliveryResult::Ok
            }
            OutputTarget::NdiSend { ref sender_name } => {
                ndi_manager.send_frame(sender_name, frame_data, self.width, self.height, fps);
                DeliveryResult::Ok
            }
            // Handed to the device's own thread (see `crate::decklink`).
            OutputTarget::DeckLink { .. } => {
                let Some(sender) = &self.decklink else {
                    return DeliveryResult::Ok;
                };
                if self.width == sender.width && self.height == sender.height {
                    sender.send(frame_data);
                }
                // Said as "Output '…' stopped: {why}" (`stop_headless_output`).
                match sender.failed() {
                    Some(why) => DeliveryResult::Failed(why),
                    None => DeliveryResult::Ok,
                }
            }
            // Syphon output is published GPU-side (zero-copy) in the headless
            // render loop before this point, so on macOS it never reaches here.
            #[cfg(not(target_os = "macos"))]
            OutputTarget::SyphonServer { .. } => {
                log::warn!("Syphon output not supported on this platform");
                DeliveryResult::Ok
            }
            _ => DeliveryResult::Ok,
        }
    }

    /// Create a new headless output with the given resolution and target.
    ///
    /// # Panics
    ///
    /// Panics if the blit, polygon or edge-blend pipeline cannot be built for
    /// the headless `Rgba8UnormSrgb` format — a device-level failure that
    /// leaves the output unusable.
    pub fn new(
        device: &wgpu::Device,
        name: String,
        source: OutputSource,
        target: OutputTarget,
        width: u32,
        height: u32,
    ) -> Self {
        let presentation_request = PresentationRequest::default();
        let resolved_presentation = resolve_headless_presentation(presentation_request, &target);
        let (format, readback_format) = Self::storage_formats(&resolved_presentation);
        let (texture, texture_view, eb_tex, eb_view) =
            Self::create_textures(device, width, height, format);
        let readback = super::ReadbackBuffer::new(device, width, height, readback_format);
        let blit_pipeline =
            BlitPipeline::new(device, format).expect("Failed to create headless blit pipeline");
        let polygon_pipeline = PolygonBlitPipeline::new(device, format)
            .expect("Failed to create headless polygon pipeline");
        let edge_blend_pipeline = super::edge_blend::EdgeBlendPipeline::new(device, format)
            .expect("Failed to create headless edge blend pipeline");
        Self {
            uuid: crate::deck::generate_short_uuid(),
            name,
            source,
            audio_bus: None,
            readback,
            target,
            texture,
            texture_view,
            blit_pipeline,
            polygon_pipeline,
            width,
            height,
            subprocess: None,
            decklink: None,
            keep_running: false,
            waiting_since: None,
            calibration_mode: CalibrationMode::Off,
            audio_pcm: None,
            active: false,
            started_at: None,
            surface_assignments: Vec::new(),
            led: None,
            edge_blend_mode: super::edge_blend::EdgeBlendMode::default(),
            edge_blend: super::edge_blend::EdgeBlendConfig::default(),
            edge_blend_pipeline,
            edge_blend_texture: eb_tex,
            edge_blend_texture_view: eb_view,
            rotation: OutputRotation::default(),
            presentation_request,
            resolved_presentation,
            nv12: None,
        }
    }

    /// Keep an NV12 converter for this output's size when `wanted` (its
    /// encoder takes NV12), and none otherwise.
    pub fn keep_nv12(&mut self, device: &wgpu::Device, wanted: bool) {
        if !wanted {
            self.nv12 = None;
        } else if self
            .nv12
            .as_ref()
            .is_none_or(|c| c.dimensions() != (self.width, self.height))
        {
            self.nv12 = super::nv12::Nv12Converter::new(device, self.width, self.height);
        }
    }

    /// The render target and the edge-blend intermediate, which are the only
    /// two resources whose size depends on the output dimensions. The pipelines
    /// are size-independent, so [`resize`](Self::resize) rebuilds just these.
    fn create_textures(
        device: &wgpu::Device,
        width: u32,
        height: u32,
        format: wgpu::TextureFormat,
    ) -> (
        wgpu::Texture,
        wgpu::TextureView,
        wgpu::Texture,
        wgpu::TextureView,
    ) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Headless Output Texture"),
            size: wgpu::Extent3d {
                width: width.max(1),
                height: height.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let eb_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Headless Edge Blend Intermediate"),
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
        let eb_view = eb_tex.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, texture_view, eb_tex, eb_view)
    }

    /// Rebuild this output's GPU resources at a new size.
    ///
    /// A headless output used to take the render resolution once, when it was
    /// created, and never look at it again. A recording made before the project
    /// was switched to a portrait resolution therefore kept a landscape buffer,
    /// the portrait composite was stretch-blitted into it, and the saved file
    /// came out the old shape with squashed content. The same held for NDI and
    /// Syphon, which published the stale dimensions.
    ///
    /// Callers must stop any subprocess-backed target before resizing: ffmpeg is
    /// spawned with a fixed `-s WxH` and raw frames of a different size desync
    /// the stream rather than being rejected. NDI and Syphon take the dimensions
    /// per frame and need no restart.
    ///
    /// A resize to the current size is a no-op, so this is safe to call
    /// unconditionally.
    pub fn resize(&mut self, device: &wgpu::Device, width: u32, height: u32) {
        if width == 0 || height == 0 || (width == self.width && height == self.height) {
            return;
        }
        let (format, readback_format) = Self::storage_formats(&self.resolved_presentation);
        let (texture, texture_view, eb_tex, eb_view) =
            Self::create_textures(device, width, height, format);
        self.texture = texture;
        self.texture_view = texture_view;
        self.edge_blend_texture = eb_tex;
        self.edge_blend_texture_view = eb_view;
        self.readback = super::ReadbackBuffer::new_with_contract(
            device,
            width,
            height,
            readback_format,
            self.resolved_presentation.color_profile,
            Self::readback_alpha_mode(&self.resolved_presentation),
        );
        self.width = width;
        self.height = height;
    }

    /// Set output rotation. Headless outputs don't have intermediate textures to rebuild,
    /// but the rotation is stored for the blit shader.
    pub fn set_rotation(&mut self, rotation: OutputRotation) {
        self.rotation = rotation;
    }

    /// Update the persisted request and resolve it against the active adapter.
    ///
    /// # Panics
    ///
    /// Panics if a format-bound GPU pipeline cannot be rebuilt for the resolved
    /// presentation format.
    pub fn set_presentation_request(
        &mut self,
        device: &wgpu::Device,
        request: PresentationRequest,
    ) {
        self.presentation_request = request;
        let resolved = resolve_headless_presentation(request, &self.target);
        self.set_resolved_presentation(device, resolved);
    }

    /// Apply an adapter-resolved presentation and rebuild format-bound GPU resources.
    ///
    /// # Panics
    ///
    /// Panics if a format-bound GPU pipeline cannot be rebuilt for the resolved
    /// presentation format.
    pub fn set_resolved_presentation(
        &mut self,
        device: &wgpu::Device,
        resolved: ResolvedPresentation,
    ) {
        let (format, readback_format) = Self::storage_formats(&resolved);
        if self.texture.format() != format
            || self.readback.format() != readback_format
            || self.readback.color_profile() != resolved.color_profile
            || self.readback.alpha_mode() != Self::readback_alpha_mode(&resolved)
        {
            let (texture, texture_view, eb_tex, eb_view) =
                Self::create_textures(device, self.width, self.height, format);
            self.texture = texture;
            self.texture_view = texture_view;
            self.edge_blend_texture = eb_tex;
            self.edge_blend_texture_view = eb_view;
            self.readback = super::ReadbackBuffer::new_with_contract(
                device,
                self.width,
                self.height,
                readback_format,
                resolved.color_profile,
                Self::readback_alpha_mode(&resolved),
            );
            self.blit_pipeline = BlitPipeline::new(device, format)
                .expect("Failed to rebuild headless blit pipeline");
            self.polygon_pipeline = PolygonBlitPipeline::new(device, format)
                .expect("Failed to rebuild headless polygon pipeline");
            self.edge_blend_pipeline = super::edge_blend::EdgeBlendPipeline::new(device, format)
                .expect("Failed to rebuild headless edge blend pipeline");
        }
        self.resolved_presentation = resolved;
    }
}

/// Unified output — wraps either a windowed or headless output.
/// Provides shared accessors for name, target, and source.
pub enum UnifiedOutput {
    Window(OutputWindow),
    Headless(HeadlessOutput),
}

impl UnifiedOutput {
    /// Stable UUID of this output.
    pub fn uuid(&self) -> &str {
        match self {
            UnifiedOutput::Window(w) => &w.uuid,
            UnifiedOutput::Headless(h) => &h.uuid,
        }
    }

    /// Human-readable name of this output.
    pub fn name(&self) -> &str {
        match self {
            UnifiedOutput::Window(w) => &w.name,
            UnifiedOutput::Headless(h) => &h.name,
        }
    }

    /// The output target for this output.
    pub fn target(&self) -> &OutputTarget {
        match self {
            UnifiedOutput::Window(w) => &w.target,
            UnifiedOutput::Headless(h) => &h.target,
        }
    }

    /// Whether this output is windowed.
    pub fn is_windowed(&self) -> bool {
        matches!(self, UnifiedOutput::Window(_))
    }

    /// Whether this output is headless.
    pub fn is_headless(&self) -> bool {
        matches!(self, UnifiedOutput::Headless(_))
    }

    /// Whether this headless output is actively streaming/recording.
    pub fn is_active(&self) -> bool {
        match self {
            UnifiedOutput::Window(_) => true, // windowed outputs are always "active"
            UnifiedOutput::Headless(h) => h.active,
        }
    }

    /// Mutable access to surface assignments for either variant.
    pub fn surface_assignments_mut(&mut self) -> &mut Vec<SurfaceAssignment> {
        match self {
            UnifiedOutput::Window(w) => &mut w.surface_assignments,
            UnifiedOutput::Headless(h) => &mut h.surface_assignments,
        }
    }

    /// Immutable access to surface assignments for either variant.
    pub fn surface_assignments(&self) -> &[SurfaceAssignment] {
        match self {
            UnifiedOutput::Window(w) => &w.surface_assignments,
            UnifiedOutput::Headless(h) => &h.surface_assignments,
        }
    }

    /// Current edge blend mode.
    pub fn edge_blend_mode(&self) -> super::edge_blend::EdgeBlendMode {
        match self {
            UnifiedOutput::Window(w) => w.edge_blend_mode,
            UnifiedOutput::Headless(h) => h.edge_blend_mode,
        }
    }

    /// Current edge blend config.
    pub fn edge_blend(&self) -> super::edge_blend::EdgeBlendConfig {
        match self {
            UnifiedOutput::Window(w) => w.edge_blend,
            UnifiedOutput::Headless(h) => h.edge_blend,
        }
    }

    /// Current output rotation.
    pub fn rotation(&self) -> OutputRotation {
        match self {
            UnifiedOutput::Window(w) => w.rotation,
            UnifiedOutput::Headless(h) => h.rotation,
        }
    }

    /// The LED screens this output's feed carries, if it is an LED feed.
    pub fn led(&self) -> Option<&crate::engine::value::led::LedLayout> {
        match self {
            UnifiedOutput::Window(w) => w.led.as_ref(),
            UnifiedOutput::Headless(h) => h.led.as_ref(),
        }
    }

    /// Make this output an LED feed carrying `layout`, or (`None`) an
    /// ordinary output again.
    pub fn set_led(&mut self, layout: Option<crate::engine::value::led::LedLayout>) {
        match self {
            UnifiedOutput::Window(w) => w.led = layout,
            UnifiedOutput::Headless(h) => h.led = layout,
        }
    }

    /// Persisted precision and dithering request.
    pub fn presentation_request(&self) -> PresentationRequest {
        match self {
            UnifiedOutput::Window(w) => w.presentation_request,
            UnifiedOutput::Headless(h) => h.presentation_request,
        }
    }

    /// What a headless output sends when it has no surfaces assigned: the
    /// master or one bus's program. `None` for a window, whose picture comes
    /// from its surfaces (a bus reaches a screen through the switcher).
    pub fn source(&self) -> Option<&OutputSource> {
        match self {
            UnifiedOutput::Window(_) => None,
            UnifiedOutput::Headless(h) => Some(&h.source),
        }
    }

    /// Runtime format selected by the active output adapter.
    pub fn resolved_presentation(&self) -> &ResolvedPresentation {
        match self {
            UnifiedOutput::Window(w) => &w.resolved_presentation,
            UnifiedOutput::Headless(h) => &h.resolved_presentation,
        }
    }

    /// Apply a new request through the variant's resolver.
    ///
    /// # Errors
    ///
    /// Returns an error when a window surface cannot be configured for any SDR format.
    pub fn set_presentation_request(
        &mut self,
        context: &GpuContext,
        request: PresentationRequest,
    ) -> Result<()> {
        match self {
            UnifiedOutput::Window(w) => w.set_presentation_request(context, request),
            UnifiedOutput::Headless(h) => {
                h.set_presentation_request(&context.device, request);
                Ok(())
            }
        }
    }

    /// Active duration for headless outputs (subprocess or NDI/Syphon).
    pub fn active_duration(&self) -> std::time::Duration {
        match self {
            UnifiedOutput::Window(_) => std::time::Duration::ZERO,
            UnifiedOutput::Headless(h) => {
                // Subprocess-based outputs (Recording/SRT) track their own duration
                if let Some(sub) = &h.subprocess {
                    return sub.duration();
                }
                // Non-subprocess outputs (NDI/Syphon) use started_at timestamp
                h.started_at.map(|t| t.elapsed()).unwrap_or_default()
            }
        }
    }
}

/// The two `NSWindow` settings winit does not expose, for output windows that
/// cover a screen (see `OutputWindow::cover_screen`).
#[cfg(target_os = "macos")]
mod macos_window {
    use std::cell::Cell;

    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyObject, Bool, NSObject};
    use objc2::{class, define_class, msg_send, AllocAnyThread, DefinedClass, Message};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// `NSTrackingMouseEnteredAndExited | NSTrackingActiveAlways |
    /// NSTrackingInVisibleRect`: told when the pointer comes and goes, whether
    /// or not the window or the app is the active one, over the whole view.
    const TRACK_POINTER: usize = 0x01 | 0x80 | 0x200;

    thread_local! {
        /// The output windows hiding the pointer, by window. Main thread
        /// only, like the windows and tracking areas themselves.
        static HIDERS: std::cell::RefCell<std::collections::HashMap<winit::window::WindowId, PointerHider>> =
            std::cell::RefCell::default();
    }

    /// Hide the pointer while it is over `window` (main thread).
    pub(super) fn hide_pointer_over(window: &winit::window::Window) {
        let id = window.id();
        HIDERS.with_borrow_mut(|hiders| {
            if let std::collections::hash_map::Entry::Vacant(slot) = hiders.entry(id) {
                if let Some(hider) = PointerHider::over(window) {
                    slot.insert(hider);
                }
            }
        });
    }

    /// Stop hiding the pointer over window `id`, showing it if it was hidden
    /// (main thread).
    pub(super) fn show_pointer_over(id: winit::window::WindowId) {
        let hider = HIDERS.with_borrow_mut(|hiders| hiders.remove(&id));
        drop(hider);
    }

    /// Hides the pointer while it is over one output window. macOS gives a
    /// window that is not the key one no cursor of its own — winit's
    /// `set_cursor_visible` works only while the window has focus, and the
    /// control window has it — so the pointer is hidden on entering and shown
    /// again on leaving, and always when this goes.
    struct PointerHider {
        view: Retained<AnyObject>,
        area: Retained<AnyObject>,
        owner: Retained<HiderOwner>,
    }

    impl PointerHider {
        fn over(window: &winit::window::Window) -> Option<Self> {
            let view = ns_view(window)?.retain();
            let owner = HiderOwner::new();
            let rect = objc2_foundation::NSRect::new(
                objc2_foundation::NSPoint::new(0.0, 0.0),
                objc2_foundation::NSSize::new(0.0, 0.0),
            );
            // Safety: an NSTrackingArea made the documented way, owned by an
            // object answering `mouseEntered:`/`mouseExited:`, added to a live
            // view on the main thread (window calls run on the event loop).
            let area: Retained<AnyObject> = unsafe {
                let alloc: Allocated<AnyObject> = msg_send![class!(NSTrackingArea), alloc];
                msg_send![
                    alloc,
                    initWithRect: rect,
                    options: TRACK_POINTER,
                    owner: &*owner,
                    userInfo: None::<&AnyObject>
                ]
            };
            // Safety: as above.
            unsafe {
                let _: () = msg_send![&*view, addTrackingArea: &*area];
            }
            Some(Self { view, area, owner })
        }
    }

    impl Drop for PointerHider {
        fn drop(&mut self) {
            // Safety: the view is kept alive by `self.view`; the area is ours.
            unsafe {
                let _: () = msg_send![&*self.view, removeTrackingArea: &*self.area];
            }
            self.owner.show();
        }
    }

    struct HiderIvars {
        hidden: Cell<bool>,
    }

    define_class!(
        // SAFETY:
        // - NSObject has no subclassing requirements.
        // - The class does not implement Drop.
        // - Only the main thread's event handling calls it.
        #[unsafe(super(NSObject))]
        #[name = "NidaOutputPointerHider"]
        #[ivars = HiderIvars]
        struct HiderOwner;

        impl HiderOwner {
            #[unsafe(method(mouseEntered:))]
            fn mouse_entered(&self, _event: &AnyObject) {
                self.hide();
            }

            #[unsafe(method(mouseExited:))]
            fn mouse_exited(&self, _event: &AnyObject) {
                self.show();
            }
        }
    );

    impl HiderOwner {
        fn new() -> Retained<Self> {
            let this = Self::alloc().set_ivars(HiderIvars { hidden: Cell::new(false) });
            // Safety: NSObject's designated initialiser.
            unsafe { msg_send![super(this), init] }
        }

        /// `NSCursor hide`/`unhide` are counted, so each is sent once a turn.
        fn hide(&self) {
            if !self.ivars().hidden.replace(true) {
                // Safety: a class method without arguments.
                unsafe {
                    let _: () = msg_send![class!(NSCursor), hide];
                }
            }
        }

        fn show(&self) {
            if self.ivars().hidden.replace(false) {
                // Safety: as above.
                unsafe {
                    let _: () = msg_send![class!(NSCursor), unhide];
                }
            }
        }
    }

    /// `NSScreenSaverWindowLevel`: above the menu bar (24), status items (25)
    /// and the Dock (20), so nothing the system draws lands on the picture.
    const COVERING_LEVEL: isize = 1000;
    /// `NSNormalWindowLevel`.
    const NORMAL_LEVEL: isize = 0;
    /// `CanJoinAllSpaces | Stationary | IgnoresCycle | FullScreenAuxiliary |
    /// CanJoinAllApplications`: on the screen whichever Space is showing,
    /// untouched by Mission Control, out of window cycling, and shown with
    /// whichever app is active under Stage Manager rather than staged with
    /// Nida's windows — on a laptop with Stage Manager on, an app switch left
    /// an output's every frame unshown (see `on_screen::Unshown`).
    const COVERING_BEHAVIOR: usize = (1 << 0) | (1 << 4) | (1 << 6) | (1 << 8) | (1 << 18);
    /// `NSWindowCollectionBehaviorDefault`.
    const NORMAL_BEHAVIOR: usize = 0;

    /// Raise the window above the menu bar, then give it `monitor`'s exact
    /// frame. In that order: below menu-bar level, macOS pushes a window's
    /// top down under the menu bar, which cut 30 pixels off the picture.
    pub(super) fn cover(window: &winit::window::Window, monitor: &winit::monitor::MonitorHandle) {
        use winit::platform::macos::MonitorHandleExtMacOS;
        let Some(ns_window) = ns_window(window) else {
            return;
        };
        // Safety: see `ns_window`; `ns_screen` is winit's live NSScreen for
        // this monitor, and these messages take and return plain values.
        unsafe {
            let _: () = msg_send![ns_window, setLevel: COVERING_LEVEL];
            let _: () = msg_send![ns_window, setCollectionBehavior: COVERING_BEHAVIOR];
            if let Some(screen) = monitor
                .ns_screen()
                .and_then(|s| s.cast::<AnyObject>().as_ref())
            {
                let frame: objc2_foundation::NSRect = msg_send![screen, frame];
                let _: () = msg_send![ns_window, setFrame: frame, display: Bool::YES];
            }
        }
    }

    /// Whether `window` is hidden — covered, minimised, on another Space —
    /// as `NSWindow.occlusionState` says (main thread).
    pub(super) fn occluded(window: &winit::window::Window) -> Option<bool> {
        /// `NSWindowOcclusionStateVisible`.
        const VISIBLE: usize = 1 << 1;
        let ns_window = ns_window(window)?;
        // Safety: see `ns_window`; a plain value.
        let state: usize = unsafe { msg_send![ns_window, occlusionState] };
        Some(state & VISIBLE == 0)
    }

    /// Back to an ordinary window level and Spaces behaviour.
    pub(super) fn uncover(window: &winit::window::Window) {
        let Some(ns_window) = ns_window(window) else {
            return;
        };
        // Safety: see `ns_window`.
        unsafe {
            let _: () = msg_send![ns_window, setLevel: NORMAL_LEVEL];
            let _: () = msg_send![ns_window, setCollectionBehavior: NORMAL_BEHAVIOR];
        }
    }

    /// The `NSWindow` behind a winit window. Window calls are made from the
    /// event loop, on the main thread, where `NSWindow` may be used, and the
    /// view winit hands out lives as long as the window.
    fn ns_window(window: &winit::window::Window) -> Option<&AnyObject> {
        let view = ns_view(window)?;
        // Safety: as above — a live NSView, on the main thread.
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            ns_window.as_ref()
        }
    }

    /// The `NSView` winit draws a window's content in (see [`ns_window`]).
    fn ns_view(window: &winit::window::Window) -> Option<&AnyObject> {
        let handle = window.window_handle().ok()?;
        let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
            return None;
        };
        // Safety: a live NSView for as long as the window, used on the main
        // thread.
        Some(unsafe { appkit.ns_view.cast().as_ref() })
    }
}

#[cfg(test)]
mod tests {
    /// A screen says what it shows once a second's frames are said shown —
    /// a few as the window starts, then none, do not tell — and one found
    /// never to say stays so whatever comes.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_screen_says_what_it_shows_after_a_seconds_frames() {
        use super::{ScreenSays, SAYS_FRAMES};
        let some = ScreenSays::Counting(0).with_shown(&[0.0, 12.5, 12.52, 0.0]);
        assert_eq!(some, ScreenSays::Counting(2), "two said shown: not yet");
        let none = some.with_shown(&[0.0; 40]);
        assert_eq!(none, ScreenSays::Counting(2), "then none");
        let shown: Vec<f64> = (0..SAYS_FRAMES).map(|i| 13.0 + f64::from(i) / 60.0).collect();
        assert_eq!(none.with_shown(&shown), ScreenSays::Yes, "a second's");
        assert_eq!(ScreenSays::Never.with_shown(&shown), ScreenSays::Never, "stays");
    }

    /// A picture a frame on its way to the screen holds is never drawn into
    /// again until the frame lets it go; there are never more than a frame
    /// waiting, one presented and one drawn need; and a window of another
    /// size or format starts afresh.
    #[test]
    fn a_picture_a_frame_holds_is_not_drawn_into_until_it_is_let_go() {
        use std::sync::Arc;
        let Ok(gpu) = super::GpuContext::new_headless() else {
            return;
        };
        let format = wgpu::TextureFormat::Bgra8Unorm;
        let blit = super::BlitPipeline::new(&gpu.device, format).expect("blit");
        let like = |width: u32, height: u32| {
            gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("window picture"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let window = like(64, 36);
        let mut pictures = super::PresentPictures::default();

        let held: Vec<_> = (0..super::PresentPictures::MOST)
            .map(|_| pictures.free(&gpu.device, &window, &blit).expect("a picture"))
            .collect();
        for (i, a) in held.iter().enumerate() {
            for b in &held[i + 1..] {
                assert!(!Arc::ptr_eq(a, b), "each frame a picture of its own");
            }
        }
        assert!(
            pictures.free(&gpu.device, &window, &blit).is_none(),
            "never more than {}",
            super::PresentPictures::MOST
        );

        let let_go = Arc::clone(&held[2]);
        drop(held);
        let again = pictures.free(&gpu.device, &window, &blit).expect("let go");
        let still_held = pictures.free(&gpu.device, &window, &blit).expect("let go");
        assert!(!Arc::ptr_eq(&again, &still_held));
        assert!(
            !Arc::ptr_eq(&again, &let_go) && !Arc::ptr_eq(&still_held, &let_go),
            "not the one a frame still holds"
        );

        let bigger = like(128, 72);
        let fresh = pictures.free(&gpu.device, &bigger, &blit).expect("a picture");
        assert_eq!((fresh.texture.width(), fresh.texture.height()), (128, 72));
    }

    /// An output's frames are held a frame's time only on a screen
    /// refreshing faster than the show: a 120 Hz screen at 60 fps, not a
    /// 60 Hz (or 59.94 Hz) one, nor one whose refresh is not known.
    #[test]
    fn frames_are_held_only_on_a_screen_faster_than_the_show() {
        let hz = |hz: f64| Some(std::time::Duration::from_secs_f64(1.0 / hz));
        let sixty = 1.0 / 60.0;
        assert!(super::refreshes_faster(hz(120.0), sixty));
        assert!(!super::refreshes_faster(hz(60.0), sixty));
        assert!(!super::refreshes_faster(hz(59.94), sixty));
        assert!(!super::refreshes_faster(None, sixty));
        assert!(
            super::refreshes_faster(hz(60.0), 1.0 / 30.0),
            "60 Hz at 30 fps"
        );
    }

    /// Presents over `frames` frames at `fps`, the `i`th frame `jitter(i)`
    /// seconds off its time, to a display refreshing at `hz`.
    fn presents(fps: f64, hz: f64, frames: usize, jitter: impl Fn(usize) -> f64) -> usize {
        let start = std::time::Instant::now();
        let refresh = Some(std::time::Duration::from_secs_f64(1.0 / hz));
        let mut pace = super::PresentPace::default();
        (0..frames)
            .filter(|&i| {
                let at = (i as f64 / fps + jitter(i)).max(0.0);
                pace.due(start + std::time::Duration::from_secs_f64(at), refresh)
            })
            .count()
    }

    /// An output presents as often as its display shows frames, never more:
    /// every frame on one as fast as the show, half on a 30 Hz TV, five in six
    /// at 50 Hz, and under a 60 fps show a 59.94 Hz TV misses one every ~17 s.
    #[test]
    fn an_output_presents_at_its_displays_rate() {
        let steady = |_| 0.0;
        assert_eq!(presents(60.0, 60.0, 600, steady), 600);
        assert!((300..=302).contains(&presents(60.0, 30.0, 600, steady)));
        assert!((500..=502).contains(&presents(60.0, 50.0, 600, steady)));
        let slow = presents(60.0, 59.94, 6000, steady);
        assert!((5993..=5997).contains(&slow), "{slow} of 6000");
        // Frames 4 ms early and late in turn: all shown.
        let uneven = |i: usize| if i.is_multiple_of(2) { -0.004 } else { 0.004 };
        assert_eq!(presents(60.0, 60.0, 600, uneven), 600);
        // A display whose rate is unknown gets every frame.
        let mut pace = super::PresentPace::default();
        let now = std::time::Instant::now();
        assert!((0..10).all(|_| pace.due(now, None)));
    }

    use super::{
        aspect_fit_rect, configured_target, fitted_in_format, resolve_headless_presentation,
        select_surface_presentation, HeadlessOutput, OutputRotation, OutputTarget, OutputWindow,
    };
    use crate::engine::value::render::{
        AlphaMode, PresentationColorProfile, PresentationDepth, PresentationPixelFormat,
        PresentationRequest, ResolvedPresentation,
    };

    fn surface_capabilities(
        format_capabilities: Vec<wgpu::SurfaceFormatCapabilities>,
    ) -> wgpu::SurfaceCapabilities {
        wgpu::SurfaceCapabilities {
            formats: vec![
                wgpu::TextureFormat::Bgra8UnormSrgb,
                wgpu::TextureFormat::Rgb10a2Unorm,
            ],
            format_capabilities,
            present_modes: vec![wgpu::PresentMode::Fifo],
            alpha_modes: vec![wgpu::CompositeAlphaMode::Opaque],
            usages: wgpu::TextureUsages::RENDER_ATTACHMENT,
        }
    }

    #[test]
    fn ten_bit_surface_requires_rgb10_and_srgb_as_a_pair() {
        let capabilities = surface_capabilities(vec![wgpu::SurfaceFormatCapabilities {
            format: wgpu::TextureFormat::Rgb10a2Unorm,
            color_spaces: wgpu::SurfaceColorSpaces::SRGB,
        }]);
        let selected = select_surface_presentation(
            PresentationRequest {
                depth: PresentationDepth::Sdr10,
                dither: true,
                video_levels: false,
            },
            &capabilities,
        )
        .unwrap();

        assert_eq!(selected.format, wgpu::TextureFormat::Rgb10a2Unorm);
        assert_eq!(selected.color_space, wgpu::SurfaceColorSpace::Srgb);
        assert_eq!(selected.resolved.resolved, PresentationDepth::Sdr10);
        assert!(selected.resolved.fallback_reason.is_none());
    }

    #[test]
    fn rgb10_without_srgb_falls_back_to_eight_bit() {
        let capabilities = surface_capabilities(vec![wgpu::SurfaceFormatCapabilities {
            format: wgpu::TextureFormat::Rgb10a2Unorm,
            color_spaces: wgpu::SurfaceColorSpaces::DISPLAY_P3,
        }]);
        let selected = select_surface_presentation(
            PresentationRequest {
                depth: PresentationDepth::Sdr10,
                dither: true,
                video_levels: false,
            },
            &capabilities,
        )
        .unwrap();

        assert_eq!(selected.format, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(selected.color_space, wgpu::SurfaceColorSpace::Auto);
        assert_eq!(selected.resolved.resolved, PresentationDepth::Sdr8);
        assert!(selected
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("not with an sRGB"));
    }

    #[test]
    fn ten_bit_display_keeps_a_float_intermediate() {
        assert_eq!(
            super::OutputWindow::intermediate_format_for(
                PresentationDepth::Sdr10,
                wgpu::TextureFormat::Rgb10a2Unorm
            ),
            super::COLOR_PATH_FORMAT
        );
        assert_eq!(
            super::OutputWindow::intermediate_format_for(
                PresentationDepth::Sdr8,
                wgpu::TextureFormat::Bgra8UnormSrgb
            ),
            wgpu::TextureFormat::Bgra8UnormSrgb
        );
    }

    #[test]
    fn syphon_eight_bit_request_has_no_fallback_reason() {
        let resolved = resolve_headless_presentation(
            PresentationRequest::default(),
            &OutputTarget::SyphonServer {
                server_name: "Precision Test".into(),
            },
        );
        assert_eq!(resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(resolved.pixel_format, PresentationPixelFormat::Bgra8);
        assert!(resolved.fallback_reason.is_none());
    }

    #[test]
    fn eight_bit_request_keeps_the_srgb_surface() {
        let capabilities = surface_capabilities(vec![wgpu::SurfaceFormatCapabilities {
            format: wgpu::TextureFormat::Rgb10a2Unorm,
            color_spaces: wgpu::SurfaceColorSpaces::SRGB,
        }]);
        let selected =
            select_surface_presentation(PresentationRequest::default(), &capabilities).unwrap();

        assert_eq!(selected.format, wgpu::TextureFormat::Bgra8UnormSrgb);
        assert_eq!(selected.resolved.resolved, PresentationDepth::Sdr8);
    }

    #[test]
    fn syphon_truthfully_resolves_ten_bit_requests_to_bgra8() {
        let resolved = resolve_headless_presentation(
            PresentationRequest {
                depth: PresentationDepth::Sdr10,
                dither: true,
                video_levels: false,
            },
            &OutputTarget::SyphonServer {
                server_name: "Precision Test".into(),
            },
        );

        assert_eq!(resolved.requested, PresentationDepth::Sdr10);
        assert_eq!(resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(resolved.pixel_format, PresentationPixelFormat::Bgra8);
        assert_eq!(resolved.alpha_mode, AlphaMode::Premultiplied);
        assert!(resolved.fallback_reason.unwrap().contains("BGRA8"));
    }

    #[test]
    fn p216_resolution_rebuilds_headless_output_as_linear_float() {
        let Ok(gpu) = super::GpuContext::new_headless() else {
            return;
        };
        let mut output = HeadlessOutput::new(
            &gpu.device,
            "P216 Test".into(),
            super::OutputSource::Master,
            OutputTarget::NdiSend {
                sender_name: "P216 Test".into(),
            },
            2,
            1,
        );

        output.set_resolved_presentation(
            &gpu.device,
            ResolvedPresentation {
                requested: PresentationDepth::Sdr10,
                resolved: PresentationDepth::Sdr10,
                pixel_format: PresentationPixelFormat::P216,
                color_profile: PresentationColorProfile::Rec709Limited,
                alpha_mode: AlphaMode::Opaque,
                dither: true,
                fallback_reason: None,
            },
        );

        assert_eq!(output.texture.format(), wgpu::TextureFormat::Rgba16Float);
        assert_eq!(output.readback.format(), super::super::ReadbackFormat::P216);
    }

    #[test]
    fn rgba16_resolution_rebuilds_headless_output_as_integer_unorm() {
        let Ok(gpu) = super::GpuContext::new_headless() else {
            return;
        };
        let required_features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        if !gpu.device.features().contains(required_features) {
            return;
        }
        let mut output = HeadlessOutput::new(
            &gpu.device,
            "RGBA16 Test".into(),
            super::OutputSource::Master,
            OutputTarget::Recording {
                path: "alpha.mov".into(),
                codec: crate::engine::value::render::RecordingCodec::ProRes4444,
                audio_device: None,
            },
            2,
            1,
        );

        output.set_resolved_presentation(
            &gpu.device,
            ResolvedPresentation {
                requested: PresentationDepth::Sdr10,
                resolved: PresentationDepth::Sdr10,
                pixel_format: PresentationPixelFormat::Rgba16,
                color_profile: PresentationColorProfile::Rec709Limited,
                alpha_mode: AlphaMode::Straight,
                dither: true,
                fallback_reason: None,
            },
        );

        assert_eq!(output.texture.format(), wgpu::TextureFormat::Rgba16Unorm);
        assert_eq!(
            output.readback.format(),
            super::super::ReadbackFormat::Rgba16Unorm
        );
        assert_eq!(output.readback.alpha_mode(), AlphaMode::Premultiplied);
    }

    /// `UnifiedOutput` holds both variants inline, and `clippy::large_enum_variant`
    /// fails the build when one exceeds the other by more than 200 bytes. The two
    /// stay close only because `HeadlessOutput`'s rarely-set `subprocess` and
    /// `audio_pcm` fields are boxed. With `subprocess` unboxed the gap was 184
    /// bytes on macOS but 224 on Windows — under the threshold on the platform
    /// most of us build on, over it on the one we don't. This catches that in the
    /// test suite, which runs everywhere, rather than in a per-platform lint job.
    #[test]
    fn output_variants_stay_under_the_large_enum_variant_threshold() {
        let window = std::mem::size_of::<OutputWindow>();
        let headless = std::mem::size_of::<HeadlessOutput>();
        let gap = window.abs_diff(headless);
        assert!(
            gap <= 200,
            "UnifiedOutput variants differ by {gap} bytes (OutputWindow {window}, \
             HeadlessOutput {headless}) — box a rarely-set field on the larger variant"
        );
    }

    /// Content and canvas agree, so nothing is inset — the case every 16:9
    /// project has always been in, and the one that must not change.
    #[test]
    fn matching_aspect_fills_the_canvas() {
        assert_eq!(
            aspect_fit_rect(16.0 / 9.0, 1920, 1080),
            [0.0, 0.0, 1.0, 1.0]
        );
        assert_eq!(aspect_fit_rect(1.0, 800, 800), [0.0, 0.0, 1.0, 1.0]);
    }

    /// A portrait project in a landscape window: full height, pillarboxed.
    #[test]
    fn portrait_content_in_a_landscape_canvas_is_pillarboxed() {
        let [x, y, w, h] = aspect_fit_rect(1080.0 / 1920.0, 1920, 1080);
        // 9:16 inside 16:9 leaves a column (9/16)/(16/9) = 0.3164 of the width.
        assert!((w - 0.316_406).abs() < 1e-4, "width {w}");
        assert_eq!(h, 1.0);
        assert!((x - (1.0 - w) / 2.0).abs() < 1e-6, "not centred: {x}");
        assert_eq!(y, 0.0);
    }

    /// The inverse, which is what a 16:9 project sent to a phone-shaped output
    /// gets: full width, bars above and below.
    #[test]
    fn landscape_content_in_a_portrait_canvas_is_letterboxed() {
        let [x, y, w, h] = aspect_fit_rect(16.0 / 9.0, 1080, 1920);
        assert_eq!(w, 1.0);
        assert!((h - 0.316_406).abs() < 1e-4, "height {h}");
        assert_eq!(x, 0.0);
        assert!((y - (1.0 - h) / 2.0).abs() < 1e-6, "not centred: {y}");
    }

    /// The fitted rectangle must stay inside the canvas whatever it is handed,
    /// or content spills off the edge of the projector.
    #[test]
    fn the_fitted_rect_never_leaves_the_unit_square() {
        for aspect in [0.1_f32, 0.5, 1.0, 1.777, 4.0, 32.0] {
            for (cw, ch) in [(1920u32, 1080u32), (1080, 1920), (1000, 1000), (3840, 800)] {
                let [x, y, w, h] = aspect_fit_rect(aspect, cw, ch);
                assert!(
                    x >= 0.0 && y >= 0.0 && x + w <= 1.000_01 && y + h <= 1.000_01,
                    "aspect {aspect} in {cw}×{ch} gave {:?}",
                    [x, y, w, h]
                );
            }
        }
    }

    /// The calibration card's foot carries a levels strip — codes 0, 16, 235
    /// and 255, a quarter of the width each — for checking an output's
    /// Levels on a screen that reads video levels.
    #[test]
    fn the_calibration_card_carries_a_levels_strip() {
        let (w, h) = (512u32, 512u32);
        let card = super::generate_calibration_card(w, h, 0);
        // The strip is the last bar; its middle row, at each quarter's middle.
        let border = (w.min(h) / 40).max(2);
        let inset = border + 1;
        let inner_h = h - inset * 2;
        let grid_h = (inner_h as f32 * 0.70) as u32;
        let bar_h = (inner_h - grid_h) / super::CALIBRATION_BARS;
        let y = inset + grid_h + bar_h * 5 + bar_h / 2;
        let inner_w = w - inset * 2;
        let codes: Vec<u8> = (0..4)
            .map(|q| {
                let x = inset + inner_w * (2 * q + 1) / 8;
                card[((y * w + x) * 4) as usize]
            })
            .collect();
        assert_eq!(codes, [0, 16, 235, 255]);
    }

    /// A Blackmagic output's 1080p frame shows a 16:9 master whole; a wider
    /// master or bus letterboxed, a narrower pillarboxed, a turned one as
    /// turned — never stretched to the format.
    #[test]
    fn a_blackmagic_output_fits_its_picture_into_the_format() {
        let hd = |master, scale, rotation| fitted_in_format(master, scale, rotation, 1920, 1080);
        let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(a, b)| (a - b).abs() < 1e-4);
        assert_eq!(hd([1920, 1080], [1.0, 1.0], OutputRotation::Deg0), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(hd([3840, 2160], [1.0, 1.0], OutputRotation::Deg0), [0.0, 0.0, 1.0, 1.0]);
        // A 32:9 LED master: full width, half height.
        assert!(close(hd([3840, 1080], [1.0, 1.0], OutputRotation::Deg0), [0.0, 0.25, 1.0, 0.5]));
        // Its left half as a bus is 16:9 again.
        assert_eq!(hd([3840, 1080], [0.5, 1.0], OutputRotation::Deg0), [0.0, 0.0, 1.0, 1.0]);
        // A square bus of it: pillarboxed.
        let [x, _, w, h] = hd([3840, 1080], [0.28125, 1.0], OutputRotation::Deg0);
        assert!((w - 0.5625).abs() < 1e-4 && h == 1.0 && (x - 0.21875).abs() < 1e-4, "{w} {x}");
        // Turned a quarter, a 16:9 master stands 9:16 in the frame.
        let [_, _, w, h] = hd([1920, 1080], [1.0, 1.0], OutputRotation::Deg90);
        assert!((w - 0.316_406).abs() < 1e-4 && h == 1.0, "{w}");
    }

    /// A malformed scene or a window mid-minimise must not produce a NaN quad.
    #[test]
    fn degenerate_inputs_fall_back_to_filling() {
        assert_eq!(aspect_fit_rect(16.0 / 9.0, 0, 0), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(aspect_fit_rect(0.0, 1920, 1080), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(aspect_fit_rect(-2.0, 1920, 1080), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(aspect_fit_rect(f32::NAN, 1920, 1080), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(
            aspect_fit_rect(f32::INFINITY, 1920, 1080),
            [0.0, 0.0, 1.0, 1.0]
        );
    }

    #[test]
    fn output_rotation_default_is_deg0() {
        assert_eq!(OutputRotation::default(), OutputRotation::Deg0);
    }

    #[test]
    fn output_rotation_index_values() {
        assert_eq!(OutputRotation::Deg0.index(), 0);
        assert_eq!(OutputRotation::Deg90.index(), 1);
        assert_eq!(OutputRotation::Deg180.index(), 2);
        assert_eq!(OutputRotation::Deg270.index(), 3);
    }

    #[test]
    fn output_rotation_swaps_dimensions() {
        assert!(!OutputRotation::Deg0.swaps_dimensions());
        assert!(OutputRotation::Deg90.swaps_dimensions());
        assert!(!OutputRotation::Deg180.swaps_dimensions());
        assert!(OutputRotation::Deg270.swaps_dimensions());
    }

    #[test]
    fn output_rotation_effective_dimensions() {
        assert_eq!(
            OutputRotation::Deg0.effective_dimensions(1920, 1080),
            (1920, 1080)
        );
        assert_eq!(
            OutputRotation::Deg90.effective_dimensions(1920, 1080),
            (1080, 1920)
        );
        assert_eq!(
            OutputRotation::Deg180.effective_dimensions(1920, 1080),
            (1920, 1080)
        );
        assert_eq!(
            OutputRotation::Deg270.effective_dimensions(1920, 1080),
            (1080, 1920)
        );
    }

    #[test]
    fn output_rotation_labels() {
        assert_eq!(OutputRotation::Deg0.label(), "0°");
        assert_eq!(OutputRotation::Deg90.label(), "90°");
        assert_eq!(OutputRotation::Deg180.label(), "180°");
        assert_eq!(OutputRotation::Deg270.label(), "270°");
    }

    #[test]
    fn output_rotation_all_contains_all_variants() {
        assert_eq!(OutputRotation::ALL.len(), 4);
        assert_eq!(OutputRotation::ALL[0], OutputRotation::Deg0);
        assert_eq!(OutputRotation::ALL[1], OutputRotation::Deg90);
        assert_eq!(OutputRotation::ALL[2], OutputRotation::Deg180);
        assert_eq!(OutputRotation::ALL[3], OutputRotation::Deg270);
    }

    #[test]
    fn output_rotation_serde_roundtrip() {
        for rot in OutputRotation::ALL {
            let json = serde_json::to_string(&rot).unwrap();
            let deserialized: OutputRotation = serde_json::from_str(&json).unwrap();
            assert_eq!(rot, deserialized);
        }
    }

    #[test]
    fn output_rotation_deserialize_default() {
        // Missing field should deserialize as Deg0
        let config: OutputRotation = serde_json::from_str("\"Deg0\"").unwrap();
        assert_eq!(config, OutputRotation::Deg0);
    }

    #[test]
    fn headless_context_enables_bc_when_adapter_supports() {
        // Headless installations must take the HAP GPU path, so the headless
        // device has to request TEXTURE_COMPRESSION_BC whenever the adapter
        // exposes it. Skips gracefully when no GPU adapter is available.
        let Ok(gpu) = super::GpuContext::new_headless() else {
            return;
        };
        let adapter_bc = gpu
            .adapter
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        let device_bc = gpu
            .device
            .features()
            .contains(wgpu::Features::TEXTURE_COMPRESSION_BC);
        assert_eq!(
            adapter_bc, device_bc,
            "headless device should request BC iff the adapter supports it"
        );
    }

    #[test]
    fn headless_context_enables_rgba16_unorm_when_adapter_supports_it() {
        let Ok(gpu) = super::GpuContext::new_headless() else {
            return;
        };
        let features = wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
            | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let renderable = gpu.adapter.features().contains(features)
            && gpu
                .adapter
                .get_texture_format_features(wgpu::TextureFormat::Rgba16Unorm)
                .allowed_usages
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT);
        if renderable {
            assert!(
                gpu.device.features().contains(features),
                "the device must request RGBA16 normalized texture support"
            );
        }
    }

    #[test]
    fn an_output_waiting_for_its_monitor_is_saved_as_that_monitor() {
        // Windowed only as a stand-in: saving it as Windowed would forget the
        // LED wall if the show restarts before the wall is powered on.
        assert_eq!(
            configured_target(&OutputTarget::Windowed, Some("LED")),
            OutputTarget::Display {
                name: "LED".into(),
                monitor_index: 0,
            }
        );
    }

    #[test]
    fn an_output_not_waiting_is_saved_as_its_live_target() {
        let on_tv = OutputTarget::Display {
            name: "TV".into(),
            monitor_index: 2,
        };
        assert_eq!(configured_target(&on_tv, None), on_tv);
        assert_eq!(
            configured_target(&OutputTarget::Windowed, None),
            OutputTarget::Windowed
        );
    }
}
