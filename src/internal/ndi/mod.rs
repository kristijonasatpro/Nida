//! NDI (Network Device Interface) — send and receive video over LAN.
//!
//! Uses dynamic loading (`libloading`) so the NDI SDK is only required at runtime.
//! Input follows the `CameraManager` pattern: background receive thread →
//! reusable packed-frame slot → main-thread GPU upload/conversion.

#[allow(non_camel_case_types, non_snake_case, dead_code)]
pub mod ffi;
mod color;
mod p216;
mod receive;
pub mod sdk;

use color::uyvy_to_rgba;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};

use crate::engine::value::render::{
    AlphaMode, PresentationCapabilities, PresentationColorProfile, PresentationDepth,
    PresentationFormat, PresentationPixelFormat, PresentationRequest, ResolvedPresentation,
};

/// Frame rate declared to receivers when the caller's rate will not fit the
/// SDK's signed field. Only reachable for absurd rates; NDI has no way to say
/// "unspecified", so it needs some honest-looking number.
const DEFAULT_NDI_FPS: i32 = 60;

/// Discovered NDI source on the network.
#[derive(Debug, Clone)]
pub struct NdiSource {
    /// Source name (e.g. "MY-PC (Source 1)")
    pub name: String,
}

/// Borrowed GPU resources for one P216 conversion dispatch.
pub(crate) struct P216FrameConversion<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub source: &'a wgpu::TextureView,
    pub width: u32,
    pub height: u32,
    pub dither: bool,
}

/// Manages NDI discovery, receive, and send.
pub struct NdiManager {
    sdk: Option<sdk::NdiSdk>,
    send_capability: sdk::NdiSendCapability,
    sources: Vec<NdiSource>,
    receivers: Vec<NdiReceiver>,
    /// Each receiver's texture; `None` once it is stopped, its memory given
    /// back (receivers are kept in place: decks know them by position).
    textures: Vec<Option<receive::ReceiveTarget>>,
    unpacker: Option<receive::UyvyUnpacker>,
    /// Active NDI senders keyed by sender name.
    senders: HashMap<String, NdiSender>,
    /// GPU P216 conversion/readback state keyed by sender name.
    p216_converters: HashMap<String, p216::P216Converter>,
    /// Watches the network for sources from the first look on, so a look
    /// never waits: null until then.
    finder: ffi::NDIlib_find_instance_t,
}

// SAFETY: the NDI SDK's finder, receiver and sender instances are not tied
// to the thread that made them: any thread may use one, one at a time. The
// manager owns them and is used by one thread at a time — the engine moves
// between the main thread and the engine thread with it.
unsafe impl Send for NdiManager {}

/// A receiver's sound: its format once some has arrived, and where it goes.
#[derive(Default)]
struct NdiAudio {
    /// Sample rate << 16 | channels; 0 until the first sound arrives.
    format: AtomicU64,
    /// Counts format changes: whoever listens picks the sound up afresh.
    generation: AtomicU32,
    /// The audio mixer's end, once it listens.
    sink: Mutex<Option<crossbeam_channel::Sender<crate::audio::PcmChunk>>>,
}

impl NdiAudio {
    /// Pass one frame of sound on, noting a change of format.
    fn take(&self, frame: &ffi::NDIlib_audio_frame_v3_t) {
        let (Ok(rate), Ok(channels), Ok(samples), Ok(stride)) = (
            u32::try_from(frame.sample_rate),
            u16::try_from(frame.no_channels),
            usize::try_from(frame.no_samples),
            usize::try_from(frame.channel_stride_in_bytes),
        ) else {
            return;
        };
        if rate == 0
            || channels == 0
            || samples == 0
            || frame.p_data.is_null()
            || frame.FourCC != ffi::NDIlib_audio_frame_v3_t::FLTP
            || stride < samples * std::mem::size_of::<f32>()
        {
            return;
        }
        let format = u64::from(rate) << 16 | u64::from(channels);
        let Ok(mut sink) = self.sink.lock() else {
            return;
        };
        if self.format.swap(format, Ordering::SeqCst) != format {
            self.generation.fetch_add(1, Ordering::SeqCst);
            *sink = None;
            log::info!("NDI sound: {rate} Hz, {channels} channels");
        }
        let Some(tx) = sink.as_ref() else {
            return;
        };
        // Read as bytes: nothing promises the buffer is aligned for floats.
        let planes: Vec<Vec<f32>> = (0..usize::from(channels))
            .map(|c| {
                let bytes = unsafe {
                    std::slice::from_raw_parts(frame.p_data.add(c * stride), samples * 4)
                };
                bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_ne_bytes(*b))
                    .collect()
            })
            .collect();
        let planes: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
        let chunk = crate::audio::PcmChunk {
            samples: interleave(&planes),
            lost_before: 0,
        };
        if let Err(crossbeam_channel::TrySendError::Disconnected(_)) = tx.try_send(chunk) {
            *sink = None;
        }
    }

    /// (sample rate, channels, generation), once sound has arrived.
    fn format(&self) -> Option<(u32, u16, u32)> {
        let format = self.format.load(Ordering::SeqCst);
        let rate = u32::try_from(format >> 16).ok()?;
        let channels = u16::try_from(format & 0xffff).ok()?;
        (rate > 0 && channels > 0).then(|| (rate, channels, self.generation.load(Ordering::SeqCst)))
    }
}

/// Planar sound (a run of samples per channel) as interleaved frames.
fn interleave(planes: &[&[f32]]) -> Vec<f32> {
    let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * planes.len());
    for i in 0..frames {
        out.extend(planes.iter().map(|p| p[i]));
    }
    out
}

struct NdiReceiver {
    #[allow(dead_code)]
    source_name: String,
    frame_data: Arc<receive::FrameSlot>,
    audio: Arc<NdiAudio>,
    stop_flag: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    #[allow(dead_code)]
    recv_instance: ffi::NDIlib_recv_instance_t,
    width: u32,
    height: u32,
    /// When its last picture was put in its texture.
    last_picture: Option<std::time::Instant>,
}

struct NdiSender {
    instance: ffi::NDIlib_send_instance_t,
    /// Its pictures, converted and sent from a thread of their own.
    video: NdiVideoSend,
    /// The audio bus it carries, sent from a thread of its own.
    audio: Option<NdiAudioSend>,
}

/// A picture for a sender's thread to send.
enum NdiPicture {
    /// Eight-bit RGBA, tightly packed: converted to UYVY on the thread.
    Rgba {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        fps: u32,
    },
    /// P216, as the GPU made it.
    P216 {
        bytes: Vec<u8>,
        width: u32,
        height: u32,
        stride: u32,
        fps: u32,
    },
}

/// What a sender's thread and the render thread share.
#[derive(Default)]
struct VideoSlot {
    /// The newest picture handed over, until the thread takes it.
    picture: Option<NdiPicture>,
    stop: bool,
    /// Pictures sent.
    sent: u64,
    /// Pictures let go for a newer before the thread took them.
    replaced: u64,
}

#[derive(Default)]
struct VideoShared {
    slot: Mutex<VideoSlot>,
    pictures: std::sync::Condvar,
}

impl VideoShared {
    fn lock(&self) -> std::sync::MutexGuard<'_, VideoSlot> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A sender's pictures, sent from a thread of their own. NDI's send returns
/// when it has compressed the picture, and an eight-bit picture is converted
/// to UYVY first — a 4K picture took the render thread 21 ms of its 16.7,
/// and the whole show ran at 41 frames a second — so the render thread
/// hands each picture over and waits on neither.
struct NdiVideoSend {
    shared: Arc<VideoShared>,
    thread: std::thread::JoinHandle<()>,
}

impl NdiVideoSend {
    /// Send the pictures handed over through `submit`, until stopped.
    ///
    /// # Errors
    ///
    /// The thread could not be started.
    fn start(
        submit: impl FnMut(*const ffi::NDIlib_video_frame_v2_t) + Send + 'static,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(VideoShared::default());
        let thread = {
            let shared = shared.clone();
            std::thread::Builder::new()
                .name("ndi-send-video".into())
                .spawn(move || send_pictures(&shared, submit))?
        };
        Ok(Self { shared, thread })
    }

    /// The newest picture: sent as soon as the thread is free. One handed
    /// over before it and not yet taken is let go. `false` when the thread
    /// has ended — as it does only by a fault in it — and takes no picture.
    fn hand(&self, picture: NdiPicture) -> bool {
        if self.thread.is_finished() {
            return false;
        }
        let mut slot = self.shared.lock();
        let waiting = slot.picture.replace(picture);
        if waiting.is_some() {
            slot.replaced += 1;
        }
        drop(slot);
        self.shared.pictures.notify_one();
        // Freed with the lock let go: a 4K picture is 33 MB.
        drop(waiting);
        true
    }

    /// Stop the thread — after the picture it is sending — and say how many
    /// pictures it sent, and how many were let go for a newer.
    fn stop(self) -> (u64, u64) {
        self.shared.lock().stop = true;
        self.shared.pictures.notify_one();
        let _ = self.thread.join();
        let slot = self.shared.lock();
        (slot.sent, slot.replaced)
    }
}

/// A sender's thread: each picture handed over is converted, if it needs
/// it, and sent, until the sender stops.
fn send_pictures(
    shared: &VideoShared,
    mut submit: impl FnMut(*const ffi::NDIlib_video_frame_v2_t),
) {
    // Kept from picture to picture: not allocated for each.
    let mut uyvy = Vec::new();
    loop {
        let picture = {
            let mut slot = shared.lock();
            loop {
                if slot.stop {
                    return;
                }
                if let Some(picture) = slot.picture.take() {
                    break picture;
                }
                slot = shared
                    .pictures
                    .wait(slot)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
        };
        let sent = match picture {
            NdiPicture::Rgba {
                bytes,
                width,
                height,
                fps,
            } => submit_uyvy_with(&bytes, &mut uyvy, width, height, fps, &mut submit),
            NdiPicture::P216 {
                mut bytes,
                width,
                height,
                stride,
                fps,
            } => match submit_p216_with(&mut bytes, width, height, stride, fps, &mut submit) {
                Ok(()) => true,
                Err(error) => {
                    log::warn!("NDI P216 picture not sent: {error}");
                    false
                }
            },
        };
        if sent {
            shared.lock().sent += 1;
        }
    }
}

/// A sender's sound: an audio bus's mix, sent as it is made.
struct NdiAudioSend {
    bus: String,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl NdiAudioSend {
    /// Send `tap`'s blocks — interleaved stereo at the mix rate — on
    /// `instance`, until stopped. NDI takes sound and pictures from
    /// different threads, so the render thread never waits on it.
    fn start(
        bus: &str,
        tap: crate::audio_mix::BusTap,
        instance: ffi::NDIlib_send_instance_t,
        send_audio: unsafe extern "C" fn(
            ffi::NDIlib_send_instance_t,
            *const ffi::NDIlib_audio_frame_v3_t,
        ),
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let instance = instance as usize;
        let thread = {
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("ndi-send-audio".into())
                .spawn(move || {
                    let instance = instance as ffi::NDIlib_send_instance_t;
                    let rate = i32::try_from(crate::audio_mix::SAMPLE_RATE).unwrap_or(48_000);
                    while !stop.load(Ordering::Relaxed) {
                        let Ok(chunk) = tap
                            .receiver
                            .recv_timeout(std::time::Duration::from_millis(100))
                        else {
                            continue;
                        };
                        let mut planar = deinterleave(&chunk.samples, 2);
                        let samples = chunk.samples.len() / 2;
                        let (Ok(no_samples), Ok(stride)) =
                            (i32::try_from(samples), i32::try_from(samples * 4))
                        else {
                            continue;
                        };
                        if samples == 0 {
                            continue;
                        }
                        let frame = ffi::NDIlib_audio_frame_v3_t {
                            sample_rate: rate,
                            no_channels: 2,
                            no_samples,
                            // NDIlib_send_timecode_synthesize
                            timecode: i64::MAX,
                            FourCC: ffi::NDIlib_audio_frame_v3_t::FLTP,
                            p_data: planar.as_mut_ptr().cast(),
                            channel_stride_in_bytes: stride,
                            p_metadata: std::ptr::null(),
                            timestamp: 0,
                        };
                        unsafe { send_audio(instance, &raw const frame) };
                    }
                })
                .ok()
        };
        log::info!("NDI sender carries audio bus {bus}");
        Self {
            bus: bus.to_string(),
            stop,
            thread,
        }
    }

    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Interleaved frames as planar sound: each channel's samples in a run.
fn deinterleave(samples: &[f32], channels: usize) -> Vec<f32> {
    let frames = samples.len() / channels.max(1);
    let mut out = vec![0.0; frames * channels];
    for (i, frame) in samples.chunks_exact(channels.max(1)).enumerate() {
        for (c, s) in frame.iter().enumerate() {
            out[c * frames + i] = *s;
        }
    }
    out
}

impl Default for NdiManager {
    fn default() -> Self {
        Self::new()
    }
}

impl NdiManager {
    pub fn new() -> Self {
        let mut sdk = sdk::NdiSdk::load();
        if let Some(loaded) = sdk.as_ref() {
            let ok = unsafe { (loaded.initialize)() };
            if ok {
                log::info!(
                    "NDI SDK {} initialized successfully",
                    loaded.runtime_version
                );
            } else {
                log::error!("NDI SDK initialize() returned false");
                // A loaded library that rejected initialization cannot submit
                // either UYVY or P216, so do not advertise runtime capability.
            }
            if !ok {
                sdk = None;
            }
        } else {
            log::info!("NDI SDK not found — NDI features disabled");
        }
        let send_capability = sdk::NdiSendCapability::from_runtime_evidence(
            sdk.as_ref()
                .map(|loaded| loaded.runtime_version.as_str())
                .filter(|version| !version.is_empty()),
            sdk.is_some(),
        );
        Self {
            sdk,
            send_capability,
            sources: Vec::new(),
            receivers: Vec::new(),
            textures: Vec::new(),
            unpacker: None,
            senders: HashMap::new(),
            p216_converters: HashMap::new(),
            finder: std::ptr::null_mut(),
        }
    }

    /// Create a disabled NDI manager (CLI `--no-ndi` flag).
    pub fn new_disabled() -> Self {
        Self {
            sdk: None,
            send_capability: sdk::NdiSendCapability::from_runtime_evidence(None, false),
            sources: Vec::new(),
            receivers: Vec::new(),
            textures: Vec::new(),
            unpacker: None,
            senders: HashMap::new(),
            p216_converters: HashMap::new(),
            finder: std::ptr::null_mut(),
        }
    }

    pub fn is_available(&self) -> bool {
        self.sdk.is_some()
    }

    /// Resolve an NDI presentation request from runtime evidence.
    ///
    /// NDI 6 plus the resolved v2 submission symbol enables P216. Older or
    /// unidentified runtimes retain the byte-compatible UYVY fallback.
    pub fn resolve_presentation(&self, request: PresentationRequest) -> ResolvedPresentation {
        Self::resolve_presentation_for_capability(request, self.send_capability)
    }

    fn resolve_presentation_for_capability(
        request: PresentationRequest,
        capability: sdk::NdiSendCapability,
    ) -> ResolvedPresentation {
        let mut formats = Vec::with_capacity(2);
        if capability.p216_confirmed() {
            formats.push(PresentationFormat {
                depth: PresentationDepth::Sdr10,
                pixel_format: PresentationPixelFormat::P216,
                color_profile: PresentationColorProfile::Rec709Limited,
                alpha_mode: AlphaMode::Opaque,
            });
        }
        formats.push(PresentationFormat {
            depth: PresentationDepth::Sdr8,
            pixel_format: PresentationPixelFormat::Uyvy,
            color_profile: PresentationColorProfile::Rec709Limited,
            alpha_mode: AlphaMode::Opaque,
        });

        PresentationCapabilities::new(formats, Some(capability.p216_unavailable_reason()))
            .resolve(request)
            .expect("NDI always provides the UYVY fallback")
    }

    pub fn sources(&self) -> &[NdiSource] {
        &self.sources
    }

    /// Return discovered source names for UI display.
    pub fn discovered_sources(&self) -> Vec<String> {
        self.sources.iter().map(|s| s.name.clone()).collect()
    }

    /// The NDI sources on the network now. The first look starts watching
    /// the network, and sources show up as they announce themselves, within
    /// a second or two; a look never waits, so it can be made mid-show
    /// without holding up a frame.
    pub fn discover(&mut self) {
        let Some(sdk) = &self.sdk else {
            return;
        };
        if self.finder.is_null() {
            let find_settings = ffi::NDIlib_find_create_t::default();
            self.finder = unsafe { (sdk.find_create_v2)(&raw const find_settings) };
            if self.finder.is_null() {
                log::warn!("NDI find_create_v2 returned null");
                return;
            }
        }
        let mut sources = Vec::new();
        unsafe {
            let mut count: std::os::raw::c_uint = 0;
            let sources_ptr = (sdk.find_get_current_sources)(self.finder, &raw mut count);
            if !sources_ptr.is_null() && count > 0 {
                let sources_slice = std::slice::from_raw_parts(sources_ptr, count as usize);
                for src in sources_slice {
                    if !src.p_ndi_name.is_null() {
                        let name = std::ffi::CStr::from_ptr(src.p_ndi_name)
                            .to_string_lossy()
                            .into_owned();
                        sources.push(NdiSource { name });
                    }
                }
            }
        }
        if sources.len() != self.sources.len() {
            log::info!("NDI: {} sources on the network", sources.len());
        }
        self.sources = sources;
    }

    /// Start receiving from a named NDI source.
    /// Spawns a background thread that captures frames into shared memory.
    ///
    /// # Panics
    ///
    /// Panics only if the fixed receiver-name literal `"Nida Receiver"` cannot be
    /// converted to a `CString`, which is impossible for that literal.
    pub fn start_receive(&mut self, source_name: &str, device: &wgpu::Device) -> Option<usize> {
        let Some(sdk) = &self.sdk else {
            log::warn!("Cannot receive NDI: SDK not available");
            return None;
        };

        if self.unpacker.is_none() {
            self.unpacker = receive::UyvyUnpacker::try_new(device);
        }
        let frame_data = Arc::new(receive::FrameSlot::new(self.unpacker.is_none()));
        let audio = Arc::new(NdiAudio::default());
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (width, height) = (1920u32, 1080u32);

        // Create the NDI source struct
        let name_c = std::ffi::CString::new(source_name).ok()?;
        let ndi_source = ffi::NDIlib_source_t {
            p_ndi_name: name_c.as_ptr(),
            p_url_address: std::ptr::null(),
        };

        let recv_name = std::ffi::CString::new("Nida Receiver").unwrap();
        let recv_settings = ffi::NDIlib_recv_create_v3_t {
            source_to_connect_to: ndi_source,
            color_format: if self.unpacker.is_some() {
                receive::RECEIVE_COLOR_FORMAT
            } else {
                0
            },
            bandwidth: 100, // highest quality
            allow_video_fields: false,
            p_ndi_recv_name: recv_name.as_ptr(),
        };

        let recv_instance = unsafe { (sdk.recv_create_v3)(&raw const recv_settings) };
        if recv_instance.is_null() {
            log::error!("NDI recv_create_v3 returned null for '{source_name}'");
            return None;
        }

        let width = width.max(1);
        let height = height.max(1);
        let target = receive::ReceiveTarget::new(
            device,
            &format!("NDI Receive: {source_name}"),
            width,
            height,
        );

        let connected = Arc::new(AtomicBool::new(false));

        // Spawn background receive thread
        let frame_clone = Arc::clone(&frame_data);
        let stop_clone = Arc::clone(&stop_flag);
        let connected_clone = Arc::clone(&connected);
        // Note: recv_instance is a raw pointer, sent across thread boundary.
        // Safe because NDI SDK guarantees thread safety for recv instances.
        let recv_ptr = recv_instance as usize;
        let recv_destroy_fn = sdk.recv_destroy as usize;
        let recv_capture_fn = sdk.recv_capture_v3 as usize;
        let recv_free_fn = sdk.recv_free_video_v2 as usize;
        let free_audio_fn = sdk.recv_free_audio_v3;
        let audio_clone = Arc::clone(&audio);

        let source_name_log = source_name.to_string();
        let thread = std::thread::Builder::new()
            .name(format!("ndi-recv-{source_name}"))
            .spawn(move || {
                let recv = recv_ptr as ffi::NDIlib_recv_instance_t;
                let capture_fn: unsafe extern "C" fn(ffi::NDIlib_recv_instance_t, *mut ffi::NDIlib_video_frame_v2_t, *mut std::ffi::c_void, *mut std::ffi::c_void, std::os::raw::c_uint) -> ffi::NDIlib_frame_type_e
                    = unsafe { std::mem::transmute(recv_capture_fn) };
                let free_fn: unsafe extern "C" fn(ffi::NDIlib_recv_instance_t, *const ffi::NDIlib_video_frame_v2_t)
                    = unsafe { std::mem::transmute(recv_free_fn) };

                let mut frame_count: u64 = 0;
                let mut none_count: u64 = 0;

                while !stop_clone.load(Ordering::SeqCst) {
                    let mut video_frame = std::mem::MaybeUninit::<ffi::NDIlib_video_frame_v2_t>::zeroed();
                    let mut audio_frame = std::mem::MaybeUninit::<ffi::NDIlib_audio_frame_v3_t>::zeroed();
                    // Sound is only asked for when it can be handed back.
                    let audio_out = if free_audio_fn.is_some() {
                        audio_frame.as_mut_ptr().cast::<std::ffi::c_void>()
                    } else {
                        std::ptr::null_mut()
                    };
                    let frame_type = unsafe {
                        capture_fn(recv, video_frame.as_mut_ptr(), audio_out, std::ptr::null_mut(), 100)
                    };

                    if frame_type == ffi::NDIlib_frame_type_e::VIDEO {
                        let vf = unsafe { video_frame.assume_init() };
                        if !vf.p_data.is_null() && vf.xres > 0 && vf.yres > 0 {
                            let w = vf.xres as u32;
                            let h = vf.yres as u32;
                            if frame_count == 0 {
                                log::info!("NDI '{}': first frame {}×{} FourCC={:?} stride={}", source_name_log, w, h, vf.FourCC, vf.line_stride_in_bytes);
                            }
                            frame_count += 1;
                            none_count = 0;
                            connected_clone.store(true, Ordering::SeqCst);
                            frame_clone.deposit(&vf);
                        }
                        unsafe { free_fn(recv, &raw const vf) };
                    } else if frame_type == ffi::NDIlib_frame_type_e::AUDIO {
                        let af = unsafe { audio_frame.assume_init() };
                        audio_clone.take(&af);
                        if let Some(free_audio) = free_audio_fn {
                            unsafe { free_audio(recv, &raw const af) };
                        }
                    } else if frame_type == ffi::NDIlib_frame_type_e::NONE {
                        none_count += 1;
                        if none_count == 30 {
                            log::warn!("NDI '{source_name_log}': 30 consecutive empty captures — source may not be sending");
                        }
                        if none_count >= 50 {
                            connected_clone.store(false, Ordering::SeqCst);
                        }
                    } else if frame_type == ffi::NDIlib_frame_type_e::STATUS_CHANGE {
                        log::info!("NDI '{source_name_log}': connection status changed");
                    } else if frame_type == ffi::NDIlib_frame_type_e::ERROR {
                        log::warn!("NDI '{source_name_log}': received ERROR frame");
                        connected_clone.store(false, Ordering::SeqCst);
                    }
                }

                log::info!("NDI '{source_name_log}': receive thread stopping (captured {frame_count} frames)");
                let destroy_fn: unsafe extern "C" fn(ffi::NDIlib_recv_instance_t)
                    = unsafe { std::mem::transmute(recv_destroy_fn) };
                unsafe { destroy_fn(recv) };
            })
            .ok();

        let idx = self.receivers.len();
        self.receivers.push(NdiReceiver {
            source_name: source_name.to_string(),
            frame_data,
            audio,
            stop_flag,
            connected,
            thread,
            recv_instance: std::ptr::null_mut(), // Owned by the thread now
            width,
            height,
            last_picture: None,
        });
        self.textures.push(Some(target));
        log::info!("NDI receiver started for '{source_name}'");
        Some(idx)
    }

    /// Upload latest frames from all receivers to GPU.
    /// Dynamically recreates textures when the received resolution changes.
    pub fn update(&mut self, context: &crate::renderer::GpuContext) {
        let mut encoder = None;
        for (receiver, target) in self.receivers.iter_mut().zip(&mut self.textures) {
            if let Some(target) = target {
                if target.update(
                    &receiver.frame_data,
                    &context.device,
                    &context.queue,
                    &mut self.unpacker,
                    &mut encoder,
                ) {
                    (receiver.width, receiver.height) = target.dimensions();
                    receiver.last_picture = Some(std::time::Instant::now());
                }
            }
        }
        if let Some(encoder) = encoder {
            context.submit([encoder.finish()]);
        }
    }

    /// Whether receiver `idx` put a new picture in its texture within
    /// `within`: one waiting for its sender has none.
    pub fn receiving(&self, idx: usize, within: std::time::Duration) -> bool {
        self.receivers
            .get(idx)
            .and_then(|r| r.last_picture)
            .is_some_and(|at| at.elapsed() <= within)
    }

    pub fn texture_view(&self, idx: usize) -> Option<&wgpu::TextureView> {
        self.textures
            .get(idx)?
            .as_ref()
            .map(receive::ReceiveTarget::view)
    }

    pub fn receiver_dimensions(&self, idx: usize) -> Option<(u32, u32)> {
        self.receivers.get(idx).map(|r| (r.width, r.height))
    }

    /// Check if a receiver is currently connected (receiving video frames).
    /// Returns `false` for out-of-bounds indices.
    pub fn is_connected(&self, idx: usize) -> bool {
        self.receivers
            .get(idx)
            .is_some_and(|r| r.connected.load(Ordering::SeqCst))
    }

    /// Encode a linear `Rgba16Float` output into P216 and enqueue asynchronous
    /// GPU readback. When both readback slots are busy the frame is dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when P216 is unavailable or the dimensions cannot form
    /// a valid even-width P216 frame.
    pub(crate) fn begin_p216_frame(
        &mut self,
        sender_name: &str,
        conversion: P216FrameConversion<'_>,
    ) -> Result<(), String> {
        let P216FrameConversion {
            device,
            queue,
            encoder,
            source,
            width,
            height,
            dither,
        } = conversion;
        if !self.send_capability.p216_confirmed() {
            return Err(self.send_capability.p216_unavailable_reason());
        }
        let recreate = self
            .p216_converters
            .get(sender_name)
            .is_none_or(|converter| converter.dimensions() != (width, height));
        if recreate {
            let converter = p216::P216Converter::new(device, width, height)
                .map_err(|error| error.to_string())?;
            self.p216_converters
                .insert(sender_name.to_string(), converter);
        }
        if let Some(converter) = self.p216_converters.get_mut(sender_name) {
            let _ = converter.encode(device, queue, encoder, source, dither);
        }
        Ok(())
    }

    /// Hand the oldest completed P216 readback frame to its sender's thread,
    /// which submits it through NDI.
    ///
    /// # Errors
    ///
    /// Returns an error when sender creation or frame-contract validation
    /// fails, or the sender's thread has ended.
    pub(crate) fn try_send_p216(
        &mut self,
        sender_name: &str,
        device: &wgpu::Device,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<(), String> {
        let Some((bytes, layout)) =
            self.p216_converters
                .get_mut(sender_name)
                .and_then(|converter| {
                    converter
                        .try_read(device)
                        .map(|bytes| (bytes, converter.layout()))
                })
        else {
            return Ok(());
        };
        if !self.ensure_sender(sender_name) {
            return Err(format!("NDI sender '{sender_name}' could not be created"));
        }
        // Checked here, where a frame out of its contract stops the output.
        p216_contract(bytes.len(), width, height, layout.stride)
            .map_err(|error| error.to_string())?;
        self.hand(
            sender_name,
            NdiPicture::P216 {
                bytes,
                width,
                height,
                stride: layout.stride,
                fps,
            },
        )
    }

    fn ensure_sender(&mut self, sender_name: &str) -> bool {
        if self.senders.contains_key(sender_name) {
            return true;
        }
        let Some(sdk) = &self.sdk else {
            return false;
        };
        let Ok(name_c) = std::ffi::CString::new(sender_name) else {
            return false;
        };
        // Varda's render loop is the clock. Enabling NDI clocking would block
        // the render thread inside the submission call.
        let settings = ffi::NDIlib_send_create_t {
            p_ndi_name: name_c.as_ptr(),
            p_groups: std::ptr::null(),
            clock_video: false,
            clock_audio: false,
        };
        let instance = unsafe { (sdk.send_create)(&raw const settings) };
        if instance.is_null() {
            log::error!("NDI send_create returned null for '{sender_name}'");
            return false;
        }
        // The sender's address, for its thread: a sender is not tied to the
        // thread that made it (see `NdiManager`'s `Send`), and NDI takes
        // pictures and sound from different threads.
        let send_video = sdk.send_send_video_v2;
        let address = instance as usize;
        let started = NdiVideoSend::start(move |frame| unsafe {
            send_video(address as ffi::NDIlib_send_instance_t, frame);
        });
        let video = match started {
            Ok(video) => video,
            Err(error) => {
                // Not kept without its thread: it would take every picture
                // and send none. Tried again with the next picture.
                log::error!("NDI sender '{sender_name}': no thread for its pictures: {error}");
                unsafe { (sdk.send_destroy)(instance) };
                return false;
            }
        };
        self.senders.insert(
            sender_name.to_string(),
            NdiSender {
                instance,
                video,
                audio: None,
            },
        );
        log::info!("NDI sender created: '{sender_name}'");
        true
    }

    /// Send a frame via NDI for a specific sender name: handed to the
    /// sender's thread, which converts it to UYVY and sends it — this never
    /// waits on either. A frame still waiting when the next comes is let go.
    /// Creates the sender instance on first call for a given name.
    ///
    /// `rgba` is the picture tightly packed, `width` × `height` × 4 bytes.
    /// `fps` is the master render rate, declared to receivers as this sender's
    /// frame rate. Pass it already resolved (see `app::state::encoder_fps`), so
    /// an uncapped stage still declares a real rate rather than zero.
    ///
    /// # Errors
    ///
    /// The sender's thread has ended (a fault in it): the sender is
    /// destroyed, and its output is to stop. No sender to be had — no NDI
    /// runtime, none made — is no error, as before: it is tried again with
    /// the next picture.
    pub fn send_frame(
        &mut self,
        sender_name: &str,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
        fps: u32,
    ) -> Result<(), String> {
        if !self.ensure_sender(sender_name) {
            return Ok(());
        }
        self.hand(
            sender_name,
            NdiPicture::Rgba {
                bytes: rgba,
                width,
                height,
                fps,
            },
        )
    }

    /// Hand `picture` to `sender_name`'s thread. A sender whose thread has
    /// ended is destroyed: one made anew sends when its output starts again.
    fn hand(&mut self, sender_name: &str, picture: NdiPicture) -> Result<(), String> {
        let Some(sender) = self.senders.get(sender_name) else {
            return Err(format!("NDI sender '{sender_name}' disappeared"));
        };
        if sender.video.hand(picture) {
            return Ok(());
        }
        self.destroy_sender(sender_name);
        Err(format!(
            "NDI sender '{sender_name}': the thread sending its pictures ended"
        ))
    }

    /// Destroy a specific sender by name.
    pub fn destroy_sender(&mut self, sender_name: &str) {
        self.p216_converters.remove(sender_name);
        if let Some(mut sender) = self.senders.remove(sender_name) {
            // Its sound and its pictures stop first: their threads send on
            // this instance.
            if let Some(audio) = sender.audio.take() {
                audio.stop();
            }
            let (sent, replaced) = sender.video.stop();
            if let Some(ref sdk) = self.sdk {
                unsafe { (sdk.send_destroy)(sender.instance) };
            }
            log::info!(
                "NDI sender destroyed: '{sender_name}' ({sent} pictures sent, {replaced} let go for a newer)"
            );
        }
    }

    /// Destroy every sender `named` does not know: one whose output was
    /// closed, or sends under another name now. Left alone it stayed a
    /// source on the network, with its thread and its converter, till the
    /// app quit. Called every frame; it acts only when one has gone.
    pub fn keep_senders(&mut self, named: impl Fn(&str) -> bool) {
        // A ten-bit output's converter is made before its sender is.
        let gone: Vec<String> = self
            .senders
            .keys()
            .chain(self.p216_converters.keys())
            .filter(|name| !named(name.as_str()))
            .cloned()
            .collect();
        for name in gone {
            self.destroy_sender(&name);
        }
    }

    /// A sender under `sender_name` with no NDI behind it — its thread
    /// takes pictures and sends them nowhere —, for a test of what holds
    /// senders: without the NDI runtime none is ever made.
    #[cfg(test)]
    pub(crate) fn hold_sender_for_test(&mut self, sender_name: &str) {
        let video = NdiVideoSend::start(|_| {}).expect("the thread starts");
        self.senders.insert(
            sender_name.to_string(),
            NdiSender {
                instance: std::ptr::null_mut(),
                video,
                audio: None,
            },
        );
    }

    /// The names of the senders held, in order.
    #[cfg(test)]
    pub(crate) fn sender_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.senders.keys().cloned().collect();
        names.sort();
        names
    }

    /// Keep a sender carrying `bus` of the audio mixer, or no sound with
    /// `None`. Called every frame the sender sends; it only acts on a
    /// change, and not before the sender's first picture has made it.
    pub fn keep_audio(
        &mut self,
        sender_name: &str,
        bus: Option<&str>,
        audio_mix: &crate::audio_mix::AudioEngine,
    ) {
        let Some(send_audio) = self.sdk.as_ref().and_then(|sdk| sdk.send_send_audio_v3) else {
            return;
        };
        let Some(sender) = self.senders.get_mut(sender_name) else {
            return;
        };
        if sender.audio.as_ref().map(|a| a.bus.as_str()) == bus {
            return;
        }
        if let Some(old) = sender.audio.take() {
            old.stop();
        }
        let Some(bus) = bus else {
            return;
        };
        let Some(tap) = audio_mix.tap(bus) else {
            log::warn!("NDI '{sender_name}': no audio bus {bus} to carry");
            return;
        };
        sender.audio = Some(NdiAudioSend::start(bus, tap, sender.instance, send_audio));
    }

    /// A receiver's sound format once sound has arrived: (sample rate,
    /// channels, generation — which changes with the format).
    pub fn audio_format(&self, idx: usize) -> Option<(u32, u16, u32)> {
        self.receivers.get(idx)?.audio.format()
    }

    /// Send a receiver's sound to `sink` from now on, as interleaved frames
    /// in its [`Self::audio_format`]. Dropped when the format changes.
    pub fn set_audio_sink(
        &self,
        idx: usize,
        sink: crossbeam_channel::Sender<crate::audio::PcmChunk>,
    ) {
        if let Some(receiver) = self.receivers.get(idx) {
            if let Ok(mut slot) = receiver.audio.sink.lock() {
                *slot = Some(sink);
            }
        }
    }

    /// Stop a receiver, and give back its texture and last picture.
    pub fn stop_receive(&mut self, idx: usize) {
        if let Some(r) = self.receivers.get_mut(idx) {
            r.stop_flag.store(true, Ordering::SeqCst);
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
            r.frame_data.clear();
        }
        if let Some(texture) = self.textures.get_mut(idx) {
            *texture = None;
        }
    }
}

/// Convert a tightly packed RGBA picture to UYVY in `uyvy` and hand it to
/// `submit`, which has done with it when it returns (as NDI's send has).
/// `false`, nothing submitted, for a picture short of its size.
fn submit_uyvy_with(
    rgba: &[u8],
    uyvy: &mut Vec<u8>,
    width: u32,
    height: u32,
    fps: u32,
    mut submit: impl FnMut(*const ffi::NDIlib_video_frame_v2_t),
) -> bool {
    let uyvy_size = (width as usize) * (height as usize) * 2;
    if uyvy_size == 0 || rgba.len() < uyvy_size * 2 {
        return false;
    }
    if uyvy.len() != uyvy_size {
        uyvy.resize(uyvy_size, 0);
    }
    rgba_to_uyvy_in_bands(rgba, uyvy, width, height);

    // The NDI C ABI takes signed dimensions; frame sizes come from GPU
    // textures and are orders of magnitude below i32::MAX, so clamp rather
    // than let an implausible value wrap into a negative extent.
    let frame = ffi::NDIlib_video_frame_v2_t {
        xres: i32::try_from(width).unwrap_or(i32::MAX),
        yres: i32::try_from(height).unwrap_or(i32::MAX),
        FourCC: ffi::NDIlib_FourCC_video_type_e::UYVY,
        frame_rate_N: i32::try_from(fps).unwrap_or(DEFAULT_NDI_FPS).max(1),
        frame_rate_D: 1,
        picture_aspect_ratio: 0.0,
        frame_format_type: 1, // progressive
        timecode: 0,          // auto
        p_data: uyvy.as_mut_ptr(),
        line_stride_in_bytes: i32::try_from(width.saturating_mul(2)).unwrap_or(i32::MAX),
        p_metadata: std::ptr::null(),
        timestamp: 0,
    };
    submit(&raw const frame);
    true
}

/// Whether `len` bytes at `stride` are a whole P216 picture of this size.
fn p216_contract(
    len: usize,
    width: u32,
    height: u32,
    stride: u32,
) -> Result<(), p216::P216Error> {
    let layout = p216::P216Layout::new(width, height)?;
    if stride != layout.stride || len as u64 != layout.byte_len {
        return Err(p216::P216Error::InvalidBuffer {
            expected: layout.byte_len,
            actual: len,
        });
    }
    Ok(())
}

fn submit_p216_with(
    data: &mut [u8],
    width: u32,
    height: u32,
    stride: u32,
    fps: u32,
    mut submit: impl FnMut(*const ffi::NDIlib_video_frame_v2_t),
) -> Result<(), p216::P216Error> {
    p216_contract(data.len(), width, height, stride)?;
    let frame = ffi::NDIlib_video_frame_v2_t {
        xres: i32::try_from(width).unwrap_or(i32::MAX),
        yres: i32::try_from(height).unwrap_or(i32::MAX),
        FourCC: ffi::NDIlib_FourCC_video_type_e::P216,
        frame_rate_N: i32::try_from(fps).unwrap_or(DEFAULT_NDI_FPS).max(1),
        frame_rate_D: 1,
        picture_aspect_ratio: 0.0,
        frame_format_type: 1,
        timecode: i64::MAX,
        p_data: data.as_mut_ptr(),
        line_stride_in_bytes: i32::try_from(stride).unwrap_or(i32::MAX),
        p_metadata: p216::REC709_METADATA.as_ptr().cast(),
        timestamp: 0,
    };
    submit(&raw const frame);
    Ok(())
}

impl Drop for NdiManager {
    fn drop(&mut self) {
        // Stop all receivers and join their threads before SDK cleanup
        for r in &mut self.receivers {
            r.stop_flag.store(true, Ordering::SeqCst);
            if let Some(t) = r.thread.take() {
                let _ = t.join();
            }
        }
        // Destroy all senders
        let sender_names: Vec<String> = self.senders.keys().cloned().collect();
        for name in sender_names {
            self.destroy_sender(&name);
        }
        // Destroy NDI SDK
        if let Some(ref sdk) = self.sdk {
            if !self.finder.is_null() {
                unsafe { (sdk.find_destroy)(self.finder) };
            }
            unsafe { (sdk.destroy)() };
        }
    }
}

/// Convert an NDI video frame (UYVY, BGRA, BGRX, or RGBA) to RGBA.
#[cfg(test)]
fn convert_ndi_frame_to_rgba(vf: &ffi::NDIlib_video_frame_v2_t, w: u32, h: u32) -> Vec<u8> {
    let pixel_count = (w * h) as usize;
    let mut rgba = vec![0u8; pixel_count * 4];

    if vf.FourCC == ffi::NDIlib_FourCC_video_type_e::UYVY {
        let stride = if vf.line_stride_in_bytes > 0 {
            vf.line_stride_in_bytes as usize
        } else {
            w as usize * 2
        };
        let uyvy_data = unsafe { std::slice::from_raw_parts(vf.p_data, h as usize * stride) };
        uyvy_to_rgba(uyvy_data, &mut rgba, w, h, stride);
    } else if vf.FourCC == ffi::NDIlib_FourCC_video_type_e::BGRA
        || vf.FourCC == ffi::NDIlib_FourCC_video_type_e::BGRX
    {
        let stride = if vf.line_stride_in_bytes > 0 {
            vf.line_stride_in_bytes as usize
        } else {
            w as usize * 4
        };
        let bgra_data = unsafe { std::slice::from_raw_parts(vf.p_data, h as usize * stride) };
        // A source sending transparency (a keyed graphic) sends BGRA, its
        // alpha straight; BGRX's fourth byte means nothing, so it is opaque.
        let keeps_alpha = vf.FourCC == ffi::NDIlib_FourCC_video_type_e::BGRA;
        for y in 0..h as usize {
            for x in 0..w as usize {
                let src = y * stride + x * 4;
                let dst = (y * w as usize + x) * 4;
                rgba[dst] = bgra_data[src + 2]; // R
                rgba[dst + 1] = bgra_data[src + 1]; // G
                rgba[dst + 2] = bgra_data[src]; // B
                rgba[dst + 3] = if keeps_alpha { bgra_data[src + 3] } else { 255 };
            }
        }
    } else if vf.FourCC == ffi::NDIlib_FourCC_video_type_e::RGBA {
        let data_size = pixel_count * 4;
        let src = unsafe { std::slice::from_raw_parts(vf.p_data, data_size) };
        rgba.copy_from_slice(src);
    } else {
        log::warn!("Unknown NDI FourCC: {:?}, filling black", vf.FourCC);
    }

    rgba
}

/// Convert RGBA to UYVY 4:2:2 for NDI sending, as NDI reads it: video
/// levels (16–235), BT.709 for HD and up, BT.601 for standard definition.
/// Each pair of pixels shares the average of their colour.
pub(crate) fn rgba_to_uyvy(rgba: &[u8], uyvy: &mut [u8], w: u32, h: u32) {
    rgba_rows_to_uyvy(rgba, uyvy, w as usize, h as usize, colour_coefficients(h));
}

/// (Kr, Kb) of the colour space of a picture `h` high.
fn colour_coefficients(h: u32) -> (f32, f32) {
    if h >= 720 {
        (0.2126, 0.0722)
    } else {
        (0.299, 0.114)
    }
}

/// Pixels to a band of [`rgba_to_uyvy_in_bands`]: a 1080p picture's.
const BAND_PIXELS: usize = 1920 * 1080;

/// The most bands a picture is converted in: a 4K picture's four.
const MAX_BANDS: usize = 4;

/// Bands a picture of this size is converted in.
fn band_count(width: usize, height: usize) -> usize {
    (width * height / BAND_PIXELS).clamp(1, MAX_BANDS)
}

/// One band's rows: RGBA in, UYVY out.
type Band<'a> = (&'a [u8], &'a mut [u8]);

/// Convert a band, unless it has been.
fn convert_band(band: &mut Option<Band<'_>>, width: usize, coefficients: (f32, f32)) {
    if let Some((from, to)) = band.take() {
        let rows = to.len() / (width * 2);
        rgba_rows_to_uyvy(from, to, width, rows, coefficients);
    }
}

/// [`rgba_to_uyvy`] — the same bytes — with a large picture's rows converted
/// in bands, on threads at once: one thread alone takes longer over a 4K
/// picture than a frame lasts at 60 a second. For a sender's own thread.
fn rgba_to_uyvy_in_bands(rgba: &[u8], uyvy: &mut [u8], w: u32, h: u32) {
    let (width, height) = (w as usize, h as usize);
    let bands = band_count(width, height);
    // In one pass: a small picture; an odd width, whose last pixel reaches
    // into the next row; a buffer short of the picture.
    if bands == 1
        || !width.is_multiple_of(2)
        || rgba.len() < width * height * 4
        || uyvy.len() < width * height * 2
    {
        rgba_to_uyvy(rgba, uyvy, w, h);
        return;
    }
    // The whole picture's colour space, whatever a band's height.
    let coefficients = colour_coefficients(h);
    let rows_each = height.div_ceil(bands);
    let mut jobs: Vec<Option<Band<'_>>> = rgba[..width * height * 4]
        .chunks(rows_each * width * 4)
        .zip(uyvy[..width * height * 2].chunks_mut(rows_each * width * 2))
        .map(Some)
        .collect();
    std::thread::scope(|scope| {
        let mut jobs = jobs.iter_mut();
        let ours = jobs.next();
        for job in jobs {
            // A band whose thread does not start is converted below.
            let _ = std::thread::Builder::new()
                .name("ndi-uyvy".into())
                .spawn_scoped(scope, move || convert_band(job, width, coefficients));
        }
        if let Some(job) = ours {
            convert_band(job, width, coefficients);
        }
    });
    for job in &mut jobs {
        convert_band(job, width, coefficients);
    }
}

/// `rows` rows of [`rgba_to_uyvy`], in the colour space of (`kr`, `kb`).
fn rgba_rows_to_uyvy(rgba: &[u8], uyvy: &mut [u8], w: usize, rows: usize, (kr, kb): (f32, f32)) {
    let kg = 1.0 - kr - kb;
    let luma = |r: f32, g: f32, b: f32| kr * r + kg * g + kb * b;
    let to_y = |l: f32| (16.0 + l * 219.0 / 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    let to_c = |c: f32| (128.0 + c * 224.0 / 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    for y in 0..rows {
        for x in (0..w).step_by(2) {
            let src0 = (y * w + x) * 4;
            let src1 = src0 + 4;

            let r0 = f32::from(rgba[src0]);
            let g0 = f32::from(rgba[src0 + 1]);
            let b0 = f32::from(rgba[src0 + 2]);

            let (r1, g1, b1) = if x + 1 < w && src1 + 2 < rgba.len() {
                (
                    f32::from(rgba[src1]),
                    f32::from(rgba[src1 + 1]),
                    f32::from(rgba[src1 + 2]),
                )
            } else {
                (r0, g0, b0)
            };

            let (l0, l1) = (luma(r0, g0, b0), luma(r1, g1, b1));
            let pair_luma = f32::midpoint(l0, l1);
            let cb = (f32::midpoint(b0, b1) - pair_luma) / (2.0 * (1.0 - kb));
            let cr = (f32::midpoint(r0, r1) - pair_luma) / (2.0 * (1.0 - kr));

            let dst = y * w * 2 + x * 2;
            if dst + 3 < uyvy.len() {
                uyvy[dst] = to_c(cb);
                uyvy[dst + 1] = to_y(l0);
                uyvy[dst + 2] = to_c(cr);
                uyvy[dst + 3] = to_y(l1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::value::render::{PresentationDepth, PresentationPixelFormat};

    /// What a sender's thread submitted.
    struct Submitted {
        /// The thread it was submitted on.
        on: std::thread::ThreadId,
        size: (i32, i32),
        kind: ffi::NDIlib_FourCC_video_type_e,
        stride: i32,
        rate: i32,
        /// The frame's first bytes.
        bytes: Vec<u8>,
    }

    /// A sender's thread whose `submit` tells the test of each frame — its
    /// first `bytes` bytes — and then waits for the test's word, as NDI
    /// holds a send while it compresses the picture. With the word's sender
    /// dropped it does not wait.
    fn held_sender(
        bytes: usize,
    ) -> (
        NdiVideoSend,
        std::sync::mpsc::Receiver<Submitted>,
        std::sync::mpsc::Sender<()>,
    ) {
        let (told, frames) = std::sync::mpsc::channel();
        let (word, words) = std::sync::mpsc::channel::<()>();
        let started = NdiVideoSend::start(move |frame_ptr| {
            let frame = unsafe { &*frame_ptr };
            let _ = told.send(Submitted {
                on: std::thread::current().id(),
                size: (frame.xres, frame.yres),
                kind: frame.FourCC,
                stride: frame.line_stride_in_bytes,
                rate: frame.frame_rate_N,
                bytes: unsafe { std::slice::from_raw_parts(frame.p_data, bytes) }.to_vec(),
            });
            // Till the test's word, or till the test lets go of it.
            let _ = words.recv();
        });
        (started.expect("the thread starts"), frames, word)
    }

    #[test]
    fn ndi_pictures_are_sent_from_a_thread_of_their_own_the_newest_first() {
        const WAIT: std::time::Duration = std::time::Duration::from_secs(5);
        const RED: [u8; 3] = [255, 0, 0];
        const GREEN: [u8; 3] = [0, 255, 0];
        const BLUE: [u8; 3] = [0, 0, 255];
        // A picture two pixels wide of one colour, and what it is as UYVY.
        let rgba = |rgb: [u8; 3]| [rgb[0], rgb[1], rgb[2], 255].repeat(2);
        let picture = |rgb: [u8; 3]| NdiPicture::Rgba {
            bytes: rgba(rgb),
            width: 2,
            height: 1,
            fps: 50,
        };
        let uyvy = |rgb: [u8; 3]| {
            let mut out = vec![0u8; 4];
            rgba_to_uyvy(&rgba(rgb), &mut out, 2, 1);
            out
        };
        let (send, frames, word) = held_sender(4);

        assert!(send.hand(picture(RED)));
        let first = frames
            .recv_timeout(WAIT)
            .expect("the first picture is sent");
        assert_ne!(
            first.on,
            std::thread::current().id(),
            "the picture was sent from the thread that handed it over"
        );
        assert_eq!(first.size, (2, 1));
        assert_eq!(first.kind, ffi::NDIlib_FourCC_video_type_e::UYVY);
        assert_eq!((first.stride, first.rate), (4, 50));
        assert_eq!(first.bytes, uyvy(RED));

        // The thread is held in its send. Two more pictures are handed over:
        // neither waits on it, and the older is let go for the newer.
        assert!(send.hand(picture(GREEN)));
        assert!(send.hand(picture(BLUE)));
        assert!(
            frames.try_recv().is_err(),
            "a picture was sent while the first still was"
        );
        word.send(()).expect("the thread is there");
        let second = frames
            .recv_timeout(WAIT)
            .expect("the newest picture is sent");
        assert_eq!(
            second.bytes,
            uyvy(BLUE),
            "the picture sent second is not the newest handed over"
        );

        // Its send let go, the thread stops: two sent, one let go.
        drop(word);
        assert_eq!(send.stop(), (2, 1));
        assert!(frames.try_recv().is_err(), "a third picture was sent");
    }

    #[test]
    fn a_p216_picture_is_sent_by_the_thread_as_it_was_made() {
        let bytes = vec![0x00, 0x10, 0x00, 0xeb, 0x00, 0x80, 0x00, 0x80];
        let (send, frames, word) = held_sender(8);
        drop(word);
        assert!(send.hand(NdiPicture::P216 {
            bytes: bytes.clone(),
            width: 2,
            height: 1,
            stride: 4,
            fps: 60,
        }));
        let frame = frames
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the picture is sent");
        assert_ne!(frame.on, std::thread::current().id());
        assert_eq!(frame.size, (2, 1));
        assert_eq!(frame.kind, ffi::NDIlib_FourCC_video_type_e::P216);
        assert_eq!((frame.stride, frame.rate), (4, 60));
        assert_eq!(frame.bytes, bytes);
        assert_eq!(send.stop(), (1, 0));
    }

    #[test]
    fn a_sender_whose_thread_has_ended_takes_no_picture() {
        let picture = || NdiPicture::Rgba {
            bytes: vec![0, 0, 0, 255, 0, 0, 0, 255],
            width: 2,
            height: 1,
            fps: 50,
        };
        let (send, frames, word) = held_sender(4);
        drop(word);
        assert!(send.hand(picture()), "a thread that runs takes a picture");
        frames
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("the picture is sent");
        // The thread ended, not through `stop`: as a fault in it would.
        send.shared.lock().stop = true;
        send.shared.pictures.notify_one();
        let since = std::time::Instant::now();
        while !send.thread.is_finished() {
            assert!(since.elapsed().as_secs() < 10, "the thread did not end");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(!send.hand(picture()), "a thread that has ended took one");
        assert!(send.shared.lock().picture.is_none());
        assert_eq!(send.stop(), (1, 0));
    }

    /// A sender no output names is destroyed, its thread ended; the others
    /// run on. A second sweep finds nothing to do.
    #[test]
    fn a_sender_no_output_names_is_destroyed_and_its_thread_ends() {
        let mut manager = NdiManager::new_disabled();
        let mut hold = |name: &str| {
            let (video, frames, word) = held_sender(4);
            manager.senders.insert(
                name.to_string(),
                NdiSender {
                    instance: std::ptr::null_mut(),
                    video,
                    audio: None,
                },
            );
            (frames, word)
        };
        let (kept_frames, _kept_word) = hold("Kept");
        let (gone_frames, _gone_word) = hold("Gone");

        manager.keep_senders(|name| name == "Kept");

        assert_eq!(manager.sender_names(), ["Kept"]);
        // The thread has ended: what it sent through is let go with it.
        assert_eq!(
            gone_frames.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Disconnected)
        );
        assert_eq!(
            kept_frames.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Empty)
        );
        manager.keep_senders(|name| name == "Kept");
        assert_eq!(manager.sender_names(), ["Kept"]);
        manager.keep_senders(|_| false);
        assert!(manager.sender_names().is_empty());
        assert_eq!(
            kept_frames.try_recv().err(),
            Some(std::sync::mpsc::TryRecvError::Disconnected)
        );
    }

    /// A ten-bit output's converter is made before its sender: closed
    /// before its first picture went out, the output leaves a converter and
    /// no sender, and that goes too.
    #[test]
    fn a_converter_no_output_names_goes_with_the_senders() {
        let Ok(context) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let mut manager = NdiManager::new_disabled();
        for name in ["Kept", "Gone"] {
            let converter = p216::P216Converter::new(&context.device, 2, 1).unwrap();
            manager.p216_converters.insert(name.to_string(), converter);
        }

        manager.keep_senders(|name| name == "Kept");

        let mut left: Vec<&str> = manager.p216_converters.keys().map(String::as_str).collect();
        left.sort_unstable();
        assert_eq!(left, ["Kept"]);
    }

    #[test]
    fn a_p216_frame_out_of_its_contract_is_refused_before_it_is_handed_over() {
        assert!(p216_contract(8, 2, 1, 4).is_ok());
        // A byte short; another stride; an odd width.
        assert!(p216_contract(7, 2, 1, 4).is_err());
        assert!(p216_contract(8, 2, 1, 2).is_err());
        assert!(p216_contract(12, 3, 1, 6).is_err());
    }

    /// A picture `w` × `h` with something else in every byte.
    fn noise(w: u32, h: u32) -> Vec<u8> {
        let mut state = 0x2545_f491_u32;
        (0..w as usize * h as usize * 4)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (state >> 24) as u8
            })
            .collect()
    }

    /// `rgba` converted whole, and in bands.
    fn whole_and_banded(rgba: &[u8], w: u32, h: u32) -> (Vec<u8>, Vec<u8>) {
        let mut whole = vec![1u8; w as usize * h as usize * 2];
        rgba_to_uyvy(rgba, &mut whole, w, h);
        let mut banded = vec![1u8; whole.len()];
        rgba_to_uyvy_in_bands(rgba, &mut banded, w, h);
        (whole, banded)
    }

    #[test]
    fn a_large_picture_converted_in_bands_is_the_same_bytes() {
        assert_eq!(band_count(1920, 1080), 1);
        assert_eq!(band_count(3840, 1083), 2);
        assert_eq!(band_count(3840, 2160), 4);
        assert_eq!(band_count(7680, 4320), 4);
        // Two bands, of 542 rows and 541: each fewer than the 720 a picture
        // is in HD's colours from, as the whole picture is.
        let (w, h) = (3840, 1083);
        let (whole, banded) = whole_and_banded(&noise(w, h), w, h);
        assert!(
            banded == whole,
            "a picture converted in bands differs from the same converted whole"
        );
    }

    #[test]
    fn a_picture_of_odd_width_is_converted_in_one_pass() {
        // Large enough for two bands; but an odd width's last pixel reaches
        // into the next row, which a band's last row does not have.
        let (w, h) = (3841, 1083);
        assert_eq!(band_count(3841, 1083), 2);
        let (whole, banded) = whole_and_banded(&noise(w, h), w, h);
        assert!(
            banded == whole,
            "an odd-width picture converted in bands differs from the same converted whole"
        );
    }

    #[test]
    fn ndi_pictures_are_sent_at_video_levels_in_their_colour_space() {
        // A picture two pixels wide, `h` high, all one colour: its first pair.
        let px = |rgba: [u8; 4], h: u32| {
            let picture = rgba.repeat(2 * h as usize);
            let mut out = vec![0u8; 4 * h as usize];
            rgba_to_uyvy(&picture, &mut out, 2, h);
            out[..4].to_vec()
        };
        // Black and white at 16 and 235, no colour.
        assert_eq!(px([0, 0, 0, 255], 1080), vec![128, 16, 128, 16]);
        assert_eq!(px([255, 255, 255, 255], 1080), vec![128, 235, 128, 235]);
        // Red in HD is BT.709's: Y 63, Cb 102, Cr 240.
        assert_eq!(px([255, 0, 0, 255], 1080), vec![102, 63, 240, 63]);
        // In standard definition, BT.601's: Y 81, Cb 90, Cr 240.
        assert_eq!(px([255, 0, 0, 255], 576), vec![90, 81, 240, 81]);
    }

    #[test]
    fn interleaved_sound_is_sent_planar() {
        let interleaved = [0.1, -0.1, 0.2, -0.2, 0.3, -0.3];
        let planar = deinterleave(&interleaved, 2);
        assert_eq!(planar, vec![0.1, 0.2, 0.3, -0.1, -0.2, -0.3]);
        let (left, right) = planar.split_at(3);
        assert_eq!(interleave(&[left, right]), interleaved);
    }

    #[test]
    fn planar_sound_is_interleaved_frame_by_frame() {
        let left = [0.1, 0.2, 0.3];
        let right = [-0.1, -0.2, -0.3];
        assert_eq!(
            interleave(&[&left, &right]),
            vec![0.1, -0.1, 0.2, -0.2, 0.3, -0.3]
        );
        assert_eq!(interleave(&[&left]), vec![0.1, 0.2, 0.3]);
        assert!(interleave(&[]).is_empty());
    }

    /// One NDI audio frame over `planes`, as the SDK hands it out.
    fn frame(rate: i32, planes: &mut [Vec<f32>]) -> (Vec<f32>, ffi::NDIlib_audio_frame_v3_t) {
        let samples = planes[0].len();
        let mut data: Vec<f32> = planes.iter().flat_map(|p| p.iter().copied()).collect();
        let af = ffi::NDIlib_audio_frame_v3_t {
            sample_rate: rate,
            no_channels: i32::try_from(planes.len()).unwrap(),
            no_samples: i32::try_from(samples).unwrap(),
            timecode: 0,
            FourCC: ffi::NDIlib_audio_frame_v3_t::FLTP,
            p_data: data.as_mut_ptr().cast(),
            channel_stride_in_bytes: i32::try_from(samples * 4).unwrap(),
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        (data, af)
    }

    #[test]
    fn a_receivers_sound_reaches_its_listener_and_a_new_format_drops_it() {
        let audio = NdiAudio::default();
        assert_eq!(audio.format(), None, "nothing until sound arrives");
        let (_keep, af) = frame(48_000, &mut [vec![0.5, 0.25], vec![-0.5, -0.25]]);
        audio.take(&af);
        let (rate, channels, first) = audio.format().expect("known after the first frame");
        assert_eq!((rate, channels), (48_000, 2));

        let (tx, rx) = crossbeam_channel::bounded(4);
        *audio.sink.lock().unwrap() = Some(tx);
        audio.take(&af);
        assert_eq!(rx.try_recv().unwrap().samples, vec![0.5, -0.5, 0.25, -0.25]);

        // A different rate: the listener has to pick it up afresh.
        let (_keep, af) = frame(44_100, &mut [vec![0.1], vec![0.2]]);
        audio.take(&af);
        let (rate, _, second) = audio.format().unwrap();
        assert_eq!(rate, 44_100);
        assert_ne!(first, second);
        assert!(audio.sink.lock().unwrap().is_none());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn ndi_manager_new_no_crash() {
        // NdiManager::new() attempts to load SDK dynamically.
        // On machines without NDI SDK, it gracefully returns with sdk=None.
        let mgr = NdiManager::new();
        // is_available depends on whether SDK is installed — just verify no panic
        let _ = mgr.is_available();
    }

    #[test]
    fn ndi_manager_sources_empty() {
        let mgr = NdiManager::new();
        assert!(mgr.sources().is_empty());
        assert!(mgr.discovered_sources().is_empty());
    }

    #[test]
    fn ndi_manager_texture_view_out_of_bounds() {
        let mgr = NdiManager::new();
        assert!(mgr.texture_view(0).is_none());
        assert!(mgr.texture_view(999).is_none());
    }

    #[test]
    fn ndi_manager_receiver_dimensions_out_of_bounds() {
        let mgr = NdiManager::new();
        assert!(mgr.receiver_dimensions(0).is_none());
    }

    #[test]
    fn ndi_manager_is_connected_out_of_bounds() {
        let mgr = NdiManager::new();
        assert!(!mgr.is_connected(0));
        assert!(!mgr.is_connected(999));
    }

    #[test]
    fn ndi_source_debug() {
        let src = NdiSource {
            name: "Test Source".to_string(),
        };
        let debug = format!("{src:?}");
        assert!(debug.contains("Test Source"));
    }

    #[test]
    fn ndi_source_clone() {
        let src = NdiSource {
            name: "Test".to_string(),
        };
        let cloned = src.clone();
        assert_eq!(src.name, cloned.name);
    }

    #[test]
    fn ndi_six_request_resolves_to_p216() {
        let request = PresentationRequest {
            depth: PresentationDepth::Sdr10,
            dither: true,
            video_levels: false,
        };
        let capability = sdk::NdiSendCapability::from_runtime_evidence(Some("NDI SDK 6.3.1"), true);

        let resolved = NdiManager::resolve_presentation_for_capability(request, capability);

        assert_eq!(resolved.requested, PresentationDepth::Sdr10);
        assert_eq!(resolved.resolved, PresentationDepth::Sdr10);
        assert_eq!(resolved.pixel_format, PresentationPixelFormat::P216);
        assert_eq!(
            resolved.color_profile,
            PresentationColorProfile::Rec709Limited
        );
        assert_eq!(resolved.alpha_mode, AlphaMode::Opaque);
        assert!(resolved.fallback_reason.is_none());
    }

    #[test]
    fn eight_bit_request_remains_uyvy_without_fallback_warning() {
        let capability = sdk::NdiSendCapability::from_runtime_evidence(Some("NDI SDK 6.3.1"), true);

        let resolved = NdiManager::resolve_presentation_for_capability(
            PresentationRequest::default(),
            capability,
        );

        assert_eq!(resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(resolved.pixel_format, PresentationPixelFormat::Uyvy);
        assert!(resolved.fallback_reason.is_none());
    }

    #[test]
    fn pre_ndi_six_request_resolves_to_uyvy_fallback() {
        let request = PresentationRequest {
            depth: PresentationDepth::Sdr10,
            dither: true,
            video_levels: false,
        };
        let capability = sdk::NdiSendCapability::from_runtime_evidence(Some("NDI SDK 5.6.0"), true);

        let resolved = NdiManager::resolve_presentation_for_capability(request, capability);

        assert_eq!(resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(resolved.pixel_format, PresentationPixelFormat::Uyvy);
        assert!(resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("predates"));
    }

    #[test]
    fn mock_sender_receives_p216_contract_and_planes() {
        let mut bytes = vec![
            0x00, 0x10, 0x00, 0xeb, // Y: limited black, limited white
            0x00, 0x80, 0x00, 0x80, // U,V: neutral
        ];
        let mut captured = None;

        submit_p216_with(&mut bytes, 2, 1, 4, 60, |frame_ptr| {
            let frame = unsafe { &*frame_ptr };
            let metadata = unsafe { std::ffi::CStr::from_ptr(frame.p_metadata) }
                .to_str()
                .unwrap()
                .to_string();
            let submitted = unsafe { std::slice::from_raw_parts(frame.p_data, 8) }.to_vec();
            captured = Some((
                frame.FourCC,
                frame.line_stride_in_bytes,
                metadata,
                submitted,
            ));
        })
        .unwrap();

        let (fourcc, stride, metadata, submitted) = captured.unwrap();
        assert_eq!(fourcc, ffi::NDIlib_FourCC_video_type_e::P216);
        assert_eq!(stride, 4);
        assert!(metadata.contains("primaries=\"bt_709\""));
        assert_eq!(submitted, bytes);
    }

    // ── Color conversion tests ──────────────────────────────────────

    #[test]
    fn uyvy_to_rgba_pure_white() {
        // UYVY for white: Y=235, U=128, V=128
        // Two pixels per macropixel: [U, Y0, V, Y1]
        let uyvy = vec![128, 235, 128, 235];
        let mut rgba = vec![0u8; 2 * 4]; // 2 pixels
        uyvy_to_rgba(&uyvy, &mut rgba, 2, 1, 4);
        // Y=235 is video white, no colour → full white
        assert_eq!(rgba[0], 255); // R
        assert_eq!(rgba[1], 255); // G
        assert_eq!(rgba[2], 255); // B
        assert_eq!(rgba[3], 255); // A
        assert_eq!(rgba[4], 255); // R (second pixel)
        assert_eq!(rgba[7], 255); // A
    }

    /// A keyed graphic sent as BGRA keeps its transparency; BGRX, whose
    /// fourth byte means nothing, is opaque. Rows padded out are read row
    /// by row.
    #[test]
    fn bgra_keeps_its_alpha_and_bgrx_is_opaque() {
        let frame = |fourcc, data: &mut Vec<u8>| ffi::NDIlib_video_frame_v2_t {
            xres: 2,
            yres: 2,
            FourCC: fourcc,
            frame_rate_N: 30,
            frame_rate_D: 1,
            picture_aspect_ratio: 0.0,
            frame_format_type: 1,
            timecode: 0,
            p_data: data.as_mut_ptr(),
            line_stride_in_bytes: 12,
            p_metadata: std::ptr::null(),
            timestamp: 0,
        };
        // Two rows of two pixels, blue then half-transparent red, each row
        // padded by four bytes.
        let mut data = vec![
            255, 0, 0, 255, 0, 0, 255, 128, 9, 9, 9, 9, //
            255, 0, 0, 255, 0, 0, 255, 128, 9, 9, 9, 9,
        ];
        let keyed = convert_ndi_frame_to_rgba(
            &frame(ffi::NDIlib_FourCC_video_type_e::BGRA, &mut data),
            2,
            2,
        );
        assert_eq!(&keyed[..8], &[0, 0, 255, 255, 255, 0, 0, 128]);
        assert_eq!(&keyed[8..], &keyed[..8], "the second row, past the padding");
        let plain = convert_ndi_frame_to_rgba(
            &frame(ffi::NDIlib_FourCC_video_type_e::BGRX, &mut data),
            2,
            2,
        );
        assert_eq!(&plain[4..8], &[255, 0, 0, 255]);
    }

    #[test]
    fn uyvy_to_rgba_pure_black() {
        // UYVY for black: Y=0, U=128, V=128
        let uyvy = vec![128, 0, 128, 0];
        let mut rgba = vec![0u8; 2 * 4];
        uyvy_to_rgba(&uyvy, &mut rgba, 2, 1, 4);
        assert_eq!(rgba[0], 0); // R
        assert_eq!(rgba[1], 0); // G
        assert_eq!(rgba[2], 0); // B
        assert_eq!(rgba[3], 255); // A (always opaque)
    }

    #[test]
    fn uyvy_to_rgba_alpha_always_255() {
        // Regardless of input, alpha should always be 255
        let uyvy = vec![100, 150, 200, 100];
        let mut rgba = vec![0u8; 2 * 4];
        uyvy_to_rgba(&uyvy, &mut rgba, 2, 1, 4);
        assert_eq!(rgba[3], 255);
        assert_eq!(rgba[7], 255);
    }

    #[test]
    fn uyvy_to_rgba_multirow() {
        let w = 4u32;
        let h = 2u32;
        let stride = w as usize * 2;
        // Fill with neutral gray (Y=128, U=128, V=128)
        let uyvy = vec![128u8; (h as usize) * stride];
        let mut rgba = vec![0u8; (w * h) as usize * 4];
        uyvy_to_rgba(&uyvy, &mut rgba, w, h, stride);
        // Check all alphas are 255
        for i in 0..(w * h) as usize {
            assert_eq!(rgba[i * 4 + 3], 255, "alpha at pixel {i} should be 255");
        }
    }

    #[test]
    fn rgba_to_uyvy_pure_black() {
        let rgba = vec![0u8; 2 * 4]; // 2 black pixels
        let mut uyvy = vec![0u8; 4]; // 1 macropixel
        rgba_to_uyvy(&rgba, &mut uyvy, 2, 1);
        // Black: Y at video black, 16; U and V at 128
        assert_eq!(uyvy[1], 16); // Y0
        assert_eq!(uyvy[3], 16); // Y1
        assert_eq!(uyvy[0], 128); // U
        assert_eq!(uyvy[2], 128); // V
    }

    #[test]
    fn rgba_to_uyvy_pure_white() {
        let rgba = vec![255, 255, 255, 255, 255, 255, 255, 255];
        let mut uyvy = vec![0u8; 4];
        rgba_to_uyvy(&rgba, &mut uyvy, 2, 1);
        // White: Y at video white, 235; U and V at 128
        assert_eq!(uyvy[1], 235); // Y0
        assert_eq!(uyvy[3], 235); // Y1
        assert_eq!(uyvy[0], 128); // U
        assert_eq!(uyvy[2], 128); // V
    }

    #[test]
    fn rgba_uyvy_roundtrip_gray() {
        // Roundtrip test: RGBA → UYVY → RGBA should be close to original
        let original_rgba = vec![128, 128, 128, 255, 128, 128, 128, 255];
        let mut uyvy = vec![0u8; 4];
        rgba_to_uyvy(&original_rgba, &mut uyvy, 2, 1);

        let mut restored = vec![0u8; 2 * 4];
        uyvy_to_rgba(&uyvy, &mut restored, 2, 1, 4);

        // Allow ±2 tolerance for floating point in color space conversion
        for i in 0..2 {
            for c in 0..3 {
                let orig = i32::from(original_rgba[i * 4 + c]);
                let rest = i32::from(restored[i * 4 + c]);
                assert!(
                    (orig - rest).abs() <= 2,
                    "pixel {i} channel {c} mismatch: {orig} vs {rest}"
                );
            }
        }
    }

    #[test]
    fn rgba_uyvy_roundtrip_red() {
        let original = vec![255, 0, 0, 255, 255, 0, 0, 255];
        let mut uyvy = vec![0u8; 4];
        rgba_to_uyvy(&original, &mut uyvy, 2, 1);

        let mut restored = vec![0u8; 2 * 4];
        uyvy_to_rgba(&uyvy, &mut restored, 2, 1, 4);

        // Red should survive roundtrip within tolerance
        for i in 0..2 {
            let r = i32::from(restored[i * 4]);
            let g = i32::from(restored[i * 4 + 1]);
            let b = i32::from(restored[i * 4 + 2]);
            assert!(r > 200, "red channel should be high: {r}");
            assert!(g < 50, "green channel should be low: {g}");
            assert!(b < 50, "blue channel should be low: {b}");
        }
    }

    #[test]
    fn an_hd_picture_comes_back_as_it_went() {
        // BT.709 both ways at 720 lines and up: colours survive the trip.
        let (w, h) = (2u32, 720u32);
        for colour in [[255, 0, 0, 255], [0, 200, 60, 255], [30, 60, 220, 255]] {
            let picture = colour.repeat((w * h) as usize);
            let mut uyvy = vec![0u8; (w * h * 2) as usize];
            rgba_to_uyvy(&picture, &mut uyvy, w, h);
            let mut back = vec![0u8; (w * h * 4) as usize];
            uyvy_to_rgba(&uyvy, &mut back, w, h, (w * 2) as usize);
            for c in 0..3 {
                let (sent, got) = (i32::from(colour[c]), i32::from(back[c]));
                assert!(
                    (sent - got).abs() <= 2,
                    "{colour:?} came back {:?}",
                    &back[..4]
                );
            }
        }
    }

    #[test]
    fn uyvy_to_rgba_stride_larger_than_width() {
        // Stride can be larger than width*2 (padding)
        let w = 2u32;
        let h = 1u32;
        let stride = 8; // padded stride (width*2=4, but stride=8)
        let mut uyvy = vec![0u8; stride];
        // Set actual pixel data in first 4 bytes
        uyvy[0] = 128; // U
        uyvy[1] = 200; // Y0
        uyvy[2] = 128; // V
        uyvy[3] = 200; // Y1
                       // Bytes 4-7 are padding

        let mut rgba = vec![0u8; 2 * 4];
        uyvy_to_rgba(&uyvy, &mut rgba, w, h, stride);
        // No colour: R = (200 - 16) × 255 / 219, video levels to full
        assert_eq!(rgba[0], 214);
        assert_eq!(rgba[3], 255); // A
    }
}
