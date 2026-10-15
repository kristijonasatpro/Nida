//! Streaming, the vMix way: one picture encoded once and sent to up to four
//! RTMP or RTMPS destinations at the same time — a platform, a CDN, the
//! client's own server — each with its own URL, stream key and login.
//!
//! The encoder writes MPEG-TS, which [`Fanout`] copies to one small ffmpeg per
//! destination that repackages it as FLV and pushes it. A destination that
//! drops is reconnected on its own while the others carry on, and each one
//! reports whether it is keeping up, which is what the STREAM button's colour
//! shows.

pub(crate) mod adaptive;
mod fanout;
#[cfg(target_os = "macos")]
pub(crate) mod flv;
pub mod network;
pub(crate) mod ts;
#[cfg(target_os = "macos")]
pub(crate) mod vt_encoder;

pub use fanout::{Fanout, Remedy};

use crate::engine::value::render::OutputSource;

/// How many destinations a stream sends to, as vMix has three.
pub const DESTINATIONS: usize = 4;

/// Which H.264 encoder a stream uses.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum StreamEncoder {
    /// The Mac's media engine (VideoToolbox): no CPU cost, so the renderer
    /// keeps its frame rate.
    Hardware,
    /// x264 on the CPU, for machines without a hardware encoder ffmpeg can use.
    Software,
}

impl StreamEncoder {
    /// Hardware where ffmpeg has it, else software.
    pub fn preferred() -> Self {
        if Self::Hardware.available() {
            Self::Hardware
        } else {
            Self::Software
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Hardware => "Hardware (Mac media engine)",
            Self::Software => "Software (x264, uses the CPU)",
        }
    }

    /// Whether the installed ffmpeg has this encoder. Asked once per run.
    pub fn available(self) -> bool {
        match self {
            Self::Software => true,
            Self::Hardware => {
                static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
                *HAS.get_or_init(|| {
                    cfg!(target_os = "macos")
                        && std::process::Command::new("ffmpeg")
                            .args(["-hide_banner", "-encoders"])
                            .output()
                            .is_ok_and(|o| {
                                String::from_utf8_lossy(&o.stdout).contains("h264_videotoolbox")
                            })
                })
            }
        }
    }

    /// The encoder that will actually open here for `codec`, and whether it
    /// takes constant bit rate: hardware as asked if it works, else software.
    pub(crate) fn resolved(self, codec: VideoCodec) -> (Self, bool) {
        // Software needs no trying: the media engine is not asked about.
        if self == Self::Software {
            return (Self::Software, true);
        }
        match (self, hardware_support(codec)) {
            (Self::Hardware, HardwareSupport::ConstantRate) => (Self::Hardware, true),
            (Self::Hardware, HardwareSupport::VariableRate) => (Self::Hardware, false),
            (Self::Hardware, HardwareSupport::Unusable) => {
                log::warn!(
                    "The hardware {} encoder does not open here; using software",
                    codec.label()
                );
                (Self::Software, true)
            }
            (Self::Software, _) => (Self::Software, true),
        }
    }

    /// How a stream in `codec` held as `mode` is encoded here: this encoder
    /// where it opens (else software, said), and the mode as asked where
    /// the encoder can hold it — a media engine with no constant rate (some
    /// Intel Macs) sends Constant and Variable as its average, said.
    pub fn plan(self, codec: VideoCodec, mode: RateMode) -> RatePlan {
        self.plan_from(self.resolved(codec), mode)
    }

    /// [`Self::plan`], this encoder having resolved to `encoder` here, and
    /// that holding a constant rate or not (`constant_ok`).
    fn plan_from(self, (encoder, constant_ok): (Self, bool), mode: RateMode) -> RatePlan {
        let fell_back = (self != encoder).then(|| {
            "The media engine does not open here: the software encoder sends it".to_string()
        });
        match (encoder, mode) {
            (Self::Hardware, RateMode::Constant | RateMode::Variable) if !constant_ok => RatePlan {
                encoder,
                mode: RateMode::Average,
                constant: false,
                note: Some(format!(
                    "This Mac's media engine holds no constant rate: {} goes out as Average",
                    mode.label()
                )),
            },
            (Self::Hardware, RateMode::Average) => RatePlan {
                encoder,
                mode,
                constant: false,
                note: fell_back,
            },
            (_, mode) => RatePlan {
                encoder,
                mode,
                constant: mode != RateMode::Average,
                note: fell_back,
            },
        }
    }

    /// ffmpeg's video encoder arguments for a stream of `kbps` in `codec`
    /// held as `plan` says, a keyframe every `gop` frames and never between,
    /// no B-frames. Constant as [`Self::video_args`] holds it; Variable the
    /// same picture without the padding — the media engine's filler units
    /// dropped as they come out, software's rate held under `kbps` without
    /// it (x264's Constant declares a buffer model the padding is part of,
    /// so it is not cut out of it); Average the rate on average, software's
    /// peaks under one and a half times it.
    pub(crate) fn stream_video_args(
        self,
        codec: VideoCodec,
        kbps: u32,
        gop: u32,
        plan: &RatePlan,
    ) -> Vec<String> {
        let mut args = self.video_args(codec, kbps, gop, plan.constant);
        let set = |args: &mut Vec<String>, key: &str, value: String| {
            if let Some(at) = args.iter().position(|a| a == key) {
                args[at + 1] = value;
            }
        };
        match (self, codec, plan.mode) {
            (_, _, RateMode::Constant) => {
                if self == Self::Software && codec == VideoCodec::Hevc {
                    // x265 holds the rate only so told; without it a still
                    // went out at 17 kbps of a 1000 asked.
                    set(&mut args, "-x265-params", "scenecut=0:log-level=error:strict-cbr=1".into());
                }
            }
            (Self::Hardware, _, RateMode::Variable) => {
                let filler = if codec == VideoCodec::H264 { 12 } else { 38 };
                args.extend(["-bsf:v".into(), format!("filter_units=remove_types={filler}")]);
            }
            (Self::Software, VideoCodec::H264, RateMode::Variable) => {
                set(&mut args, "-x264-params", "force-cfr=1".into());
            }
            // x265 held under the rate without being told to pad; the media
            // engine on its own average (`plan.constant` false).
            (Self::Software, VideoCodec::Hevc, RateMode::Variable)
            | (Self::Hardware, _, RateMode::Average) => {}
            (Self::Software, _, RateMode::Average) => {
                set(&mut args, "-maxrate", format!("{}k", kbps * 3 / 2));
                set(&mut args, "-bufsize", format!("{}k", kbps * 3));
                if codec == VideoCodec::H264 {
                    set(&mut args, "-x264-params", "force-cfr=1".into());
                }
            }
        }
        args
    }

    /// ffmpeg's video encoder arguments for `codec`: `kbps` (held constant
    /// where the encoder can, `constant_rate`), a keyframe every `gop` frames
    /// and never between, no B-frames — what streaming platforms ask for,
    /// and what keeps a recording easy to cut.
    ///
    /// The media engine gets no `-maxrate`: ffmpeg turns it into a one-second
    /// data-rate limit, and with one set the engine drops constant rate and
    /// plays safe far under it — a 6000 kbps stream of a slowly moving
    /// background went out at 1700, and moving pictures got the fewer bits.
    /// The pixel format this encoder is handed: NV12 as it comes from the
    /// GPU where it takes it (the media engine does, and x264), else 4:2:0
    /// planar — x265 has no NV12 input.
    pub(crate) fn pixel_format(self, codec: VideoCodec, nv12: bool) -> &'static str {
        if nv12 && !(self == Self::Software && codec == VideoCodec::Hevc) {
            "nv12"
        } else {
            "yuv420p"
        }
    }

    pub(crate) fn video_args(
        self,
        codec: VideoCodec,
        kbps: u32,
        gop: u32,
        constant_rate: bool,
    ) -> Vec<String> {
        let rate = format!("{kbps}k");
        let buffer = format!("{}k", kbps * 2);
        let gop = gop.max(1).to_string();
        let mut args: Vec<&str> = match (self, codec) {
            (Self::Hardware, VideoCodec::H264) => {
                vec![
                    "-c:v",
                    "h264_videotoolbox",
                    "-realtime",
                    "1",
                    "-profile:v",
                    "high",
                ]
            }
            (Self::Hardware, VideoCodec::Hevc) => {
                vec![
                    "-c:v",
                    "hevc_videotoolbox",
                    "-realtime",
                    "1",
                    "-profile:v",
                    "main",
                    "-tag:v",
                    "hvc1",
                ]
            }
            (Self::Software, VideoCodec::H264) => vec![
                "-c:v",
                "libx264",
                "-preset",
                "veryfast",
                "-tune",
                "zerolatency",
                "-profile:v",
                "high",
                "-x264-params",
                "nal-hrd=cbr:force-cfr=1",
                "-sc_threshold",
                "0",
            ],
            (Self::Software, VideoCodec::Hevc) => vec![
                "-c:v",
                "libx265",
                "-preset",
                "veryfast",
                "-tune",
                "zerolatency",
                "-x265-params",
                "scenecut=0:log-level=error",
                "-tag:v",
                "hvc1",
            ],
        };
        if self == Self::Hardware && constant_rate {
            args.extend(["-constant_bit_rate", "1"]);
        }
        args.extend(["-b:v", rate.as_str()]);
        if self == Self::Software {
            args.extend(["-maxrate", rate.as_str(), "-bufsize", buffer.as_str()]);
        }
        args.extend(["-g", gop.as_str(), "-keyint_min", gop.as_str(), "-bf", "0"]);
        args.into_iter().map(str::to_string).collect()
    }
}

/// How a stream's video bit rate is held. Measured at 1080p50 against the
/// rate set (see docs/17-streaming.md): the media engine's Constant pads a
/// talk's slides and camera bubbles to the rate with three quarters filler.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RateMode {
    /// The rate set, held, padded where the picture needs less: what
    /// `YouTube` and Cloudflare Stream ask for.
    #[default]
    Constant,
    /// Constant's picture without the padding: less where the picture needs
    /// less, about the rate set where it needs it all.
    Variable,
    /// The rate set on average: the picture gets it all, and busy stretches
    /// go over it.
    Average,
}

impl RateMode {
    pub const ALL: [Self; 3] = [Self::Constant, Self::Variable, Self::Average];

    pub fn label(self) -> &'static str {
        match self {
            Self::Constant => "Constant (CBR)",
            Self::Variable => "Variable (VBR)",
            Self::Average => "Average (ABR)",
        }
    }
}

/// How a stream will be encoded here: its encoder and rate mode as asked or
/// as near as this machine allows, and why where they differ.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatePlan {
    pub encoder: StreamEncoder,
    pub mode: RateMode,
    /// The media engine holds its rate constant (Constant and Variable);
    /// else its own average.
    pub constant: bool,
    /// What differs from what was asked, and why.
    pub note: Option<String>,
}

/// The video codec a stream or recording is encoded in.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VideoCodec {
    /// Plays everywhere; what every platform takes over RTMP.
    #[default]
    H264,
    /// H.265: the same picture at about 60% of the bit rate. Over RTMP only
    /// platforms that take Enhanced RTMP (`YouTube`) accept it.
    Hevc,
}

impl VideoCodec {
    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC (H.265)",
        }
    }
}

/// How the Mac's hardware encoder opens on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HardwareSupport {
    /// With constant bit rate — Apple silicon on macOS 13 or newer.
    ConstantRate,
    /// Only with a bit-rate ceiling. An Intel Mac's encoder can refuse the
    /// constant-rate setting, and ffmpeg then fails to open it at all.
    VariableRate,
    /// Not at all: not a Mac, or ffmpeg built without `VideoToolbox`.
    Unusable,
}

/// Try the media engine's encoders on a thread of their own, so the first
/// REC or STREAM after a launch finds the answer waiting instead of stopping
/// the show's frames for the half second it takes (see [`hardware_support`]).
pub fn try_encoders_meanwhile() {
    let spawned = std::thread::Builder::new()
        .name("encoder-probe".into())
        .spawn(|| {
            for codec in [VideoCodec::H264, VideoCodec::Hevc] {
                let _ = hardware_support(codec);
            }
            let _ = crate::record::prores_hardware();
            crate::renderer::subprocess::find_streaming_capabilities();
        });
    if let Err(e) = spawned {
        log::warn!("Encoders not tried ahead: {e}");
    }
}

/// Found by opening the encoder for a few frames, once per run and codec:
/// the list of encoders ffmpeg prints says what was built, not what this
/// machine opens.
pub(crate) fn hardware_support(codec: VideoCodec) -> HardwareSupport {
    static H264: std::sync::OnceLock<HardwareSupport> = std::sync::OnceLock::new();
    static HEVC: std::sync::OnceLock<HardwareSupport> = std::sync::OnceLock::new();
    let cell = match codec {
        VideoCodec::H264 => &H264,
        VideoCodec::Hevc => &HEVC,
    };
    *cell.get_or_init(|| {
        let opens = |constant_rate: bool| {
            let mut cmd = std::process::Command::new("ffmpeg");
            cmd.args(["-hide_banner", "-loglevel", "error", "-f", "lavfi"])
                .args(["-i", "testsrc2=size=320x180:rate=25", "-frames:v", "5"])
                .args(StreamEncoder::Hardware.video_args(codec, 1000, 50, constant_rate))
                .args(["-pix_fmt", "yuv420p", "-f", "null", "-"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd.status().is_ok_and(|s| s.success())
        };
        let support = classify(StreamEncoder::Hardware.available(), opens);
        log::info!("Hardware {} encoder: {support:?}", codec.label());
        support
    })
}

/// What trying the encoder found: constant rate first, then without it.
fn classify(listed: bool, opens: impl Fn(bool) -> bool) -> HardwareSupport {
    if !listed {
        HardwareSupport::Unusable
    } else if opens(true) {
        HardwareSupport::ConstantRate
    } else if opens(false) {
        HardwareSupport::VariableRate
    } else {
        HardwareSupport::Unusable
    }
}

fn master() -> OutputSource {
    OutputSource::Master
}
fn default_width() -> u32 {
    1920
}
fn default_height() -> u32 {
    1080
}
fn default_video_kbps() -> u32 {
    6000
}
fn default_audio_kbps() -> u32 {
    160
}
fn default_keyframe_secs() -> u32 {
    2
}
fn default_buffer_secs() -> u32 {
    10
}
fn yes() -> bool {
    true
}

/// What is streamed and how: shared by every destination, since the picture
/// is encoded once.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct StreamOptions {
    /// The picture: the master, or a bus's Program.
    #[serde(default = "master")]
    pub source: OutputSource,
    /// The audio-mixer bus the stream carries, by UUID. `None` is the first
    /// bus (Master); with no bus at all the stream carries silence, since
    /// some platforms refuse a stream without sound.
    #[serde(default)]
    pub audio_bus: Option<String>,
    /// Picture size sent, scaled on the GPU from the source.
    #[serde(default = "default_width")]
    pub width: u32,
    #[serde(default = "default_height")]
    pub height: u32,
    /// Video bit rate in kbit/s, constant. Changes while live for a stream
    /// encoded in Nida (see [`StreamStatus::rate_live`]).
    #[serde(default = "default_video_kbps")]
    pub video_kbps: u32,
    /// AAC audio bit rate in kbit/s.
    #[serde(default = "default_audio_kbps")]
    pub audio_kbps: u32,
    #[serde(default = "StreamEncoder::preferred")]
    pub encoder: StreamEncoder,
    /// Seconds between keyframes. Two is what most platforms ask for.
    #[serde(default = "default_keyframe_secs")]
    pub keyframe_secs: u32,
    /// Frames per second sent; `None` is Varda's own frame rate.
    #[serde(default)]
    pub fps: Option<u32>,
    #[serde(default)]
    pub codec: VideoCodec,
    /// Seconds of stream each destination can fall behind when its upload
    /// slows, and catch up on afterwards, before any is lost. Bigger rides
    /// out longer dips in venue internet; it cannot make up for an upload
    /// that is too slow all the time.
    #[serde(default = "default_buffer_secs")]
    pub buffer_secs: u32,
    /// How the video bit rate is held (see [`RateMode`]).
    #[serde(default)]
    pub rate_mode: RateMode,
    /// Lower the video bit rate while live when the upload cannot carry it,
    /// and raise it again when it can, instead of losing parts of the stream
    /// (see [`adaptive`]): on the media engine, encoded in Nida. Can be
    /// switched while live there.
    #[serde(default = "yes")]
    pub adaptive: bool,
}

impl Default for StreamOptions {
    fn default() -> Self {
        Self {
            source: master(),
            audio_bus: None,
            width: default_width(),
            height: default_height(),
            video_kbps: default_video_kbps(),
            audio_kbps: default_audio_kbps(),
            encoder: StreamEncoder::preferred(),
            keyframe_secs: default_keyframe_secs(),
            buffer_secs: default_buffer_secs(),
            fps: None,
            codec: VideoCodec::H264,
            rate_mode: RateMode::Constant,
            adaptive: true,
        }
    }
}

impl StreamOptions {
    /// The options with every number in a range ffmpeg and the platforms
    /// accept: an even picture size from 160×90 up to 4K, sane bit rates.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        let even = |v: u32, lo: u32, hi: u32| (v.clamp(lo, hi) / 2) * 2;
        self.width = even(self.width, 160, 3840);
        self.height = even(self.height, 90, 2160);
        self.video_kbps = self.video_kbps.clamp(300, 50_000);
        self.audio_kbps = self.audio_kbps.clamp(64, 320);
        self.keyframe_secs = self.keyframe_secs.clamp(1, 10);
        self.buffer_secs = self.buffer_secs.clamp(1, 60);
        self.fps = self.fps.map(|f| f.clamp(10, 120));
        self
    }
}

/// One place the stream goes, as vMix's destination fields: the server URL
/// and the stream key as the platform gives them, and a username and password
/// for servers that ask for a login.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct StreamDestination {
    /// Sent to when the stream starts; can be switched while live.
    #[serde(default)]
    pub enabled: bool,
    /// What the operator calls it, such as `YouTube` or `Venue`.
    #[serde(default)]
    pub name: String,
    /// `rtmp://` or `rtmps://` server and application, e.g.
    /// `rtmps://live.example.com:443/live/`.
    #[serde(default)]
    pub url: String,
    /// The stream key or stream name. Empty when the URL already ends in it.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
}

/// Everything about the stream that is saved with the stage.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct StreamSettings {
    #[serde(default)]
    pub options: StreamOptions,
    /// Always [`DESTINATIONS`] long once loaded, blank ones included.
    #[serde(default)]
    pub destinations: Vec<StreamDestination>,
}

impl Default for StreamSettings {
    fn default() -> Self {
        Self {
            options: StreamOptions::default(),
            destinations: vec![StreamDestination::default(); DESTINATIONS],
        }
    }
}

impl StreamSettings {
    /// Exactly [`DESTINATIONS`] destinations and options in range, whatever
    /// the file said.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.destinations
            .resize_with(DESTINATIONS, StreamDestination::default);
        self.options = self.options.clamped();
        self
    }
}

/// Where a destination's sender pushes: the URL ffmpeg opens and the options
/// that name the application and stream key exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelayTarget {
    /// `rtmp[s]://[user:password@]host[:port]/...`.
    pub url: String,
    /// `-rtmp_app` / `-rtmp_playpath`, when the key came separately.
    pub args: Vec<String>,
    /// The server and application without key or password, for logs and
    /// the status line.
    pub display: String,
}

impl StreamDestination {
    /// Nothing entered: the slot is free.
    pub fn is_blank(&self) -> bool {
        self.url.trim().is_empty() && self.key.trim().is_empty()
    }

    /// Why this destination cannot be sent to, if it cannot.
    pub fn problem(&self) -> Option<String> {
        self.relay_target().err()
    }

    /// Build what the sender needs, the way vMix and OBS read these fields:
    /// the URL is the server and application, the key is the stream name
    /// within it. Passing the two to ffmpeg separately, rather than joined
    /// into one URL for ffmpeg to split again, keeps a key with `?` or `/` in
    /// it, or an application of several parts, arriving intact.
    ///
    /// The login goes into the URL, where ffmpeg answers a server that
    /// challenges for one (Adobe or Limelight authentication, as Wowza,
    /// Akamai and FMLE-era servers use). ffmpeg does not decode `%` escapes
    /// there, so a `/`, `?` or `#` in it cannot be carried and is refused
    /// here rather than failing at the server.
    pub(crate) fn relay_target(&self) -> Result<RelayTarget, String> {
        let url = self.url.trim();
        let key = self.key.trim();
        let lower = url.to_ascii_lowercase();
        let scheme = if lower.starts_with("rtmps://") {
            "rtmps"
        } else if lower.starts_with("rtmp://") {
            "rtmp"
        } else if url.is_empty() {
            return Err("Enter the server URL".into());
        } else {
            return Err("The URL must start with rtmp:// or rtmps://".into());
        };
        let rest = &url[scheme.len() + 3..];
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path = path.trim_matches('/');
        if authority.is_empty() {
            return Err("The URL has no server name".into());
        }
        let (url_login, host) = match authority.rsplit_once('@') {
            Some((login, host)) => (Some(login), host),
            None => (None, authority),
        };
        let username = self.username.trim();
        if url_login.is_some() && !username.is_empty() {
            return Err("The login is both in the URL and in Username — keep one of them".into());
        }
        if username.is_empty() && !self.password.is_empty() {
            return Err("Enter the username that goes with the password".into());
        }
        let login = if username.is_empty() {
            url_login.map(str::to_string)
        } else {
            if username.contains(':') {
                return Err("A username cannot contain ':'".into());
            }
            if [username, self.password.as_str()]
                .iter()
                .any(|s| s.contains(['/', '?', '#']))
            {
                return Err(
                    "The username or password contains / ? or #, which RTMP logins cannot carry"
                        .into(),
                );
            }
            Some(if self.password.is_empty() {
                username.to_string()
            } else {
                format!("{username}:{}", self.password)
            })
        };
        let base = match &login {
            Some(login) => format!("{scheme}://{login}@{host}"),
            None => format!("{scheme}://{host}"),
        };
        let display = if path.is_empty() {
            format!("{scheme}://{host}")
        } else {
            format!("{scheme}://{host}/{path}")
        };
        if key.is_empty() {
            // The URL carries the stream name itself: application, then key.
            if !path.contains('/') {
                return Err("Enter the stream key".into());
            }
            return Ok(RelayTarget {
                url: format!("{base}/{path}"),
                args: Vec::new(),
                display,
            });
        }
        if path.is_empty() {
            return Err("The URL needs the application after the server, e.g. /live".into());
        }
        Ok(RelayTarget {
            url: format!("{base}/{path}/{key}"),
            args: vec![
                "-rtmp_app".into(),
                path.to_string(),
                "-rtmp_playpath".into(),
                key.to_string(),
            ],
            display,
        })
    }
}

/// How one destination is doing.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DestinationState {
    /// Not sending: switched off, or the stream is stopped.
    Off,
    /// Switched on but cannot be sent to as entered (see the message).
    Invalid,
    /// Connecting to the server; nothing has gone out yet.
    Connecting,
    /// Sending in real time.
    Live,
    /// Connected but not keeping up — the upload is too slow or stalled.
    Struggling,
    /// Lost, and trying again shortly.
    Reconnecting,
}

impl DestinationState {
    /// Worth the operator's attention: anything on but not plainly live.
    pub fn is_problem(self) -> bool {
        !matches!(self, Self::Off | Self::Live)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Invalid => "Check settings",
            Self::Connecting => "Connecting…",
            Self::Live => "Live",
            Self::Struggling => "Struggling",
            Self::Reconnecting => "Reconnecting…",
        }
    }
}

/// One destination's state for the status line and the API.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DestinationStatus {
    pub state: DestinationState,
    /// Bit rate actually going out, in kbit/s, over the last few seconds.
    pub kbps: f32,
    /// What is wrong, in plain words, when something is.
    pub message: Option<String>,
    /// ffmpeg's own line behind `message`, for the curious.
    pub detail: Option<String>,
    /// Times it has reconnected since the stream started.
    pub reconnects: u32,
    /// Pieces of stream thrown away because this upload fell behind.
    pub dropped: u64,
    /// Their bytes, in all.
    #[serde(default)]
    pub dropped_bytes: u64,
    /// The time they held, in seconds, at the bit rate the stream had then.
    #[serde(default)]
    pub dropped_secs: f32,
    /// Seconds live since it last connected.
    pub live_secs: f32,
    /// Bytes of stream waiting to be sent.
    #[serde(default)]
    pub backlog_bytes: u64,
    /// The connection's number: a new one each time it connects, so figures
    /// from before are not taken for this one's.
    #[serde(default)]
    pub generation: u64,
    /// The count of network changes when the connection began (see
    /// [`network::epoch`]): a connection begun at a higher one went out a
    /// new way.
    #[serde(default)]
    pub network: u64,
}

impl DestinationStatus {
    pub fn off() -> Self {
        Self {
            state: DestinationState::Off,
            kbps: 0.0,
            message: None,
            detail: None,
            reconnects: 0,
            dropped: 0,
            dropped_bytes: 0,
            dropped_secs: 0.0,
            live_secs: 0.0,
            backlog_bytes: 0,
            generation: 0,
            network: 0,
        }
    }

    pub(crate) fn invalid(message: String) -> Self {
        Self {
            state: DestinationState::Invalid,
            message: Some(message),
            ..Self::off()
        }
    }
}

/// The whole stream's state.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct StreamStatus {
    /// The stream is started.
    pub live: bool,
    /// Seconds since it was started.
    pub live_secs: f32,
    /// Something wrong with the encoder itself: it stopped, or cannot keep
    /// up with the frame rate.
    pub encoder_problem: Option<String>,
    /// Frames the encoder could not take since the stream started; each is
    /// covered by repeating the one before.
    #[serde(default)]
    pub encoder_dropped: u64,
    /// The time those frames would have covered, in seconds: each counts its
    /// own slot at the pace the show ran then.
    #[serde(default)]
    pub encoder_dropped_secs: f32,
    /// The rate frames reach the encoder at, as it was started — 0 while
    /// stopped.
    #[serde(default)]
    pub encoder_fps: u32,
    /// The rate the stream is sent at, as the encoder was started.
    #[serde(default)]
    pub stream_fps: u32,
    /// The video bit rate following the upload, while it does; `None` while
    /// it stays as set.
    #[serde(default)]
    pub adaptive: Option<AdaptiveStatus>,
    /// The video bit rate, and whether it follows the upload, can be changed
    /// while live: the stream is encoded in Nida.
    #[serde(default)]
    pub rate_live: bool,
    /// How the running stream's rate is held, which may differ from the one
    /// set (see [`RatePlan`]); `None` while stopped.
    #[serde(default)]
    pub rate_mode: Option<RateMode>,
    /// Why it differs from the one set, or the encoder from the one chosen.
    #[serde(default)]
    pub rate_note: Option<String>,
    /// What the stream is at, sound and picture, in kbit/s, as the encoder
    /// puts it out (see `Fanout::input_kbps`); `None` until measured.
    #[serde(default)]
    pub stream_kbps: Option<u32>,
    /// One per destination, in order.
    pub destinations: Vec<DestinationStatus>,
}

/// A stream's video bit rate following its upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct AdaptiveStatus {
    /// The rate set, in kbit/s: never more.
    pub asked_kbps: u32,
    /// The rate the encoder is at now.
    pub kbps: u32,
    /// The least it goes to for this picture.
    pub floor_kbps: u32,
}

impl AdaptiveStatus {
    /// Under the rate set: the upload cannot carry it.
    pub fn lowered(&self) -> bool {
        self.kbps < self.asked_kbps
    }

    /// As low as it goes.
    pub fn at_floor(&self) -> bool {
        self.kbps <= self.floor_kbps
    }
}

impl StreamStatus {
    pub fn stopped() -> Self {
        Self {
            live: false,
            live_secs: 0.0,
            encoder_problem: None,
            encoder_dropped: 0,
            encoder_dropped_secs: 0.0,
            encoder_fps: 0,
            stream_fps: 0,
            adaptive: None,
            rate_live: false,
            rate_mode: None,
            rate_note: None,
            stream_kbps: None,
            destinations: vec![DestinationStatus::off(); DESTINATIONS],
        }
    }

    /// Live, and everything switched on is sending in real time — the
    /// STREAM button's red.
    pub fn is_healthy(&self) -> bool {
        self.live
            && self.encoder_problem.is_none()
            && self
                .destinations
                .iter()
                .any(|d| d.state == DestinationState::Live)
            && !self.destinations.iter().any(|d| d.state.is_problem())
    }
}

/// The stream as the API and state show it. Stream keys and passwords never
/// leave Varda: a key is shown by its last characters, a password only as
/// set or not.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct StreamSnapshot {
    pub options: StreamOptions,
    pub destinations: Vec<DestinationSummary>,
    pub status: StreamStatus,
}

/// A destination without its secrets.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct DestinationSummary {
    pub enabled: bool,
    pub name: String,
    /// The URL, less any login written into it.
    pub url: String,
    pub username: String,
    pub has_key: bool,
    /// The key's last four characters, to tell keys apart.
    pub key_hint: String,
    pub has_password: bool,
}

impl StreamSnapshot {
    pub fn of(settings: &StreamSettings, status: StreamStatus) -> Self {
        Self {
            options: settings.options.clone(),
            destinations: settings
                .destinations
                .iter()
                .map(|d| DestinationSummary {
                    enabled: d.enabled,
                    name: d.name.clone(),
                    url: without_login(&d.url),
                    username: d.username.clone(),
                    has_key: !d.key.is_empty(),
                    key_hint: key_hint(&d.key),
                    has_password: !d.password.is_empty(),
                })
                .collect(),
            status,
        }
    }
}

impl Default for StreamSnapshot {
    fn default() -> Self {
        Self::of(&StreamSettings::default(), StreamStatus::stopped())
    }
}

/// `…` and the key's last four characters; the key itself when that short.
pub fn key_hint(key: &str) -> String {
    let chars: Vec<char> = key.chars().collect();
    if chars.len() <= 4 {
        "•".repeat(chars.len())
    } else {
        format!("…{}", chars[chars.len() - 4..].iter().collect::<String>())
    }
}

/// A URL with any `user:password@` taken out.
fn without_login(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (authority, path) = rest
        .split_once('/')
        .map_or((rest, None), |(a, p)| (a, Some(p)));
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    match path {
        Some(p) => format!("{scheme}://{host}/{p}"),
        None => format!("{scheme}://{host}"),
    }
}

/// Turn ffmpeg's last words into what an operator can act on.
pub(crate) fn explain(line: &str) -> String {
    let lower = line.to_ascii_lowercase();
    let has = |w: &[&str]| w.iter().any(|w| lower.contains(w));
    if has(&[
        "authentication",
        "authmod",
        "unauthorized",
        "access denied",
        "code=403",
        "rejected",
    ]) {
        "The server refused the login — check the stream key, username and password".into()
    } else if has(&["network changed"]) {
        "The network changed — reconnecting".into()
    } else if has(&["connection refused"]) {
        "The server refused the connection — check the URL and port".into()
    } else if has(&["resolve", "nodename", "not known", "no address"]) {
        "Unknown server — check the URL".into()
    } else if has(&["timed out", "timeout"]) {
        "The server did not answer".into()
    } else if has(&["tls", "ssl", "certificate", "handshake"]) {
        "The secure connection (RTMPS) failed".into()
    } else if has(&[
        "broken pipe",
        "connection reset",
        "end of file",
        "input/output error",
    ]) {
        "The server closed the connection".into()
    } else {
        "The sender stopped".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(url: &str, key: &str) -> StreamDestination {
        StreamDestination {
            enabled: true,
            url: url.into(),
            key: key.into(),
            ..StreamDestination::default()
        }
    }

    #[test]
    fn url_and_key_are_passed_as_application_and_stream_name() {
        let t = dest("rtmps://live.example.com:443/live/", "example-key?x=1")
            .relay_target()
            .unwrap();
        assert_eq!(t.url, "rtmps://live.example.com:443/live/example-key?x=1");
        assert_eq!(
            t.args,
            ["-rtmp_app", "live", "-rtmp_playpath", "example-key?x=1"],
            "the key reaches the server whole, query and all"
        );
        assert_eq!(t.display, "rtmps://live.example.com:443/live");
    }

    #[test]
    fn an_application_of_several_parts_is_kept_whole() {
        let t = dest("rtmp://ingest.example.com/app/instance", "stream")
            .relay_target()
            .unwrap();
        assert_eq!(t.args[1], "app/instance");
    }

    #[test]
    fn a_url_ending_in_the_key_needs_no_key_field() {
        let t = dest("rtmp://ingest.example.com/live2/xxxx-yyyy", "")
            .relay_target()
            .unwrap();
        assert_eq!(t.url, "rtmp://ingest.example.com/live2/xxxx-yyyy");
        assert!(t.args.is_empty());
        assert_eq!(
            dest("rtmp://ingest.example.com/live2", "")
                .problem()
                .as_deref(),
            Some("Enter the stream key")
        );
    }

    #[test]
    fn the_login_goes_into_the_url_and_never_into_what_is_shown() {
        let mut d = dest("rtmp://stream.example.com/live", "event1");
        d.username = "vj@example.com".into();
        d.password = "not@real:pw".into();
        let t = d.relay_target().unwrap();
        assert_eq!(
            t.url,
            "rtmp://vj@example.com:not@real:pw@stream.example.com/live/event1"
        );
        assert!(!t.display.contains("real:pw") && !t.display.contains("event1"));
    }

    #[test]
    fn logins_ffmpeg_cannot_carry_are_refused_up_front() {
        let mut d = dest("rtmp://h/live", "k");
        d.username = "u".into();
        d.password = "a/b".into();
        assert!(d.problem().unwrap().contains("/ ? or #"));
        d.password = "ok".into();
        d.username = "a:b".into();
        assert!(d.problem().unwrap().contains("':'"));
        d.username = String::new();
        assert!(d.problem().unwrap().contains("username"));
        let mut both = dest("rtmp://u:p@h/live", "k");
        both.username = "u".into();
        assert!(both.problem().unwrap().contains("both"));
    }

    #[test]
    fn a_login_already_in_the_url_is_kept() {
        let t = dest("rtmp://u:p@h:1935/live", "k").relay_target().unwrap();
        assert_eq!(t.url, "rtmp://u:p@h:1935/live/k");
        assert_eq!(t.display, "rtmp://h:1935/live");
    }

    #[test]
    fn other_protocols_and_missing_parts_are_explained() {
        assert!(dest("srt://h:9000", "k")
            .problem()
            .unwrap()
            .contains("rtmp://"));
        assert_eq!(
            dest("", "").problem().as_deref(),
            Some("Enter the server URL")
        );
        assert!(dest("rtmp://host", "k")
            .problem()
            .unwrap()
            .contains("application"));
        assert!(
            dest("RTMPS://Host/live", "k").problem().is_none(),
            "any case"
        );
    }

    #[test]
    fn settings_always_have_four_destinations_and_sane_numbers() {
        let s = StreamSettings {
            options: StreamOptions {
                width: 1921,
                height: 5000,
                video_kbps: 10,
                ..StreamOptions::default()
            },
            destinations: vec![dest("rtmp://h/a", "k")],
        }
        .normalized();
        assert_eq!(s.destinations.len(), DESTINATIONS);
        assert_eq!((s.options.width, s.options.height), (1920, 2160));
        assert_eq!(s.options.video_kbps, 300);
        let old: StreamSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.normalized().destinations.len(), DESTINATIONS);
    }

    #[test]
    fn the_encoder_is_asked_for_constant_rate_and_fixed_keyframes() {
        for enc in [StreamEncoder::Hardware, StreamEncoder::Software] {
            for constant in [true, false] {
                let args = enc
                    .video_args(VideoCodec::H264, 6000, 100, constant)
                    .join(" ");
                for want in ["-b:v 6000k", "-g 100", "-keyint_min 100", "-bf 0"] {
                    assert!(args.contains(want), "{enc:?}: {want} in {args}");
                }
                // x264 holds the rate with its buffer; the media engine
                // undershoots with a ceiling (see the test below).
                let capped = args.contains("-maxrate 6000k -bufsize 12000k");
                assert_eq!(capped, enc == StreamEncoder::Software, "{enc:?}: {args}");
            }
        }
        let asks = |c| {
            StreamEncoder::Hardware
                .video_args(VideoCodec::H264, 6000, 100, c)
                .join(" ")
                .contains("-constant_bit_rate 1")
        };
        assert!(
            asks(true) && !asks(false),
            "left out where the encoder refuses it"
        );
    }

    fn plan(encoder: StreamEncoder, mode: RateMode) -> RatePlan {
        RatePlan {
            encoder,
            mode,
            constant: mode != RateMode::Average,
            note: None,
        }
    }

    /// Each encoder holds each mode as measured to work: the media engine
    /// constant, padding dropped for Variable, its own average for Average;
    /// x264 and x265 held under the rate without padding for Variable — x264
    /// declaring no buffer model then — and under one and a half times it
    /// for Average; x265's Constant told to hold its rate.
    #[test]
    fn each_encoder_holds_each_rate_mode() {
        use StreamEncoder::{Hardware, Software};
        use VideoCodec::{Hevc, H264};
        let args = |enc: StreamEncoder, codec: VideoCodec, mode: RateMode| {
            enc.stream_video_args(codec, 4000, 100, &plan(enc, mode)).join(" ")
        };
        for codec in [H264, Hevc] {
            let filler = if codec == H264 { 12 } else { 38 };
            let constant = args(Hardware, codec, RateMode::Constant);
            assert!(constant.contains("-constant_bit_rate 1") && !constant.contains("filter_units"), "{constant}");
            let variable = args(Hardware, codec, RateMode::Variable);
            assert!(variable.contains("-constant_bit_rate 1"), "{variable}");
            assert!(variable.contains(&format!("-bsf:v filter_units=remove_types={filler}")), "{variable}");
            let average = args(Hardware, codec, RateMode::Average);
            assert!(!average.contains("-constant_bit_rate") && !average.contains("filter_units"), "{average}");
        }
        let constant = args(Software, H264, RateMode::Constant);
        assert!(constant.contains("nal-hrd=cbr") && constant.contains("-maxrate 4000k"), "{constant}");
        let variable = args(Software, H264, RateMode::Variable);
        assert!(!variable.contains("nal-hrd") && variable.contains("-maxrate 4000k -bufsize 8000k"), "{variable}");
        let average = args(Software, H264, RateMode::Average);
        assert!(!average.contains("nal-hrd") && average.contains("-maxrate 6000k -bufsize 12000k"), "{average}");
        let constant = args(Software, Hevc, RateMode::Constant);
        assert!(constant.contains("strict-cbr=1"), "{constant}");
        let variable = args(Software, Hevc, RateMode::Variable);
        assert!(!variable.contains("strict-cbr") && variable.contains("-maxrate 4000k"), "{variable}");
        let average = args(Software, Hevc, RateMode::Average);
        assert!(!average.contains("strict-cbr") && average.contains("-maxrate 6000k"), "{average}");
    }

    /// A media engine with no constant rate (some Intel Macs) sends
    /// Constant and Variable as its average, and says so; one that does not
    /// open sends nothing, the software encoder does, said too.
    #[test]
    fn what_cannot_be_held_here_is_said() {
        use StreamEncoder::{Hardware, Software};
        let intel = |mode| Hardware.plan_from((Hardware, false), mode);
        for asked in [RateMode::Constant, RateMode::Variable] {
            let plan = intel(asked);
            assert_eq!((plan.mode, plan.constant), (RateMode::Average, false));
            assert!(plan.note.as_deref().is_some_and(|n| n.contains("no constant rate") && n.contains(asked.label())));
        }
        assert_eq!(intel(RateMode::Average).note, None);
        let gone = Hardware.plan_from((Software, true), RateMode::Variable);
        assert_eq!((gone.encoder, gone.mode, gone.constant), (Software, RateMode::Variable, true));
        assert!(gone.note.as_deref().is_some_and(|n| n.contains("does not open")));
        let software = Software.plan_from((Software, true), RateMode::Average);
        assert_eq!((software.mode, software.constant, software.note), (RateMode::Average, false, None));
    }

    /// ffmpeg takes every encoder, codec and mode as asked: a few frames of
    /// a test picture encoded with each. The media engine only where it
    /// opens here.
    #[test]
    fn ffmpeg_takes_every_rate_mode() {
        if std::process::Command::new("ffmpeg").arg("-version").output().is_err() {
            return;
        }
        for encoder in [StreamEncoder::Software, StreamEncoder::Hardware] {
            for codec in [VideoCodec::H264, VideoCodec::Hevc] {
                if encoder == StreamEncoder::Hardware && hardware_support(codec) != HardwareSupport::ConstantRate {
                    continue;
                }
                for mode in RateMode::ALL {
                    let out = std::process::Command::new("ffmpeg")
                        .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi"])
                        .args(["-i", "testsrc2=size=320x180:rate=25", "-frames:v", "10"])
                        .args(encoder.stream_video_args(codec, 1000, 50, &plan(encoder, mode)))
                        .args(["-pix_fmt", "yuv420p", "-f", "null", "-"])
                        .output()
                        .unwrap();
                    assert!(
                        out.status.success(),
                        "{encoder:?} {codec:?} {mode:?}: {}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                }
            }
        }
    }

    /// The bit rate asked for is what goes out, even for a picture that
    /// needs far less — as a platform expects of a constant-rate stream.
    #[test]
    fn the_media_engine_sends_the_rate_asked_for() {
        if hardware_support(VideoCodec::H264) != HardwareSupport::ConstantRate {
            return;
        }
        let out = std::env::temp_dir().join(format!("varda-rate-{}.ts", std::process::id()));
        let secs = 3;
        let ok = std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "gradients=s=1280x720:r=60:speed=0.03"])
            .args(["-t", &secs.to_string()])
            .args(StreamEncoder::Hardware.video_args(VideoCodec::H264, 3000, 120, true))
            .args(["-pix_fmt", "yuv420p", "-f", "mpegts"])
            .arg(&out)
            .status()
            .is_ok_and(|s| s.success());
        let bytes = std::fs::metadata(&out).map_or(0, |m| m.len());
        let _ = std::fs::remove_file(&out);
        assert!(ok, "the encoder ran");
        let kbps = bytes * 8 / secs / 1000;
        assert!((2600..3600).contains(&kbps), "{kbps} kbps for 3000 asked");
    }

    #[test]
    fn an_encoder_that_refuses_constant_rate_is_used_without_it() {
        assert_eq!(classify(true, |_| true), HardwareSupport::ConstantRate);
        assert_eq!(
            classify(true, |constant| !constant),
            HardwareSupport::VariableRate,
            "an Intel Mac's encoder"
        );
        assert_eq!(classify(true, |_| false), HardwareSupport::Unusable);
        assert_eq!(
            classify(false, |_| true),
            HardwareSupport::Unusable,
            "not built in"
        );
    }

    #[test]
    fn the_encoder_chosen_is_one_that_opens_here() {
        let (enc, _) = StreamEncoder::Hardware.resolved(VideoCodec::H264);
        if hardware_support(VideoCodec::H264) == HardwareSupport::Unusable {
            assert_eq!(
                enc,
                StreamEncoder::Software,
                "falls back rather than failing"
            );
        } else {
            assert_eq!(enc, StreamEncoder::Hardware);
        }
        assert_eq!(
            StreamEncoder::Software.resolved(VideoCodec::Hevc).0,
            StreamEncoder::Software
        );
    }

    #[test]
    fn healthy_means_live_everywhere_that_is_on() {
        let mut s = StreamStatus::stopped();
        assert!(!s.is_healthy());
        s.live = true;
        s.destinations[0].state = DestinationState::Live;
        assert!(s.is_healthy());
        s.destinations[2].state = DestinationState::Reconnecting;
        assert!(!s.is_healthy(), "one destination down is a problem");
        s.destinations[2].state = DestinationState::Off;
        s.encoder_problem = Some("x".into());
        assert!(!s.is_healthy());
    }

    #[test]
    fn the_snapshot_never_carries_keys_or_passwords() {
        let mut settings = StreamSettings::default();
        settings.destinations[0] = StreamDestination {
            enabled: true,
            name: "Venue".into(),
            url: "rtmp://me:fake-url-pw@stream.example.com/live".into(),
            key: "fake-key-0123456789".into(),
            username: String::new(),
            password: "fake-password".into(),
        };
        let json =
            serde_json::to_string(&StreamSnapshot::of(&settings, StreamStatus::stopped())).unwrap();
        for secret in ["fake-url-pw", "fake-key-012345", "fake-password"] {
            assert!(!json.contains(secret), "{secret} leaked: {json}");
        }
        assert!(json.contains("…6789") && json.contains("rtmp://stream.example.com/live"));
    }

    #[test]
    fn ffmpeg_errors_become_advice() {
        assert!(explain("[rtmp @ 0x1] Server error: Authentication failed").contains("login"));
        assert!(explain("Connection refused").contains("URL and port"));
        assert!(explain("Failed to resolve hostname x: nodename nor servname").contains("Unknown"));
    }
}
