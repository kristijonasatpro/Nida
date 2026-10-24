//! Stream receive — background ffmpeg decode thread → shared buffer → GPU upload.
//!
//! SRT output streaming is handled per-output via `FfmpegSubprocess` in
//! `src/renderer/subprocess.rs`. This module handles stream *input* (receiving
//! video from SRT/HLS/DASH sources and displaying them as deck sources).
//!
//! Architecture mirrors `NdiManager`: background thread decodes frames into
//! `Arc<Mutex<Option<Vec<u8>>>>`, main thread uploads to GPU each frame.

use std::sync::{
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Serializes decoder creation (`avcodec_open2`). Held only while a decoder
/// opens — never while a stream connects: a listener waiting for its sender
/// would hold every other stream input waiting with it.
static FFMPEG_INIT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A stream's latest picture, RGBA at the stream's own size.
struct StreamFrame {
    data: Vec<u8>,
    width: u32,
    height: u32,
}

/// Where a receive thread leaves its latest picture.
type FrameSlot = Mutex<Option<StreamFrame>>;

/// A stream's sound: its format once some has come, and where it goes (the
/// audio mixer, as a strip of its own, as NDI's sound).
#[derive(Default)]
struct StreamAudio {
    /// Sample rate << 16 | channels; 0 until the first sound comes.
    format: AtomicU64,
    /// Counts format changes: whoever listens picks the sound up afresh.
    generation: AtomicU32,
    /// The audio mixer's end, once it listens.
    sink: Mutex<Option<crossbeam_channel::Sender<crate::audio::PcmChunk>>>,
}

impl StreamAudio {
    /// Pass on interleaved stereo sound at `rate`, noting a change of rate.
    fn take(&self, samples: Vec<f32>, rate: u32) {
        let format = u64::from(rate) << 16 | 2;
        let Ok(mut sink) = self.sink.lock() else {
            return;
        };
        if self.format.swap(format, Ordering::SeqCst) != format {
            self.generation.fetch_add(1, Ordering::SeqCst);
            *sink = None;
            log::info!("Stream sound: {rate} Hz");
        }
        let Some(tx) = sink.as_ref() else {
            return;
        };
        if let Err(crossbeam_channel::TrySendError::Disconnected(_)) =
            tx.try_send(crate::audio::PcmChunk {
                samples,
                lost_before: 0,
            })
        {
            *sink = None;
        }
    }

    /// (sample rate, channels, generation), once sound has come.
    fn format(&self) -> Option<(u32, u16, u32)> {
        let format = self.format.load(Ordering::SeqCst);
        let rate = u32::try_from(format >> 16).ok()?;
        let channels = u16::try_from(format & 0xffff).ok()?;
        (rate > 0 && channels > 0).then(|| (rate, channels, self.generation.load(Ordering::SeqCst)))
    }
}

/// A stream's sound decoder, and the resampler to interleaved stereo at
/// its own rate.
struct SoundDecoder {
    stream: usize,
    decoder: ffmpeg_next::decoder::Audio,
    resampler: Option<ffmpeg_next::software::resampling::Context>,
    decoded: ffmpeg_next::frame::Audio,
}

impl SoundDecoder {
    /// The sound of `ctx`'s best audio stream; `None` when it has none, or
    /// none that decodes.
    fn open(ctx: &ffmpeg_next::format::context::Input) -> Option<Self> {
        let stream = ctx.streams().best(ffmpeg_next::media::Type::Audio)?;
        let decoder = {
            let _guard = FFMPEG_INIT_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ffmpeg_next::codec::context::Context::from_parameters(stream.parameters())
                .and_then(|c| c.decoder().audio())
        };
        match decoder {
            Ok(decoder) => Some(Self {
                stream: stream.index(),
                decoder,
                resampler: None,
                decoded: ffmpeg_next::frame::Audio::empty(),
            }),
            Err(e) => {
                log::warn!("Stream sound will not decode: {e}");
                None
            }
        }
    }

    /// Decode a packet of sound into `audio`.
    fn decode(&mut self, packet: &ffmpeg_next::Packet, audio: &StreamAudio) {
        use ffmpeg_next::format::sample::Type as SampleType;
        use ffmpeg_next::format::Sample;
        // A corrupt packet costs a moment of sound, not the stream.
        if self.decoder.send_packet(packet).is_err() {
            return;
        }
        while self.decoder.receive_frame(&mut self.decoded).is_ok() {
            let rate = self.decoded.rate().max(1);
            let mut layout = self.decoded.channel_layout();
            if layout.is_empty() {
                layout =
                    ffmpeg_next::ChannelLayout::default(i32::from(self.decoded.channels().max(1)));
                self.decoded.set_channel_layout(layout);
            }
            let format = self.decoded.format();
            let stale = self.resampler.as_ref().is_none_or(|r| {
                let input = r.input();
                input.format != format || input.rate != rate || input.channel_layout != layout
            });
            if stale {
                self.resampler = ffmpeg_next::software::resampling::Context::get(
                    format,
                    layout,
                    rate,
                    Sample::F32(SampleType::Packed),
                    ffmpeg_next::ChannelLayout::STEREO,
                    rate,
                )
                .ok();
            }
            let Some(resampler) = self.resampler.as_mut() else {
                return;
            };
            let mut converted = ffmpeg_next::frame::Audio::new(
                Sample::F32(SampleType::Packed),
                self.decoded.samples() + 256,
                ffmpeg_next::ChannelLayout::STEREO,
            );
            if resampler.run(&self.decoded, &mut converted).is_err() {
                continue;
            }
            let n = converted.samples();
            let samples = converted.data(0)[..n * 8]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|b| f32::from_le_bytes(*b))
                .collect();
            audio.take(samples, rate);
        }
    }
}

/// The largest picture a stream is received at: a larger one is scaled to
/// fit within it.
const MAX_SIZE: (u32, u32) = (3840, 2160);

/// Shows a stream's frames at the times they carry, not as they arrive:
/// HLS arrives a segment at a time, RTMP and SRT in network bursts, and a
/// frame shown on arrival stutters and then freezes.
#[derive(Default)]
struct Pacer {
    /// When the frame at stream time `.1` is shown.
    clock: Option<(Instant, f64)>,
}

impl Pacer {
    /// Held in hand when the clock starts, to ride out the network's
    /// unevenness.
    const HOLD: f64 = 0.1;
    /// A frame this far behind its time, or this far ahead, means the
    /// stream's clock jumped (a new sender, a reconnect, a stall): the
    /// clock starts again from it.
    const LATE: f64 = 0.5;
    const FAR: f64 = 3.0;

    /// How long to wait before showing the frame at `at` on the stream's
    /// clock; no wait for a frame without a time.
    fn wait(&mut self, at: Option<f64>, now: Instant) -> Duration {
        let Some(at) = at else {
            return Duration::ZERO;
        };
        if let Some((start, first)) = self.clock {
            let since = if now >= start {
                now.duration_since(start).as_secs_f64()
            } else {
                -start.duration_since(now).as_secs_f64()
            };
            let ahead = (at - first) - since;
            if (-Self::LATE..=Self::FAR).contains(&ahead) {
                return Duration::from_secs_f64(ahead.max(0.0));
            }
        }
        let hold = Duration::from_secs_f64(Self::HOLD);
        self.clock = Some((now + hold, at));
        hold
    }
}

/// Stream protocol for input receivers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub enum StreamProtocol {
    /// SRT (Secure Reliable Transport) with connection mode.
    Srt { mode: SrtMode },
    /// HLS (HTTP Live Streaming) — reads `.m3u8` manifest.
    Hls,
    /// DASH (Dynamic Adaptive Streaming over HTTP) — reads `.mpd` manifest.
    Dash,
    /// RTMP stream source with connection mode.
    Rtmp { mode: RtmpMode },
}

/// SRT connection mode — listener waits for connections, caller connects out.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub enum SrtMode {
    /// Bind to a port and wait for incoming connections.
    Listener,
    /// Connect to a remote SRT endpoint.
    Caller,
}

impl std::fmt::Display for SrtMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SrtMode::Listener => write!(f, "Listener"),
            SrtMode::Caller => write!(f, "Caller"),
        }
    }
}

/// RTMP connection mode: pull from a remote server or listen for incoming pushes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub enum RtmpMode {
    /// Connect to a remote RTMP server and pull the stream.
    Pull,
    /// Listen on a local RTMP port for incoming pushes (e.g. from OBS).
    Listen,
}

impl std::fmt::Display for RtmpMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RtmpMode::Pull => write!(f, "Pull"),
            RtmpMode::Listen => write!(f, "Listen"),
        }
    }
}

#[allow(dead_code)]
struct StreamReceiver {
    url: String,
    protocol: StreamProtocol,
    frame_data: Arc<FrameSlot>,
    /// Pictures shown so far.
    frames: Arc<AtomicU64>,
    audio: Arc<StreamAudio>,
    stop_flag: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    /// Frames are coming off the Mac's media engine.
    hardware: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    width: u32,
    height: u32,
}

/// Manages stream input receivers (background decode threads + GPU textures).
/// Handles SRT, HLS, and DASH input protocols.
pub struct StreamManager {
    receivers: Vec<StreamReceiver>,
    textures: Vec<(wgpu::Texture, wgpu::TextureView)>,
}

impl Default for StreamManager {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamManager {
    pub fn new() -> Self {
        // Init ffmpeg once on the main thread before any receiver threads spawn.
        ffmpeg_next::init().ok();
        Self {
            receivers: Vec::new(),
            textures: Vec::new(),
        }
    }

    /// Start receiving from a stream URL. Spawns a background ffmpeg decode thread.
    /// Returns receiver index on success. If an active receiver for the same URL
    /// already exists, returns its index.
    pub fn start_receive(
        &mut self,
        url: &str,
        protocol: StreamProtocol,
        device: &wgpu::Device,
    ) -> Option<usize> {
        // Reuse existing receiver for the same URL if it's still alive
        if let Some(idx) = self
            .receivers
            .iter()
            .position(|r| r.url == url && !r.stop_flag.load(Ordering::SeqCst))
        {
            log::info!("Reusing existing stream receiver {idx} for '{url}'");
            return Some(idx);
        }

        let frame_data: Arc<FrameSlot> = Arc::new(Mutex::new(None));
        let frames = Arc::new(AtomicU64::new(0));
        let audio = Arc::new(StreamAudio::default());
        let stop_flag = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let hardware = Arc::new(AtomicBool::new(false));
        let (width, height) = (1920u32, 1080u32);

        // Until the first picture says its size.
        // Build the full URL with protocol-specific params
        let full_url = match &protocol {
            StreamProtocol::Srt { mode } => build_srt_url(url, *mode),
            StreamProtocol::Hls | StreamProtocol::Dash => url.to_string(),
            StreamProtocol::Rtmp { mode } => build_rtmp_url(url, *mode),
        };

        let label = match &protocol {
            StreamProtocol::Srt { .. } => format!("SRT Receive: {url}"),
            StreamProtocol::Hls => format!("HLS Receive: {url}"),
            StreamProtocol::Dash => format!("DASH Receive: {url}"),
            StreamProtocol::Rtmp { .. } => format!("RTMP Receive: {url}"),
        };

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&label),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let frame_clone = Arc::clone(&frame_data);
        let shown_count = Arc::clone(&frames);
        let sound = Arc::clone(&audio);
        let stop_clone = Arc::clone(&stop_flag);
        let connected_clone = Arc::clone(&connected);
        let hardware_clone = Arc::clone(&hardware);
        let url_for_thread = full_url.clone();
        let url_display = url.to_string();

        let thread_name = match &protocol {
            StreamProtocol::Srt { .. } => format!("srt-recv-{url}"),
            StreamProtocol::Hls => format!("hls-recv-{url}"),
            StreamProtocol::Dash => format!("dash-recv-{url}"),
            StreamProtocol::Rtmp { .. } => format!("rtmp-recv-{url}"),
        };

        let thread = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                stream_receive_thread(
                    &url_for_thread,
                    &url_display,
                    &frame_clone,
                    &shown_count,
                    &sound,
                    &stop_clone,
                    &connected_clone,
                    &hardware_clone,
                );
            })
            .ok();

        if thread.is_none() {
            log::error!("Failed to spawn stream receive thread for '{url}'");
            return None;
        }

        let idx = self.receivers.len();
        self.receivers.push(StreamReceiver {
            url: url.to_string(),
            protocol,
            frame_data,
            frames,
            audio,
            stop_flag,
            connected,
            hardware,
            thread,
            width,
            height,
        });
        self.textures.push((texture, texture_view));
        log::info!("Stream receiver started for '{url}'");
        Some(idx)
    }

    /// Start receiving from an SRT URL (convenience wrapper).
    pub fn start_srt_receive(
        &mut self,
        url: &str,
        mode: SrtMode,
        device: &wgpu::Device,
    ) -> Option<usize> {
        self.start_receive(url, StreamProtocol::Srt { mode }, device)
    }

    /// Start receiving from an RTMP URL (convenience wrapper).
    pub fn start_rtmp_receive(
        &mut self,
        url: &str,
        mode: RtmpMode,
        device: &wgpu::Device,
    ) -> Option<usize> {
        self.start_receive(url, StreamProtocol::Rtmp { mode }, device)
    }

    /// Upload latest frames from all receivers to GPU, at each stream's own
    /// size: its texture is made again when that changes.
    pub fn update(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        for (i, receiver) in self.receivers.iter_mut().enumerate() {
            let Some(frame) = receiver
                .frame_data
                .try_lock()
                .ok()
                .and_then(|mut guard| guard.take())
            else {
                continue;
            };
            let expected = (frame.width * frame.height * 4) as usize;
            if frame.data.len() < expected || frame.width == 0 || frame.height == 0 {
                continue;
            }
            if (frame.width, frame.height) != (receiver.width, receiver.height) {
                log::info!(
                    "Stream '{}' is {}×{}",
                    receiver.url,
                    frame.width,
                    frame.height
                );
                let texture = stream_texture(device, &receiver.url, frame.width, frame.height);
                let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
                self.textures[i] = (texture, view);
                (receiver.width, receiver.height) = (frame.width, frame.height);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &self.textures[i].0,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.data[..expected],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(frame.width * 4),
                    rows_per_image: Some(frame.height),
                },
                wgpu::Extent3d {
                    width: frame.width,
                    height: frame.height,
                    depth_or_array_layers: 1,
                },
            );
        }
    }

    pub fn texture_view(&self, idx: usize) -> Option<&wgpu::TextureView> {
        self.textures.get(idx).map(|(_, v)| v)
    }

    pub fn receiver_dimensions(&self, idx: usize) -> Option<(u32, u32)> {
        self.receivers.get(idx).map(|r| (r.width, r.height))
    }

    /// Whether the receiver at `idx` has successfully connected and received a frame.
    /// Whether receiver `idx` is decoding on the Mac's media engine.
    pub fn is_hardware_decode(&self, idx: usize) -> bool {
        self.receivers
            .get(idx)
            .is_some_and(|r| r.hardware.load(Ordering::Relaxed))
    }

    /// A receiver's sound: (sample rate, channels, generation), once some
    /// has come.
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

    pub fn is_connected(&self, idx: usize) -> bool {
        self.receivers
            .get(idx)
            .is_some_and(|r| r.connected.load(Ordering::SeqCst))
    }

    /// Number of active receivers.
    pub fn receiver_count(&self) -> usize {
        self.receivers.len()
    }

    /// Get URL of receiver at index.
    pub fn receiver_url(&self, idx: usize) -> Option<&str> {
        self.receivers.get(idx).map(|r| r.url.as_str())
    }

    /// Get protocol of receiver at index.
    pub fn receiver_protocol(&self, idx: usize) -> Option<&StreamProtocol> {
        self.receivers.get(idx).map(|r| &r.protocol)
    }

    /// Get SRT mode of receiver at index (convenience for SRT receivers).
    pub fn receiver_mode(&self, idx: usize) -> Option<SrtMode> {
        self.receivers.get(idx).and_then(|r| match &r.protocol {
            StreamProtocol::Srt { mode } => Some(*mode),
            _ => None,
        })
    }

    /// Get RTMP mode of receiver at index (convenience for RTMP receivers).
    pub fn receiver_rtmp_mode(&self, idx: usize) -> Option<RtmpMode> {
        self.receivers.get(idx).and_then(|r| match &r.protocol {
            StreamProtocol::Rtmp { mode } => Some(*mode),
            _ => None,
        })
    }

    pub fn stop_receive(&mut self, idx: usize) {
        if let Some(r) = self.receivers.get_mut(idx) {
            r.stop_flag.store(true, Ordering::SeqCst);
            // Don't join — the thread may be blocked in ffmpeg I/O.
            // The interrupt callback will cause ffmpeg to abort, and
            // the thread holds only Arc clones so it's safe to detach.
            if let Some(t) = r.thread.take() {
                drop(t);
            }
        }
    }
}

/// A stream's texture, `width` × `height`.
fn stream_texture(device: &wgpu::Device, label: &str, width: u32, height: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size: wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    })
}

impl Drop for StreamManager {
    fn drop(&mut self) {
        for r in &mut self.receivers {
            r.stop_flag.store(true, Ordering::SeqCst);
        }
    }
}

/// Build an SRT URL with mode query parameter.
fn build_srt_url(url: &str, mode: SrtMode) -> String {
    let mode_str = match mode {
        SrtMode::Listener => "listener",
        SrtMode::Caller => "caller",
    };
    if url.contains('?') {
        format!("{url}&mode={mode_str}")
    } else {
        format!("{url}?mode={mode_str}")
    }
}

/// Build an RTMP URL, appending `listen=1` for Listen mode.
fn build_rtmp_url(url: &str, mode: RtmpMode) -> String {
    match mode {
        RtmpMode::Pull => url.to_string(),
        RtmpMode::Listen => {
            if url.contains('?') {
                format!("{url}&listen=1")
            } else {
                format!("{url}?listen=1")
            }
        }
    }
}

/// Background thread: decode stream via `ffmpeg_next` into RGBA frames at
/// the stream's own size, shown at the times they carry.
/// Protocol-agnostic — ffmpeg handles SRT, HLS, DASH, and RTMP URLs natively.
// `decoder` (the ffmpeg decoder) and `decoded` (the frame it produced) are both
// the clearest names for what they hold; renaming either would obscure the code.
// Each reference is a distinct channel to the render thread or the mixer,
// with no shared invariant that a struct would express.
#[allow(
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::too_many_arguments
)]
fn stream_receive_thread(
    url: &str,
    url_display: &str,
    frame_data: &FrameSlot,
    frames: &AtomicU64,
    audio: &StreamAudio,
    stop_flag: &Arc<AtomicBool>,
    connected: &AtomicBool,
    hardware: &AtomicBool,
) {
    const MIN_BACKOFF_MS: u64 = 500;
    const MAX_BACKOFF_MS: u64 = 10_000;
    const MAX_CONSECUTIVE_ERRORS: u32 = 50;
    /// Not reading: connecting, which waits as long as it must (a listener
    /// waits for its sender).
    const NOT_READING: u64 = u64::MAX;

    log::info!("Stream receive thread starting for '{url_display}'");

    // Exponential backoff: starts at 500ms, caps at 10s, resets on successful frame
    let mut backoff_ms: u64 = 500;
    // Decode on the media engine until it fails on this stream once; after
    // that, software for good.
    let mut use_hardware = crate::video::hwdecode::wanted();
    // HLS/DASH segments can have multi-second gaps; use a longer timeout
    let is_segment_protocol = url.contains(".m3u8")
        || url.contains(".mpd")
        || url.starts_with("http://")
        || url.starts_with("https://");
    let stall_timeout = if is_segment_protocol {
        Duration::from_secs(15)
    } else {
        Duration::from_secs(5)
    };
    // When the thread last came back from a read, in ms from `epoch`. A read
    // that blocks longer than the stall timeout — a connection dead without
    // closing — is interrupted, and the stream connected again.
    let epoch = Instant::now();
    let last_read = Arc::new(AtomicU64::new(NOT_READING));
    let now_ms = move || u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX - 1);
    let stall_ms = u64::try_from(stall_timeout.as_millis()).unwrap_or(u64::MAX);

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            return;
        }
        last_read.store(NOT_READING, Ordering::SeqCst);

        let stop_for_interrupt = Arc::clone(stop_flag);
        let read_for_interrupt = Arc::clone(&last_read);
        let setup_result = (|| -> Option<_> {
            let ctx = match ffmpeg_next::format::input_with_interrupt(&url, move || {
                let at = read_for_interrupt.load(Ordering::SeqCst);
                stop_for_interrupt.load(Ordering::SeqCst)
                    || (at != NOT_READING && now_ms().saturating_sub(at) > stall_ms)
            }) {
                Ok(c) => c,
                Err(e) => {
                    log::warn!(
                        "Stream '{url_display}' connection failed: {e}, retrying in {backoff_ms}ms..."
                    );
                    return None;
                }
            };

            let stream_idx = ctx
                .streams()
                .best(ffmpeg_next::media::Type::Video)
                .map(|s| s.index())
                .or_else(|| {
                    log::warn!(
                        "Stream '{url_display}': no video stream found, retrying in {backoff_ms}ms..."
                    );
                    None
                })?;

            let stream = ctx.stream(stream_idx).or_else(|| {
                log::warn!(
                    "Stream '{url_display}': stream index {stream_idx} not found, retrying in {backoff_ms}ms..."
                );
                None
            })?;
            let codec_params = stream.parameters();
            let tb = stream.time_base();
            let time_base = f64::from(tb.numerator()) / f64::from(tb.denominator().max(1));
            let decoder = open_decoder(&codec_params, &mut use_hardware)
                .map_err(|e| {
                    log::warn!(
                        "Stream '{url_display}': failed to create video decoder: {e}, retrying in {backoff_ms}ms..."
                    );
                })
                .ok()?;
            Some((ctx, stream_idx, codec_params, time_base, decoder))
        })();

        let Some((mut input_ctx, stream_idx, codec_params, time_base, mut decoder)) = setup_result
        else {
            connected.store(false, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(backoff_ms));
            backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
            continue;
        };

        log::info!("Stream '{url_display}' connected, decoding video");
        // Its sound, read in step with the pictures (the pictures' pacing
        // paces the reading), for the mixer.
        let mut sound = SoundDecoder::open(&input_ctx);
        // The scaler, and what it was built for; rebuilt only when frames change.
        let mut scaler: Option<(
            (u32, u32, ffmpeg_next::format::Pixel),
            ffmpeg_next::software::scaling::Context,
        )> = None;
        let mut yuv = crate::video::yuv::YuvSetup::default();
        let mut pacer = Pacer::default();
        let mut decoded = ffmpeg_next::frame::Video::empty();
        let mut engine_frame = crate::video::hwdecode::Download::default();
        let mut rgb_frame = ffmpeg_next::frame::Video::empty();

        let mut last_frame_time = Instant::now();
        let mut consecutive_errors: u32 = 0;
        last_read.store(now_ms(), Ordering::SeqCst);

        for (stream, packet) in input_ctx.packets() {
            last_read.store(now_ms(), Ordering::SeqCst);
            if stop_flag.load(Ordering::SeqCst) {
                return;
            }
            if let Some(sound) = sound.as_mut().filter(|s| s.stream == stream.index()) {
                sound.decode(&packet, audio);
                continue;
            }
            if stream.index() != stream_idx {
                continue;
            }

            if decoder.send_packet(&packet).is_err() {
                consecutive_errors += 1;
                // The media engine refusing a stream: software at once, in
                // place — not after a reconnect.
                if use_hardware && consecutive_errors >= 5 {
                    log::warn!("Stream '{url_display}': retrying with software decoding");
                    use_hardware = false;
                    if let Ok(d) = open_decoder(&codec_params, &mut use_hardware) {
                        decoder = d;
                        consecutive_errors = 0;
                    }
                    continue;
                }
                if consecutive_errors >= MAX_CONSECUTIVE_ERRORS {
                    log::warn!(
                        "Stream '{url_display}': {consecutive_errors} consecutive decode errors, reconnecting"
                    );
                    break;
                }
                continue;
            }
            consecutive_errors = 0;

            while decoder.receive_frame(&mut decoded).is_ok() {
                if stop_flag.load(Ordering::SeqCst) {
                    return;
                }
                // Off the media engine: copy to memory for the conversion.
                let on_engine = crate::video::hwdecode::on_engine(&decoded);
                if on_engine {
                    if let Err(e) = crate::video::hwdecode::to_memory(&decoded, &mut engine_frame) {
                        log::warn!("Stream '{url_display}': {e}; retrying with software decoding");
                        use_hardware = false;
                        if let Ok(d) = open_decoder(&codec_params, &mut use_hardware) {
                            decoder = d;
                        }
                        break;
                    }
                }
                hardware.store(on_engine, Ordering::Relaxed);
                let at = decoded.timestamp().map(|t| t as f64 * time_base);
                let source = if on_engine {
                    &engine_frame.frame
                } else {
                    &decoded
                };

                // Rebuild the scaler when the incoming frames change shape or
                // format. The picture keeps its size, up to `MAX_SIZE`.
                let input = (source.width(), source.height(), source.format());
                if scaler.as_ref().is_none_or(|(built, _)| *built != input) {
                    let (w, h) = crate::screen_capture::resample::fit_within(
                        input.0, input.1, MAX_SIZE.0, MAX_SIZE.1,
                    );
                    scaler = ffmpeg_next::software::scaling::Context::get(
                        input.2,
                        input.0,
                        input.1,
                        ffmpeg_next::format::Pixel::RGBA,
                        w,
                        h,
                        crate::video::yuv::rgba_flags(),
                    )
                    .ok()
                    .map(|sws| (input, sws));
                    yuv.reset();
                }
                let Some((_, sws)) = scaler.as_mut() else {
                    continue;
                };
                yuv.apply(sws, source);
                if sws.run(source, &mut rgb_frame).is_err() {
                    continue;
                }
                let Some(frame) = packed_rgba(&rgb_frame) else {
                    continue;
                };

                // Shown at its time.
                let mut wait = pacer.wait(at, Instant::now());
                while !wait.is_zero() {
                    if stop_flag.load(Ordering::SeqCst) {
                        return;
                    }
                    let step = wait.min(Duration::from_millis(20));
                    std::thread::sleep(step);
                    wait -= step;
                }
                let (width, height) = (frame.width, frame.height);
                if let Ok(mut guard) = frame_data.lock() {
                    *guard = Some(frame);
                }
                frames.fetch_add(1, Ordering::Relaxed);
                last_frame_time = Instant::now();
                // The wait was not a stalled read.
                last_read.store(now_ms(), Ordering::SeqCst);
                // Reset backoff on successful frame delivery
                backoff_ms = MIN_BACKOFF_MS;
                if !connected.load(Ordering::SeqCst) {
                    connected.store(true, Ordering::SeqCst);
                    log::info!("Stream '{url_display}' first frame received ({width}x{height})");
                }
            }

            // Packets but no pictures for a while: the demuxer is likely
            // stuck (e.g. HLS segments deleted). Force reconnect.
            if last_frame_time.elapsed() > stall_timeout {
                log::warn!(
                    "Stream '{}': no frames for {:.1}s, reconnecting",
                    url_display,
                    last_frame_time.elapsed().as_secs_f32()
                );
                break;
            }
        }

        log::warn!("Stream '{url_display}' ended, reconnecting in {backoff_ms}ms...");
        connected.store(false, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(backoff_ms));
        backoff_ms = (backoff_ms * 2).min(MAX_BACKOFF_MS);
    }
}

/// A decoder for a stream: on the media engine while `use_hardware`, which
/// is cleared for good when the engine will not take the stream.
fn open_decoder(
    params: &ffmpeg_next::codec::Parameters,
    use_hardware: &mut bool,
) -> anyhow::Result<ffmpeg_next::decoder::Video> {
    let _guard = FFMPEG_INIT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match crate::video::hwdecode::open_with(params.clone(), *use_hardware, true) {
        Err(e) if *use_hardware => {
            log::warn!("Stream: hardware decoder would not open ({e}); software");
            *use_hardware = false;
            crate::video::hwdecode::open_with(params.clone(), false, true)
                .map(|(decoder, _)| decoder)
        }
        Ok((decoder, hardware)) => {
            *use_hardware = hardware;
            Ok(decoder)
        }
        Err(error) => Err(error),
    }
}

/// An RGBA frame's rows packed tight, as a texture takes them.
fn packed_rgba(frame: &ffmpeg_next::frame::Video) -> Option<StreamFrame> {
    let (width, height) = (frame.width(), frame.height());
    let row_bytes = width as usize * 4;
    let stride = frame.stride(0);
    let data = frame.data(0);
    let mut out = Vec::with_capacity(row_bytes * height as usize);
    for row in 0..height as usize {
        out.extend_from_slice(data.get(row * stride..row * stride + row_bytes)?);
    }
    Some(StreamFrame {
        data: out,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A local HLS stream (H.264) decodes on the media engine and delivers
    /// frames, as a remote one would.
    #[test]
    fn a_stream_decodes_on_the_media_engine() {
        let dir = tempfile::tempdir().unwrap();
        let playlist = dir.path().join("live.m3u8");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=640x360:rate=25", "-t", "4"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "25"])
            .args(["-f", "hls", "-hls_time", "1", "-hls_list_size", "0"])
            .arg(&playlist)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let receiver = Receiving::start(&playlist.to_string_lossy());
        let until = Instant::now() + Duration::from_secs(10);
        while receiver.frames.load(Ordering::Relaxed) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        // Frames come at the stream's rate, 25 a second, not all at once as
        // the playlist's segments are read.
        let first = receiver.frames.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_secs(1));
        let in_a_second = receiver.frames.load(Ordering::Relaxed) - first;
        let on_engine = receiver.hardware.load(Ordering::Relaxed);
        let got = receiver.stop().expect("a frame came through");
        assert!(first > 0, "frames came");
        assert!(
            (15..=32).contains(&in_a_second),
            "{in_a_second} frames in a second, at 25 fps"
        );
        assert_eq!((got.width, got.height), (640, 360), "at its own size");
        assert_eq!(got.data.len(), 640 * 360 * 4);
        assert!(
            got.data.chunks(4).any(|px| px[0] > 200),
            "a picture, not black"
        );
        if cfg!(target_os = "macos") {
            assert!(on_engine, "on the media engine");
        }
    }

    /// A stream waiting for its sender — a listener nobody has called yet —
    /// does not hold up another: a second presenter can connect.
    #[test]
    fn a_stream_waiting_for_its_sender_holds_up_no_other() {
        let dir = tempfile::tempdir().unwrap();
        let playlist = dir.path().join("live.m3u8");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=320x180:rate=25", "-t", "3"])
            .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "25"])
            .args(["-f", "hls", "-hls_time", "1", "-hls_list_size", "0"])
            .arg(&playlist)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let waiting = Receiving::start(&format!("tcp://127.0.0.1:{port}?listen=1"));
        std::thread::sleep(Duration::from_millis(300));
        let other = Receiving::start(&playlist.to_string_lossy());
        let until = Instant::now() + Duration::from_secs(8);
        while other.frames.load(Ordering::Relaxed) == 0 && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let got = other.frames.load(Ordering::Relaxed);
        // The waiting one first: were it holding the other up, the other
        // could not stop.
        assert!(waiting.stop().is_none(), "nobody called the listener");
        other.stop();
        assert!(got > 0, "the other stream played while the first waited");
    }

    /// A sender that goes quiet without hanging up — a dead network, a
    /// crashed encoder behind a live socket — is given up on after the
    /// stall timeout and called again, rather than freezing the input for
    /// good.
    #[test]
    fn a_connection_gone_quiet_is_made_again() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let ts = dir.path().join("sent.ts");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=320x180:rate=25", "-t", "2"])
            .args([
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "25", "-f", "mpegts",
            ])
            .arg(&ts)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let bytes = std::fs::read(&ts).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (calls_tx, calls) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for mut socket in listener.incoming().flatten() {
                // A second of pictures, then silence, the line kept open.
                let _ = socket.write_all(&bytes[..bytes.len() / 2]);
                held.push(socket);
                if calls_tx.send(()).is_err() {
                    return;
                }
            }
        });
        let receiver = Receiving::start(&format!("tcp://127.0.0.1:{port}"));
        calls
            .recv_timeout(Duration::from_secs(5))
            .expect("it called");
        let again = calls.recv_timeout(Duration::from_secs(12));
        let frames = receiver.frames.load(Ordering::Relaxed);
        receiver.stop();
        assert!(frames > 0, "pictures came before the silence");
        assert!(again.is_ok(), "called again after going quiet");
    }

    /// A stream's sound comes through, for the mixer to play: a stream
    /// input is a remote presenter's voice as well as their picture.
    #[test]
    fn a_streams_sound_comes_through() {
        let dir = tempfile::tempdir().unwrap();
        let playlist = dir.path().join("live.m3u8");
        let made = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "testsrc2=size=320x180:rate=25", "-f", "lavfi"])
            .args(["-i", "sine=frequency=440:sample_rate=44100", "-t", "3"])
            .args([
                "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "25", "-c:a", "aac",
            ])
            .args(["-f", "hls", "-hls_time", "1", "-hls_list_size", "0"])
            .arg(&playlist)
            .status()
            .is_ok_and(|s| s.success());
        if !made {
            return;
        }
        let receiver = Receiving::start(&playlist.to_string_lossy());
        let until = Instant::now() + Duration::from_secs(8);
        let mut format = None;
        while format.is_none() && Instant::now() < until {
            format = receiver.audio.format();
            std::thread::sleep(Duration::from_millis(10));
        }
        let (tx, rx) = crossbeam_channel::bounded(64);
        if let Ok(mut sink) = receiver.audio.sink.lock() {
            *sink = Some(tx);
        }
        let chunk = rx.recv_timeout(Duration::from_secs(5));
        receiver.stop();
        let (rate, channels, _) = format.expect("sound came");
        assert_eq!((rate, channels), (44_100, 2), "at its own rate, in stereo");
        let chunk = chunk.expect("sound to the mixer");
        let loudest = chunk.samples.iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!(loudest > 0.05, "a tone, not silence: {loudest}");
    }

    /// SRT, both ways round: a stream sent to Nida listening, and one Nida
    /// calls. Needs an FFmpeg built with SRT (Homebrew's `ffmpeg-full`), to
    /// send and to receive.
    #[test]
    fn an_srt_stream_comes_through_either_way() {
        let sender = std::path::Path::new("/opt/homebrew/opt/ffmpeg-full/bin/ffmpeg");
        if !sender.exists() {
            eprintln!("skipping: no ffmpeg-full to send SRT");
            return;
        }
        for (ours, theirs) in [("caller", "listener"), ("listener", "caller")] {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let receiver = Receiving::start(&format!("srt://127.0.0.1:{port}?mode={ours}"));
            if ours == "listener" {
                std::thread::sleep(Duration::from_millis(500));
            }
            let mut send = std::process::Command::new(sender)
                .args(["-hide_banner", "-loglevel", "error", "-re", "-f", "lavfi"])
                .args(["-i", "testsrc2=size=320x180:rate=25", "-t", "8"])
                .args(["-c:v", "libx264", "-g", "25", "-f", "mpegts"])
                .arg(format!("srt://127.0.0.1:{port}?mode={theirs}"))
                .spawn()
                .expect("ffmpeg-full");
            let until = Instant::now() + Duration::from_secs(8);
            while receiver.frames.load(Ordering::Relaxed) < 10 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(20));
            }
            let frames = receiver.frames.load(Ordering::Relaxed);
            let _ = send.kill();
            let _ = send.wait();
            let last = receiver.stop();
            assert!(frames >= 10, "Nida as {ours}: {frames} frames");
            let last = last.expect("a picture");
            assert_eq!((last.width, last.height), (320, 180));
        }
    }

    /// Nida's own SRT output (Setup → Outputs → SRT, listening) reaches an
    /// SRT input calling it. Runs only where the `ffmpeg` found first can
    /// send SRT — Homebrew's `ffmpeg-full`, which Nida puts first on its own
    /// path; the plain `ffmpeg` cannot.
    #[test]
    fn nidas_srt_output_reaches_an_srt_input() {
        let protocols = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-protocols"])
            .output();
        let can = protocols.is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.trim() == "srt")
        });
        if !can {
            eprintln!("skipping: the ffmpeg on PATH has no SRT");
            return;
        }
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let url = format!("srt://127.0.0.1:{port}");
        let mut output = crate::renderer::FfmpegSubprocess::spawn_srt(
            &url,
            &crate::renderer::context::SrtCodec::H264,
            crate::engine::value::render::PresentationRequest::default(),
            320,
            180,
            25,
            None,
        )
        .expect("SRT output");
        let receiver = Receiving::start(&format!("{url}?mode=caller"));
        let frame = vec![200u8; 320 * 180 * 4];
        let until = Instant::now() + Duration::from_secs(10);
        while receiver.frames.load(Ordering::Relaxed) < 10 && Instant::now() < until {
            output.feed_frame(&frame);
            std::thread::sleep(Duration::from_millis(40));
        }
        let frames = receiver.frames.load(Ordering::Relaxed);
        output.stop();
        let last = receiver.stop();
        assert!(frames >= 10, "{frames} frames came through");
        let last = last.expect("a picture");
        assert_eq!((last.width, last.height), (320, 180));
        assert!(last.data[0] > 150, "the grey sent: {}", last.data[0]);
    }

    /// A receive thread on `url`, for a test.
    struct Receiving {
        frame: Arc<FrameSlot>,
        frames: Arc<AtomicU64>,
        audio: Arc<StreamAudio>,
        stop: Arc<AtomicBool>,
        hardware: Arc<AtomicBool>,
        thread: std::thread::JoinHandle<()>,
    }

    impl Receiving {
        fn start(url: &str) -> Self {
            let url = url.to_string();
            let frame: Arc<FrameSlot> = Arc::new(Mutex::new(None));
            let frames = Arc::new(AtomicU64::new(0));
            let audio = Arc::new(StreamAudio::default());
            let stop = Arc::new(AtomicBool::new(false));
            let connected = Arc::new(AtomicBool::new(false));
            let hardware = Arc::new(AtomicBool::new(false));
            let thread = {
                let (frame, frames, audio, stop, hardware) = (
                    frame.clone(),
                    frames.clone(),
                    audio.clone(),
                    stop.clone(),
                    hardware.clone(),
                );
                std::thread::spawn(move || {
                    stream_receive_thread(
                        &url, "test", &frame, &frames, &audio, &stop, &connected, &hardware,
                    );
                })
            };
            Self {
                frame,
                frames,
                audio,
                stop,
                hardware,
                thread,
            }
        }

        /// Stop it, and the last picture it left.
        fn stop(self) -> Option<StreamFrame> {
            self.stop.store(true, Ordering::SeqCst);
            let _ = self.thread.join();
            self.frame.lock().unwrap().take()
        }
    }

    #[test]
    fn frames_are_shown_at_the_times_they_carry() {
        let start = Instant::now();
        let mut pacer = Pacer::default();
        let hold = Duration::from_secs_f64(Pacer::HOLD);
        // The first is held a moment, to ride out the network.
        assert_eq!(pacer.wait(Some(10.0), start), hold);
        // One 40 ms on waits till then — a burst arrives all at once.
        let at = |ms: u64| start + Duration::from_millis(ms);
        assert_eq!(
            pacer.wait(Some(10.04), at(0)),
            hold + Duration::from_millis(40)
        );
        // One that arrives late shows at once.
        assert_eq!(pacer.wait(Some(10.08), at(300)), Duration::ZERO);
        // A frame without a time shows at once.
        assert_eq!(pacer.wait(None, at(300)), Duration::ZERO);
    }

    /// A jump in the stream's clock — a sender restarted, a new HLS
    /// segment numbering — starts the clock again rather than waiting on it.
    #[test]
    fn a_jump_in_the_streams_clock_starts_the_clock_again() {
        let start = Instant::now();
        let hold = Duration::from_secs_f64(Pacer::HOLD);
        let mut pacer = Pacer::default();
        pacer.wait(Some(5.0), start);
        assert_eq!(pacer.wait(Some(500.0), start), hold, "far ahead");
        assert_eq!(pacer.wait(Some(2.0), start), hold, "back in time");
        let later = start + Duration::from_secs(2);
        assert_eq!(pacer.wait(Some(2.04), later), hold, "long behind: a stall");
    }

    #[test]
    fn stream_manager_new_empty() {
        let mgr = StreamManager::new();
        assert_eq!(mgr.receivers.len(), 0);
        assert_eq!(mgr.textures.len(), 0);
        assert!(!mgr.is_connected(0));
        assert!(mgr.texture_view(0).is_none());
        assert!(mgr.receiver_dimensions(0).is_none());
    }

    #[test]
    fn stream_manager_receiver_count_empty() {
        let mgr = StreamManager::new();
        assert_eq!(mgr.receiver_count(), 0);
    }

    #[test]
    fn stream_manager_receiver_url_out_of_bounds() {
        let mgr = StreamManager::new();
        assert!(mgr.receiver_url(0).is_none());
        assert!(mgr.receiver_url(999).is_none());
    }

    #[test]
    fn stream_manager_receiver_mode_out_of_bounds() {
        let mgr = StreamManager::new();
        assert!(mgr.receiver_mode(0).is_none());
        assert!(mgr.receiver_mode(999).is_none());
    }

    #[test]
    fn stream_manager_is_connected_out_of_bounds() {
        let mgr = StreamManager::new();
        assert!(!mgr.is_connected(0));
        assert!(!mgr.is_connected(100));
    }

    #[test]
    fn stream_manager_receiver_dimensions_out_of_bounds() {
        let mgr = StreamManager::new();
        assert!(mgr.receiver_dimensions(0).is_none());
    }

    #[test]
    fn stream_manager_stop_receive_out_of_bounds_no_panic() {
        let mut mgr = StreamManager::new();
        mgr.stop_receive(0);
        mgr.stop_receive(999);
    }

    #[test]
    fn stream_protocol_srt_serialization() {
        let proto = StreamProtocol::Srt {
            mode: SrtMode::Listener,
        };
        let json = serde_json::to_string(&proto).unwrap();
        let restored: StreamProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(
            restored,
            StreamProtocol::Srt {
                mode: SrtMode::Listener
            }
        );
    }

    #[test]
    fn stream_protocol_hls_serialization() {
        let proto = StreamProtocol::Hls;
        let json = serde_json::to_string(&proto).unwrap();
        let restored: StreamProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, StreamProtocol::Hls);
    }

    #[test]
    fn stream_protocol_dash_serialization() {
        let proto = StreamProtocol::Dash;
        let json = serde_json::to_string(&proto).unwrap();
        let restored: StreamProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, StreamProtocol::Dash);
    }

    #[test]
    fn srt_mode_serialization_roundtrip() {
        let listener = SrtMode::Listener;
        let json = serde_json::to_string(&listener).unwrap();
        let restored: SrtMode = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, SrtMode::Listener);

        let caller = SrtMode::Caller;
        let json = serde_json::to_string(&caller).unwrap();
        let restored: SrtMode = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, SrtMode::Caller);
    }

    #[test]
    fn srt_mode_display() {
        assert_eq!(format!("{}", SrtMode::Listener), "Listener");
        assert_eq!(format!("{}", SrtMode::Caller), "Caller");
    }

    #[test]
    fn srt_mode_equality() {
        assert_eq!(SrtMode::Listener, SrtMode::Listener);
        assert_eq!(SrtMode::Caller, SrtMode::Caller);
        assert_ne!(SrtMode::Listener, SrtMode::Caller);
    }

    #[test]
    fn srt_mode_clone() {
        let original = SrtMode::Caller;
        let cloned = original;
        assert_eq!(original, cloned);
    }

    #[test]
    fn srt_mode_debug() {
        let debug = format!("{:?}", SrtMode::Listener);
        assert!(debug.contains("Listener"));
        let debug = format!("{:?}", SrtMode::Caller);
        assert!(debug.contains("Caller"));
    }

    #[test]
    fn srt_mode_deserialize_from_string() {
        // Verify it can deserialize from JSON strings
        let listener: SrtMode = serde_json::from_str("\"Listener\"").unwrap();
        assert_eq!(listener, SrtMode::Listener);
        let caller: SrtMode = serde_json::from_str("\"Caller\"").unwrap();
        assert_eq!(caller, SrtMode::Caller);
    }

    #[test]
    fn srt_mode_deserialize_invalid() {
        let result: Result<SrtMode, _> = serde_json::from_str("\"InvalidMode\"");
        assert!(result.is_err());
    }

    #[test]
    fn build_srt_url_caller() {
        let url = build_srt_url("srt://192.168.1.100:9000", SrtMode::Caller);
        assert_eq!(url, "srt://192.168.1.100:9000?mode=caller");
    }

    #[test]
    fn build_srt_url_listener() {
        let url = build_srt_url("srt://:9000", SrtMode::Listener);
        assert_eq!(url, "srt://:9000?mode=listener");
    }

    #[test]
    fn build_srt_url_with_existing_params() {
        let url = build_srt_url("srt://host:9000?latency=0", SrtMode::Caller);
        assert_eq!(url, "srt://host:9000?latency=0&mode=caller");
    }

    #[test]
    fn build_srt_url_localhost_default_port() {
        let url = build_srt_url("srt://127.0.0.1:9001", SrtMode::Caller);
        assert_eq!(url, "srt://127.0.0.1:9001?mode=caller");
    }

    #[test]
    fn build_srt_url_listener_wildcard() {
        let url = build_srt_url("srt://:9001", SrtMode::Listener);
        assert_eq!(url, "srt://:9001?mode=listener");
    }

    #[test]
    fn build_srt_url_multiple_existing_params() {
        let url = build_srt_url(
            "srt://host:9000?latency=0&passphrase=test",
            SrtMode::Listener,
        );
        assert_eq!(
            url,
            "srt://host:9000?latency=0&passphrase=test&mode=listener"
        );
    }

    #[test]
    fn stream_protocol_rtmp_serialization() {
        let protocol = StreamProtocol::Rtmp {
            mode: RtmpMode::Pull,
        };
        let json = serde_json::to_string(&protocol).unwrap();
        let deserialized: StreamProtocol = serde_json::from_str(&json).unwrap();
        assert_eq!(protocol, deserialized);
    }

    #[test]
    fn rtmp_mode_display() {
        assert_eq!(RtmpMode::Pull.to_string(), "Pull");
        assert_eq!(RtmpMode::Listen.to_string(), "Listen");
    }

    #[test]
    fn rtmp_mode_serialization_roundtrip() {
        let pull = RtmpMode::Pull;
        let listen = RtmpMode::Listen;
        let pull_json = serde_json::to_string(&pull).unwrap();
        let listen_json = serde_json::to_string(&listen).unwrap();
        assert_eq!(serde_json::from_str::<RtmpMode>(&pull_json).unwrap(), pull);
        assert_eq!(
            serde_json::from_str::<RtmpMode>(&listen_json).unwrap(),
            listen
        );
    }
}
