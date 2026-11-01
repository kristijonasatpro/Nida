//! `FfmpegSubprocess` — shared ffmpeg lifecycle for recording and SRT streaming.
//!
//! Spawns an ffmpeg process with a background writer thread that feeds frames
//! via a bounded channel. The render thread never blocks on pipe writes — if
//! ffmpeg can't keep up (e.g. SRT listener waiting for client), frames are dropped.

use std::io::Write;
use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;

use crate::audio::PcmChunk;
use crate::engine::value::render::{
    AlphaMode, PresentationColorProfile, PresentationDepth, PresentationPixelFormat,
    PresentationRequest, RecordingCodec, ResolvedPresentation, RtmpCodecContract, SrtCodec,
    StreamingCodec,
};
use crate::renderer::{ReadbackFormat, ReadbackFrame};

#[cfg(target_os = "macos")]
mod native;

/// No encoder in Nida off the Mac: the stream's encoder is always ffmpeg's.
#[cfg(not(target_os = "macos"))]
mod native {
    use std::sync::atomic::AtomicBool;
    use std::sync::{mpsc, Arc};
    use std::time::Instant;

    use super::{FrameCounters, NativeVideo};
    use crate::streaming::{RatePlan, StreamOptions, VideoCodec};

    pub(super) type Opened = std::convert::Infallible;

    pub(super) fn open(
        _: &StreamOptions,
        _: (u32, u32),
        _: u32,
        _: bool,
        _: &RatePlan,
    ) -> Option<Opened> {
        None
    }

    pub(super) fn open_for_record(_: VideoCodec, _: (u32, u32), _: u32, _: u32) -> Option<Opened> {
        None
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn start(
        opened: Opened,
        _: std::process::ChildStdin,
        _: mpsc::Receiver<(Instant, Vec<u8>)>,
        _: (u32, VideoCodec, u32, u64),
        _: FrameCounters,
        _: (Arc<AtomicBool>, Arc<AtomicBool>),
        _: &str,
        _: Option<Instant>,
    ) -> (std::thread::JoinHandle<()>, NativeVideo) {
        match opened {}
    }
}

/// What a stream's encoder in Nida shares between its threads and the app
/// that steers its rate. Made on a Mac only.
#[derive(Clone)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct NativeShared {
    /// The rate the app asks for now.
    wanted_kbps: Arc<AtomicU32>,
    /// The rate the engine last took.
    applied_kbps: Arc<AtomicU32>,
    audio_kbps: u32,
    /// The stream's frame rate: a slot is this long.
    fps: u32,
    failed: Arc<AtomicBool>,
    /// Frames the engine dropped.
    dropped: Arc<AtomicU64>,
    epoch: std::time::Instant,
    /// Frames handed to the engine, and frames whose end the output thread
    /// has taken: the difference is what the engine and the pipe still hold.
    submitted: Arc<AtomicU64>,
    finished: Arc<AtomicU64>,
    /// When the engine last went from holding nothing to holding a frame,
    /// and when a frame's end last came out, in ms from `epoch`.
    busy_since_ms: Arc<AtomicU64>,
    last_out_ms: Arc<AtomicU64>,
    /// The media engine's padding is dropped as it comes out: the stream is
    /// Variable (see `streaming::RateMode`). Switched while live.
    strip_filler: Arc<AtomicBool>,
}

/// Frames held this long with none ending: the engine — or the pipe to
/// ffmpeg — has stopped, and the stream's encoder is started again.
const NATIVE_STUCK_MS: u64 = 3000;

/// How long a stop waits for an encoder in Nida to end its session. Ending
/// it waits for every frame the engine holds, which an engine that has
/// stopped may never give back: past this the threads are left to end on
/// their own, and the app goes on.
const NATIVE_STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Join `threads` on a thread of its own, waiting up to `wait`: whether they
/// all ended in time. Those that did not are left to end on their own.
fn join_within(threads: Vec<std::thread::JoinHandle<()>>, wait: std::time::Duration) -> bool {
    let (done_tx, done_rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("stream-encoder-end".into())
        .spawn(move || {
            for thread in threads {
                let _ = thread.join();
            }
            let _ = done_tx.send(());
        });
    spawned.is_ok() && done_rx.recv_timeout(wait).is_ok()
}

impl NativeShared {
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn new(kbps: u32, audio_kbps: u32, fps: u32) -> Self {
        Self {
            wanted_kbps: Arc::new(AtomicU32::new(kbps)),
            applied_kbps: Arc::new(AtomicU32::new(kbps)),
            audio_kbps,
            fps: fps.max(1),
            failed: Arc::default(),
            dropped: Arc::default(),
            epoch: std::time::Instant::now(),
            submitted: Arc::default(),
            finished: Arc::default(),
            busy_since_ms: Arc::default(),
            last_out_ms: Arc::default(),
            strip_filler: Arc::default(),
        }
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn wanted_kbps(&self) -> u32 {
        self.wanted_kbps.load(Ordering::Relaxed)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn applied(&self, kbps: u32) {
        self.applied_kbps.store(kbps, Ordering::Relaxed);
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn fail(&self) {
        self.failed.store(true, Ordering::Relaxed);
    }

    fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn note_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// A frame about to go to the engine: counted before it goes, so its end
    /// is never counted first.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn going_in(&self) {
        let (submitted, finished) = (
            self.submitted.load(Ordering::Relaxed),
            self.finished.load(Ordering::Relaxed),
        );
        if submitted == finished {
            self.busy_since_ms.store(self.now_ms(), Ordering::Relaxed);
        }
        self.submitted.fetch_add(1, Ordering::Relaxed);
    }

    /// A frame's end taken by the output thread.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn came_out(&self) {
        self.last_out_ms.store(self.now_ms(), Ordering::Relaxed);
        self.finished.fetch_add(1, Ordering::Relaxed);
    }

    /// Failed, or holding frames for [`NATIVE_STUCK_MS`] with none ending —
    /// counted from when it last had nothing, so a show idle for a while
    /// and then drawing again is given the time.
    fn stuck(&self) -> bool {
        self.stuck_at(self.now_ms())
    }

    /// [`Self::stuck`] at `now_ms` from `epoch`.
    fn stuck_at(&self, now_ms: u64) -> bool {
        let holding = self.submitted.load(Ordering::Relaxed) > self.finished.load(Ordering::Relaxed);
        let since = self
            .last_out_ms
            .load(Ordering::Relaxed)
            .max(self.busy_since_ms.load(Ordering::Relaxed));
        self.failed() || (holding && now_ms > since + NATIVE_STUCK_MS)
    }
}

/// A stream's encoder in Nida, and the thread writing what it makes to
/// ffmpeg. Made on a Mac only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct NativeVideo {
    shared: NativeShared,
    output: Option<std::thread::JoinHandle<()>>,
}

/// Fully pinned video contract for one recording subprocess.
///
/// Resolution happens before the writer queue starts. A ten-bit request is only
/// accepted when both the encoder and the typed GPU readback can carry it.
#[derive(Debug, Clone)]
struct RecordingPlan {
    /// Runtime precision and encoded pixel format reported to output status.
    resolved: ResolvedPresentation,
    input_pixel_format: &'static str,
    output_pixel_format: &'static str,
    expected_readback: ReadbackFormat,
    video_args: Vec<String>,
    unpremultiply: bool,
}

impl RecordingPlan {
    fn resolve(
        codec: &RecordingCodec,
        request: PresentationRequest,
        readback: ReadbackFormat,
        encoder_help: Option<&str>,
    ) -> Self {
        let encoder = recording_encoder(codec);
        let ten_bit_output = ten_bit_output_format(codec);
        let codec_supports_ten_bit = ten_bit_output.is_some();
        let encoder_supports_ten_bit = ten_bit_output.is_some_and(|format| {
            encoder_help.is_some_and(|help| encoder_supports_pixel_format(help, format))
        });
        let required_readback = if matches!(codec, RecordingCodec::ProRes4444) {
            ReadbackFormat::Rgba16Unorm
        } else {
            ReadbackFormat::Rgb10A2
        };

        let fallback_reason = if request.depth != PresentationDepth::Sdr10 {
            None
        } else if !codec_supports_ten_bit {
            Some(format!(
                "{codec} is an eight-bit recording codec; 10-bit SDR is unavailable"
            ))
        } else if matches!(codec, RecordingCodec::ProRes4444)
            && readback == ReadbackFormat::Rgba16Float
        {
            Some(
                "10-bit ProRes 4444 requires integer RGBA16 readback; RGBA16 half-float bytes \
                 cannot be passed to FFmpeg as rgba64le"
                    .to_string(),
            )
        } else if matches!(codec, RecordingCodec::ProRes4444)
            && readback != ReadbackFormat::Rgba16Unorm
        {
            Some(
                "10-bit ProRes 4444 is unavailable because the GPU does not support normalized \
                 16-bit RGBA render targets"
                    .to_string(),
            )
        } else if required_readback != readback {
            Some(format!(
                "10-bit SDR recording requires packed RGB10 readback, but the active renderer \
                 supplies {}",
                readback_label(readback)
            ))
        } else if encoder_help.is_none() {
            Some(format!(
                "FFmpeg encoder '{encoder}' is unavailable or could not be queried"
            ))
        } else if !encoder_supports_ten_bit {
            Some(format!(
                "FFmpeg encoder '{encoder}' does not support {}",
                ten_bit_output.expect("ten-bit codec has an output format")
            ))
        } else {
            None
        };

        let use_ten_bit = request.depth == PresentationDepth::Sdr10 && fallback_reason.is_none();
        let (input_pixel_format, output_pixel_format, expected_readback) = if use_ten_bit {
            if matches!(codec, RecordingCodec::ProRes4444) {
                (
                    "rgba64le",
                    ten_bit_output.expect("resolved ten-bit recording has an output format"),
                    ReadbackFormat::Rgba16Unorm,
                )
            } else {
                (
                    packed_rgb10_input_format(),
                    ten_bit_output.expect("resolved ten-bit recording has an output format"),
                    ReadbackFormat::Rgb10A2,
                )
            }
        } else {
            (
                "rgba",
                eight_bit_output_format(codec),
                ReadbackFormat::Rgba8,
            )
        };
        let alpha_mode = if codec_preserves_alpha(codec) {
            AlphaMode::Straight
        } else {
            AlphaMode::Opaque
        };
        let resolved_depth = if use_ten_bit {
            PresentationDepth::Sdr10
        } else {
            PresentationDepth::Sdr8
        };
        let pixel_format = if use_ten_bit && matches!(codec, RecordingCodec::ProRes4444) {
            PresentationPixelFormat::Rgba16
        } else {
            PresentationPixelFormat::EncoderNative(output_pixel_format.to_string())
        };
        let resolved = ResolvedPresentation {
            requested: request.depth,
            resolved: resolved_depth,
            pixel_format,
            color_profile: PresentationColorProfile::Rec709Limited,
            alpha_mode,
            dither: request.dither,
            fallback_reason,
        };
        let video_args = recording_video_args(codec, resolved_depth, output_pixel_format);

        Self {
            resolved,
            input_pixel_format,
            output_pixel_format,
            expected_readback,
            video_args,
            unpremultiply: codec_preserves_alpha(codec),
        }
    }
}

fn recording_encoder(codec: &RecordingCodec) -> &'static str {
    match codec {
        RecordingCodec::H264 => "libx264",
        RecordingCodec::H265 => "libx265",
        RecordingCodec::AV1 => "libsvtav1",
        RecordingCodec::ProRes | RecordingCodec::ProRes4444 => "prores_ks",
        RecordingCodec::Hap | RecordingCodec::HapAlpha | RecordingCodec::HapQ => "hap",
    }
}

fn ten_bit_output_format(codec: &RecordingCodec) -> Option<&'static str> {
    match codec {
        RecordingCodec::H265 | RecordingCodec::AV1 => Some("yuv420p10le"),
        RecordingCodec::ProRes => Some("yuv422p10le"),
        RecordingCodec::ProRes4444 => Some("yuva444p10le"),
        RecordingCodec::H264
        | RecordingCodec::Hap
        | RecordingCodec::HapAlpha
        | RecordingCodec::HapQ => None,
    }
}

fn eight_bit_output_format(codec: &RecordingCodec) -> &'static str {
    match codec {
        RecordingCodec::H264 | RecordingCodec::H265 | RecordingCodec::AV1 => "yuv420p",
        RecordingCodec::ProRes => "yuv422p10le",
        RecordingCodec::ProRes4444 => "yuva444p10le",
        RecordingCodec::Hap | RecordingCodec::HapAlpha | RecordingCodec::HapQ => "rgba",
    }
}

fn codec_preserves_alpha(codec: &RecordingCodec) -> bool {
    matches!(codec, RecordingCodec::ProRes4444 | RecordingCodec::HapAlpha)
}

fn encoder_supports_pixel_format(help: &str, pixel_format: &str) -> bool {
    help.lines()
        .find(|line| line.trim_start().starts_with("Supported pixel formats:"))
        .is_some_and(|line| line.split_whitespace().any(|item| item == pixel_format))
}

fn readback_label(format: ReadbackFormat) -> &'static str {
    match format {
        ReadbackFormat::Rgba8 => "RGBA8",
        ReadbackFormat::Bgra8 => "BGRA8",
        ReadbackFormat::Rgb10A2 => "RGB10A2",
        ReadbackFormat::Rgba16Float => "RGBA16 half-float",
        ReadbackFormat::Rgba16Unorm => "RGBA16",
        ReadbackFormat::Uyvy => "UYVY",
        ReadbackFormat::P216 => "P216",
        ReadbackFormat::Nv12 => "NV12",
    }
}

fn preferred_recording_readback(
    codec: &RecordingCodec,
    request: PresentationRequest,
    rgba16_unorm_supported: bool,
) -> ReadbackFormat {
    if request.depth != PresentationDepth::Sdr10 {
        ReadbackFormat::Rgba8
    } else if matches!(codec, RecordingCodec::ProRes4444) && rgba16_unorm_supported {
        ReadbackFormat::Rgba16Unorm
    } else if matches!(codec, RecordingCodec::ProRes4444) {
        ReadbackFormat::Rgba8
    } else {
        ReadbackFormat::Rgb10A2
    }
}

/// wgpu `Rgb10a2Unorm` stores R in bits 0..9, G in 10..19, and B in
/// 20..29. FFmpeg names that little-endian word `x2bgr10le` (the component
/// names describe most-significant to least-significant fields).
const fn packed_rgb10_input_format() -> &'static str {
    "x2bgr10le"
}

fn unpremultiply_filter(input_pixel_format: &str) -> &'static str {
    if input_pixel_format == "rgba64le" {
        "setparams=alpha_mode=premultiplied,format=gbrap16le,unpremultiply=inplace=1"
    } else {
        "setparams=alpha_mode=premultiplied,format=gbrap,unpremultiply=inplace=1"
    }
}

fn recording_video_args(
    codec: &RecordingCodec,
    depth: PresentationDepth,
    output_pixel_format: &str,
) -> Vec<String> {
    let mut args: Vec<String> = match codec {
        RecordingCodec::H264 => vec![
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "18",
            "-profile:v",
            "high",
        ],
        RecordingCodec::H265 => {
            let profile = if depth == PresentationDepth::Sdr10 {
                "main10"
            } else {
                "main"
            };
            vec![
                "-c:v",
                "libx265",
                "-preset",
                "ultrafast",
                "-crf",
                "20",
                "-profile:v",
                profile,
            ]
        }
        RecordingCodec::AV1 => vec![
            "-c:v",
            "libsvtav1",
            "-preset",
            "10",
            "-crf",
            "28",
            "-profile:v",
            "0",
        ],
        RecordingCodec::ProRes => {
            vec!["-c:v", "prores_ks", "-profile:v", "2"]
        }
        RecordingCodec::ProRes4444 => {
            vec!["-c:v", "prores_ks", "-profile:v", "4", "-alpha_bits", "16"]
        }
        RecordingCodec::Hap => vec!["-c:v", "hap", "-format", "hap"],
        RecordingCodec::HapAlpha => vec!["-c:v", "hap", "-format", "hap_alpha"],
        RecordingCodec::HapQ => vec!["-c:v", "hap", "-format", "hap_q"],
    }
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend(
        [
            "-pix_fmt",
            output_pixel_format,
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
        ]
        .into_iter()
        .map(str::to_string),
    );
    args
}

fn probe_encoder_help(encoder: &str) -> Option<String> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-h", &format!("encoder={encoder}")])
        .output()
        .ok()?;
    output.status.success().then(|| {
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    })
}

#[derive(Debug, Clone, Copy, Default)]
// Each flag is an independent result from an installed FFmpeg capability probe.
#[allow(clippy::struct_excessive_bools)]
struct StreamingCapabilities {
    hevc_main10: bool,
    av1_10: bool,
    enhanced_hevc: bool,
    enhanced_av1: bool,
}

impl StreamingCapabilities {
    fn detect() -> Self {
        let hevc_help = probe_encoder_help("libx265");
        let av1_help = probe_encoder_help("libsvtav1");
        let hevc_main10 = hevc_help
            .as_deref()
            .is_some_and(|help| encoder_supports_pixel_format(help, "yuv420p10le"));
        let av1_10 = av1_help
            .as_deref()
            .is_some_and(|help| encoder_supports_pixel_format(help, "yuv420p10le"));
        Self {
            hevc_main10,
            av1_10,
            enhanced_hevc: hevc_main10 && probe_enhanced_flv("libx265", "main10"),
            enhanced_av1: av1_10 && probe_enhanced_flv("libsvtav1", "main"),
        }
    }

    fn installed() -> Self {
        static CAPABILITIES: std::sync::OnceLock<StreamingCapabilities> =
            std::sync::OnceLock::new();
        *CAPABILITIES.get_or_init(Self::detect)
    }

    /// What `request` needs to know of the installed FFmpeg.
    fn for_request(request: PresentationRequest) -> Self {
        Self::needed_for(request, Self::installed)
    }

    /// What the installed FFmpeg can do, asked of `installed` only for ten
    /// bits: asking runs ffmpeg, and eight bits — every Record and Stream —
    /// need none of it and must not wait for it, as a REC or STREAM started
    /// as Nida launched once did (codex, 2026-10-04).
    fn needed_for(request: PresentationRequest, installed: impl FnOnce() -> Self) -> Self {
        if request.depth == PresentationDepth::Sdr10 {
            installed()
        } else {
            Self::default()
        }
    }
}

/// Find what the installed FFmpeg can stream in ten bits, ahead — on a
/// thread of its own at launch: it runs ffmpeg several times.
pub(crate) fn find_streaming_capabilities() {
    let _ = StreamingCapabilities::installed();
}

fn probe_enhanced_flv(encoder: &str, profile: &str) -> bool {
    Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=size=16x16:rate=1",
            "-frames:v",
            "1",
            "-c:v",
            encoder,
            "-pix_fmt",
            "yuv420p10le",
            "-profile:v",
            profile,
            "-f",
            "flv",
            "-",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamingProtocol {
    Srt,
    Hls,
    Dash,
    Rtmp(RtmpCodecContract),
}

/// Fully negotiated codec, pixel format, metadata, and muxer contract for a stream.
#[derive(Debug, Clone)]
pub(crate) struct StreamingPlan {
    /// Runtime precision and encoded pixel format reported to output status.
    pub resolved: ResolvedPresentation,
    input_pixel_format: &'static str,
    expected_readback: ReadbackFormat,
    effective_codec: StreamingCodec,
    video_args: Vec<String>,
    muxer_args: Vec<String>,
}

impl StreamingPlan {
    pub(crate) const fn expected_readback(&self) -> ReadbackFormat {
        self.expected_readback
    }

    fn resolve(
        protocol: StreamingProtocol,
        configured_codec: StreamingCodec,
        request: PresentationRequest,
        capabilities: StreamingCapabilities,
    ) -> Self {
        let unavailable = if request.depth == PresentationDepth::Sdr10 {
            match (protocol, &configured_codec) {
                (_, StreamingCodec::H264) => {
                    Some("H.264 streaming is limited to eight-bit SDR".to_string())
                }
                (StreamingProtocol::Srt, StreamingCodec::AV1) => {
                    Some("AV1 is not supported by the interoperable SRT MPEG-TS path".to_string())
                }
                (
                    StreamingProtocol::Srt | StreamingProtocol::Hls | StreamingProtocol::Dash,
                    StreamingCodec::H265,
                ) if !capabilities.hevc_main10 => {
                    Some("installed FFmpeg encoder lacks HEVC Main10".to_string())
                }
                (StreamingProtocol::Hls | StreamingProtocol::Dash, StreamingCodec::AV1)
                    if !capabilities.av1_10 =>
                {
                    Some("installed FFmpeg encoder lacks AV1 10-bit support".to_string())
                }
                (StreamingProtocol::Rtmp(RtmpCodecContract::Legacy), _) => {
                    Some("the endpoint contract is legacy RTMP".to_string())
                }
                (StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced), StreamingCodec::H265)
                    if !capabilities.enhanced_hevc =>
                {
                    Some("FFmpeg cannot mux HEVC Main10 with Enhanced RTMP signaling".to_string())
                }
                (StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced), StreamingCodec::AV1)
                    if !capabilities.enhanced_av1 =>
                {
                    Some("FFmpeg cannot mux AV1 10-bit with Enhanced RTMP signaling".to_string())
                }
                _ => None,
            }
        } else {
            None
        };
        let use_ten_bit = request.depth == PresentationDepth::Sdr10 && unavailable.is_none();
        let legacy_rtmp = matches!(protocol, StreamingProtocol::Rtmp(RtmpCodecContract::Legacy));
        let effective_codec = if legacy_rtmp
            || (request.depth == PresentationDepth::Sdr10 && unavailable.is_some())
            || (matches!(protocol, StreamingProtocol::Srt)
                && configured_codec == StreamingCodec::AV1)
        {
            StreamingCodec::H264
        } else {
            configured_codec
        };
        let resolved_depth = if use_ten_bit {
            PresentationDepth::Sdr10
        } else {
            PresentationDepth::Sdr8
        };
        let output_pixel_format = if use_ten_bit {
            "yuv420p10le"
        } else {
            "yuv420p"
        };
        let input_pixel_format = if use_ten_bit {
            packed_rgb10_input_format()
        } else {
            "rgba"
        };
        let expected_readback = if use_ten_bit {
            ReadbackFormat::Rgb10A2
        } else {
            ReadbackFormat::Rgba8
        };
        let video_args =
            streaming_video_args(&effective_codec, resolved_depth, output_pixel_format);
        let muxer_args = streaming_muxer_args(protocol, use_ten_bit, &effective_codec);
        let resolved = ResolvedPresentation {
            requested: request.depth,
            resolved: resolved_depth,
            pixel_format: PresentationPixelFormat::EncoderNative(output_pixel_format.to_string()),
            color_profile: PresentationColorProfile::Rec709Limited,
            alpha_mode: AlphaMode::Opaque,
            dither: request.dither,
            fallback_reason: unavailable,
        };
        Self {
            resolved,
            input_pixel_format,
            expected_readback,
            effective_codec,
            video_args,
            muxer_args,
        }
    }

    pub(crate) fn for_target(
        target: &crate::engine::value::render::OutputTarget,
        request: PresentationRequest,
    ) -> Option<Self> {
        let capabilities = StreamingCapabilities::for_request(request);
        match target {
            crate::engine::value::render::OutputTarget::SrtStream { codec, .. } => {
                let codec = match codec {
                    SrtCodec::H264 => StreamingCodec::H264,
                    SrtCodec::H265 => StreamingCodec::H265,
                };
                Some(Self::resolve(
                    StreamingProtocol::Srt,
                    codec,
                    request,
                    capabilities,
                ))
            }
            crate::engine::value::render::OutputTarget::HlsStream { codec, .. } => Some(
                Self::resolve(StreamingProtocol::Hls, codec.clone(), request, capabilities),
            ),
            crate::engine::value::render::OutputTarget::DashStream { codec, .. } => {
                Some(Self::resolve(
                    StreamingProtocol::Dash,
                    codec.clone(),
                    request,
                    capabilities,
                ))
            }
            crate::engine::value::render::OutputTarget::RtmpStream {
                codec,
                codec_contract,
                ..
            } => Some(Self::resolve(
                StreamingProtocol::Rtmp(*codec_contract),
                codec.clone(),
                request,
                capabilities,
            )),
            // Eight-bit H.264, the same plan `spawn_stream` uses.
            crate::engine::value::render::OutputTarget::Stream
            | crate::engine::value::render::OutputTarget::Record => Some(Self::resolve(
                StreamingProtocol::Rtmp(RtmpCodecContract::Legacy),
                StreamingCodec::H264,
                request,
                capabilities,
            )),
            _ => None,
        }
    }
}

fn streaming_video_args(
    codec: &StreamingCodec,
    depth: PresentationDepth,
    output_pixel_format: &str,
) -> Vec<String> {
    let mut args: Vec<String> = match codec {
        StreamingCodec::H264 => {
            vec![
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-tune",
                "zerolatency",
                "-profile:v",
                "high",
            ]
        }
        StreamingCodec::H265 => vec![
            "-c:v",
            "libx265",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            if depth == PresentationDepth::Sdr10 {
                "main10"
            } else {
                "main"
            },
        ],
        StreamingCodec::AV1 => vec!["-c:v", "libsvtav1", "-preset", "10", "-profile:v", "0"],
    }
    .into_iter()
    .map(str::to_string)
    .collect();
    args.extend(
        [
            "-pix_fmt",
            output_pixel_format,
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
        ]
        .into_iter()
        .map(str::to_string),
    );
    args
}

fn streaming_muxer_args(
    protocol: StreamingProtocol,
    ten_bit: bool,
    codec: &StreamingCodec,
) -> Vec<String> {
    let args: Vec<&str> = match protocol {
        StreamingProtocol::Srt => vec!["-f", "mpegts"],
        StreamingProtocol::Hls if ten_bit => vec!["-tag:v", codec_tag(codec), "-f", "hls"],
        StreamingProtocol::Hls => vec!["-f", "hls"],
        StreamingProtocol::Dash => vec!["-tag:v", codec_tag(codec), "-f", "dash"],
        StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced) if ten_bit => {
            vec!["-flvflags", "no_sequence_end+no_metadata", "-f", "flv"]
        }
        StreamingProtocol::Rtmp(_) => vec!["-f", "flv"],
    };
    args.into_iter().map(str::to_string).collect()
}

fn codec_tag(codec: &StreamingCodec) -> &'static str {
    match codec {
        StreamingCodec::H264 => "avc1",
        StreamingCodec::H265 => "hvc1",
        StreamingCodec::AV1 => "av01",
    }
}

fn hls_segment_args(dir: &str, low_latency: bool, use_fmp4: bool) -> Vec<String> {
    let (time, list_size, flags, extension) = if low_latency {
        ("1", "6", "independent_segments+delete_segments", "m4s")
    } else if use_fmp4 {
        ("2", "30", "delete_segments+independent_segments", "m4s")
    } else {
        ("2", "30", "delete_segments", "ts")
    };
    let mut args = vec![
        "-hls_time".to_string(),
        time.to_string(),
        "-hls_list_size".to_string(),
        list_size.to_string(),
        "-hls_flags".to_string(),
        flags.to_string(),
    ];
    if low_latency || use_fmp4 {
        args.extend([
            "-hls_segment_type".to_string(),
            "fmp4".to_string(),
            "-hls_fmp4_init_filename".to_string(),
            "init.mp4".to_string(),
        ]);
    }
    args.extend([
        "-hls_segment_filename".to_string(),
        format!("{dir}/seg_%05d.{extension}"),
    ]);
    args
}

/// Write a self-contained HTML player page into a stream directory.
/// Uses hls.js for HLS streams and dash.js for DASH streams.
/// For LL-HLS, enables hls.js low-latency mode with live-edge tuning.
fn write_stream_player(dir: &str, kind: &str, manifest_filename: &str, low_latency: bool) {
    let (lib_url, lib_setup) = match kind {
        "hls" if low_latency => (
            "https://cdn.jsdelivr.net/npm/hls.js@latest",
            format!(
                r"if(Hls.isSupported()){{var h=new Hls({{lowLatencyMode:true,liveSyncDurationCount:2,liveMaxLatencyDurationCount:4,maxBufferLength:4,backBufferLength:0}});h.loadSource('{manifest_filename}');h.attachMedia(v);}}else if(v.canPlayType('application/vnd.apple.mpegurl')){{v.src='{manifest_filename}';}}",
            ),
        ),
        "hls" => (
            "https://cdn.jsdelivr.net/npm/hls.js@latest",
            format!(
                r"if(Hls.isSupported()){{var h=new Hls();h.loadSource('{manifest_filename}');h.attachMedia(v);}}else if(v.canPlayType('application/vnd.apple.mpegurl')){{v.src='{manifest_filename}';}}",
            ),
        ),
        _ => (
            "https://cdn.jsdelivr.net/npm/dashjs@latest/dist/dash.all.min.js",
            format!(
                r"var p=dashjs.MediaPlayer().create();p.updateSettings({{streaming:{{delay:{{liveDelay:2}},buffer:{{fastSwitchEnabled:true}}}}}});p.initialize(v,'{manifest_filename}',true);v.play().catch(function(){{}});",
            ),
        ),
    };
    let title = if low_latency {
        format!("LL-{}", kind.to_uppercase())
    } else {
        kind.to_uppercase()
    };
    let html = format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>Nida — {title} stream</title>
<style>*{{margin:0;padding:0;background:#000}}video{{width:100vw;height:100vh;object-fit:contain}}</style>
<script src="{lib_url}"></script></head>
<body><video id="v" autoplay muted controls></video>
<script>var v=document.getElementById('v');{lib_setup}</script></body></html>"#,
    );
    let path = format!("{dir}/player.html");
    if let Err(e) = std::fs::write(&path, html) {
        log::warn!("Failed to write stream player to '{path}': {e}");
    }
}

/// Shared ffmpeg subprocess for recording and SRT streaming.
///
/// Frames are sent to a background writer thread via a bounded channel.
/// This prevents the render thread from blocking when ffmpeg's stdin pipe is full
/// (e.g. SRT listener waiting for a client connection).
pub struct FfmpegSubprocess {
    child: Child,
    /// Channel sender for frames, with when each was drawn → writer thread
    frame_tx: Option<mpsc::SyncSender<(std::time::Instant, Vec<u8>)>>,
    /// Writer thread handle
    writer_thread: Option<std::thread::JoinHandle<()>>,
    /// Frame tallies, shared with the writer thread.
    counters: FrameCounters,
    /// Writer thread error flag (set when write fails during normal operation)
    write_failed: Arc<AtomicBool>,
    /// Set by `stop()` before killing ffmpeg — tells the writer thread that a
    /// broken pipe is expected and should not be logged as ERROR.
    shutting_down: Arc<AtomicBool>,
    /// Human-readable label (path or URL)
    label: String,
    /// Start time (for duration display)
    start_time: std::time::Instant,
    /// Whether `stop()` has already been called (prevent double-wait)
    stopped: bool,
    /// Optional audio passthrough side-channel (None = video-only).
    audio: Option<AudioPipe>,
    /// When true, `stop()` closes stdin and waits for ffmpeg to exit naturally
    /// (so it can finalize the container — e.g. write the MP4 moov atom).
    /// When false, `stop()` kills ffmpeg immediately (safe for streams, required
    /// when the writer thread may be blocked on a full network pipe).
    graceful_shutdown: bool,
    /// Typed frame contract negotiated before the FFmpeg process starts.
    frame_contract: Option<FrameContract>,
    /// Resolved precision selected by the recording or streaming adapter.
    presentation: Option<ResolvedPresentation>,
    /// ffmpeg's last lines of output, read for as long as it runs.
    stderr_tail: StderrTail,
    /// The stream's destinations, fed from this encoder's output. `None`
    /// for everything but a stream.
    fanout: Option<crate::streaming::Fanout>,
    /// The stream's video encoded in Nida, its rate followed while live;
    /// `None` where ffmpeg encodes it.
    native: Option<NativeVideo>,
    /// How the stream is encoded: its encoder and rate mode as they are,
    /// and why where they differ from those set. `None` but for a stream.
    rate_plan: Option<crate::streaming::RatePlan>,
}

/// ffmpeg's stderr, read by a thread for as long as the process runs, keeping
/// the last lines for the log and for telling the operator what went wrong.
///
/// A pipe nobody reads holds 64 KB. When it is full, ffmpeg blocks on its next
/// log line and stops reading frames: with the default half-second progress
/// line that was five or six minutes into every recording and stream. Reading
/// it continuously is what keeps a long take alive; `-nostats` keeps the tail
/// for the lines that matter.
#[derive(Clone, Default)]
pub(crate) struct StderrTail {
    lines: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
}

/// Lines of ffmpeg output kept for the log and error messages.
const STDERR_TAIL_LINES: usize = 40;

impl StderrTail {
    /// Take `child`'s stderr and start reading it. A child without a piped
    /// stderr gets an empty tail.
    pub(crate) fn attach(child: &mut Child) -> Self {
        let tail = Self::default();
        if let Some(stderr) = child.stderr.take() {
            tail.read_from(stderr);
        }
        tail
    }

    /// Read `stream` to its end on a thread, line by line — a carriage return
    /// ends a line too, as in ffmpeg's progress output.
    pub(crate) fn read_from(&self, mut stream: impl std::io::Read + Send + 'static) {
        let lines = self.lines.clone();
        let spawned = std::thread::Builder::new()
            .name("ffmpeg-stderr".into())
            .spawn(move || {
                let mut buf = [0u8; 4096];
                let mut line = Vec::new();
                let push = |line: &mut Vec<u8>| {
                    let text = String::from_utf8_lossy(line).trim().to_string();
                    line.clear();
                    if text.is_empty() {
                        return;
                    }
                    let mut lines = lines
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if lines.len() == STDERR_TAIL_LINES {
                        lines.pop_front();
                    }
                    lines.push_back(text);
                };
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            for &byte in &buf[..n] {
                                if byte == b'\n' || byte == b'\r' {
                                    push(&mut line);
                                } else {
                                    line.push(byte);
                                }
                            }
                        }
                    }
                }
                push(&mut line);
            });
        if let Err(error) = spawned {
            log::warn!("Could not start the ffmpeg stderr reader: {error}");
        }
    }

    /// The lines kept so far, oldest first.
    pub(crate) fn lines(&self) -> Vec<String> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .cloned()
            .collect()
    }

    /// The most telling line for an operator: the last one that reads like an
    /// error, else the last line.
    pub(crate) fn last_error(&self) -> Option<String> {
        let lines = self.lines();
        lines
            .iter()
            .rev()
            .find(|l| is_error_line(l))
            .or(lines.last())
            .cloned()
    }

    /// Log the kept lines — errors as errors, the rest at debug — and forget
    /// them, so a second call logs nothing twice.
    fn log(&self, label: &str) {
        let lines: Vec<String> = self
            .lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .drain(..)
            .collect();
        for line in lines {
            if is_error_line(&line) {
                log::error!("ffmpeg [{label}]: {line}");
            } else {
                log::debug!("ffmpeg [{label}]: {line}");
            }
        }
    }
}

fn is_error_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error",
        "failed",
        "invalid",
        "fatal",
        "refused",
        "denied",
        "unauthorized",
    ]
    .iter()
    .any(|w| lower.contains(w))
}

#[derive(Debug, Clone, Copy)]
struct FrameContract {
    format: ReadbackFormat,
    width: u32,
    height: u32,
}

/// Bounded channel capacity — 2 frames of buffer allows the writer thread
/// to stay one frame ahead without accumulating unbounded latency.
const FRAME_CHANNEL_CAPACITY: usize = 2;

/// Memory a stream's frame queue may take.
const STREAM_QUEUE_BYTES: usize = 128 * 1024 * 1024;

/// Frames the stream's encoder may fall behind before one is dropped: a
/// fifth of a second, where a recording has two frames. The encoder is fast
/// enough on average but pauses now and then; with two frames of slack each
/// pause dropped one, and a platform's stream is seconds behind anyway, so
/// 0.2 s more costs nothing. Capped in memory for large pictures.
/// What NV12 made on the GPU is, said to FFmpeg before it reads it: BT.709
/// at video levels. Left unsaid, FFmpeg takes it for the older BT.601 and
/// converts it to the BT.709 the encoder is set to — reds and greens a few
/// codes darker in every recording.
fn nv12_input_colour(nv12: bool) -> &'static [&'static str] {
    if nv12 {
        &[
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
        ]
    } else {
        &[]
    }
}

fn stream_frame_capacity(width: u32, height: u32, fps: u32, nv12: bool) -> usize {
    let pixels = width as usize * height as usize;
    let frame_bytes = if nv12 { pixels * 3 / 2 } else { pixels * 4 }.max(1);
    (fps as usize / 5)
        .min(STREAM_QUEUE_BYTES / frame_bytes)
        .max(FRAME_CHANNEL_CAPACITY)
}

/// The longest gap filled before a frame, seconds: the whole of any freeze
/// short of this goes in before the frame after it, which keeps its own slot
/// — filled in parts over the frames that followed, as it once was, that
/// frame landed in the file up to a second and a half early. Past two
/// minutes frozen the show was not being recorded anyway, and one frame must
/// not become an hour of repeats.
const MAX_GAP_SECS: f64 = 120.0;

/// Where each frame goes in a raw video stream, which ffmpeg times by
/// position alone: frame N plays at N/fps, whenever it was made.
///
/// A frame goes to the slot of the moment it was drawn, counted from `start`
/// — when the sound's first sample was taken, so both tracks start together.
/// One that gets here late, held up on the GPU or behind an encoder still
/// starting, still goes to its own slot; slots no frame came for are filled
/// with the frame before; one for a slot already written is left out.
/// Placed by when frames arrived instead, the half second a first take
/// waited for the encoder, and every hold-up after, went into the picture's
/// timeline but not the sound's, and the two came apart.
struct Timeline {
    /// The show's frames as the take's slots, when its rate is a whole
    /// multiple or share of the take's (see [`Timeline::on_grid`]).
    grid: Option<Grid>,
    /// The show's rate the grid was made for, when it follows the engine's
    /// as it changes (a take's; not a test's).
    follows: Option<u32>,
    start: Option<std::time::Instant>,
    /// When the frame written last was drawn: one drawn no later is left out.
    last: Option<std::time::Instant>,
    fps: f64,
    /// Where the slots fall, in frames, now and as the first frame set them:
    /// at the earliest frames, as a refresh's call can come late but never
    /// early. They move to an earlier frame at once — never more than half a
    /// frame from where the first frame set them, which keeps the tracks
    /// within a frame of each other — and never later: frames that keep
    /// coming later are a show slower than the take's rate, its frames
    /// repeated now and then.
    phase: Option<(f64, f64)>,
    /// Frames written so far, repeats included.
    written: u64,
}

/// How late, in frames, a frame may come after the next slot to write and
/// still go there. Frames are timed by the refresh they were drawn for, as
/// its call reached Nida: never early, a little late as a rule, more when
/// macOS is busy. Put in whichever slot was nearest, one frame in a second
/// landed a slot late, a frame repeated before it and the next left out (a
/// laptop, 2026-10-03). Only a frame a whole slot late or more — a refresh
/// the show drew no frame for — has slots filled before it.
const SLOT_LATE: f64 = 1.0;

/// How early, in frames, a frame may come before the next slot to write and
/// still go there: one earlier is for a slot already written.
const SLOT_EARLY: f64 = 0.5;


/// Frames waiting for a live stream's writer at which it is behind real
/// time and takes only the newest (see [`take_live`]); and slots a frame may
/// fall past the next to write and still have them filled (see
/// [`Timeline::place_live`]): more, they are skipped. Counted, not timed:
/// however long a frame takes to come back from the GPU, frames come one by
/// one, and a show slower than the stream leaves a slot or two to fill.
const LIVE_BACKLOG: usize = 5;

/// The longest a live stream skips at once, seconds; further behind, one
/// frame goes out at each step. A gap of ten seconds or more in its time
/// the sender's FFmpeg closes up, its sound not — the two apart for good —
/// and a platform wants a keyframe at least every four seconds (`YouTube`),
/// which the engine puts in only on a frame it gets: half a second keeps
/// both, even with a step's frame dropped by the engine.
const LONGEST_SKIP: f64 = 0.5;

/// The show's frame rate as the engine last gave it (see [`set_show_fps`]);
/// 0 while unknown.
static SHOW_FPS: AtomicU32 = AtomicU32::new(0);

/// The show's frame rate, for the takes' timelines (see `Timeline::on_grid`):
/// set by the engine as it draws.
pub fn set_show_fps(fps: u32) {
    SHOW_FPS.store(fps, Ordering::Relaxed);
}

/// The engine's frames lately: when each was drawn — the time a recording
/// or stream is given with it — and its number, counted as drawn. Counted
/// by number, a refresh call that came late cannot make a frame look like
/// the next one (Codex: one 10 ms late took frames 0, 2, 3, 6).
static FRAMES_DRAWN: std::sync::Mutex<std::collections::VecDeque<(std::time::Instant, u64, u32)>> =
    std::sync::Mutex::new(std::collections::VecDeque::new());
static FRAMES_COUNTED: AtomicU64 = AtomicU64::new(0);
/// Over a minute of a 60 fps show: a frame further behind than that is
/// placed by its time alone.
const FRAMES_KEPT: usize = 4096;

/// The engine drew a frame at `at` (the time its recordings and streams get
/// with it), frames coming at `fps` then.
pub fn note_frame_drawn(at: std::time::Instant, fps: u32) {
    let number = FRAMES_COUNTED.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut drawn) = FRAMES_DRAWN.lock() {
        if drawn.len() >= FRAMES_KEPT {
            drawn.pop_front();
        }
        drawn.push_back((at, number, fps));
    }
}

/// The number of the engine's frame drawn at `at`, if it is one of late.
pub fn frame_number(at: std::time::Instant) -> Option<u64> {
    frame_drawn(at).map(|(number, _)| number)
}

/// The number of the engine's frame drawn at `at`, and the rate frames came
/// at then, if it is one of late.
pub fn frame_drawn(at: std::time::Instant) -> Option<(u64, u32)> {
    let drawn = FRAMES_DRAWN.lock().ok()?;
    drawn
        .iter()
        .rev()
        .find(|(when, ..)| *when == at)
        .map(|(_, n, fps)| (*n, *fps))
}

/// A take at a whole share or multiple of the show's rate, placed by the
/// show's frames themselves: 60 to 30, every second frame; 30 to 60, each
/// twice. Placed by the moment each was drawn instead, every other frame of
/// a 60 fps show fell right on a 30 fps slot's edge, and the least jitter
/// took it in or left it out — steps of one and three frames, a recording
/// that "stutters a lot" (the Studio, the user's 60 show recorded at 30).
#[derive(Debug, Clone, Copy)]
struct Grid {
    /// Show frames to a slot (2 for 60 to 30), or slots to a show frame (2
    /// for 30 to 60); one of them 1.
    per_slot: u64,
    per_frame: u64,
    /// The show's own rate.
    show: u32,
    /// The first frame's place among the show's frames, counted from the
    /// take's start, less a whole frame: where its frames fall.
    offset: Option<f64>,
    /// The last frame's place among the show's frames, when it was drawn,
    /// and its number as the engine counted it (see [`note_frame_drawn`]).
    last: Option<(u64, std::time::Instant, Option<u64>)>,
}

/// How far, in show frames, counting frames may stray from their time
/// before it is put right — a show slower or faster than its rate, a frame
/// it never drew: one frame repeated or left out then, not each edge
/// crossed and crossed back.
const GRID_STRAY: f64 = 0.75;

impl Grid {
    /// The grid of a take at `fps` from a show at `show` fps, when one is a
    /// whole multiple of the other (not the same) — exactly: 239 for 120
    /// would drift from the take's time (Codex).
    fn of(fps: u32, show: u32) -> Option<Self> {
        if fps == 0 || show == 0 || fps == show {
            return None;
        }
        let (per_slot, per_frame) = if show.is_multiple_of(fps) {
            (u64::from(show / fps), 1)
        } else if fps.is_multiple_of(show) {
            (1, u64::from(fps / show))
        } else {
            return None;
        };
        Some(Self {
            per_slot,
            per_frame,
            show,
            offset: None,
            last: None,
        })
    }

    /// The place among the show's frames of the one drawn at `made`, `since`
    /// seconds into the take: one on from the last for each frame the engine
    /// drew between them (by number; by the time between them when a frame
    /// has none), put right to its own time only when that strays more than
    /// [`GRID_STRAY`]. `None` before the take.
    fn place(&mut self, made: std::time::Instant, since: f64) -> Option<u64> {
        let show = f64::from(self.show);
        let number = frame_number(made);
        let frames = since * show;
        let offset = *self.offset.get_or_insert(frames - frames.round());
        let own = frames - offset;
        if own < -0.5 {
            return None;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Non-negative, bounded.
        let at = match self.last {
            None => own.round() as u64,
            Some((last, then, last_number)) => {
                let between = match (number, last_number) {
                    (Some(n), Some(l)) if n > l => n - l,
                    _ => (made.saturating_duration_since(then).as_secs_f64() * show)
                        .round()
                        .max(1.0) as u64,
                };
                let counted = last + between;
                #[allow(clippy::cast_precision_loss)] // Frame counts stay far below 2^52.
                let stray = own - counted as f64;
                if stray.abs() > GRID_STRAY {
                    own.round().max(0.0) as u64
                } else {
                    counted
                }
            }
        };
        self.last = Some((at, made, number));
        Some(at)
    }

    /// The slot of the show's frame `at`.
    fn slot(&self, at: u64) -> u64 {
        at / self.per_slot * self.per_frame
    }
}

/// What [`Timeline::place`] makes of a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placed {
    /// Write it, after this many repeats of the frame before it.
    After(u64),
    /// Drawn before the take began, or for a slot already written.
    LeftOut,
}

impl Timeline {
    fn new(start: Option<std::time::Instant>, fps: f64) -> Self {
        Self {
            grid: None,
            follows: None,
            start,
            last: None,
            fps,
            phase: None,
            written: 0,
        }
    }

    /// The take's rate, whole.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Made from a u32.
    fn whole_fps(&self) -> u32 {
        self.fps.round() as u32
    }

    /// A take of a show at `show` fps: on the show's frames when the rates
    /// divide evenly.
    fn with_show(mut self, show: u32) -> Self {
        self.grid = Grid::of(self.whole_fps(), show);
        self
    }

    /// A take of the show at its rate now, following it as it changes (see
    /// [`set_show_fps`]).
    fn of_show(start: Option<std::time::Instant>, fps: f64) -> Self {
        let show = SHOW_FPS.load(Ordering::Relaxed);
        let mut timeline = Self::new(start, fps).with_show(show);
        timeline.follows = Some(show);
        timeline
    }

    /// The show's rate changed mid-take — as frames came, the frame `made`
    /// at the new rate (the engine notes each frame's; a screen's clock
    /// follows a new target only after a moment): the grid made for the new
    /// one, its frames counted afresh from this one's time, and the slots'
    /// phase taken from it for the old way — its start and the slots
    /// written kept. Kept on the old rate, the count ran at the wrong share
    /// until put right, then waited for the clock: a second and a half of
    /// double speed then a freeze at each change (Max5 on the Studio).
    fn follow_show(&mut self, made: std::time::Instant) {
        const WHOLE: f64 = 1e-6;
        let Some(was) = self.follows else {
            return;
        };
        let now = frame_drawn(made).map_or_else(|| SHOW_FPS.load(Ordering::Relaxed), |(_, fps)| fps);
        if now == was {
            return;
        }
        self.follows = Some(now);
        self.grid = Grid::of(self.whole_fps(), now);
        if let (Some(start), Some(_)) = (self.start, self.phase) {
            let since = made.saturating_duration_since(start).as_secs_f64() * self.fps;
            let first = since - (since + WHOLE).floor();
            self.phase = Some((first, first));
        }
    }

    /// [`Self::place`] on the show's own frames (see [`Grid`]): each to the
    /// slot its frame falls in, those no frame came for filled with the one
    /// before, one for a slot already written left out.
    fn on_grid(&mut self, made: std::time::Instant) -> Option<Placed> {
        self.follow_show(made);
        let mut grid = self.grid?;
        let start = *self.start.get_or_insert(made);
        if made < start || self.last.is_some_and(|last| made <= last) {
            return Some(Placed::LeftOut);
        }
        let since = (made - start).as_secs_f64();
        let placed = match grid.place(made, since) {
            None => Placed::LeftOut,
            Some(at) => {
                let slot = grid.slot(at);
                if slot < self.written {
                    Placed::LeftOut
                } else {
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Bounded.
                    let most = (self.fps * MAX_GAP_SECS) as u64;
                    let repeats = (slot - self.written).min(most);
                    self.written += repeats + 1;
                    self.last = Some(made);
                    // Where the slots fall, for a live stream's skips.
                    self.phase.get_or_insert((0.0, 0.0));
                    Placed::After(repeats)
                }
            }
        };
        self.grid = Some(grid);
        Some(placed)
    }

    fn place(&mut self, made: std::time::Instant) -> Placed {
        // Clear of rounding at whole frames, where steady frames fall.
        const WHOLE: f64 = 1e-6;
        if let Some(placed) = self.on_grid(made) {
            return placed;
        }
        let start = *self.start.get_or_insert(made);
        if made < start || self.last.is_some_and(|last| made <= last) {
            return Placed::LeftOut;
        }
        let since = (made - start).as_secs_f64() * self.fps;
        let first = since - (since + WHOLE).floor();
        let (mut phase, first) = self.phase.unwrap_or((first, first));
        // How many slots past the next one to write the frame falls.
        #[allow(clippy::cast_precision_loss)] // Frame counts stay far below 2^52.
        let mut past = since - phase - self.written as f64;
        if past < 0.0 {
            // Earlier than the slots: they were set by a frame whose call
            // came late. Moved back to this one, as far as they may go.
            let moved = (phase + past).max(first - 0.5);
            past += phase - moved;
            phase = moved;
        }
        if past < -SLOT_EARLY {
            self.phase.get_or_insert((first, first));
            return Placed::LeftOut;
        }
        let repeats = if past + WHOLE >= SLOT_LATE {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Positive, bounded.
            let (behind, most) = ((past + WHOLE).floor() as u64, (self.fps * MAX_GAP_SECS) as u64);
            behind.min(most)
        } else {
            0
        };
        self.phase = Some((phase, first));
        self.last = Some(made);
        self.written += repeats + 1;
        Placed::After(repeats)
    }

    /// [`Self::place_live`] on the show's own frames (see [`Grid`]), each
    /// frame's slot worked out once and used for all: the first frame at the
    /// first slot, nothing filled before it; one more than `fill` slots past
    /// the next to write skipped to (at most [`LONGEST_SKIP`] at once, the
    /// frame before marking each step); else written after the slots before
    /// it filled. Worked out apart, skips and slots came apart for good and
    /// every frame was left out (Codex: a 24 fps show streamed at 120).
    fn live_on_grid(&mut self, made: std::time::Instant, fill: u64) -> Option<(u64, Option<Placed>)> {
        self.follow_show(made);
        let mut grid = self.grid?;
        let start = *self.start.get_or_insert(made);
        if made < start || self.last.is_some_and(|last| made <= last) {
            return Some((0, Some(Placed::LeftOut)));
        }
        let since = (made - start).as_secs_f64();
        let at = grid.place(made, since);
        self.grid = Some(grid);
        let Some(slot) = at.map(|at| grid.slot(at)) else {
            return Some((0, Some(Placed::LeftOut)));
        };
        self.phase.get_or_insert((0.0, 0.0));
        if self.written == 0 {
            // The first frame, however late: at the first slot, nothing
            // filled before it; the frames after skip on to their own.
            self.written = 1;
            self.last = Some(made);
            return Some((0, Some(Placed::After(0))));
        }
        if slot < self.written {
            return Some((0, Some(Placed::LeftOut)));
        }
        let ahead = slot - self.written;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Positive, bounded.
        let longest = (LONGEST_SKIP * self.fps) as u64;
        if ahead > fill {
            let skipped = ahead.min(longest);
            self.written += skipped;
            if ahead > longest {
                return Some((skipped, None));
            }
            self.written += 1;
            self.last = Some(made);
            return Some((skipped, Some(Placed::After(0))));
        }
        self.written += ahead + 1;
        self.last = Some(made);
        Some((0, Some(Placed::After(ahead))))
    }

    /// For a live stream, each frame sent with its own time: on to the slot
    /// of the frame drawn at `made`, those before it skipped rather than
    /// filled, at most [`LONGEST_SKIP`]. The slots skipped, and whether that
    /// stopped short of it.
    fn skip_to(&mut self, made: std::time::Instant) -> (u64, bool) {
        const WHOLE: f64 = 1e-6;
        let (Some(start), Some((phase, _))) = (self.start, self.phase) else {
            return (0, false);
        };
        #[allow(clippy::cast_precision_loss)] // Frame counts stay far below 2^52.
        let written = self.written as f64;
        let own = (made.saturating_duration_since(start).as_secs_f64() * self.fps - phase + WHOLE)
            .floor();
        if own <= written {
            return (0, false);
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Positive, bounded.
        let (gap, longest) = ((own - written) as u64, (LONGEST_SKIP * self.fps) as u64);
        let skipped = gap.min(longest);
        self.written += skipped;
        (skipped, gap > longest)
    }

    /// One slot written outside [`Self::place`]: a frame repeated to mark
    /// a step of a long skip (see [`Self::place_live`]).
    fn wrote_one(&mut self) {
        self.written += 1;
    }

    /// [`Self::place`] for a live stream, each frame sent with its own time:
    /// a frame more than [`LIVE_BACKLOG`] slots past the next to write — the
    /// writer fallen behind real time, or the show held up — goes to its own
    /// slot, those before it skipped rather than filled, at most
    /// [`LONGEST_SKIP`] at once. Filled at the stream's own rate, it never
    /// came level again: each frame taken owed the time since the one before
    /// it, frames were refused while those were made up, and the picture
    /// stood still for good (a 4K60 stream with a 4K60 recording started
    /// beside it, 2026-10-29). The slots skipped, and where the frame goes —
    /// nowhere when the skip stopped short of it: a step, the frame before
    /// to go out once there.
    fn place_live(&mut self, made: std::time::Instant, fill: u64) -> (u64, Option<Placed>) {
        const WHOLE: f64 = 1e-6;
        if let Some(placed) = self.live_on_grid(made, fill) {
            return placed;
        }
        // The first frame, late — the encoder slow to start: at the first
        // slot, nothing filled before it, so the picture's time starts at
        // the sound's. FFmpeg starts each input at nought: a first frame at
        // its own slot, half a second in, had the picture half a second
        // ahead of the sound for good; filled up to it, a late start cost
        // the encoder every slot it missed. The frames after it skip ahead
        // from there, the first marking each step.
        if let (Some(start), None) = (self.start, self.phase) {
            let since = made.saturating_duration_since(start).as_secs_f64() * self.fps;
            let first = since - (since + WHOLE).floor();
            self.phase = Some((first, first));
            self.last = Some(made);
            self.written = 1;
            return (0, Some(Placed::After(0)));
        }
        if let (Some(start), Some((phase, _))) = (self.start, self.phase) {
            #[allow(clippy::cast_precision_loss)] // Frame counts stay far below 2^52.
            let ahead = made.saturating_duration_since(start).as_secs_f64() * self.fps - phase
                + WHOLE
                - self.written as f64;
            #[allow(clippy::cast_precision_loss)] // A handful.
            if ahead > fill as f64 {
                let (skipped, step) = self.skip_to(made);
                if step {
                    return (skipped, None);
                }
                return (skipped, Some(self.place(made)));
            }
        }
        (0, Some(self.place(made)))
    }
}

/// A live stream's frames waiting for its writer (see `native::encode_in`),
/// the one it took first: each in turn — unless [`LIVE_BACKLOG`] or more
/// wait, the writer fallen behind real time, an engine shared or held up:
/// then only the newest, its time skipped to as it is placed (see
/// [`Timeline::place_live`]). With how many were let go.
fn take_live<F>(mut waiting: Vec<(std::time::Instant, F)>) -> (Vec<(std::time::Instant, F)>, u64) {
    if waiting.len() < LIVE_BACKLOG {
        return (waiting, 0);
    }
    let let_go = waiting.len() as u64 - 1;
    let newest = waiting.split_off(waiting.len() - 1);
    (newest, let_go)
}

/// Frames a live stream's render thread could not hand on, not yet matched
/// to the slots they left (see `native::encode_in`): a skip over their
/// slots counts only past them, already counted as lost; slots filled over
/// them use them up too.
#[derive(Debug, Default)]
struct RefusedCredit {
    open: u64,
}

impl RefusedCredit {
    /// `n` more frames refused.
    fn refused(&mut self, n: u64) {
        self.open += n;
    }

    /// `slots` skipped: those lost beyond frames already counted.
    fn skipped(&mut self, slots: u64) -> u64 {
        let matched = slots.min(self.open);
        self.open -= matched;
        slots - matched
    }

    /// `slots` filled with repeats, over refused frames' slots first.
    fn filled(&mut self, slots: u64) {
        self.open = self.open.saturating_sub(slots);
    }
}

/// What the writer thread has put down the pipe, split by where it came from.
///
/// `written` counts frames the renderer actually produced and is the health
/// stat; `padded` counts repeats the writer inserted to cover gaps where it
/// did not. The two together are the length of the video timeline.
#[derive(Clone)]
struct FrameCounters {
    written: Arc<AtomicU64>,
    padded: Arc<AtomicU64>,
    /// Frames the render thread offered while the writer channel was full.
    /// Distinct from `padded`: a drop never entered the pipe.
    dropped: Arc<AtomicU64>,
    /// Frames left out by the writer: drawn before the take began, or for a
    /// slot already written — the show running faster than the encoder's rate.
    left_out: Arc<AtomicU64>,
    /// Slots a live stream skipped, fallen behind real time (see
    /// [`take_live`]): its encoder's trouble.
    skipped: Arc<AtomicU64>,
    /// Slots a live stream skipped that the show drew no frame for, held up
    /// (see [`Timeline::place_live`]): not the encoder's.
    held: Arc<AtomicU64>,
    /// The time the dropped frames would have covered.
    drop_clock: Arc<std::sync::Mutex<DropClock>>,
}

impl FrameCounters {
    fn new() -> Self {
        Self {
            written: Arc::new(AtomicU64::new(0)),
            padded: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
            left_out: Arc::new(AtomicU64::new(0)),
            skipped: Arc::new(AtomicU64::new(0)),
            held: Arc::new(AtomicU64::new(0)),
            drop_clock: Arc::default(),
        }
    }
}

/// The time the frames an encoder could not take would have covered: each
/// one counts the stretch since the frame offered before it — its own slot,
/// at whatever pace the show runs then — so a frame rate changed while live
/// does not recount what went before.
#[derive(Debug, Default)]
struct DropClock {
    last_offer: Option<std::time::Instant>,
    lost: std::time::Duration,
}

impl DropClock {
    /// The most one dropped frame counts: a stall before it is the show's,
    /// not the encoder's.
    const MOST: std::time::Duration = std::time::Duration::from_millis(250);

    /// A frame drawn at `made` was offered, and `dropped` or taken.
    fn offer(&mut self, made: std::time::Instant, dropped: bool) {
        if let (true, Some(before)) = (dropped, self.last_offer) {
            self.lost += made.saturating_duration_since(before).min(Self::MOST);
        }
        self.last_offer = Some(made);
    }
}

/// Offer one video frame to the writer without blocking.
///
/// `Ok` / `Full` both return true: the subprocess is still alive. `Full`
/// increments `dropped` so the operator can see backpressure. `Disconnected`
/// returns false.
fn try_enqueue_frame(
    tx: &mpsc::SyncSender<(std::time::Instant, Vec<u8>)>,
    dropped: &AtomicU64,
    label: &str,
    made: std::time::Instant,
    rgba: &[u8],
) -> bool {
    match tx.try_send((made, rgba.to_vec())) {
        Ok(()) => true,
        Err(mpsc::TrySendError::Full(_)) => {
            let n = dropped.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 || n.is_multiple_of(120) {
                log::warn!("ffmpeg video backpressure on '{label}': dropped {n} frames");
            }
            true
        }
        Err(mpsc::TrySendError::Disconnected(_)) => false,
    }
}

/// Compute video/buffer bitrate in kbps for RTMP output based on resolution and frame rate.
fn compute_rtmp_bitrate(width: u32, height: u32, fps: u32) -> (u32, u32) {
    let pixels = width * height;
    let base = match pixels {
        p if p <= 921_600 => 3000,   // ≤720p
        p if p <= 2_073_600 => 6000, // ≤1080p
        p if p <= 3_686_400 => 9000, // ≤1440p
        _ => 15000,                  // 4K+
    };
    let maxrate = if fps > 30 { base * 3 / 2 } else { base };
    (maxrate, maxrate * 2)
}

/// AAC output bitrate for passthrough audio.
const AUDIO_BITRATE: &str = "192k";
/// Normalized sample rate for streaming targets (Twitch/YouTube expect 48k AAC).
const STREAM_SAMPLE_RATE: &str = "48000";

/// Optional second (audio) input for an ffmpeg subprocess: a stream of raw
/// interleaved `f32` PCM plus the capture device's native format. Built from an
/// `AudioManager` PCM subscription; `None` keeps the byte-for-byte video-only path.
pub struct AudioInput {
    /// Raw interleaved PCM, drained by the audio writer thread into the socket.
    pub rx: crossbeam_channel::Receiver<PcmChunk>,
    /// Device native sample rate (Hz).
    pub sample_rate: u32,
    /// Device native channel count.
    pub channels: u16,
    /// Samples the capture callback discarded because this subscriber's channel
    /// was full. The writer replaces them with silence so the sample count keeps
    /// matching elapsed time — see [`AudioPipe::start`].
    pub lost_samples: Arc<AtomicU64>,
    /// When the first sample in `rx` was taken: the moment the subscription
    /// began. The picture's timeline starts here too, so both tracks start
    /// together (see [`Timeline`]).
    pub started: std::time::Instant,
}

/// ffmpeg argument vectors + the live listener/receiver, computed before the
/// `Command` is assembled so audio input args can be interleaved after the video
/// input and audio output args before the destination.
struct PreparedAudio {
    in_args: Vec<String>,
    out_args: Vec<String>,
    listener: TcpListener,
    rx: crossbeam_channel::Receiver<PcmChunk>,
    started: std::time::Instant,
}

/// Build the ffmpeg audio input/output args and bind the loopback TCP endpoint
/// for an optional audio passthrough. `is_stream` selects the sample-rate policy:
/// native rate for Recording, normalized 48k for streaming targets (Decision 5).
fn prepare_audio(
    audio: Option<AudioInput>,
    is_stream: bool,
) -> anyhow::Result<Option<PreparedAudio>> {
    prepare_audio_at(audio, is_stream, AUDIO_BITRATE)
}

/// [`prepare_audio`] at a given AAC bit rate, such as `"160k"`.
fn prepare_audio_at(
    audio: Option<AudioInput>,
    is_stream: bool,
    bitrate: &str,
) -> anyhow::Result<Option<PreparedAudio>> {
    prepare_audio_with(
        audio,
        is_stream,
        vec!["-c:a".into(), "aac".into(), "-b:a".into(), bitrate.into()],
    )
}

/// [`prepare_audio`] with the sound codec arguments given, such as
/// `-c:a pcm_s16le` beside `ProRes`.
fn prepare_audio_with(
    audio: Option<AudioInput>,
    is_stream: bool,
    codec_args: Vec<String>,
) -> anyhow::Result<Option<PreparedAudio>> {
    let Some(audio) = audio else {
        return Ok(None);
    };
    let (listener, audio_url) = create_audio_endpoint()?;
    // Input opts (must precede the audio `-i`); f32le matches the raw PCM tap.
    //
    // Timestamps come from the sample count, which is ffmpeg's default for a raw
    // input: sample N sits at N/sample_rate. This used to pass
    // `-use_wallclock_as_timestamps 1`, stamping each buffer with the moment it
    // arrived over the socket, and that was the cause of audio breaking up
    // whenever the renderer hitched. Arrival time is not a clock — it carries
    // scheduler jitter, and it stalls outright when ffmpeg stops draining the
    // socket to wait on the video pipe. Every one of those stalls was written
    // into the file as a timing hole.
    //
    // The capture device's sample clock has none of those problems: the hardware
    // delivers exactly `sample_rate` samples per second no matter what the rest
    // of the process is doing. It is the most accurate clock available here, so
    // it is the one the recording is built on. See /spec/av-sync.md.
    let in_args = vec![
        "-f".into(),
        "f32le".into(),
        "-ar".into(),
        audio.sample_rate.to_string(),
        "-ac".into(),
        audio.channels.to_string(),
        "-i".into(),
        audio_url,
    ];
    // Output opts: AAC, stereo downmix (Decision: stereo for v1), async resample
    // to absorb A/V drift; force 48k on streams, leave native on recordings.
    let mut out_args = codec_args;
    out_args.extend([
        "-ac".into(),
        "2".into(),
        "-af".into(),
        "aresample=async=1:first_pts=0".into(),
    ]);
    if is_stream {
        out_args.push("-ar".into());
        out_args.push(STREAM_SAMPLE_RATE.into());
    }
    // Explicit stream mapping once a second input exists.
    out_args.push("-map".into());
    out_args.push("0:v:0".into());
    out_args.push("-map".into());
    out_args.push("1:a:0".into());
    Ok(Some(PreparedAudio {
        in_args,
        out_args,
        listener,
        rx: audio.rx,
        started: audio.started,
    }))
}

/// Bind a loopback TCP listener on an ephemeral port and return it with the
/// `tcp://127.0.0.1:<port>` URL ffmpeg connects to as the audio input. Loopback
/// TCP is the cross-platform second-input transport (no `mkfifo`/named pipes and
/// no new crate, per the audio-passthrough transport decision).
fn create_audio_endpoint() -> anyhow::Result<(TcpListener, String)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| anyhow::anyhow!("Failed to bind audio TCP listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| anyhow::anyhow!("Failed to read audio listener address: {e}"))?
        .port();
    Ok((listener, format!("tcp://127.0.0.1:{port}")))
}

/// Start the audio writer thread for a prepared passthrough, if any. Called
/// after the ffmpeg child is spawned so the writer can accept ffmpeg's connection.
fn finalize_audio(
    prepared: Option<PreparedAudio>,
    label: String,
) -> anyhow::Result<Option<AudioPipe>> {
    match prepared {
        Some(p) => Ok(Some(AudioPipe::start(p.listener, p.rx, label)?)),
        None => Ok(None),
    }
}

/// Audio side-channel for an [`FfmpegSubprocess`]: a loopback TCP connection plus
/// a writer thread that drains raw PCM into it, symmetric with the video writer.
pub struct AudioPipe {
    /// Set before teardown so an expected broken pipe isn't logged as ERROR.
    shutting_down: Arc<AtomicBool>,
    writer_thread: Option<std::thread::JoinHandle<()>>,
    /// PCM chunks written to the socket so far (health stat).
    frames_written: Arc<AtomicU64>,
    /// Samples of silence spliced in to replace PCM lost to backpressure.
    silence_spliced: Arc<AtomicU64>,
}

/// Samples of silence written per `write_all` when filling a gap. Only used on
/// the rare backpressure path, so a modest buffer is plenty.
const SILENCE_BLOCK: usize = 4096;

impl AudioPipe {
    /// Start the audio writer thread. It accepts ffmpeg's connection to the
    /// loopback listener, then drains `rx` into the stream as f32le bytes.
    ///
    /// Each chunk says how many samples were lost just before it
    /// (`PcmChunk::lost_before`): discarded because this pipe was backed up,
    /// or never mixed. The writer puts that much silence in their place,
    /// before the chunk. That matters as timestamps come from the sample
    /// count: a gap left unfilled does not read as a gap, it pulls every later
    /// sample earlier, so a single dropout would desynchronise the rest of the
    /// recording. Filling it costs a brief mute and keeps the timeline exact.
    fn start(
        listener: TcpListener,
        rx: crossbeam_channel::Receiver<PcmChunk>,
        label: String,
    ) -> anyhow::Result<Self> {
        let shutting_down = Arc::new(AtomicBool::new(false));
        let frames_written = Arc::new(AtomicU64::new(0));
        let silence_spliced = Arc::new(AtomicU64::new(0));
        let sd = shutting_down.clone();
        let fw = frames_written.clone();
        let spliced = silence_spliced.clone();
        // Non-blocking accept so teardown can interrupt a wait for an ffmpeg that
        // never connects (e.g. it died at startup) instead of a wedged thread.
        listener
            .set_nonblocking(true)
            .map_err(|e| anyhow::anyhow!("Failed to set audio listener non-blocking: {e}"))?;
        let writer_thread = std::thread::Builder::new()
            .name(format!("ffmpeg-audio-{label}"))
            .spawn(move || {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((s, _)) => break s,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if sd.load(Ordering::SeqCst) {
                                return;
                            }
                            std::thread::sleep(std::time::Duration::from_millis(20));
                        }
                        Err(e) => {
                            if !sd.load(Ordering::SeqCst) {
                                log::error!("audio TCP accept failed for '{label}': {e}");
                            }
                            return;
                        }
                    }
                };
                // Blocking writes once connected; disable Nagle to minimize latency.
                if let Err(e) = stream.set_nonblocking(false) {
                    log::error!("audio TCP set-blocking failed for '{label}': {e}");
                    return;
                }
                let _ = stream.set_nodelay(true);
                let silence = [0f32; SILENCE_BLOCK];
                loop {
                    match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                        // Stopping wins over a producer that never pauses. A
                        // capture device goes quiet once it is released, but
                        // an audio-mixer bus sends a block every 10 ms for as
                        // long as it exists: checking only on a timeout meant
                        // the writer never quit, ffmpeg never saw the end of
                        // its audio input, and the recording was never
                        // finished.
                        Ok(_) if sd.load(Ordering::SeqCst) => {
                            let _ = stream.flush();
                            return;
                        }
                        Ok(chunk) => {
                            // Restore the time lost just before this chunk —
                            // thrown away on a full channel, or jumped over by
                            // the mixer — there, before it: the chunks that came
                            // earlier keep their places.
                            let mut lost = chunk.lost_before;
                            if lost > 0 {
                                spliced.fetch_add(lost, Ordering::Relaxed);
                                log::warn!(
                                    "audio backpressure on '{label}': spliced {lost} samples of \
                                     silence to hold the timeline"
                                );
                            }
                            while lost > 0 {
                                let n = lost.min(SILENCE_BLOCK as u64) as usize;
                                let bytes: &[u8] = bytemuck::cast_slice(&silence[..n]);
                                if stream.write_all(bytes).is_err() {
                                    return;
                                }
                                lost -= n as u64;
                            }

                            let bytes: &[u8] = bytemuck::cast_slice(&chunk.samples);
                            if let Err(e) = stream.write_all(bytes) {
                                if sd.load(Ordering::SeqCst) {
                                    log::debug!(
                                        "audio pipe closed during shutdown for '{label}': {e}"
                                    );
                                } else {
                                    log::error!("audio pipe write error for '{label}': {e}");
                                }
                                return;
                            }
                            fw.fetch_add(1, Ordering::Relaxed);
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            if sd.load(Ordering::SeqCst) {
                                let _ = stream.flush();
                                return;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                            let _ = stream.flush();
                            return;
                        }
                    }
                }
            })
            .map_err(|e| anyhow::anyhow!("Failed to spawn audio writer thread: {e}"))?;

        Ok(Self {
            shutting_down,
            silence_spliced,
            writer_thread: Some(writer_thread),
            frames_written,
        })
    }

    /// Tell the writer thread to stop, not waiting for it (see
    /// [`Self::stop`]).
    fn begin_stop(&self) {
        self.shutting_down.store(true, Ordering::SeqCst);
    }

    /// Whether the writer thread has ended.
    fn is_finished(&self) -> bool {
        self.writer_thread
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }

    /// Tear down the writer thread. Idempotent. Setting `shutting_down` unblocks
    /// a pending accept-poll (~20ms) or `recv_timeout` drain (~100ms).
    fn stop(&mut self) {
        self.shutting_down.store(true, Ordering::SeqCst);
        if let Some(handle) = self.writer_thread.take() {
            let _ = handle.join();
        }
    }

    /// PCM chunks written to the socket so far.
    fn frames_written(&self) -> u64 {
        self.frames_written.load(Ordering::Relaxed)
    }

    /// Samples of silence written in place of PCM lost to backpressure.
    fn silence_spliced(&self) -> u64 {
        self.silence_spliced.load(Ordering::Relaxed)
    }
}

impl Drop for AudioPipe {
    fn drop(&mut self) {
        self.stop();
    }
}

impl FfmpegSubprocess {
    /// Start the video writer thread.
    ///
    /// The raw video input is declared at a constant `fps`, so ffmpeg times each
    /// frame by its position in the stream: frame N is at N/fps regardless of
    /// when it was produced. The sound is timed by its sample count from
    /// `start`, when its first sample was taken. So every frame goes where the
    /// moment it was drawn falls after `start` (see [`Timeline`]): a slot no
    /// frame came for repeats the frame before it — nearly free to encode, and
    /// the file stays constant-frame-rate, which is what editors want — and a
    /// frame for a slot already written is left out. Without `start` (no
    /// sound), the first frame starts the take.
    #[allow(clippy::too_many_arguments)] // Each is one shared piece of the encoder's state.
    fn start_writer_thread(
        mut stdin: std::process::ChildStdin,
        rx: mpsc::Receiver<(std::time::Instant, Vec<u8>)>,
        fps: u32,
        counters: FrameCounters,
        write_failed: Arc<AtomicBool>,
        shutting_down: Arc<AtomicBool>,
        label: String,
        start: Option<std::time::Instant>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::Builder::new()
            .name(format!("ffmpeg-writer-{label}"))
            .spawn(move || {
                let mut timeline = Timeline::of_show(start, f64::from(fps.max(1)));
                // The frame written last, repeated over slots no frame came
                // for: what was on screen through them.
                let mut last: Option<Vec<u8>> = None;
                for (made, frame) in rx {
                    let Placed::After(repeats) = timeline.place(made) else {
                        counters.left_out.fetch_add(1, Ordering::Relaxed);
                        continue;
                    };
                    let fill = last.as_deref().unwrap_or(&frame);
                    for _ in 0..repeats {
                        if let Err(e) = stdin.write_all(fill) {
                            Self::report_write_error(&e, &shutting_down, &write_failed, &label);
                            return;
                        }
                    }
                    if repeats > 0 {
                        counters.padded.fetch_add(repeats, Ordering::Relaxed);
                    }
                    if let Err(e) = stdin.write_all(&frame) {
                        Self::report_write_error(&e, &shutting_down, &write_failed, &label);
                        return;
                    }
                    counters.written.fetch_add(1, Ordering::Relaxed);
                    last = Some(frame);
                }
                // Channel closed — normal shutdown, flush stdin
                let _ = stdin.flush();
            })
            .expect("failed to spawn ffmpeg writer thread")
    }

    fn report_write_error(
        e: &std::io::Error,
        shutting_down: &Arc<AtomicBool>,
        write_failed: &Arc<AtomicBool>,
        label: &str,
    ) {
        if shutting_down.load(Ordering::SeqCst) {
            log::debug!("ffmpeg pipe closed during shutdown for '{label}': {e}");
        } else {
            log::error!("ffmpeg write error for '{label}': {e}");
            write_failed.store(true, Ordering::SeqCst);
        }
    }

    /// Spawn an ffmpeg recording subprocess.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio passthrough endpoint cannot be prepared or
    /// started, or if the `ffmpeg` binary cannot be spawned (not installed).
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin was not piped, or if the writer thread cannot
    /// be spawned — both indicate the process/thread limits are exhausted.
    #[cfg(test)]
    fn spawn_recording(
        path: &str,
        codec: &RecordingCodec,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<AudioInput>,
    ) -> anyhow::Result<Self> {
        Self::spawn_recording_with_presentation(
            path,
            codec,
            width,
            height,
            fps,
            audio,
            PresentationRequest::default(),
            ReadbackFormat::Rgba8,
        )
    }

    /// Resolve recording precision before configuring the headless render target.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured FFmpeg encoder cannot be queried.
    pub fn probe_recording_presentation(
        codec: &RecordingCodec,
        request: PresentationRequest,
        rgba16_unorm_supported: bool,
    ) -> anyhow::Result<ResolvedPresentation> {
        let encoder = recording_encoder(codec);
        let encoder_help = probe_encoder_help(encoder).ok_or_else(|| {
            anyhow::anyhow!("FFmpeg encoder '{encoder}' is not installed or cannot be queried")
        })?;
        Ok(RecordingPlan::resolve(
            codec,
            request,
            preferred_recording_readback(codec, request, rgba16_unorm_supported),
            Some(&encoder_help),
        )
        .resolved)
    }

    /// Spawn a recording after resolving codec, FFmpeg, and typed readback
    /// capabilities as one complete presentation path.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio endpoint or FFmpeg process cannot start.
    ///
    /// # Panics
    ///
    /// Panics if FFmpeg's piped stdin is unavailable or the bounded writer
    /// thread cannot be spawned.
    #[allow(
        clippy::too_many_arguments,
        reason = "the spawn boundary mirrors FFmpeg's fixed recording inputs"
    )]
    pub fn spawn_recording_with_presentation(
        path: &str,
        codec: &RecordingCodec,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<AudioInput>,
        request: PresentationRequest,
        readback_format: ReadbackFormat,
    ) -> anyhow::Result<Self> {
        let encoder = recording_encoder(codec);
        let encoder_help = probe_encoder_help(encoder).ok_or_else(|| {
            anyhow::anyhow!("FFmpeg encoder '{encoder}' is not installed or cannot be queried")
        })?;
        let plan = RecordingPlan::resolve(codec, request, readback_format, Some(&encoder_help));
        // Recording keeps the device's native sample rate (Decision 5).
        let prepared = prepare_audio(audio, false)?;
        let empty: Vec<String> = Vec::new();
        let (a_in, a_out) = match &prepared {
            Some(p) => (&p.in_args, &p.out_args),
            None => (&empty, &empty),
        };
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", plan.input_pixel_format])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "-"])
            .args(a_in);
        if plan.unpremultiply {
            cmd.args(["-vf", unpremultiply_filter(plan.input_pixel_format)]);
        }
        cmd.args(&plan.video_args);
        cmd.args(a_out)
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn ffmpeg: {e}. Is ffmpeg installed?"))?;

        log::info!(
            "Recording started: {path} ({codec}, {width}x{height} @ {fps}fps, {} -> {})",
            plan.input_pixel_format,
            plan.output_pixel_format
        );

        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        let writer_thread = Self::start_writer_thread(
            stdin,
            rx,
            fps,
            counters.clone(),
            write_failed.clone(),
            shutting_down.clone(),
            path.to_string(),
            prepared.as_ref().map(|p| p.started),
        );
        let audio = finalize_audio(prepared, path.to_string())?;

        Ok(Self {
            child,
            stderr_tail,
            fanout: None,
            rate_plan: None,
            native: None,
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label: path.to_string(),
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: true,
            frame_contract: Some(FrameContract {
                format: plan.expected_readback,
                width,
                height,
            }),
            presentation: Some(plan.resolved),
        })
    }

    /// Spawn an ffmpeg SRT streaming subprocess in listener (server) mode.
    /// Starts an SRT server on the specified port and broadcasts frames to connected clients.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio passthrough endpoint cannot be prepared or
    /// started, or if the `ffmpeg` binary cannot be spawned (not installed).
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin was not piped, or if the writer thread cannot
    /// be spawned — both indicate the process/thread limits are exhausted.
    pub fn spawn_srt(
        url: &str,
        codec: &super::context::SrtCodec,
        request: PresentationRequest,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<AudioInput>,
    ) -> anyhow::Result<Self> {
        // Streaming target: normalize audio to 48k (Decision 5).
        let prepared = prepare_audio(audio, true)?;
        let empty: Vec<String> = Vec::new();
        let (a_in, a_out) = match &prepared {
            Some(p) => (&p.in_args, &p.out_args),
            None => (&empty, &empty),
        };
        // Ensure listener mode so ffmpeg acts as an SRT server
        let srt_url = if url.contains("mode=") {
            url.to_string()
        } else if url.contains('?') {
            format!("{url}&mode=listener")
        } else {
            format!("{url}?mode=listener")
        };

        let configured_codec = match codec {
            super::context::SrtCodec::H264 => StreamingCodec::H264,
            super::context::SrtCodec::H265 => StreamingCodec::H265,
        };
        let plan = StreamingPlan::resolve(
            StreamingProtocol::Srt,
            configured_codec,
            request,
            StreamingCapabilities::for_request(request),
        );

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", plan.input_pixel_format])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "-"])
            .args(a_in)
            .args(&plan.video_args)
            .args(a_out)
            .args(&plan.muxer_args)
            .arg(&srt_url)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn ffmpeg for SRT: {e}. Is ffmpeg installed?")
        })?;

        log::info!("SRT server started: {srt_url} ({width}x{height} @ {fps}fps)");

        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        let writer_thread = Self::start_writer_thread(
            stdin,
            rx,
            fps,
            counters.clone(),
            write_failed.clone(),
            shutting_down.clone(),
            url.to_string(),
            prepared.as_ref().map(|p| p.started),
        );
        let audio = finalize_audio(prepared, url.to_string())?;

        Ok(Self {
            child,
            stderr_tail,
            fanout: None,
            rate_plan: None,
            native: None,
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label: url.to_string(),
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: false,
            frame_contract: Some(FrameContract {
                format: plan.expected_readback,
                width,
                height,
            }),
            presentation: Some(plan.resolved),
        })
    }

    /// Spawn an ffmpeg HLS output subprocess.
    /// Writes HLS segments to `.varda/streams/<name>/` with `-hls_list_size 0` for VOD archive.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio passthrough endpoint cannot be prepared or
    /// started, if the stream output directory cannot be created, or if the
    /// `ffmpeg` binary cannot be spawned (not installed).
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin was not piped, or if the writer thread cannot
    /// be spawned — both indicate the process/thread limits are exhausted.
    // Arguments mirror the persisted HLS target plus frame/audio transport.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_hls(
        name: &str,
        codec: &super::context::StreamingCodec,
        request: PresentationRequest,
        width: u32,
        height: u32,
        fps: u32,
        low_latency: bool,
        audio: Option<AudioInput>,
    ) -> anyhow::Result<Self> {
        // Streaming target: normalize audio to 48k (Decision 5).
        let prepared = prepare_audio(audio, true)?;
        let empty: Vec<String> = Vec::new();
        let (a_in, a_out) = match &prepared {
            Some(p) => (&p.in_args, &p.out_args),
            None => (&empty, &empty),
        };
        let dir = format!(".varda/streams/{name}");
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("Failed to create HLS output dir '{dir}': {e}"))?;
        let playlist = format!("{dir}/index.m3u8");
        write_stream_player(&dir, "hls", "index.m3u8", low_latency);

        let plan = StreamingPlan::resolve(
            StreamingProtocol::Hls,
            codec.clone(),
            request,
            StreamingCapabilities::for_request(request),
        );

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", plan.input_pixel_format])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "-"])
            .args(a_in)
            .args(&plan.video_args)
            .args(a_out)
            .args(&plan.muxer_args);

        cmd.args(hls_segment_args(
            &dir,
            low_latency,
            plan.resolved.resolved == PresentationDepth::Sdr10
                || plan.effective_codec != StreamingCodec::H264,
        ));

        cmd.arg(&playlist)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn ffmpeg for HLS: {e}. Is ffmpeg installed?")
        })?;

        let mode = if low_latency { "LL-HLS" } else { "HLS" };
        log::info!("{mode} output started: {playlist} ({width}x{height} @ {fps}fps)");

        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        let writer_thread = Self::start_writer_thread(
            stdin,
            rx,
            fps,
            counters.clone(),
            write_failed.clone(),
            shutting_down.clone(),
            name.to_string(),
            prepared.as_ref().map(|p| p.started),
        );
        let audio = finalize_audio(prepared, name.to_string())?;

        Ok(Self {
            child,
            stderr_tail,
            fanout: None,
            rate_plan: None,
            native: None,
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label: name.to_string(),
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: false,
            frame_contract: Some(FrameContract {
                format: plan.expected_readback,
                width,
                height,
            }),
            presentation: Some(plan.resolved),
        })
    }

    /// Spawn an ffmpeg RTMP output subprocess.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio passthrough endpoint cannot be prepared or
    /// started, or if the `ffmpeg` binary cannot be spawned (not installed).
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin was not piped, or if the writer thread cannot
    /// be spawned — both indicate the process/thread limits are exhausted.
    // Arguments mirror the persisted RTMP target plus frame/audio transport.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_rtmp(
        url: &str,
        codec: &super::context::StreamingCodec,
        codec_contract: RtmpCodecContract,
        request: PresentationRequest,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<AudioInput>,
    ) -> anyhow::Result<Self> {
        // Streaming target: normalize audio to 48k (Decision 5).
        let prepared = prepare_audio(audio, true)?;
        let empty: Vec<String> = Vec::new();
        let (a_in, a_out) = match &prepared {
            Some(p) => (&p.in_args, &p.out_args),
            None => (&empty, &empty),
        };
        let plan = StreamingPlan::resolve(
            StreamingProtocol::Rtmp(codec_contract),
            codec.clone(),
            request,
            StreamingCapabilities::for_request(request),
        );

        let (maxrate, bufsize) = compute_rtmp_bitrate(width, height, fps);
        let gop = fps * 2;

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", plan.input_pixel_format])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "-"])
            .args(a_in)
            .args(&plan.video_args)
            .args(["-b:v", &format!("{maxrate}k")])
            .args(["-maxrate", &format!("{maxrate}k")])
            .args(["-bufsize", &format!("{bufsize}k")])
            .args(["-g", &gop.to_string()])
            .args(a_out)
            .args(&plan.muxer_args)
            .arg(url)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn ffmpeg for RTMP: {e}. Is ffmpeg installed?")
        })?;

        log::info!("RTMP output started: {url} ({width}x{height} @ {fps}fps, {maxrate}kbps)");

        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        let label = url.to_string();
        let writer_thread = Self::start_writer_thread(
            stdin,
            rx,
            fps,
            counters.clone(),
            write_failed.clone(),
            shutting_down.clone(),
            label.clone(),
            prepared.as_ref().map(|p| p.started),
        );
        let audio = finalize_audio(prepared, label.clone())?;

        Ok(Self {
            child,
            stderr_tail,
            fanout: None,
            rate_plan: None,
            native: None,
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label,
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: false,
            frame_contract: Some(FrameContract {
                format: plan.expected_readback,
                width,
                height,
            }),
            presentation: Some(plan.resolved),
        })
    }

    /// Spawn the stream's encoder: the picture and sound encoded once, as
    /// MPEG-TS on ffmpeg's stdout, which a [`crate::streaming::Fanout`] sends
    /// on to every destination in `settings`.
    ///
    /// The stream always carries sound — silence when no audio is given —
    /// because some platforms refuse a stream without an audio track.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio endpoint cannot be prepared or ffmpeg
    /// cannot be spawned.
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin or stdout was not piped, or the writer thread
    /// cannot be spawned.
    pub fn spawn_stream(
        settings: &crate::streaming::StreamSettings,
        request: PresentationRequest,
        size: (u32, u32),
        fps: u32,
        audio: Option<AudioInput>,
    ) -> anyhow::Result<Self> {
        Self::spawn_stream_with(settings, request, size, fps, audio, false)
    }

    /// [`Self::spawn_stream`], fed NV12 frames made on the GPU when `nv12`
    /// (see [`Self::feed_nv12`]) rather than RGBA for FFmpeg to convert.
    ///
    /// # Errors
    ///
    /// As [`Self::spawn_stream`].
    ///
    /// # Panics
    ///
    /// As [`Self::spawn_stream`].
    pub fn spawn_stream_with(
        settings: &crate::streaming::StreamSettings,
        request: PresentationRequest,
        (width, height): (u32, u32),
        fps: u32,
        audio: Option<AudioInput>,
        nv12: bool,
    ) -> anyhow::Result<Self> {
        let options = &settings.options;
        let audio_rate = format!("{}k", options.audio_kbps);
        let prepared = prepare_audio_at(audio, true, &audio_rate)?;
        let (a_in, a_out): (Vec<String>, Vec<String>) = match &prepared {
            Some(p) => (p.in_args.clone(), p.out_args.clone()),
            None => (
                [
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=channel_layout=stereo:sample_rate=48000",
                ]
                .map(str::to_string)
                .to_vec(),
                [
                    "-c:a",
                    "aac",
                    "-b:a",
                    &audio_rate,
                    "-ar",
                    "48000",
                    "-ac",
                    "2",
                    "-map",
                    "0:v:0",
                    "-map",
                    "1:a:0",
                ]
                .map(str::to_string)
                .to_vec(),
            ),
        };
        // Eight-bit H.264, as RTMP carries: the plan pins the pixel formats
        // and colour signalling; the encoder itself is the stream's own.
        let plan = StreamingPlan::resolve(
            StreamingProtocol::Rtmp(RtmpCodecContract::Legacy),
            StreamingCodec::H264,
            request,
            StreamingCapabilities::for_request(request),
        );
        let rate_plan = options.encoder.plan(options.codec, options.rate_mode);
        if let Some(note) = &rate_plan.note {
            log::warn!("Stream: {note}");
        }
        let encoder = rate_plan.encoder;
        // Frames arrive at Varda's rate; ffmpeg drops or repeats them to the
        // rate the stream is sent at, and keyframes count in those frames.
        let out_fps = options.fps.unwrap_or(fps).max(1);
        let gop = out_fps * options.keyframe_secs.max(1);
        // Encoded in Nida where it can be, its rate followed while live.
        let native = native::open(options, (width, height), out_fps, nv12, &rate_plan);

        let input_format = if nv12 {
            "nv12"
        } else {
            plan.input_pixel_format
        };
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"]);
        if native.is_some() {
            // Nida's encoded frames, each with its time (see `streaming::flv`):
            // read at once, not looked ahead into for seconds, and copied.
            cmd.args(["-f", "flv", "-probesize", "32", "-analyzeduration", "0"])
                .args(["-i", "pipe:0"])
                .args(&a_in)
                .args(["-c:v", "copy"]);
        } else {
            cmd.args(["-f", "rawvideo"])
                .args(["-pix_fmt", input_format])
                .args(nv12_input_colour(nv12))
                .args(["-s", &format!("{width}x{height}")])
                .args(["-r", &fps.to_string()])
                .args(["-i", "-"])
                .args(&a_in)
                .args(["-r", &out_fps.to_string()])
                .args(encoder.stream_video_args(options.codec, options.video_kbps, gop, &rate_plan))
                .args([
                    "-pix_fmt",
                    encoder.pixel_format(options.codec, nv12),
                    "-color_primaries",
                    "bt709",
                    "-color_trc",
                    "bt709",
                    "-colorspace",
                    "bt709",
                    "-color_range",
                    "tv",
                ]);
        }
        cmd.args(&a_out)
            .args(["-f", "mpegts", "pipe:1"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn ffmpeg for the stream: {e}. Is ffmpeg installed?")
        })?;
        let label = "stream".to_string();
        log::info!(
            "Stream encoder started: {width}x{height} @ {out_fps}fps (from {fps}), {} {} kbps {}, {}",
            options.codec.label(),
            options.video_kbps,
            rate_plan.mode.label(),
            if native.is_some() {
                "the media engine, in Nida (the rate changes while live)"
            } else {
                encoder.label()
            }
        );

        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stdout = child.stdout.take().expect("ffmpeg stdout not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let bytes_per_sec = u64::from(options.video_kbps + options.audio_kbps) * 1000 / 8;
        let fanout = crate::streaming::Fanout::start(
            stdout,
            &settings.destinations,
            bytes_per_sec,
            options.keyframe_secs,
            options.buffer_secs,
        );
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(stream_frame_capacity(width, height, fps, nv12));
        let started = prepared.as_ref().map(|p| p.started);
        let (writer_thread, native) = match native {
            Some(opened) => {
                let (writer, native) = native::start(
                    opened,
                    stdin,
                    rx,
                    (
                        out_fps,
                        options.codec,
                        options.audio_kbps,
                        LIVE_BACKLOG as u64,
                    ),
                    counters.clone(),
                    (write_failed.clone(), shutting_down.clone()),
                    &label,
                    started,
                );
                // A lost piece is worth the time it held at the rate the
                // engine took, whoever changed it.
                fanout.reckon_at(native.shared.applied_kbps.clone(), native.shared.audio_kbps);
                native
                    .shared
                    .strip_filler
                    .store(rate_plan.mode == crate::streaming::RateMode::Variable, Ordering::Relaxed);
                (writer, Some(native))
            }
            None => (
                Self::start_writer_thread(
                    stdin,
                    rx,
                    fps,
                    counters.clone(),
                    write_failed.clone(),
                    shutting_down.clone(),
                    label.clone(),
                    started,
                ),
                None,
            ),
        };
        let audio = finalize_audio(prepared, label.clone())?;

        Ok(Self {
            child,
            stderr_tail,
            fanout: Some(fanout),
            native,
            rate_plan: Some(rate_plan),
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label,
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: false,
            frame_contract: Some(FrameContract {
                format: if nv12 {
                    ReadbackFormat::Nv12
                } else {
                    plan.expected_readback
                },
                width,
                height,
            }),
            presentation: Some(plan.resolved),
        })
    }

    /// Spawn a recording's encoder: RGBA frames and the sound into `path`,
    /// encoded as `options` say — on the media engine where it can.
    /// Stopping finishes the file on a background thread (see [`Self::stop`]).
    ///
    /// # Errors
    ///
    /// Returns an error if the folder cannot be made, the audio endpoint
    /// cannot be prepared, or ffmpeg cannot be spawned.
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin was not piped, or the writer thread cannot be
    /// spawned.
    pub fn spawn_record(
        options: &crate::record::RecordOptions,
        size: (u32, u32),
        fps: u32,
        audio: Option<AudioInput>,
        path: &std::path::Path,
    ) -> anyhow::Result<Self> {
        Self::spawn_record_with(options, size, fps, audio, path, false)
    }

    /// [`Self::spawn_record`], fed NV12 frames made on the GPU when `nv12`
    /// (see [`Self::feed_nv12`]) — for H.264 and HEVC; `ProRes` takes RGBA.
    ///
    /// # Errors
    ///
    /// As [`Self::spawn_record`].
    ///
    /// # Panics
    ///
    /// As [`Self::spawn_record`].
    pub fn spawn_record_with(
        options: &crate::record::RecordOptions,
        (width, height): (u32, u32),
        fps: u32,
        audio: Option<AudioInput>,
        path: &std::path::Path,
        nv12: bool,
    ) -> anyhow::Result<Self> {
        let nv12 = nv12 && options.takes_nv12();
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|e| {
                anyhow::anyhow!("Cannot make the recording folder {}: {e}", folder.display())
            })?;
        }
        let prepared = prepare_audio_with(audio, false, options.audio_codec_args())?;
        let (a_in, a_out): (Vec<String>, Vec<String>) = if let Some(p) = &prepared {
            (p.in_args.clone(), p.out_args.clone())
        } else {
            let mut out = options.audio_codec_args();
            out.extend(
                ["-ar", "48000", "-ac", "2", "-map", "0:v:0", "-map", "1:a:0"].map(str::to_string),
            );
            (
                [
                    "-f",
                    "lavfi",
                    "-i",
                    "anullsrc=channel_layout=stereo:sample_rate=48000",
                ]
                .map(str::to_string)
                .to_vec(),
                out,
            )
        };
        let out_fps = options.fps.unwrap_or(fps).max(1);
        // H.264 or HEVC on the media engine: encoded in Nida, each frame at
        // its own time (see `native::open_for_record`); else ffmpeg encodes.
        let native = options.engine_codec().filter(|_| nv12).and_then(|codec| {
            let kbps = options.effective_kbps((width, height), out_fps);
            native::open_for_record(codec, (width, height), out_fps, kbps).map(|o| (o, codec))
        });
        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"]);
        if native.is_some() {
            cmd.args(["-f", "flv", "-probesize", "32", "-analyzeduration", "0"])
                .args(["-i", "pipe:0"])
                .args(&a_in)
                .args(options.copied_video_args());
        } else {
            cmd.args([
                "-f",
                "rawvideo",
                "-pix_fmt",
                if nv12 { "nv12" } else { "rgba" },
            ])
            .args(nv12_input_colour(nv12))
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "-"])
            .args(&a_in)
            .args(options.output_args_for((width, height), fps, nv12));
        }
        cmd.args(&a_out)
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn ffmpeg for the recording: {e}. Is ffmpeg installed?")
        })?;
        let label = path.display().to_string();
        log::info!(
            "Recording started: {label} ({width}x{height} @ {}fps, {}{})",
            options.fps.unwrap_or(fps),
            options.codec.label(),
            if native.is_some() {
                ", the media engine, in Nida"
            } else {
                ""
            }
        );
        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(stream_frame_capacity(width, height, fps, nv12));
        let started = prepared.as_ref().map(|p| p.started);
        let (writer_thread, native) = match native {
            // Gaps up to a second filled, so the file keeps its frame rate
            // through a hitch; frames backed up, it skips ahead.
            Some((opened, codec)) => {
                let (writer, native) = native::start(
                    opened,
                    stdin,
                    rx,
                    (out_fps, codec, options.audio_kbps, u64::from(out_fps)),
                    counters.clone(),
                    (write_failed.clone(), shutting_down.clone()),
                    &label,
                    started,
                );
                (writer, Some(native))
            }
            None => (
                Self::start_writer_thread(
                    stdin,
                    rx,
                    fps,
                    counters.clone(),
                    write_failed.clone(),
                    shutting_down.clone(),
                    label.clone(),
                    started,
                ),
                None,
            ),
        };
        let audio = finalize_audio(prepared, label.clone())?;
        Ok(Self {
            child,
            stderr_tail,
            fanout: None,
            rate_plan: None,
            native,
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label,
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: true,
            frame_contract: Some(FrameContract {
                format: if nv12 {
                    ReadbackFormat::Nv12
                } else {
                    ReadbackFormat::Rgba8
                },
                width,
                height,
            }),
            presentation: None,
        })
    }

    /// Kill ffmpeg abruptly, as a crash would — for tests of what survives.
    #[doc(hidden)]
    pub fn kill_process(&mut self) {
        let _ = self.child.kill();
    }

    /// The stream's destinations, when this is the stream's encoder.
    pub fn fanout(&self) -> Option<&crate::streaming::Fanout> {
        self.fanout.as_ref()
    }

    /// Whether ffmpeg is still running — and the encoder in Nida, for a
    /// stream encoded there, still encoding.
    pub fn is_running(&mut self) -> bool {
        !self.stopped
            && !self.write_failed.load(Ordering::SeqCst)
            && !self.native.as_ref().is_some_and(|n| n.shared.stuck())
            && matches!(self.child.try_wait(), Ok(None))
    }

    /// Whether this stream's video rate changes while live — to follow the
    /// upload, or as the operator sets it: encoded in Nida.
    pub fn changes_rate_live(&self) -> bool {
        self.native.is_some()
    }

    /// How the stream is encoded, as it runs (see [`crate::streaming::RatePlan`]).
    pub fn rate_plan(&self) -> Option<&crate::streaming::RatePlan> {
        self.rate_plan.as_ref()
    }

    /// Hold the stream's rate as `mode` from the next frame on: between
    /// Constant and Variable, for a stream encoded in Nida — only whether the
    /// engine's padding is dropped. Whether it was taken.
    pub fn set_rate_mode(&mut self, mode: crate::streaming::RateMode) -> bool {
        use crate::streaming::RateMode;
        let (Some(native), Some(plan)) = (&self.native, self.rate_plan.as_mut()) else {
            return false;
        };
        let held = |m: RateMode| matches!(m, RateMode::Constant | RateMode::Variable);
        if !(held(mode) && held(plan.mode)) {
            return false;
        }
        native
            .shared
            .strip_filler
            .store(mode == RateMode::Variable, Ordering::Relaxed);
        plan.mode = mode;
        true
    }

    /// Ask for a video rate of `kbps` from the next frame on, for a stream
    /// whose rate changes while live.
    pub fn set_video_kbps(&self, kbps: u32) {
        if let Some(native) = &self.native {
            native.shared.wanted_kbps.store(kbps, Ordering::Relaxed);
        }
    }

    /// The video rate the engine last took, for a stream whose rate changes
    /// while live — the rate a lost piece is reckoned at, sound and picture,
    /// follows it by itself (see `Fanout::reckon_at`).
    pub fn video_kbps(&self) -> Option<u32> {
        let native = self.native.as_ref()?;
        Some(native.shared.applied_kbps.load(Ordering::Relaxed))
    }

    /// Spawn an ffmpeg DASH output subprocess.
    /// Writes DASH segments to `.varda/streams/<name>/` with `-window_size 0` for VOD archive.
    ///
    /// # Errors
    ///
    /// Returns an error if the audio passthrough endpoint cannot be prepared or
    /// started, if the stream output directory cannot be created, or if the
    /// `ffmpeg` binary cannot be spawned (not installed).
    ///
    /// # Panics
    ///
    /// Panics if ffmpeg's stdin was not piped, or if the writer thread cannot
    /// be spawned — both indicate the process/thread limits are exhausted.
    pub fn spawn_dash(
        name: &str,
        codec: &super::context::StreamingCodec,
        request: PresentationRequest,
        width: u32,
        height: u32,
        fps: u32,
        audio: Option<AudioInput>,
    ) -> anyhow::Result<Self> {
        // Streaming target: normalize audio to 48k (Decision 5).
        let prepared = prepare_audio(audio, true)?;
        let empty: Vec<String> = Vec::new();
        let (a_in, a_out) = match &prepared {
            Some(p) => (&p.in_args, &p.out_args),
            None => (&empty, &empty),
        };
        let dir = format!(".varda/streams/{name}");
        std::fs::create_dir_all(&dir)
            .map_err(|e| anyhow::anyhow!("Failed to create DASH output dir '{dir}': {e}"))?;
        let manifest = format!("{dir}/manifest.mpd");
        write_stream_player(&dir, "dash", "manifest.mpd", false);

        let plan = StreamingPlan::resolve(
            StreamingProtocol::Dash,
            codec.clone(),
            request,
            StreamingCapabilities::for_request(request),
        );

        let mut cmd = Command::new("ffmpeg");
        cmd.args(["-hide_banner", "-nostats", "-y"])
            .args(["-f", "rawvideo"])
            .args(["-pix_fmt", plan.input_pixel_format])
            .args(["-s", &format!("{width}x{height}")])
            .args(["-r", &fps.to_string()])
            .args(["-i", "-"])
            .args(a_in)
            .args(&plan.video_args)
            .args(a_out)
            .args(&plan.muxer_args)
            .args(["-seg_duration", "2"])
            .args(["-window_size", "30"])
            .args(["-extra_window_size", "5"])
            .arg(&manifest)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!("Failed to spawn ffmpeg for DASH: {e}. Is ffmpeg installed?")
        })?;

        log::info!("DASH output started: {manifest} ({width}x{height} @ {fps}fps)");

        let stdin = child.stdin.take().expect("ffmpeg stdin not piped");
        let stderr_tail = StderrTail::attach(&mut child);
        let counters = FrameCounters::new();
        let write_failed = Arc::new(AtomicBool::new(false));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(FRAME_CHANNEL_CAPACITY);
        let writer_thread = Self::start_writer_thread(
            stdin,
            rx,
            fps,
            counters.clone(),
            write_failed.clone(),
            shutting_down.clone(),
            name.to_string(),
            prepared.as_ref().map(|p| p.started),
        );
        let audio = finalize_audio(prepared, name.to_string())?;

        Ok(Self {
            child,
            stderr_tail,
            fanout: None,
            rate_plan: None,
            native: None,
            frame_tx: Some(tx),
            writer_thread: Some(writer_thread),
            counters,
            write_failed,
            shutting_down,
            label: name.to_string(),
            start_time: std::time::Instant::now(),
            stopped: false,
            audio,
            graceful_shutdown: false,
            frame_contract: Some(FrameContract {
                format: plan.expected_readback,
                width,
                height,
            }),
            presentation: Some(plan.resolved),
        })
    }

    /// Presentation selected for this FFmpeg output.
    pub fn presentation(&self) -> Option<&ResolvedPresentation> {
        self.presentation.as_ref()
    }

    /// Feed one typed GPU readback frame to an FFmpeg subprocess.
    ///
    /// The format, dimensions, and stride must match the contract negotiated
    /// before FFmpeg was spawned. A mismatch stops bytes from entering the raw
    /// video pipe, where they would otherwise silently desynchronize frames.
    pub fn feed_readback_frame(&mut self, frame: &ReadbackFrame) -> bool {
        let Some(contract) = self.frame_contract else {
            log::error!(
                "typed frame sent without an FFmpeg contract for '{}'",
                self.label
            );
            return false;
        };
        if frame.format() != contract.format
            || frame.width() != contract.width
            || frame.height() != contract.height
        {
            log::error!(
                "FFmpeg frame contract mismatch for '{}': expected {} {}x{}, got {} {}x{}",
                self.label,
                readback_label(contract.format),
                contract.width,
                contract.height,
                readback_label(frame.format()),
                frame.width(),
                frame.height()
            );
            return false;
        }
        let bytes_per_pixel = match contract.format {
            ReadbackFormat::Rgba8
            | ReadbackFormat::Bgra8
            | ReadbackFormat::Rgb10A2
            | ReadbackFormat::P216 => 4,
            ReadbackFormat::Rgba16Float | ReadbackFormat::Rgba16Unorm => 8,
            ReadbackFormat::Uyvy => 2,
            // Comes in through `feed_nv12`, whole.
            ReadbackFormat::Nv12 => return false,
        };
        let expected_stride = contract.width * bytes_per_pixel;
        if frame.stride() != expected_stride {
            log::error!(
                "FFmpeg frame stride mismatch for '{}': expected {}, got {}",
                self.label,
                expected_stride,
                frame.stride()
            );
            return false;
        }
        self.feed_frame_made(frame.bytes(), frame.made())
    }

    /// Feed an NV12 frame made on the GPU, drawn at `made`, to an encoder
    /// started for them (`spawn_stream_with` / `spawn_record_with`). A frame
    /// of another size, or an encoder waiting for RGBA, is refused rather than
    /// let through to put the pipe out of step.
    pub fn feed_nv12(&mut self, frame: &[u8], made: std::time::Instant) -> bool {
        let Some(contract) = self.frame_contract else {
            return false;
        };
        let expected = crate::renderer::nv12::nv12_len(contract.width, contract.height);
        if contract.format != ReadbackFormat::Nv12 || frame.len() != expected {
            log::error!(
                "NV12 frame refused for '{}': the encoder takes {} {}x{} ({expected} bytes), got {} bytes",
                self.label,
                readback_label(contract.format),
                contract.width,
                contract.height,
                frame.len()
            );
            return false;
        }
        self.feed_frame_made(frame, made)
    }

    /// Whether this encoder takes NV12 frames (`feed_nv12`).
    pub fn takes_nv12(&self) -> bool {
        self.frame_contract
            .is_some_and(|c| c.format == ReadbackFormat::Nv12)
    }

    /// Feed a frame of raw data to a byte-oriented streaming subprocess,
    /// timed by when it arrives here.
    /// Never blocks — drops the frame if the writer thread can't keep up.
    /// Returns false if the subprocess has failed (write error or process exited).
    pub fn feed_frame(&mut self, rgba: &[u8]) -> bool {
        self.feed_frame_made(rgba, std::time::Instant::now())
    }

    /// [`Self::feed_frame`] for a frame drawn at `made`: the encoder puts it
    /// where that moment falls in the take, however late it gets here.
    pub fn feed_frame_made(&mut self, rgba: &[u8], made: std::time::Instant) -> bool {
        // Check if writer thread reported an error
        if self.write_failed.load(Ordering::SeqCst) {
            self.drain_stderr();
            return false;
        }
        // Check if ffmpeg already exited (non-blocking)
        if let Some(status) = self.child.try_wait().ok().flatten() {
            if !status.success() {
                self.drain_stderr();
                log::error!(
                    "ffmpeg exited with status {} for '{}' before frame could be written",
                    status,
                    self.label
                );
            }
            return false;
        }
        if let Some(ref tx) = self.frame_tx {
            let before = self.counters.dropped.load(Ordering::Relaxed);
            let alive = try_enqueue_frame(tx, &self.counters.dropped, &self.label, made, rgba);
            let dropped = self.counters.dropped.load(Ordering::Relaxed) > before;
            if let Ok(mut clock) = self.counters.drop_clock.lock() {
                clock.offer(made, dropped);
            }
            if alive {
                true
            } else {
                self.drain_stderr();
                false
            }
        } else {
            false
        }
    }

    /// Read and log any stderr output from ffmpeg.
    /// Each line is classified individually: lines containing error indicators
    /// are logged at ERROR, everything else (version info, codec config) at DEBUG.
    fn drain_stderr(&mut self) {
        self.stderr_tail.log(&self.label);
    }

    /// ffmpeg's most telling recent line — the last error it printed, if any.
    pub fn last_error(&self) -> Option<String> {
        self.stderr_tail.last_error()
    }

    /// Stop the subprocess. For recordings (`graceful_shutdown`), the heavy
    /// work (joining threads, waiting for ffmpeg to write the moov atom) runs
    /// on a detached background thread so the caller (UI / main thread) returns
    /// immediately. For streams, kills ffmpeg inline (fast).
    /// Idempotent — safe to call multiple times.
    ///
    /// # Panics
    ///
    /// On the recording path, panics if the placeholder child process or the
    /// background finalize thread cannot be spawned.
    pub fn stop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;

        let duration = self.start_time.elapsed();

        // 1. Signal shutdown so writer threads know a broken pipe is expected
        self.shutting_down.store(true, Ordering::SeqCst);

        // 2. Drop the sender to close the channel — no more frames queued
        drop(self.frame_tx.take());

        if self.graceful_shutdown {
            // --- Recording path: finalize on a background thread ---
            // Move all owned resources out of `self` so the thread owns them.
            let mut audio = self.audio.take();
            let writer_thread = self.writer_thread.take();
            let mut child = std::mem::replace(
                &mut self.child,
                // Placeholder — never used again (stopped == true).
                Command::new("true")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .expect("failed to spawn placeholder"),
            );
            let label = self.label.clone();
            let counters = self.counters.clone();
            let stderr = self.stderr_tail.clone();

            std::thread::Builder::new()
                .name(format!("ffmpeg-finalize-{label}"))
                .spawn(move || {
                    const FINALIZE_TIMEOUT: std::time::Duration =
                        std::time::Duration::from_secs(30);

                    // 3. Both writers told to stop — the sound's closes its
                    //    socket, the video's drains its last frames and drops
                    //    stdin — so ffmpeg sees the end of both inputs; waited
                    //    for within the deadline: an encoder in Nida the media
                    //    engine stopped answering held the video writer, and
                    //    ffmpeg, no longer reading, the sound's, for ever.
                    //    Past it ffmpeg is stopped, which frees both.
                    let deadline = std::time::Instant::now() + FINALIZE_TIMEOUT;
                    if let Some(ref a) = audio {
                        a.begin_stop();
                    }
                    let finished = |audio: &Option<AudioPipe>| {
                        audio.as_ref().is_none_or(AudioPipe::is_finished)
                            && writer_thread
                                .as_ref()
                                .is_none_or(std::thread::JoinHandle::is_finished)
                    };
                    while !finished(&audio) && std::time::Instant::now() < deadline {
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                    if !finished(&audio) {
                        log::warn!("The writers for '{label}' did not finish; stopping ffmpeg");
                        let _ = child.kill();
                    }
                    if let Some(ref mut a) = audio {
                        a.stop();
                    }
                    if let Some(handle) = writer_thread.filter(std::thread::JoinHandle::is_finished)
                    {
                        let _ = handle.join();
                    }

                    // 4. Wait for ffmpeg to finalize the container (moov atom).
                    loop {
                        match child.try_wait() {
                            Ok(Some(_status)) => break,
                            Ok(None) => {
                                if std::time::Instant::now() >= deadline {
                                    log::warn!(
                                        "ffmpeg did not exit within {}s for '{}', killing",
                                        FINALIZE_TIMEOUT.as_secs(),
                                        label
                                    );
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(50));
                            }
                            Err(e) => {
                                log::error!("Failed to wait for ffmpeg '{label}': {e}");
                                break;
                            }
                        }
                    }

                    // 5. Log completion
                    stderr.log(&label);
                    let frames = counters.written.load(Ordering::Relaxed);
                    log::info!(
                        "ffmpeg finished: {} ({} frames{}, {:.1}s)",
                        label,
                        frames,
                        Self::timeline_summary(
                            counters.padded.load(Ordering::Relaxed),
                            counters.left_out.load(Ordering::Relaxed),
                            counters.skipped.load(Ordering::Relaxed),
                            counters.held.load(Ordering::Relaxed),
                        ),
                        duration.as_secs_f32()
                    );
                })
                .expect("failed to spawn ffmpeg finalize thread");
        } else {
            // --- Streaming path: kill immediately (inline, fast) ---

            // 3. Kill ffmpeg BEFORE joining the writer thread. The writer
            //    thread may be blocked on stdin.write_all() (e.g. SRT listener
            //    with a full pipe buffer). Killing the child breaks the pipe,
            //    which unblocks the write and lets the thread exit.
            let _ = self.child.kill();

            // 3b. Tear down the audio side-channel (socket + writer thread).
            //     Done after the kill so a writer blocked on a full socket sees
            //     a broken pipe.
            if let Some(audio) = self.audio.as_mut() {
                audio.stop();
            }

            // 4. Now safe to join — the writer thread will see a broken pipe
            //    or a closed channel and exit promptly. An encoder in Nida
            //    ends its session there, and the thread writing what it made
            //    ends after it — waited for only so long, off this thread:
            //    an engine that has stopped may never let it end.
            if let Some(native) = self.native.as_mut() {
                let threads: Vec<_> = self
                    .writer_thread
                    .take()
                    .into_iter()
                    .chain(native.output.take())
                    .collect();
                if !join_within(threads, NATIVE_STOP_WAIT) {
                    log::warn!(
                        "The stream's encoder in Nida did not end within {} s; left to end on its own",
                        NATIVE_STOP_WAIT.as_secs()
                    );
                }
            } else if let Some(handle) = self.writer_thread.take() {
                let _ = handle.join();
            }

            // 5. Reap the child process
            let _ = self.child.wait();

            // 6. The encoder is gone, so its destinations have seen the last
            //    of the stream: let each finish sending and hang up.
            if let Some(mut fanout) = self.fanout.take() {
                fanout.stop();
            }

            let frames = self.counters.written.load(Ordering::Relaxed);
            let padded = self.counters.padded.load(Ordering::Relaxed);
            let left_out = self.counters.left_out.load(Ordering::Relaxed);
            self.drain_stderr();
            log::info!(
                "ffmpeg finished: {} ({} frames{}, {:.1}s)",
                self.label,
                frames,
                Self::timeline_summary(
                    padded,
                    left_out,
                    self.counters.skipped.load(Ordering::Relaxed),
                    self.counters.held.load(Ordering::Relaxed),
                ),
                duration.as_secs_f32()
            );
        }
    }

    /// Completion-log fragment naming repeated and left-out frames, empty
    /// when there were none. Neither is an error — repeats are how a hitchy
    /// session still comes out in sync, and a show drawn a little faster than
    /// the encoder's rate leaves the odd frame out — but it is worth knowing.
    fn timeline_summary(padded: u64, left_out: u64, skipped: u64, held: u64) -> String {
        let repeated = if padded > 0 {
            format!(" + {padded} repeated to cover renderer gaps")
        } else {
            String::new()
        };
        let left = if left_out > 0 {
            format!(", {left_out} left out (drawn before the take, or for a slot already written)")
        } else {
            String::new()
        };
        let behind = if skipped > 0 {
            format!(", {skipped} skipped to stay live")
        } else {
            String::new()
        };
        let still = if held > 0 {
            format!(", {held} the show drew nothing for, skipped")
        } else {
            String::new()
        };
        repeated + &left + &behind + &still
    }

    /// Duration since the subprocess was started.
    pub fn duration(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }

    /// Number of frames written so far.
    pub fn frames_written(&self) -> u64 {
        self.counters.written.load(Ordering::Relaxed)
    }

    /// Number of audio PCM chunks written to the socket so far, or `None` for a
    /// video-only output (no audio passthrough).
    pub fn audio_frames_written(&self) -> Option<u64> {
        self.audio.as_ref().map(AudioPipe::frames_written)
    }

    /// Repeated frames emitted to cover gaps where the renderer missed its
    /// slot. Nonzero means the session dropped frames; the recording is still
    /// in sync, but the visible result is a brief freeze.
    pub fn frames_padded(&self) -> u64 {
        self.counters.padded.load(Ordering::Relaxed)
    }

    /// Frames offered while the writer channel was full — the take
    /// continued; those pixels never reached ffmpeg — and, for a stream
    /// encoded in Nida, frames the media engine dropped.
    pub fn frames_dropped(&self) -> u64 {
        self.counters.dropped.load(Ordering::Relaxed)
            + self.counters.skipped.load(Ordering::Relaxed)
            + self.native.as_ref().map_or(0, |n| n.shared.dropped())
    }

    /// The time those frames would have covered, in seconds (see
    /// [`DropClock`]); one the media engine dropped, its slot.
    pub fn dropped_secs(&self) -> f64 {
        let offered = self
            .counters
            .drop_clock
            .lock()
            .map_or(0.0, |clock| clock.lost.as_secs_f64());
        // The media engine's drops and the slots skipped to stay live (see
        // `take_live`), each a slot of the stream's.
        #[allow(clippy::cast_precision_loss)] // Frame counts stay far below 2^52.
        let engine = self.native.as_ref().map_or(0.0, |n| {
            (n.shared.dropped() + self.counters.skipped.load(Ordering::Relaxed)) as f64
                / f64::from(n.shared.fps)
        });
        offered + engine
    }

    /// Samples of silence spliced into the audio to replace PCM lost to
    /// backpressure. Nonzero means audio was audibly interrupted, as opposed to
    /// merely delayed. `None` for a video-only output.
    pub fn audio_silence_spliced(&self) -> Option<u64> {
        self.audio.as_ref().map(AudioPipe::silence_spliced)
    }

    /// The label (path or URL) for this subprocess.
    pub fn label(&self) -> &str {
        &self.label
    }
}

impl Drop for FfmpegSubprocess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    /// What a live stream fills before it skips (see `Timeline::place_live`).
    const LIVE_FILL: u64 = super::LIVE_BACKLOG as u64;

    use super::*;

    /// Threads that end in time are joined; one that never does is left,
    /// and the wait for it ends at the bound.
    #[test]
    fn a_stop_waits_for_an_encoder_only_so_long() {
        let quick = std::thread::spawn(|| {});
        assert!(join_within(vec![quick], std::time::Duration::from_secs(2)));
        let (_hold, never) = mpsc::channel::<()>();
        let stuck = std::thread::spawn(move || {
            let _ = never.recv();
        });
        let start = std::time::Instant::now();
        assert!(!join_within(vec![stuck], std::time::Duration::from_millis(200)));
        assert!(start.elapsed() < std::time::Duration::from_secs(1), "not held past the bound");
    }

    /// An encoder in Nida holding frames with none ending for three seconds
    /// has stopped — however few it holds — while one idle, or ending what it
    /// takes, has not; a show idle for a while is given the time again.
    #[test]
    fn an_encoder_in_nida_that_stops_ending_frames_is_stuck() {
        let shared = NativeShared::new(6000, 160, 50);
        let at = |ms: u64| shared.stuck_at(ms);
        assert!(!at(60_000), "idle: holding nothing");
        shared.going_in();
        shared.going_in();
        let start = shared.busy_since_ms.load(Ordering::Relaxed);
        assert!(!at(start + 2_000), "two held, but not yet long");
        assert!(at(start + 3_001), "two held three seconds, none ending: stuck");
        shared.came_out();
        shared.came_out();
        assert!(!at(start + 60_000), "all ended: idle again");
        // Drawing again after an idle minute: the three seconds start over.
        shared.busy_since_ms.store(start + 60_000, Ordering::Relaxed);
        shared.submitted.fetch_add(1, Ordering::Relaxed);
        assert!(!at(start + 61_000));
        shared.fail();
        assert!(at(start + 61_000), "failed is stuck at once");
    }
    use crate::engine::value::render::{PresentationDepth, PresentationRequest};
    use crate::renderer::ReadbackFormat;

    /// Check if ffmpeg is available on this system.
    fn ffmpeg_available() -> bool {
        Command::new("ffmpeg").arg("-version").output().is_ok()
    }

    // ── SRT URL mode injection tests (pure logic) ──────────────────

    /// Eight bits — every Record and Stream — never ask what the installed
    /// FFmpeg can do: asking runs ffmpeg, and the REC or STREAM that asked
    /// would wait for it. Ten bits ask.
    #[test]
    fn only_ten_bits_ask_what_ffmpeg_can_do() {
        let eight = PresentationRequest::default();
        assert_eq!(eight.depth, PresentationDepth::Sdr8);
        let _ = StreamingCapabilities::needed_for(eight, || panic!("FFmpeg asked for eight bits"));
        let mut asked = false;
        let ten = PresentationRequest {
            depth: PresentationDepth::Sdr10,
            ..eight
        };
        let found = StreamingCapabilities::needed_for(ten, || {
            asked = true;
            StreamingCapabilities {
                hevc_main10: true,
                ..StreamingCapabilities::default()
            }
        });
        assert!(asked && found.hevc_main10);
    }

    #[test]
    fn a_stream_can_fall_a_fifth_of_a_second_behind() {
        assert_eq!(stream_frame_capacity(1920, 1080, 60, false), 12);
        assert_eq!(stream_frame_capacity(1280, 720, 30, false), 6);
        assert!(
            stream_frame_capacity(3840, 2160, 60, false) < 12,
            "held in memory at 4K"
        );
        assert_eq!(
            stream_frame_capacity(3840, 2160, 60, true),
            10,
            "NV12 frames, 12 MB at 4K: most of a fifth of a second"
        );
        assert_eq!(
            stream_frame_capacity(7680, 4320, 60, false),
            FRAME_CHANNEL_CAPACITY,
            "never less than a recording's"
        );
    }

    #[test]
    fn a_chatty_ffmpeg_is_not_stalled_by_its_own_log() {
        // More than a pipe holds (64 KB), then the line that matters.
        let mut child = Command::new("sh")
            .args([
                "-c",
                "i=0; while [ $i -lt 3000 ]; do \
                 echo \"noise line $i ................................................\" >&2; \
                 i=$((i+1)); done; echo 'rtmp: Connection refused' >&2",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let tail = StderrTail::attach(&mut child);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while child.try_wait().unwrap().is_none() {
            assert!(
                std::time::Instant::now() < deadline,
                "the process blocked on a full stderr pipe"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            tail.last_error().as_deref(),
            Some("rtmp: Connection refused")
        );
        assert!(tail.lines().len() <= STDERR_TAIL_LINES);
    }

    #[test]
    fn spawn_srt_url_adds_listener_mode() {
        // Verify the URL mode injection logic without spawning
        let url = "srt://127.0.0.1:9001";
        let srt_url = if url.contains("mode=") {
            url.to_string()
        } else if url.contains('?') {
            format!("{url}&mode=listener")
        } else {
            format!("{url}?mode=listener")
        };
        assert_eq!(srt_url, "srt://127.0.0.1:9001?mode=listener");
    }

    #[test]
    fn spawn_srt_url_preserves_existing_mode() {
        let url = "srt://127.0.0.1:9001?mode=caller";
        let srt_url = if url.contains("mode=") {
            url.to_string()
        } else if url.contains('?') {
            format!("{url}&mode=listener")
        } else {
            format!("{url}?mode=listener")
        };
        assert_eq!(srt_url, "srt://127.0.0.1:9001?mode=caller");
    }

    #[test]
    fn spawn_srt_url_appends_to_existing_params() {
        let url = "srt://127.0.0.1:9001?latency=0";
        let srt_url = if url.contains("mode=") {
            url.to_string()
        } else if url.contains('?') {
            format!("{url}&mode=listener")
        } else {
            format!("{url}?mode=listener")
        };
        assert_eq!(srt_url, "srt://127.0.0.1:9001?latency=0&mode=listener");
    }

    // ── Recording codec display ────────────────────────────────────

    #[test]
    fn recording_codec_display() {
        use crate::renderer::context::SrtCodec;

        assert_eq!(format!("{}", RecordingCodec::H264), "H.264");
        assert_eq!(format!("{}", RecordingCodec::H265), "H.265 (HEVC)");
        assert_eq!(format!("{}", RecordingCodec::AV1), "AV1");
        assert_eq!(format!("{}", RecordingCodec::ProRes), "ProRes 422");
        assert_eq!(format!("{}", RecordingCodec::ProRes4444), "ProRes 4444");
        assert_eq!(format!("{}", RecordingCodec::Hap), "HAP");
        assert_eq!(format!("{}", RecordingCodec::HapAlpha), "HAP Alpha");
        assert_eq!(format!("{}", RecordingCodec::HapQ), "HAP Q");

        // SrtCodec display
        assert_eq!(format!("{}", SrtCodec::H264), "H.264");
        assert_eq!(format!("{}", SrtCodec::H265), "H.265 (HEVC)");
    }

    fn ten_bit_request() -> PresentationRequest {
        PresentationRequest {
            depth: PresentationDepth::Sdr10,
            dither: true,
            video_levels: false,
        }
    }

    #[test]
    fn hap_family_ten_bit_request_falls_back_by_product_contract() {
        for codec in [
            RecordingCodec::Hap,
            RecordingCodec::HapAlpha,
            RecordingCodec::HapQ,
        ] {
            let plan = RecordingPlan::resolve(
                &codec,
                ten_bit_request(),
                ReadbackFormat::Rgb10A2,
                Some("Supported pixel formats: rgba"),
            );
            assert_eq!(plan.resolved.requested, PresentationDepth::Sdr10, "{codec}");
            assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8, "{codec}");
            assert_eq!(plan.input_pixel_format, "rgba", "{codec}");
            assert!(
                plan.resolved
                    .fallback_reason
                    .as_deref()
                    .unwrap()
                    .contains("HAP"),
                "{codec}: {:?}",
                plan.resolved.fallback_reason
            );
        }
    }

    #[test]
    fn h264_ten_bit_request_falls_back_by_product_contract() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::H264,
            ten_bit_request(),
            ReadbackFormat::Rgb10A2,
            Some("Supported pixel formats: yuv420p yuv420p10le"),
        );

        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(plan.input_pixel_format, "rgba");
        assert_eq!(plan.output_pixel_format, "yuv420p");
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("H.264"));
    }

    #[test]
    fn hevc_main10_uses_packed_rgb10_and_explicit_rec709_metadata() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::H265,
            ten_bit_request(),
            ReadbackFormat::Rgb10A2,
            Some("Supported pixel formats: yuv420p yuv420p10le yuv422p10le"),
        );

        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr10);
        assert_eq!(plan.input_pixel_format, "x2bgr10le");
        assert_eq!(plan.output_pixel_format, "yuv420p10le");
        assert!(has_pair(&plan.video_args, "-profile:v", "main10"));
        assert!(has_pair(&plan.video_args, "-color_primaries", "bt709"));
        assert!(has_pair(&plan.video_args, "-color_trc", "bt709"));
        assert!(has_pair(&plan.video_args, "-colorspace", "bt709"));
        assert!(has_pair(&plan.video_args, "-color_range", "tv"));
    }

    #[test]
    fn ten_bit_codec_falls_back_when_renderer_only_supplies_rgba8() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::ProRes,
            ten_bit_request(),
            ReadbackFormat::Rgba8,
            Some("Supported pixel formats: yuv422p10le"),
        );

        assert_eq!(plan.resolved.requested, PresentationDepth::Sdr10);
        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(plan.input_pixel_format, "rgba");
        assert_eq!(
            plan.resolved.fallback_reason.as_deref(),
            Some(
                "10-bit SDR recording requires packed RGB10 readback, but the active renderer \
                 supplies RGBA8"
            )
        );
    }

    #[test]
    fn ten_bit_codec_falls_back_when_encoder_lacks_required_pixel_format() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::AV1,
            ten_bit_request(),
            ReadbackFormat::Rgb10A2,
            Some("Supported pixel formats: yuv420p"),
        );

        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("does not support yuv420p10le"));
    }

    #[test]
    fn every_recording_codec_pins_its_eight_bit_contract() {
        let cases = [
            (RecordingCodec::H264, "yuv420p"),
            (RecordingCodec::H265, "yuv420p"),
            (RecordingCodec::AV1, "yuv420p"),
            (RecordingCodec::ProRes, "yuv422p10le"),
            (RecordingCodec::ProRes4444, "yuva444p10le"),
            (RecordingCodec::Hap, "rgba"),
            (RecordingCodec::HapAlpha, "rgba"),
            (RecordingCodec::HapQ, "rgba"),
        ];
        for (codec, expected_output) in cases {
            let plan = RecordingPlan::resolve(
                &codec,
                PresentationRequest::default(),
                ReadbackFormat::Rgba8,
                Some(&format!("Supported pixel formats: {expected_output}")),
            );
            assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8, "{codec}");
            assert_eq!(plan.input_pixel_format, "rgba", "{codec}");
            assert_eq!(plan.output_pixel_format, expected_output, "{codec}");
            assert!(has_pair(&plan.video_args, "-pix_fmt", expected_output));
            assert!(has_pair(&plan.video_args, "-color_primaries", "bt709"));
            assert!(has_pair(&plan.video_args, "-color_trc", "bt709"));
            assert!(has_pair(&plan.video_args, "-colorspace", "bt709"));
            assert!(has_pair(&plan.video_args, "-color_range", "tv"));
        }
    }

    #[test]
    fn supported_opaque_ten_bit_codecs_pin_profiles_and_formats() {
        let cases = [
            (RecordingCodec::H265, "yuv420p10le", "main10"),
            (RecordingCodec::AV1, "yuv420p10le", "0"),
            (RecordingCodec::ProRes, "yuv422p10le", "2"),
        ];
        for (codec, output, profile) in cases {
            let plan = RecordingPlan::resolve(
                &codec,
                ten_bit_request(),
                ReadbackFormat::Rgb10A2,
                Some(&format!("Supported pixel formats: {output}")),
            );
            assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr10, "{codec}");
            assert_eq!(plan.input_pixel_format, "x2bgr10le", "{codec}");
            assert_eq!(plan.output_pixel_format, output, "{codec}");
            assert!(has_pair(&plan.video_args, "-profile:v", profile));
        }
    }

    #[test]
    fn missing_encoder_probe_never_claims_ten_bit() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::H265,
            ten_bit_request(),
            ReadbackFormat::Rgb10A2,
            None,
        );
        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("unavailable"));
    }

    #[test]
    fn prores_4444_uses_integer_rgba16_and_explicit_alpha_contract() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::ProRes4444,
            ten_bit_request(),
            ReadbackFormat::Rgba16Unorm,
            Some("Supported pixel formats: yuva444p10le"),
        );

        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr10);
        assert_eq!(plan.resolved.pixel_format, PresentationPixelFormat::Rgba16);
        assert_eq!(plan.input_pixel_format, "rgba64le");
        assert_eq!(plan.output_pixel_format, "yuva444p10le");
        assert_eq!(plan.expected_readback, ReadbackFormat::Rgba16Unorm);
        assert!(plan.unpremultiply);
        assert!(has_pair(&plan.video_args, "-profile:v", "4"));
        assert!(has_pair(&plan.video_args, "-pix_fmt", "yuva444p10le"));
        assert!(has_pair(&plan.video_args, "-alpha_bits", "16"));
        assert_eq!(
            unpremultiply_filter(plan.input_pixel_format),
            "setparams=alpha_mode=premultiplied,format=gbrap16le,unpremultiply=inplace=1"
        );
    }

    #[test]
    fn prores_4444_rejects_half_float_bytes_as_rgba64le() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::ProRes4444,
            ten_bit_request(),
            ReadbackFormat::Rgba16Float,
            Some("Supported pixel formats: yuva444p10le"),
        );

        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("integer RGBA16"));
    }

    #[test]
    fn prores_4444_falls_back_when_gpu_lacks_rgba16_unorm() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::ProRes4444,
            ten_bit_request(),
            ReadbackFormat::Rgba8,
            Some("Supported pixel formats: yuva444p10le"),
        );

        assert_eq!(plan.resolved.requested, PresentationDepth::Sdr10);
        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("GPU does not support"));
    }

    #[test]
    fn prores_4444_falls_back_when_encoder_lacks_yuva444p10le() {
        let plan = RecordingPlan::resolve(
            &RecordingCodec::ProRes4444,
            ten_bit_request(),
            ReadbackFormat::Rgba16Unorm,
            Some("Supported pixel formats: yuv422p10le"),
        );

        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("does not support yuva444p10le"));
    }

    #[test]
    fn prores_4444_preserves_known_rgb_and_alpha_ramp() {
        const WIDTH: u32 = 16;
        const HEIGHT: u32 = 16;

        let Some(help) = probe_encoder_help("prores_ks") else {
            eprintln!("Skipping test: prores_ks encoder unavailable");
            return;
        };
        if !encoder_supports_pixel_format(&help, "yuva444p10le") {
            eprintln!("Skipping test: prores_ks lacks yuva444p10le");
            return;
        }

        let mut input = Vec::with_capacity((WIDTH * HEIGHT * 8) as usize);
        for _y in 0..HEIGHT {
            for x in 0..WIDTH {
                let alpha = x as f32 / (WIDTH - 1) as f32;
                for straight in [0.75_f32, 0.5, 0.25] {
                    input.extend_from_slice(
                        &((straight * alpha * f32::from(u16::MAX)).round() as u16).to_le_bytes(),
                    );
                }
                input.extend_from_slice(
                    &((alpha * f32::from(u16::MAX)).round() as u16).to_le_bytes(),
                );
            }
        }

        let path = std::env::temp_dir().join(format!(
            "varda-prores-alpha-{}-{}.mov",
            std::process::id(),
            crate::deck::generate_short_uuid()
        ));
        let mut encoder = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "rawvideo"])
            .args(["-pix_fmt", "rgba64le", "-s", "16x16", "-i", "-"])
            .args(["-vf", unpremultiply_filter("rgba64le")])
            .args(["-frames:v", "1", "-c:v", "prores_ks"])
            .args(["-profile:v", "4", "-alpha_bits", "16"])
            .args(["-pix_fmt", "yuva444p10le"])
            .arg(&path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn prores alpha encode");
        encoder
            .stdin
            .take()
            .expect("piped stdin")
            .write_all(&input)
            .expect("write RGBA16 frame");
        let encode_output = encoder.wait_with_output().expect("finish prores encode");
        assert!(
            encode_output.status.success(),
            "{}",
            String::from_utf8_lossy(&encode_output.stderr)
        );

        let decoded = Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&path)
            .args([
                "-frames:v",
                "1",
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba64le",
                "-",
            ])
            .output()
            .expect("decode prores alpha frame");
        let _ = std::fs::remove_file(path);
        assert!(
            decoded.status.success(),
            "{}",
            String::from_utf8_lossy(&decoded.stderr)
        );
        let words: Vec<u16> = decoded
            .stdout
            .as_chunks::<2>()
            .0
            .iter()
            .map(|bytes| u16::from_le_bytes(*bytes))
            .collect();
        assert_eq!(words.len(), (WIDTH * HEIGHT * 4) as usize);

        let samples: Vec<[u16; 4]> = [5_usize, 10, 15]
            .into_iter()
            .map(|x| {
                let offset = x * 4;
                words[offset..offset + 4].try_into().unwrap()
            })
            .collect();
        for [red, green, blue, _alpha] in &samples {
            assert!(red.abs_diff(49_151) < 256, "red code {red}");
            assert!(green.abs_diff(32_768) < 256, "green code {green}");
            assert!(blue.abs_diff(16_384) < 256, "blue code {blue}");
        }
        assert!(samples[0][3] < samples[1][3] && samples[1][3] < samples[2][3]);
        assert!(samples[2][3].abs_diff(u16::MAX) < 32);
    }

    #[test]
    fn wgpu_rgb10a2_channel_order_maps_to_ffmpeg_x2bgr10le() {
        // wgpu packs a red-only texel as 0x0000_03ff. FFmpeg's
        // x2bgr10le layout assigns those low ten bits to R; x2rgb10le would
        // decode the same word as blue.
        let red_only_wgpu_word = 0x0000_03ff_u32;
        assert_eq!(red_only_wgpu_word & 0x3ff, 1023);
        assert_eq!(packed_rgb10_input_format(), "x2bgr10le");
    }

    #[test]
    fn recording_preflight_selects_renderer_storage_before_spawn() {
        assert_eq!(
            preferred_recording_readback(&RecordingCodec::H265, ten_bit_request(), true),
            ReadbackFormat::Rgb10A2
        );
        assert_eq!(
            preferred_recording_readback(
                &RecordingCodec::H264,
                PresentationRequest::default(),
                true
            ),
            ReadbackFormat::Rgba8
        );
        assert_eq!(
            preferred_recording_readback(&RecordingCodec::ProRes4444, ten_bit_request(), true),
            ReadbackFormat::Rgba16Unorm
        );
        assert_eq!(
            preferred_recording_readback(&RecordingCodec::ProRes4444, ten_bit_request(), false),
            ReadbackFormat::Rgba8
        );
    }

    fn all_streaming_capabilities() -> StreamingCapabilities {
        StreamingCapabilities {
            hevc_main10: true,
            av1_10: true,
            enhanced_hevc: true,
            enhanced_av1: true,
        }
    }

    #[test]
    fn srt_hevc_main10_plan_pins_mpegts_and_rec709() {
        let plan = StreamingPlan::resolve(
            StreamingProtocol::Srt,
            StreamingCodec::H265,
            ten_bit_request(),
            all_streaming_capabilities(),
        );
        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr10);
        assert_eq!(plan.input_pixel_format, "x2bgr10le");
        assert_eq!(plan.expected_readback, ReadbackFormat::Rgb10A2);
        assert!(has_pair(&plan.video_args, "-profile:v", "main10"));
        assert!(has_pair(&plan.video_args, "-pix_fmt", "yuv420p10le"));
        assert!(has_pair(&plan.video_args, "-color_primaries", "bt709"));
        assert!(has_pair(&plan.video_args, "-color_trc", "bt709"));
        assert!(has_pair(&plan.video_args, "-colorspace", "bt709"));
        assert!(has_pair(&plan.video_args, "-color_range", "tv"));
        assert!(has_pair(&plan.muxer_args, "-f", "mpegts"));
    }

    #[test]
    fn hls_and_dash_ten_bit_plans_pin_isobmff_codec_tags() {
        let hls = StreamingPlan::resolve(
            StreamingProtocol::Hls,
            StreamingCodec::H265,
            ten_bit_request(),
            all_streaming_capabilities(),
        );
        assert_eq!(hls.resolved.resolved, PresentationDepth::Sdr10);
        assert!(has_pair(&hls.muxer_args, "-tag:v", "hvc1"));
        assert!(has_pair(&hls.muxer_args, "-f", "hls"));
        let hls_segments = hls_segment_args("stream", false, true);
        assert!(has_pair(&hls_segments, "-hls_segment_type", "fmp4"));
        assert!(has_pair(
            &hls_segments,
            "-hls_segment_filename",
            "stream/seg_%05d.m4s"
        ));

        let dash = StreamingPlan::resolve(
            StreamingProtocol::Dash,
            StreamingCodec::AV1,
            ten_bit_request(),
            all_streaming_capabilities(),
        );
        assert_eq!(dash.resolved.resolved, PresentationDepth::Sdr10);
        assert!(has_pair(&dash.muxer_args, "-tag:v", "av01"));
        assert!(has_pair(&dash.muxer_args, "-f", "dash"));
    }

    #[test]
    fn legacy_rtmp_forces_truthful_h264_eight_bit_fallback() {
        let plan = StreamingPlan::resolve(
            StreamingProtocol::Rtmp(RtmpCodecContract::Legacy),
            StreamingCodec::H265,
            ten_bit_request(),
            all_streaming_capabilities(),
        );
        assert_eq!(plan.resolved.requested, PresentationDepth::Sdr10);
        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert_eq!(plan.expected_readback, ReadbackFormat::Rgba8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("legacy RTMP"));
        assert!(has_pair(&plan.video_args, "-c:v", "libx264"));
        assert!(has_pair(&plan.video_args, "-pix_fmt", "yuv420p"));
        assert!(has_pair(&plan.muxer_args, "-f", "flv"));
    }

    #[test]
    fn enhanced_rtmp_requires_ffmpeg_muxer_capability() {
        let missing = StreamingCapabilities {
            hevc_main10: true,
            enhanced_hevc: false,
            ..all_streaming_capabilities()
        };
        let fallback = StreamingPlan::resolve(
            StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced),
            StreamingCodec::H265,
            ten_bit_request(),
            missing,
        );
        assert_eq!(fallback.resolved.resolved, PresentationDepth::Sdr8);
        assert!(fallback
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("Enhanced RTMP"));

        let supported = StreamingPlan::resolve(
            StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced),
            StreamingCodec::AV1,
            ten_bit_request(),
            all_streaming_capabilities(),
        );
        assert_eq!(supported.resolved.resolved, PresentationDepth::Sdr10);
        assert!(has_pair(&supported.video_args, "-c:v", "libsvtav1"));
        assert!(has_pair(
            &supported.muxer_args,
            "-flvflags",
            "no_sequence_end+no_metadata"
        ));
    }

    #[test]
    fn ten_bit_stream_falls_back_when_encoder_lacks_profile() {
        let plan = StreamingPlan::resolve(
            StreamingProtocol::Dash,
            StreamingCodec::AV1,
            ten_bit_request(),
            StreamingCapabilities::default(),
        );
        assert_eq!(plan.resolved.resolved, PresentationDepth::Sdr8);
        assert!(plan
            .resolved
            .fallback_reason
            .as_deref()
            .unwrap()
            .contains("AV1 10-bit"));
    }

    #[test]
    fn streaming_resolver_covers_protocol_codec_contract_matrix() {
        let cases = [
            (
                StreamingProtocol::Srt,
                StreamingCodec::H264,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Srt,
                StreamingCodec::H265,
                PresentationDepth::Sdr10,
            ),
            (
                StreamingProtocol::Hls,
                StreamingCodec::H264,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Hls,
                StreamingCodec::H265,
                PresentationDepth::Sdr10,
            ),
            (
                StreamingProtocol::Hls,
                StreamingCodec::AV1,
                PresentationDepth::Sdr10,
            ),
            (
                StreamingProtocol::Dash,
                StreamingCodec::H264,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Dash,
                StreamingCodec::H265,
                PresentationDepth::Sdr10,
            ),
            (
                StreamingProtocol::Dash,
                StreamingCodec::AV1,
                PresentationDepth::Sdr10,
            ),
            (
                StreamingProtocol::Rtmp(RtmpCodecContract::Legacy),
                StreamingCodec::H264,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Rtmp(RtmpCodecContract::Legacy),
                StreamingCodec::H265,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Rtmp(RtmpCodecContract::Legacy),
                StreamingCodec::AV1,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced),
                StreamingCodec::H264,
                PresentationDepth::Sdr8,
            ),
            (
                StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced),
                StreamingCodec::H265,
                PresentationDepth::Sdr10,
            ),
            (
                StreamingProtocol::Rtmp(RtmpCodecContract::Enhanced),
                StreamingCodec::AV1,
                PresentationDepth::Sdr10,
            ),
        ];
        for (protocol, codec, expected) in cases {
            let plan = StreamingPlan::resolve(
                protocol,
                codec.clone(),
                ten_bit_request(),
                all_streaming_capabilities(),
            );
            assert_eq!(
                plan.resolved.resolved, expected,
                "{protocol:?} {codec:?} resolved incorrectly"
            );
        }
    }

    // ── Subprocess lifecycle (requires ffmpeg) ─────────────────────

    #[test]
    fn spawn_recording_h264_and_feed_frames() {
        if !ffmpeg_available() {
            eprintln!("Skipping test: ffmpeg not available");
            return;
        }
        let dir = std::env::temp_dir();
        let path = dir.join("varda_test_recording.mp4");
        let path_str = path.to_str().unwrap();

        let mut sub =
            FfmpegSubprocess::spawn_recording(path_str, &RecordingCodec::H264, 64, 64, 30, None)
                .expect("failed to spawn recording");

        assert_eq!(sub.label(), path_str);
        assert_eq!(sub.frames_written(), 0);

        // Feed a few frames
        let frame = vec![0u8; 64 * 64 * 4]; // black RGBA
        for _ in 0..5 {
            let ok = sub.feed_frame(&frame);
            assert!(ok, "feed_frame should succeed");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Stop and verify
        sub.stop();
        assert!(sub.duration().as_millis() > 0);

        // Cleanup
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn spawn_recording_stop_is_idempotent() {
        if !ffmpeg_available() {
            eprintln!("Skipping test: ffmpeg not available");
            return;
        }
        let dir = std::env::temp_dir();
        let path = dir.join("varda_test_idempotent.mp4");
        let path_str = path.to_str().unwrap();

        let mut sub =
            FfmpegSubprocess::spawn_recording(path_str, &RecordingCodec::H264, 64, 64, 30, None)
                .unwrap();
        sub.stop();
        sub.stop();

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn spawn_srt_and_stop() {
        if !ffmpeg_available() {
            eprintln!("Skipping test: ffmpeg not available");
            return;
        }
        // Use a high port unlikely to conflict
        let url = "srt://127.0.0.1:19876";
        let mut sub = FfmpegSubprocess::spawn_srt(
            url,
            &crate::renderer::context::SrtCodec::H264,
            PresentationRequest::default(),
            64,
            64,
            30,
            None,
        )
        .expect("failed to spawn SRT");

        assert_eq!(sub.label(), url);
        assert_eq!(sub.frames_written(), 0);

        // Feed a frame (won't block because of background writer thread)
        let frame = vec![128u8; 64 * 64 * 4];
        let _ = sub.feed_frame(&frame);

        // Stop cleanly
        sub.stop();
    }

    /// Each dropped frame counts its own slot, at the pace the show runs
    /// then: three at 60 fps are 50 ms, thirty at 30 fps a second; the first
    /// offer has nothing before it, and a stall before a drop counts at most
    /// a quarter second.
    #[test]
    fn a_dropped_frame_counts_the_stretch_since_the_one_before() {
        use std::time::{Duration, Instant};
        let mut clock = DropClock::default();
        let start = Instant::now();
        clock.offer(start, true);
        assert_eq!(clock.lost, Duration::ZERO, "nothing before the first");
        let mut at = start;
        let mut offer = |clock: &mut DropClock, gap: Duration, dropped: bool| {
            at += gap;
            clock.offer(at, dropped);
        };
        let sixtieth = Duration::from_nanos(16_666_667);
        for _ in 0..3 {
            offer(&mut clock, sixtieth, true);
        }
        assert!((clock.lost.as_secs_f64() - 0.05).abs() < 1e-6, "{:?}", clock.lost);
        offer(&mut clock, sixtieth, false);
        let thirtieth = Duration::from_nanos(33_333_333);
        for _ in 0..30 {
            offer(&mut clock, thirtieth, true);
        }
        assert!((clock.lost.as_secs_f64() - 1.05).abs() < 1e-6, "{:?}", clock.lost);
        offer(&mut clock, Duration::from_secs(2), true);
        assert!((clock.lost.as_secs_f64() - 1.3).abs() < 1e-6, "a stall: {:?}", clock.lost);
    }

    #[test]
    fn feed_frame_returns_false_after_stop() {
        if !ffmpeg_available() {
            eprintln!("Skipping test: ffmpeg not available");
            return;
        }
        let dir = std::env::temp_dir();
        let path = dir.join("varda_test_after_stop.mp4");
        let path_str = path.to_str().unwrap();

        let mut sub =
            FfmpegSubprocess::spawn_recording(path_str, &RecordingCodec::H264, 64, 64, 30, None)
                .unwrap();

        sub.stop();

        // After stop, feed_frame should return false (channel closed)
        let frame = vec![0u8; 64 * 64 * 4];
        assert!(!sub.feed_frame(&frame));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn recording_prores_codec() {
        if !ffmpeg_available() {
            eprintln!("Skipping test: ffmpeg not available");
            return;
        }
        let dir = std::env::temp_dir();
        let path = dir.join("varda_test_prores.mov");
        let path_str = path.to_str().unwrap();

        let mut sub =
            FfmpegSubprocess::spawn_recording(path_str, &RecordingCodec::ProRes, 64, 64, 30, None)
                .expect("failed to spawn ProRes recording");

        let frame = vec![0u8; 64 * 64 * 4];
        for _ in 0..3 {
            sub.feed_frame(&frame);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        sub.stop();

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn frame_channel_capacity_is_bounded() {
        // Verify the channel capacity constant
        assert_eq!(FRAME_CHANNEL_CAPACITY, 2);
    }

    #[test]
    fn full_channel_counts_as_a_drop_and_keeps_the_pipe_alive() {
        let (tx, _rx) = mpsc::sync_channel(1);
        let now = std::time::Instant::now();
        tx.send((now, vec![0u8; 4])).unwrap();
        let dropped = AtomicU64::new(0);
        assert!(
            try_enqueue_frame(&tx, &dropped, "test-rec", now, &[1, 2, 3, 4]),
            "Full must not look like a dead subprocess"
        );
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
        assert!(try_enqueue_frame(&tx, &dropped, "test-rec", now, &[5, 6, 7, 8]));
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn disconnected_channel_is_a_hard_failure() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let dropped = AtomicU64::new(0);
        let now = std::time::Instant::now();
        assert!(!try_enqueue_frame(&tx, &dropped, "test-rec", now, &[1, 2, 3, 4]));
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    // ── Phase 19b: audio passthrough arg construction ──────────────

    fn dummy_audio(sample_rate: u32, channels: u16) -> AudioInput {
        let (_tx, rx) = crossbeam_channel::bounded::<PcmChunk>(4);
        AudioInput {
            rx,
            sample_rate,
            channels,
            lost_samples: Arc::new(AtomicU64::new(0)),
            started: std::time::Instant::now(),
        }
    }

    fn has_pair(args: &[String], flag: &str, value: &str) -> bool {
        args.windows(2).any(|w| w[0] == flag && w[1] == value)
    }

    #[test]
    fn prepare_audio_none_is_video_only() {
        // No audio input → no socket, no args: the video-only path is unchanged.
        let prepared = prepare_audio(None, false).unwrap();
        assert!(prepared.is_none());
    }

    #[test]
    fn prepare_audio_recording_uses_native_rate() {
        let p = prepare_audio(Some(dummy_audio(44100, 2)), false)
            .unwrap()
            .expect("audio prepared");
        // Input: raw f32le at the device's native rate + channel count.
        assert!(has_pair(&p.in_args, "-f", "f32le"));
        assert!(has_pair(&p.in_args, "-ar", "44100"));
        assert!(has_pair(&p.in_args, "-ac", "2"));
        assert!(p.in_args.contains(&"-i".to_string()));
        // Output: AAC, stereo downmix, async resample, explicit mapping.
        assert!(has_pair(&p.out_args, "-c:a", "aac"));
        assert!(has_pair(&p.out_args, "-ac", "2"));
        assert!(has_pair(&p.out_args, "-map", "0:v:0"));
        assert!(has_pair(&p.out_args, "-map", "1:a:0"));
        // Recording must NOT force 48k — native rate is preserved (Decision 5).
        assert!(!has_pair(&p.out_args, "-ar", "48000"));
    }

    /// Audio must be timed by its own sample count, never by when it turned up.
    ///
    /// This is the whole of the recording's timebase. A raw f32le input with no
    /// timestamp option gets PTS from the sample count, which is exactly the
    /// device's clock; adding `-use_wallclock_as_timestamps` replaces that with
    /// the moment each buffer reached the socket. The two look identical while
    /// everything is keeping up and diverge the instant anything stalls —
    /// notably when ffmpeg stops draining the audio socket because it is
    /// waiting on the video pipe. Every such stall lands in the file as a hole
    /// in the audio timeline, which is what "the music goes off-beat and drops
    /// bits when the renderer hitches" sounded like.
    ///
    /// Checked on both policies because all five spawners share `prepare_audio`
    /// and the streaming ones would otherwise regress unnoticed.
    #[test]
    fn audio_is_timed_by_its_sample_clock_not_by_arrival() {
        for is_stream in [false, true] {
            let p = prepare_audio(Some(dummy_audio(48000, 2)), is_stream)
                .unwrap()
                .expect("audio prepared");
            assert!(
                !p.in_args
                    .contains(&"-use_wallclock_as_timestamps".to_string()),
                "audio timestamps must come from the sample count, not arrival time \
                 (is_stream={is_stream}); see /spec/av-sync.md"
            );
        }
    }

    #[test]
    fn prepare_audio_stream_forces_48k() {
        let p = prepare_audio(Some(dummy_audio(44100, 2)), true)
            .unwrap()
            .expect("audio prepared");
        // Stream targets normalize to 48k (Decision 5).
        assert!(has_pair(&p.out_args, "-ar", "48000"));
    }

    #[test]
    fn prepare_audio_binds_tcp_endpoint() {
        let p = prepare_audio(Some(dummy_audio(48000, 1)), false)
            .unwrap()
            .expect("audio prepared");
        // The second input is the loopback TCP URL of the bound listener.
        let port = p.listener.local_addr().expect("listener addr").port();
        let expected = format!("tcp://127.0.0.1:{port}");
        assert!(
            p.in_args.contains(&expected),
            "audio input should be the bound loopback TCP URL"
        );
        // Mono device still reported faithfully on the input side.
        assert!(has_pair(&p.in_args, "-ac", "1"));
    }

    // ── A/V sync: holding the video timeline against the audio clock ────

    /// The show frames written by `timeline` for frames drawn at
    /// `times` (seconds from `start`), slot by slot: a repeat writes the
    /// frame before again.
    fn written_frames(timeline: &mut Timeline, start: std::time::Instant, times: &[f64]) -> Vec<usize> {
        let mut slots = Vec::new();
        for (i, secs) in times.iter().enumerate() {
            if let Placed::After(repeats) = timeline.place(start + std::time::Duration::from_secs_f64(*secs)) {
                let before = slots.last().copied().unwrap_or(i);
                slots.extend(std::iter::repeat_n(before, repeats as usize));
                slots.push(i);
            }
        }
        slots
    }

    /// A little late, as refresh calls come: 0–5 ms, scattered.
    fn late_by(i: usize) -> f64 {
        ((i * 7919) % 11) as f64 * 0.0005
    }

    /// A 60 fps show recorded at 30 takes every second frame, exactly —
    /// with its frames falling right on the slots' edges (the take started
    /// as a frame was drawn) and each call a little late: placed by the
    /// moment drawn, the jitter took edge frames in and out, steps of one
    /// and three frames (the user's 60 show recorded at 30 "stutters a
    /// lot").
    #[test]
    fn a_60_show_recorded_at_30_takes_every_second_frame() {
        let start = std::time::Instant::now();
        let times: Vec<f64> = (0..600).map(|i| i as f64 / 60.0 + late_by(i)).collect();
        let mut timeline = Timeline::new(Some(start), 30.0).with_show(60);
        let slots = written_frames(&mut timeline, start, &times);
        let steps: std::collections::BTreeSet<usize> = slots.windows(2).map(|w| w[1] - w[0]).collect();
        assert_eq!(steps, [2].into(), "every second frame: {:?}", &slots[..20]);
        assert_eq!(slots.len(), 300);
        // A take started between frames, too.
        let mut timeline = Timeline::new(Some(start), 30.0).with_show(60);
        let later: Vec<f64> = times.iter().map(|t| t + 0.007).collect();
        let slots = written_frames(&mut timeline, start, &later);
        assert!(slots.windows(2).skip(1).all(|w| w[1] - w[0] == 2), "{:?}", &slots[..20]);
    }

    /// A 30 fps show recorded at 60 shows each frame twice, evenly.
    #[test]
    fn a_30_show_recorded_at_60_shows_each_frame_twice() {
        let start = std::time::Instant::now();
        let times: Vec<f64> = (0..300).map(|i| i as f64 / 30.0 + late_by(i)).collect();
        let mut timeline = Timeline::new(Some(start), 60.0).with_show(30);
        let slots = written_frames(&mut timeline, start, &times);
        // (The last frame's second slot is written with the next frame.)
        for (i, pair) in slots.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(*pair, [i, i], "frame {i} twice: {:?}", &slots[..20]);
        }
        assert_eq!(slots.len(), 599);
    }

    /// A show a little slower than its rate (59.94 for 60), recorded at 30:
    /// a frame repeated now and then to keep up with the sound — the take
    /// within a frame of its time throughout, and the steps otherwise two.
    #[test]
    fn a_show_a_little_slow_recorded_at_half_keeps_its_time() {
        let start = std::time::Instant::now();
        let times: Vec<f64> = (0..36_000).map(|i| i as f64 / 59.94 + late_by(i)).collect();
        let mut timeline = Timeline::new(Some(start), 30.0).with_show(60);
        let slots = written_frames(&mut timeline, start, &times);
        let end = times.last().copied().unwrap_or_default() * 30.0;
        assert!((slots.len() as f64 - end).abs() < 2.0, "{} slots for {end:.1}", slots.len());
        let odd = slots.windows(2).filter(|w| w[1] - w[0] != 2).count();
        // 600 s at 0.06 frames a second short: about 36 frames' worth, each
        // a repeat (a step of nought) or a step of one.
        assert!(odd < 80, "{odd} uneven steps");
    }

    /// A refresh call 10 ms late does not make its frame look like the next
    /// one: frames are counted by the engine's numbers (Codex: frames 0, 2,
    /// 3, 6 by the time between them).
    #[test]
    fn a_late_call_does_not_take_the_next_frames_place() {
        let start = std::time::Instant::now();
        let at: Vec<std::time::Instant> = (0..60)
            .map(|i| {
                let late = if i == 3 { 0.010 } else { 0.0 };
                start + std::time::Duration::from_secs_f64(f64::from(i) / 60.0 + late)
            })
            .collect();
        for made in &at {
            note_frame_drawn(*made, 60);
        }
        let mut timeline = Timeline::new(Some(start), 30.0).with_show(60);
        let mut slots = Vec::new();
        for (i, made) in at.iter().enumerate() {
            if let Placed::After(repeats) = timeline.place(*made) {
                slots.extend(std::iter::repeat_n(slots.last().copied().unwrap_or(i), repeats as usize));
                slots.push(i);
            }
        }
        assert!(slots.windows(2).all(|w| w[1] - w[0] == 2), "{slots:?}");
    }

    /// A live stream whose first frame comes late, the show at a whole
    /// share of its rate: its frames skip on and keep going out (Codex: a
    /// 24 fps show streamed at 120 left every frame out).
    #[test]
    fn a_live_stream_on_the_shows_frames_keeps_going_after_a_late_start() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::new(Some(start), 120.0).with_show(24);
        let mut out = 0;
        for i in 0..240 {
            let made = start + std::time::Duration::from_secs_f64(0.010 + f64::from(i) / 24.0);
            if let (_, Some(Placed::After(_))) = timeline.place_live(made, LIVE_FILL) {
                out += 1;
            }
        }
        assert!(out >= 238, "{out} of 240 went out");
    }

    /// The show's rate changed mid-take — 60, 30, then 60 again, each frame
    /// noted at the rate it came — recorded at 30: each slot a frame a
    /// slot's time on from the last, but for a slot or two at each change
    /// (Max5: a second and a half of double speed then a freeze at each).
    #[test]
    fn a_take_follows_the_shows_rate_as_it_changes() {
        let start = std::time::Instant::now();
        let mut drawn: Vec<(f64, u32)> = Vec::new();
        let mut t = 0.0;
        for (secs, fps) in [(2.0, 60_u32), (2.0, 30), (2.0, 60)] {
            let until = t + secs;
            while t < until - 1e-9 {
                drawn.push((t + late_by(drawn.len()) * 0.5, fps));
                t += 1.0 / f64::from(fps);
            }
        }
        let at = |secs: f64| start + std::time::Duration::from_secs_f64(secs);
        for (secs, fps) in &drawn {
            note_frame_drawn(at(*secs), *fps);
        }
        let mut timeline = Timeline::new(Some(start), 30.0).with_show(60);
        timeline.follows = Some(60);
        let mut shown: Vec<f64> = Vec::new();
        for (secs, _) in &drawn {
            if let Placed::After(repeats) = timeline.place(at(*secs)) {
                let before = shown.last().copied().unwrap_or(*secs);
                shown.extend(std::iter::repeat_n(before, repeats as usize));
                shown.push(*secs);
            }
        }
        let uneven: Vec<(usize, f64)> = shown
            .windows(2)
            .map(|w| w[1] - w[0])
            .enumerate()
            .filter(|(_, step)| (step - 1.0 / 30.0).abs() > 0.006)
            .collect();
        assert!(uneven.len() <= 4, "{} uneven slots: {uneven:?}", uneven.len());
        // And the take keeps its time: as many slots as seconds at 30.
        assert!((shown.len() as f64 - 6.0 * 30.0).abs() <= 2.0, "{} slots", shown.len());
    }

    /// Rates that only nearly divide are not taken as a whole share.
    #[test]
    fn only_rates_that_divide_exactly_are_on_the_shows_frames() {
        assert!(Grid::of(30, 60).is_some());
        assert!(Grid::of(60, 30).is_some());
        assert!(Grid::of(25, 50).is_some());
        assert!(Grid::of(120, 239).is_none());
        assert!(Grid::of(60, 60).is_none(), "the same rate: as before");
        assert!(Grid::of(50, 60).is_none());
        assert!(Grid::of(30, 0).is_none());
    }

    /// A live stream at half the show's rate takes every second frame too.
    #[test]
    fn a_live_stream_at_half_the_shows_rate_takes_every_second_frame() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::new(Some(start), 30.0).with_show(60);
        let mut sent = Vec::new();
        for i in 0..600 {
            let made = start + std::time::Duration::from_secs_f64(i as f64 / 60.0 + late_by(i));
            if let (0, Some(Placed::After(0))) = timeline.place_live(made, LIVE_FILL) {
                sent.push(i);
            }
        }
        assert!(sent.windows(2).all(|w| w[1] - w[0] == 2), "{:?}", &sent[..20]);
        assert_eq!(sent.len(), 300);
    }

    /// `n` frames of a 60 fps show drawn from `start` on, each moved by
    /// `jitter(i)` seconds, placed in order on a 60 fps timeline.
    fn placed(
        start: std::time::Instant,
        from: std::time::Instant,
        n: usize,
        jitter: impl Fn(usize) -> f64,
    ) -> (Timeline, Vec<Placed>) {
        let mut timeline = Timeline::new(Some(start), 60.0);
        let placed = (0..n)
            .map(|i| {
                let secs = (i as f64 / 60.0 + jitter(i)).max(0.0);
                timeline.place(from + std::time::Duration::from_secs_f64(secs))
            })
            .collect();
        (timeline, placed)
    }

    /// A writer taking 60 fps frames — drawn every `every` show frames,
    /// each coming back from the GPU `readback` seconds on — through a
    /// queue of ten into an engine that does `rate` slots a second, held up
    /// for `held` seconds at 10 s (a recording started beside it), as a live
    /// stream takes them (see `take_live`) or each in turn (`live` false),
    /// for 40 s: the frames it refused, and the fresh frames it took, each
    /// second from 30 s on; and the slots it skipped in all.
    fn writer_held_up(
        rate: f64,
        live: bool,
        held: f64,
        (every, readback): (u32, f64),
    ) -> (f64, f64, u64) {
        let start = std::time::Instant::now();
        let at = |secs: f64| start + std::time::Duration::from_secs_f64(secs);
        let mut timeline = Timeline::new(Some(start), 60.0);
        let mut queue: std::collections::VecDeque<(std::time::Instant, ())> =
            std::collections::VecDeque::new();
        let (mut free_at, mut refused, mut taken, mut skipped) = (0.0_f64, 0_u32, 0_u32, 0_u64);
        for tick in 0..(40 * 60) {
            let now = f64::from(tick) / 60.0;
            // Engine free: what waits is taken, until it is busy past now.
            while free_at <= now && !queue.is_empty() {
                let from = free_at.max(now.min(free_at + 1.0 / 60.0));
                let waiting: Vec<_> = if live {
                    queue.drain(..).collect()
                } else {
                    queue.pop_front().into_iter().collect()
                };
                let frames = if live { take_live(waiting).0 } else { waiting };
                let mut slots = 0.0;
                for (made, ()) in frames {
                    let placed = if live {
                        let (more, placed) = timeline.place_live(made, LIVE_FILL);
                        skipped += more;
                        placed.or_else(|| {
                            timeline.wrote_one();
                            None
                        })
                    } else {
                        Some(timeline.place(made))
                    };
                    if let Some(Placed::After(repeats)) = placed {
                        #[allow(clippy::cast_precision_loss)]
                        let more = (repeats + 1) as f64;
                        slots += more;
                        if now >= 30.0 {
                            taken += 1;
                        }
                    }
                }
                free_at = from + slots.max(1.0) / rate;
                if from < 10.0 + held && free_at > 10.0 {
                    free_at = free_at.max(10.0 + held);
                }
            }
            // The frame drawn `readback` ago comes in, if one was drawn then.
            let drawn = now - readback;
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let drawn_tick = (drawn * 60.0).round() as i64;
            if drawn >= 0.0 && drawn_tick % i64::from(every) == 0 {
                if queue.len() >= 10 {
                    if now >= 30.0 {
                        refused += 1;
                    }
                } else {
                    queue.push_back((at(drawn), ()));
                }
            }
        }
        (f64::from(refused) / 10.0, f64::from(taken) / 10.0, skipped)
    }

    /// A live stream held up for a moment comes back: fallen behind real
    /// time, it takes the newest frame and skips ahead to it, rather than
    /// making up the time at its own rate — made up, each frame taken owed
    /// the time since the one before, and it never came level again (a
    /// 4K60 stream with a 4K60 recording started beside it, 2026-10-29).
    #[test]
    fn a_live_stream_held_up_skips_ahead_and_comes_back() {
        let (refused, taken, skipped) = writer_held_up(61.0, true, 0.5, (1, 0.02));
        assert!(refused < 1.0, "refused {refused:.1} a second");
        assert!(taken > 55.0, "took {taken:.1} fresh frames a second");
        assert!(skipped > 0, "skipped ahead");
        // Each in turn: twenty seconds on, still refusing frames.
        let (refused, taken, _) = writer_held_up(61.0, false, 0.5, (1, 0.02));
        assert!(
            refused > 10.0 && taken < 50.0,
            "each in turn: {refused:.1} refused, {taken:.1} taken a second"
        );
        // Held up for twenty seconds: back all the same, in steps of no
        // more than five seconds each.
        let (refused, taken, skipped) = writer_held_up(61.0, true, 20.0, (1, 0.02));
        assert!(
            refused < 1.0 && taken > 55.0,
            "{refused:.1} refused, {taken:.1} taken"
        );
        assert!(skipped > 0);
    }

    /// A live stream that keeps up never skips — even for a show at half
    /// its rate whose frames take long to come back from the GPU: each
    /// frame comes on its own, and the gaps between are filled.
    #[test]
    fn a_live_stream_keeping_up_never_skips() {
        let (refused, taken, skipped) = writer_held_up(240.0, true, 0.0, (2, 0.12));
        assert_eq!(skipped, 0, "skipped while keeping up");
        assert!(refused < 0.1, "refused {refused:.1} a second");
        assert!(taken > 29.0, "took {taken:.1} a second");
    }

    /// A live stream's frames waiting: fewer than `LIVE_BACKLOG`, each in
    /// turn; that many or more, only the newest. A frame a slot or two past
    /// the next to write has them filled; one further on skips to its own
    /// slot, nothing filled; one further than `LONGEST_SKIP`, a step that
    /// far, the frame let go.
    #[test]
    fn a_live_stream_skips_ahead_only_once_behind() {
        let start = std::time::Instant::now();
        let at = |secs: f64| start + std::time::Duration::from_secs_f64(secs);
        let mut timeline = Timeline::new(Some(start), 60.0);
        assert_eq!(timeline.place(at(0.0)), Placed::After(0));
        let (few, let_go) = take_live(vec![(at(0.05), 1), (at(0.07), 2)]);
        assert_eq!((few.len(), let_go), (2, 0), "in turn");
        assert_eq!(
            timeline.place_live(at(0.05), LIVE_FILL),
            (0, Some(Placed::After(2))),
            "filled"
        );
        let many: Vec<_> = (0..5).map(|i| (at(0.4 + f64::from(i) / 60.0), i)).collect();
        let (newest, let_go) = take_live(many);
        assert_eq!((newest.len(), newest[0].1, let_go), (1, 4, 4), "the newest");
        let (skipped, placed) = timeline.place_live(newest[0].0, LIVE_FILL);
        assert_eq!(
            (skipped, placed),
            (28 - 4, Some(Placed::After(0))),
            "nothing filled"
        );
        // Twelve seconds further on: a step of half a second, the frame let
        // go.
        assert_eq!(timeline.place_live(at(13.4), LIVE_FILL), (30, None));
    }

    /// Refused frames are matched to the slots skipped once, across as many
    /// catch-ups as it takes, and to slots filled over them: never counted
    /// lost twice.
    #[test]
    fn refused_frames_are_counted_lost_once() {
        let mut credit = super::RefusedCredit::default();
        credit.refused(30);
        assert_eq!(credit.skipped(10), 0, "all already counted");
        assert_eq!(credit.skipped(25), 5, "the rest of the 30, then new");
        assert_eq!(credit.skipped(7), 7, "none left to match");
        credit.refused(4);
        credit.filled(3);
        assert_eq!(credit.skipped(5), 4, "one left after the fill");
    }

    /// A live stream's first frame, late — its encoder slow to start — goes
    /// to the first slot, nothing filled before it, so picture and sound
    /// start together; the frames after it skip ahead from there.
    #[test]
    fn a_live_streams_late_first_frame_fills_nothing() {
        let start = std::time::Instant::now();
        let at = |secs: f64| start + std::time::Duration::from_secs_f64(secs);
        let mut timeline = Timeline::new(Some(start), 60.0);
        assert_eq!(
            timeline.place_live(at(20.0), LIVE_FILL),
            (0, Some(Placed::After(0))),
            "at the first slot"
        );
        // The next: on from there, a step at a time.
        assert_eq!(
            timeline.place_live(at(20.0 + 1.0 / 60.0), LIVE_FILL),
            (30, None)
        );
    }

    /// Frames drawn on time are written once each, nothing repeated or left
    /// out — even jittered by a third of a frame, which the slots, set where
    /// the first frame fell, take in their stride.
    #[test]
    fn frames_drawn_on_time_are_written_once_each() {
        let start = std::time::Instant::now();
        for jitter in [0.0, 0.33 / 60.0] {
            let wobble = |i: usize| if i % 2 == 1 { jitter } else { -jitter / 2.0 };
            // Drawn 0.4 of a frame after the sound began: without the slots
            // set by the first frame, a third of a frame late lands in the
            // next slot.
            let from = start + std::time::Duration::from_secs_f64(0.4 / 60.0);
            let (timeline, placed) = placed(start, from, 120, wobble);
            assert!(
                placed.iter().all(|p| *p == Placed::After(0)),
                "jitter {jitter}: {placed:?}"
            );
            assert_eq!(timeline.written, 120);
        }
    }

    /// Frames timed by refresh calls that reach Nida late now and then — a
    /// few milliseconds as a rule, half a frame at times, the first of all
    /// among the late ones — are still written once each.
    #[test]
    fn frames_timed_by_late_refresh_calls_are_written_once_each() {
        let start = std::time::Instant::now();
        let frame = 1.0 / 60.0;
        let late = |i: usize| {
            let wobble = (i.wrapping_mul(2_654_435_761) % 1000) as f64 / 1000.0 * 0.004;
            let spike = if i.is_multiple_of(37) { 0.6 * frame } else { 0.0 };
            wobble + spike
        };
        let (timeline, placed) = placed(start, start, 1200, late);
        // The first goes to whichever slot it falls nearest; every frame
        // after it, to the next.
        let Placed::After(first) = placed[0] else {
            panic!("the first frame is written");
        };
        let (repeats, left_out) = placed[1..].iter().fold((0, 0), |(r, l), p| match p {
            Placed::After(n) => (r + n, l),
            Placed::LeftOut => (r, l + 1),
        });
        assert_eq!((repeats, left_out), (0, 0), "{placed:?}");
        assert_eq!(timeline.written, 1200 + first);
    }

    /// A show drawn at 60.1 fps into a 60 fps take leaves a frame out about
    /// every ten seconds, and one at 59.94 repeats one about every sixteen —
    /// never both, as a real difference of rate.
    #[test]
    fn a_show_a_little_off_the_takes_rate_leaves_out_or_repeats_now_and_then() {
        for (rate, repeats_wanted, left_out_wanted) in [(60.1, 0..=0, 3..=4), (59.94, 2..=3, 0..=0)] {
            let start = std::time::Instant::now();
            let mut timeline = Timeline::new(Some(start), 60.0);
            let (mut repeats, mut left_out) = (0, 0);
            for i in 0..(rate * 40.0) as u32 {
                let made = start + std::time::Duration::from_secs_f64(f64::from(i) / rate);
                match timeline.place(made) {
                    Placed::After(n) => repeats += n,
                    Placed::LeftOut => left_out += 1,
                }
            }
            assert!(repeats_wanted.contains(&repeats), "{rate}: {repeats} repeated");
            assert!(left_out_wanted.contains(&left_out), "{rate}: {left_out} left out");
        }
    }

    /// What decides a frame's place is when it was drawn, never when it got
    /// to the writer: the frames that waited behind an encoder still starting
    /// went into the picture once by when they arrived and again as repeats,
    /// and every take started with the picture behind the sound. A timeline
    /// fed frames drawn at an even pace writes them once each, however they
    /// came.
    #[test]
    fn frames_that_waited_on_their_way_take_their_own_slots() {
        let start = std::time::Instant::now();
        let (timeline, placed) = placed(start, start, 30, |_| 0.0);
        assert!(placed.iter().all(|p| *p == Placed::After(0)), "{placed:?}");
        assert_eq!(timeline.written, 30, "no repeats for frames that were drawn");
    }

    /// The picture starts with the sound: frames drawn half a second after
    /// the sound began — a first take waiting for the encoder — are preceded
    /// by that half second of the first frame, not pulled forward over it.
    #[test]
    fn the_take_starts_when_the_sound_does() {
        let start = std::time::Instant::now();
        let late = start + std::time::Duration::from_millis(400);
        let (timeline, placed) = placed(start, late, 3, |_| 0.0);
        assert_eq!(placed, [Placed::After(24), Placed::After(0), Placed::After(0)]);
        assert_eq!(timeline.written, 27);
    }

    /// A frame drawn before the take began is not in it.
    #[test]
    fn frames_drawn_before_the_take_are_left_out() {
        let start = std::time::Instant::now() + std::time::Duration::from_millis(100);
        let mut timeline = Timeline::new(Some(start), 60.0);
        for before in [50, 5] {
            let early = start.checked_sub(std::time::Duration::from_millis(before)).unwrap();
            assert_eq!(timeline.place(early), Placed::LeftOut, "{before} ms before");
        }
        assert_eq!(timeline.place(start), Placed::After(0));
        assert_eq!(timeline.place(start), Placed::LeftOut, "drawn at the same moment again");
        assert_eq!(timeline.written, 1);
    }

    /// A show a little off the take's rate whose refresh calls come a
    /// millisecond or two late — what the frame trace showed on a laptop —
    /// leaves out or repeats a frame now and then, as the rates must, but
    /// never a repeat and a frame left out together.
    #[test]
    fn a_show_off_the_takes_rate_with_late_calls_never_pairs_a_repeat_with_a_drop() {
        for (rate, repeats_wanted, left_out_wanted) in [(60.1, 0..=0, 3..=4), (59.94, 2..=3, 0..=0)] {
            let start = std::time::Instant::now();
            let mut timeline = Timeline::new(Some(start), 60.0);
            let (mut repeats, mut left_out) = (0, 0);
            for i in 0..(rate * 40.0) as u32 {
                let late = f64::from(i.wrapping_mul(2_654_435_761) % 1000) / 1000.0 * 0.002;
                let made = start + std::time::Duration::from_secs_f64(f64::from(i) / rate + late);
                match timeline.place(made) {
                    Placed::After(n) => repeats += n,
                    Placed::LeftOut => left_out += 1,
                }
            }
            assert!(repeats_wanted.contains(&repeats), "{rate}: {repeats} repeated");
            assert!(left_out_wanted.contains(&left_out), "{rate}: {left_out} left out");
        }
    }

    /// A gap in the renderer is covered by exactly as many frames as it
    /// swallowed, so the picture stays with the sound after it.
    #[test]
    fn a_renderer_gap_is_covered_frame_for_frame() {
        let start = std::time::Instant::now();
        // Ten frames, then 100 ms (six frames) with none: the next frame goes
        // to slot 16, after six repeats.
        let (timeline, placed) =
            placed(start, start, 11, |i| if i == 10 { 0.1 } else { 0.0 });
        assert_eq!(placed[10], Placed::After(6));
        assert_eq!(timeline.written, 17);
    }

    /// A show drawn faster than the encoder's rate leaves frames out rather
    /// than stretching the picture past the sound.
    #[test]
    fn frames_for_a_slot_already_written_are_left_out() {
        let start = std::time::Instant::now();
        let mut timeline = Timeline::new(Some(start), 60.0);
        let placed: Vec<Placed> = (0..240)
            .map(|i| timeline.place(start + std::time::Duration::from_secs_f64(f64::from(i) / 120.0)))
            .collect();
        // The last frame, drawn at 239/120 s, falls in slot 120: every slot
        // from 0 to 120 written once, every other frame left out.
        let left_out = placed.iter().filter(|p| **p == Placed::LeftOut).count();
        assert_eq!(timeline.written, 121, "{placed:?}");
        assert_eq!(left_out, 240 - 121, "two seconds at 120 fps into 60 fps");
        assert!(placed.iter().all(|p| matches!(p, Placed::After(0) | Placed::LeftOut)));
    }

    /// A two-second freeze is filled before the frame after it, which keeps
    /// its own slot — the picture stays with the sound through it.
    #[test]
    fn a_long_freeze_is_filled_before_the_frame_after_it() {
        let start = std::time::Instant::now();
        let (timeline, placed) =
            placed(start, start, 10, |i| if i >= 1 { 2.0 } else { 0.0 });
        // Frame 1 was drawn at 1/60 s + 2 s: slot 121, after 120 repeats.
        assert_eq!(placed[1], Placed::After(120));
        assert!(placed[2..].iter().all(|p| *p == Placed::After(0)), "{placed:?}");
        assert_eq!(timeline.written, 130, "{placed:?}");
    }

    /// A freeze of a minute is filled in full before the frame after it;
    /// past two minutes it is not: the show was not being recorded, and one
    /// frame must not become an hour of repeats.
    #[test]
    fn a_freeze_is_filled_in_full_up_to_two_minutes() {
        let start = std::time::Instant::now();
        let (_, minute) = placed(start, start, 2, |i| if i == 1 { 60.0 } else { 0.0 });
        assert_eq!(minute[1], Placed::After(3600), "a minute: all of it");
        let (_, ten) = placed(start, start, 2, |i| if i == 1 { 600.0 } else { 0.0 });
        assert_eq!(ten[1], Placed::After(7200), "ten minutes: two of them");
    }

    /// With no sound to start from, the first frame starts the take.
    #[test]
    fn without_sound_the_first_frame_starts_the_take() {
        let mut timeline = Timeline::new(None, 60.0);
        let first = std::time::Instant::now();
        assert_eq!(timeline.place(first), Placed::After(0));
        assert_eq!(
            timeline.place(first + std::time::Duration::from_secs_f64(1.0 / 60.0)),
            Placed::After(0)
        );
    }

    /// PCM lost to backpressure is replaced by an equal span of silence.
    ///
    /// Now that audio is timed by its sample count, a dropped chunk does not
    /// leave a hole — it pulls everything after it earlier, so one dropout
    /// would put the rest of the recording out of sync with the picture.
    /// Writing silence in its place costs a brief mute and keeps the count
    /// exact. The assertion is on the byte count for that reason: it is the
    /// only thing the timeline is made of.
    #[test]
    fn lost_audio_is_replaced_by_an_equal_span_of_silence() {
        use std::io::Read as _;

        const CHUNK: usize = 64;
        const LOST: u64 = 500;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = crossbeam_channel::bounded::<PcmChunk>(4);

        let mut pipe = AudioPipe::start(listener, rx, "test".into()).expect("pipe");

        // Stand in for ffmpeg: connect and read everything written. A chunk
        // queued before the loss, then the one after it.
        let mut client = std::net::TcpStream::connect(addr).expect("connect");
        tx.send(PcmChunk {
            samples: vec![0.25; CHUNK],
            lost_before: 0,
        })
        .expect("send");
        tx.send(PcmChunk {
            samples: vec![0.5; CHUNK],
            lost_before: LOST,
        })
        .expect("send");

        let expected_samples = CHUNK + LOST as usize + CHUNK;
        let mut buf = vec![0u8; expected_samples * 4];
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("timeout");
        client.read_exact(&mut buf).expect("read all PCM");

        let samples: &[f32] = bytemuck::cast_slice(&buf);
        assert!(
            samples[..CHUNK].iter().all(|s| *s == 0.25),
            "what came before the loss keeps its place"
        );
        assert!(
            samples[CHUNK..CHUNK + LOST as usize].iter().all(|s| *s == 0.0),
            "the gap should be filled with silence, where it was"
        );
        assert!(
            samples[CHUNK + LOST as usize..].iter().all(|s| *s == 0.5),
            "the real chunk should follow the silence, intact"
        );
        assert_eq!(pipe.silence_spliced(), LOST);

        pipe.stop();
    }

    /// With nothing lost, not a single extra sample is invented.
    #[test]
    fn an_unimpeded_audio_pipe_writes_only_captured_samples() {
        use std::io::Read as _;

        const CHUNK: usize = 32;

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = crossbeam_channel::bounded::<PcmChunk>(4);
        let mut pipe = AudioPipe::start(listener, rx, "test-clean".into()).expect("pipe");

        let mut client = std::net::TcpStream::connect(addr).expect("connect");
        tx.send(PcmChunk {
            samples: vec![0.25; CHUNK],
            lost_before: 0,
        })
        .expect("send");

        let mut buf = vec![0u8; CHUNK * 4];
        client
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .expect("timeout");
        client.read_exact(&mut buf).expect("read chunk");

        let samples: &[f32] = bytemuck::cast_slice(&buf);
        assert!(samples.iter().all(|s| *s == 0.25));
        assert_eq!(pipe.silence_spliced(), 0);

        pipe.stop();
    }

    #[test]
    fn compute_rtmp_bitrate_720p() {
        let (maxrate, bufsize) = compute_rtmp_bitrate(1280, 720, 30);
        assert_eq!(maxrate, 3000);
        assert_eq!(bufsize, 6000);
    }

    #[test]
    fn compute_rtmp_bitrate_1080p() {
        let (maxrate, bufsize) = compute_rtmp_bitrate(1920, 1080, 30);
        assert_eq!(maxrate, 6000);
        assert_eq!(bufsize, 12000);
    }

    #[test]
    fn compute_rtmp_bitrate_1080p60() {
        let (maxrate, bufsize) = compute_rtmp_bitrate(1920, 1080, 60);
        assert_eq!(maxrate, 9000);
        assert_eq!(bufsize, 18000);
    }

    #[test]
    fn compute_rtmp_bitrate_4k() {
        let (maxrate, bufsize) = compute_rtmp_bitrate(3840, 2160, 30);
        assert_eq!(maxrate, 15000);
        assert_eq!(bufsize, 30000);
    }
}
