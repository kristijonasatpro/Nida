//! Recording, the vMix way: one REC button, a folder, and files named by the
//! date and time they were started, encoded on the Mac's media engine.
//!
//! MP4 is written in fragments (a small index every second) so a recording
//! survives Varda or the Mac crashing — everything up to the last second
//! plays. MOV carries `ProRes` for editing; MKV survives a crash too.

use std::path::{Path, PathBuf};

use crate::engine::value::render::OutputSource;
use crate::streaming::{StreamEncoder, VideoCodec};

/// The file a recording is written as.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecordFormat {
    /// Plays everywhere; written so that a crash loses at most a second.
    #[default]
    Mp4,
    /// `QuickTime`, for `ProRes` and editing.
    Mov,
    /// Matroska: survives a crash; VLC and editors that read it.
    Mkv,
}

impl RecordFormat {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mp4 => "MP4 — plays everywhere, survives a crash",
            Self::Mov => "MOV — QuickTime, for editing",
            Self::Mkv => "MKV — survives a crash (VLC)",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
        }
    }
}

/// The video codec of a recording.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RecordCodec {
    #[default]
    H264,
    /// About 60% of H.264's size for the same picture.
    Hevc,
    /// `ProRes` 422: large files, light to edit. MOV only.
    ProRes,
    /// `ProRes` 422 HQ: larger still, for grading. MOV only.
    ProResHq,
}

impl RecordCodec {
    pub fn label(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::Hevc => "HEVC (H.265)",
            Self::ProRes => "ProRes 422",
            Self::ProResHq => "ProRes 422 HQ",
        }
    }

    /// `ProRes` has its own data rate; the bit rate setting is for the rest.
    pub fn has_bit_rate(self) -> bool {
        matches!(self, Self::H264 | Self::Hevc)
    }

    fn video_codec(self) -> Option<VideoCodec> {
        match self {
            Self::H264 => Some(VideoCodec::H264),
            Self::Hevc => Some(VideoCodec::Hevc),
            Self::ProRes | Self::ProResHq => None,
        }
    }
}

fn master() -> OutputSource {
    OutputSource::Master
}

fn default_folder() -> PathBuf {
    dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("Nida")
}

fn default_name() -> String {
    "Nida".into()
}

/// Keep names usable as a single filename component on every supported OS.
pub(crate) fn recording_name(name: &str, fallback: &str) -> String {
    let mut safe = String::new();
    for c in name.chars() {
        let c = if c.is_control() || "<>:\"/\\|?*".contains(c) {
            '_'
        } else {
            c
        };
        if safe.len() + c.len_utf8() > 160 {
            break;
        }
        safe.push(c);
    }
    let name = safe;
    let name = name.trim_matches(|c: char| c.is_whitespace() || c == '.');
    if name.is_empty() {
        fallback.to_string()
    } else {
        name.to_string()
    }
}

pub(crate) fn output_recording_folder(path: &str) -> &Path {
    Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) fn output_recording_base(
    folder: &Path,
    name: &str,
    codec: &crate::engine::value::render::RecordingCodec,
) -> PathBuf {
    use crate::engine::value::render::RecordingCodec;
    let extension = match codec {
        RecordingCodec::H264 | RecordingCodec::H265 | RecordingCodec::AV1 => "mp4",
        _ => "mov",
    };
    folder.join(format!("{}.{extension}", recording_name(name, "Recording")))
}

/// A uniquely reserved output take. The configured path supplies its folder,
/// stem and extension; it is never used as an overwrite destination.
pub(crate) struct OutputRecordingFile {
    path: PathBuf,
    keep: bool,
}

impl OutputRecordingFile {
    pub(crate) fn reserve(configured: &Path) -> std::io::Result<Self> {
        if configured.file_name().is_none() || configured.as_os_str().is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Choose a recording file name",
            ));
        }
        let configured = std::path::absolute(configured)?;
        let folder = configured
            .parent()
            .expect("absolute recording has a parent");
        std::fs::create_dir_all(folder)?;
        let stem = configured.file_stem().unwrap_or_default().to_string_lossy();
        let extension = configured.extension().unwrap_or_default().to_string_lossy();
        let stamp = timestamp_now();
        for take in 1_u64.. {
            let suffix = if take == 1 {
                String::new()
            } else {
                format!(" ({take})")
            };
            let name = if extension.is_empty() {
                format!("{stem} {stamp}{suffix}")
            } else {
                format!("{stem} {stamp}{suffix}.{extension}")
            };
            let path = folder.join(name);
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path, keep: false }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
        }
        unreachable!("recording take numbers exhausted")
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn keep(mut self) {
        self.keep = true;
    }
}

impl Drop for OutputRecordingFile {
    fn drop(&mut self) {
        if !self.keep {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn default_audio_kbps() -> u32 {
    256
}

/// What is recorded and how.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RecordOptions {
    /// Filename prefix, followed by the take's timestamp.
    #[serde(default = "default_name")]
    pub name: String,
    /// The picture: the master, or a bus's Program.
    #[serde(default = "master")]
    pub source: OutputSource,
    /// The audio-mixer bus recorded; `None` is the first (Master). With no
    /// bus at all the file carries silence, so it always has a sound track.
    #[serde(default)]
    pub audio_bus: Option<String>,
    /// Where files go.
    #[serde(default = "default_folder")]
    #[schema(value_type = String)]
    pub folder: PathBuf,
    #[serde(default)]
    pub format: RecordFormat,
    #[serde(default)]
    pub codec: RecordCodec,
    #[serde(default = "StreamEncoder::preferred")]
    pub encoder: StreamEncoder,
    /// Video bit rate in kbit/s; `None` picks one for the size and frame
    /// rate. Not used for `ProRes`.
    #[serde(default)]
    pub video_kbps: Option<u32>,
    /// AAC bit rate in kbit/s (`ProRes` files carry uncompressed sound).
    #[serde(default = "default_audio_kbps")]
    pub audio_kbps: u32,
    /// Picture size; `None` is Varda's own.
    #[serde(default)]
    pub size: Option<(u32, u32)>,
    /// Frames per second; `None` is Varda's own.
    #[serde(default)]
    pub fps: Option<u32>,
}

impl Default for RecordOptions {
    fn default() -> Self {
        Self {
            name: default_name(),
            source: master(),
            audio_bus: None,
            folder: default_folder(),
            format: RecordFormat::Mp4,
            codec: RecordCodec::H264,
            encoder: StreamEncoder::preferred(),
            video_kbps: None,
            audio_kbps: default_audio_kbps(),
            size: None,
            fps: None,
        }
    }
}

impl RecordOptions {
    /// The options made consistent: `ProRes` only in MOV, numbers in range.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        self.name = recording_name(&self.name, "Nida");
        if !self.codec.has_bit_rate() {
            self.format = RecordFormat::Mov;
        }
        self.video_kbps = self.video_kbps.map(|k| k.clamp(1_000, 200_000));
        self.audio_kbps = self.audio_kbps.clamp(96, 320);
        self.size = self
            .size
            .map(|(w, h)| ((w.clamp(160, 7680) / 2) * 2, (h.clamp(90, 4320) / 2) * 2));
        self.fps = self.fps.map(|f| f.clamp(10, 120));
        self
    }

    /// The bit rate used: the one set, or one for the picture — about 12
    /// Mbit/s for 1080p at 30 fps with H.264, scaled by pixels and frame
    /// rate, and 60% of that for HEVC.
    pub fn effective_kbps(&self, (width, height): (u32, u32), fps: u32) -> u32 {
        if let Some(kbps) = self.video_kbps {
            return kbps;
        }
        let pixels = f64::from(width) * f64::from(height) / (1920.0 * 1080.0);
        let motion = (f64::from(fps) / 30.0).sqrt();
        let codec = if self.codec == RecordCodec::Hevc {
            0.6
        } else {
            1.0
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let kbps = (12_000.0 * pixels * motion * codec).round() as u32;
        kbps.clamp(2_000, 150_000)
    }

    /// Whether the recording can take NV12 frames made on the GPU: H.264 and
    /// HEVC can; `ProRes` takes RGBA.
    pub(crate) fn takes_nv12(&self) -> bool {
        self.codec.video_codec().is_some()
    }

    /// ffmpeg's video, sound and container arguments, after the inputs:
    /// input 0 is RGBA frames at `in_fps`, input 1 the sound.
    #[cfg(test)]
    pub(crate) fn output_args(&self, size: (u32, u32), in_fps: u32) -> Vec<String> {
        self.output_args_for(size, in_fps, false)
    }

    /// [`Self::output_args`], input 0 NV12 when `nv12`.
    pub(crate) fn output_args_for(&self, size: (u32, u32), in_fps: u32, nv12: bool) -> Vec<String> {
        let fps = self.fps.unwrap_or(in_fps).max(1);
        let mut args: Vec<String> = vec!["-r".into(), fps.to_string()];
        let tags = [
            "-color_primaries",
            "bt709",
            "-color_trc",
            "bt709",
            "-colorspace",
            "bt709",
            "-color_range",
            "tv",
        ];
        if let Some(codec) = self.codec.video_codec() {
            let (encoder, constant) = self.encoder.resolved(codec);
            // A keyframe a second: easy to cut, and what fragments close on.
            // A recording keeps its average rather than a constant rate.
            let _ = constant;
            args.extend(encoder.video_args(codec, self.effective_kbps(size, fps), fps, false));
            args.extend(["-pix_fmt", encoder.pixel_format(codec, nv12)].map(str::to_string));
        } else {
            let profile = if self.codec == RecordCodec::ProResHq {
                "hq"
            } else {
                "standard"
            };
            if self.encoder == StreamEncoder::Hardware && prores_hardware() {
                // BGRA straight in: the media engine does the 4:2:2.
                args.extend(
                    [
                        "-c:v",
                        "prores_videotoolbox",
                        "-realtime",
                        "1",
                        "-profile:v",
                        profile,
                        "-pix_fmt",
                        "bgra",
                    ]
                    .map(str::to_string),
                );
            } else {
                let profile = if self.codec == RecordCodec::ProResHq {
                    "3"
                } else {
                    "2"
                };
                args.extend(
                    [
                        "-c:v",
                        "prores_ks",
                        "-profile:v",
                        profile,
                        "-pix_fmt",
                        "yuv422p10le",
                    ]
                    .map(str::to_string),
                );
            }
        }
        args.extend(tags.map(str::to_string));
        // Every packet to disk as it is made, not held in a buffer: what a
        // crash can lose is what the OS has not written yet, not seconds of
        // picture sitting in ffmpeg.
        args.extend(["-flush_packets", "1"].map(str::to_string));
        match self.format {
            RecordFormat::Mp4 => args.extend(
                [
                    "-movflags",
                    "+frag_keyframe+empty_moov+default_base_moof",
                    "-f",
                    "mp4",
                ]
                .map(str::to_string),
            ),
            RecordFormat::Mov => args.extend(["-f", "mov"].map(str::to_string)),
            RecordFormat::Mkv => args.extend(["-f", "matroska"].map(str::to_string)),
        }
        args
    }

    /// The sound codec arguments: uncompressed beside `ProRes`, AAC otherwise.
    pub(crate) fn audio_codec_args(&self) -> Vec<String> {
        if self.codec.has_bit_rate() {
            vec![
                "-c:a".into(),
                "aac".into(),
                "-b:a".into(),
                format!("{}k", self.audio_kbps),
            ]
        } else {
            vec!["-c:a".into(), "pcm_s16le".into()]
        }
    }
}

/// Whether the media engine takes `ProRes` here. Asked once per run — at
/// launch, on a thread of its own (see `streaming::try_encoders_meanwhile`).
pub(crate) fn prores_hardware() -> bool {
    static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *HAS.get_or_init(|| {
        cfg!(target_os = "macos")
            && std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi"])
                .args(["-i", "testsrc2=size=320x180:rate=25", "-frames:v", "3"])
                .args([
                    "-pix_fmt",
                    "bgra",
                    "-c:v",
                    "prores_videotoolbox",
                    "-f",
                    "null",
                    "-",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
    })
}

/// Everything about recording saved with the stage.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct RecordSettings {
    #[serde(default)]
    pub options: RecordOptions,
}

/// How the recording is doing.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct RecordStatus {
    pub recording: bool,
    /// Seconds since Record was pressed.
    pub secs: f32,
    /// The file being written now.
    pub file: Option<String>,
    /// Every file of this take, in order — more than one if the encoder had
    /// to start again.
    pub files: Vec<String>,
    /// Bytes written to the current file so far.
    pub bytes: u64,
    /// Free space where the files go, in bytes.
    pub free_bytes: Option<u64>,
    /// What is wrong, when something is.
    pub problem: Option<String>,
    /// Frames the encoder could not take; each is covered by repeating the
    /// one before.
    pub dropped: u64,
    /// The rate frames reach the encoder at, as it was started.
    #[serde(default)]
    pub fps: u32,
    /// The last finished recording, for "Show in Finder".
    pub last_file: Option<String>,
}

impl RecordStatus {
    pub fn idle(last_file: Option<String>) -> Self {
        Self {
            recording: false,
            secs: 0.0,
            file: None,
            files: Vec::new(),
            bytes: 0,
            free_bytes: None,
            problem: None,
            dropped: 0,
            fps: 0,
            last_file,
        }
    }

    /// Recording, and nothing wrong — the REC button's red.
    pub fn is_healthy(&self) -> bool {
        self.recording && self.problem.is_none()
    }
}

/// Recording as the API and state show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct RecordSnapshot {
    pub options: RecordOptions,
    pub status: RecordStatus,
}

impl Default for RecordSnapshot {
    fn default() -> Self {
        Self {
            options: RecordOptions::default(),
            status: RecordStatus::idle(None),
        }
    }
}

/// Below this much free space the recording is flagged: at 40 Mbit/s five
/// gigabytes is about seventeen minutes.
pub const LOW_SPACE_BYTES: u64 = 5 * 1024 * 1024 * 1024;

/// A new file's path: `Nida 2026-09-26 14-05-09.mp4` in `folder`, with
/// ` (2)`, ` (3)` … when a take continues in more files or the name is taken.
pub fn file_path(folder: &Path, name: &str, stamp: &str, part: usize, extension: &str) -> PathBuf {
    let name = recording_name(name, "Nida");
    let base = if part > 1 {
        format!("{name} {stamp} ({part})")
    } else {
        format!("{name} {stamp}")
    };
    let mut path = folder.join(format!("{base}.{extension}"));
    let mut n = 2;
    while path.exists() {
        path = folder.join(format!("{base} {n}.{extension}"));
        n += 1;
    }
    path
}

/// The local date and time, as file names carry it: `2026-09-26 14-05-09`.
pub fn timestamp_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    local_timestamp(secs)
}

#[cfg(unix)]
fn local_timestamp(secs: u64) -> String {
    // Safety: `localtime_r` fills the `tm` we own from a valid time.
    unsafe {
        let t = libc::time_t::try_from(secs).unwrap_or_default();
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&raw const t, &raw mut tm).is_null() {
            return utc_timestamp(secs);
        }
        format!(
            "{:04}-{:02}-{:02} {:02}-{:02}-{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

#[cfg(not(unix))]
fn local_timestamp(secs: u64) -> String {
    utc_timestamp(secs)
}

/// Days since 1970 → civil date (Howard Hinnant's algorithm), for systems
/// without `localtime_r`.
fn utc_timestamp(secs: u64) -> String {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}-{:02}-{:02}",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Free bytes on the disk holding `path` — or its nearest folder that
/// exists, before the recording folder is made — as `df` gives them:
/// available to anyone writing there. Asked of that file system alone, in
/// microseconds: listing every disk, as this once did, took 20 ms, and every
/// two seconds of a recording a show frame came late — a TV output showed
/// one twice (a laptop, 2026-10-04).
pub fn free_space(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    available_on(existing)
}

/// How often a take's file size and the free space are looked up.
const DISK_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// Free space not known yet, or not found.
const UNKNOWN: u64 = u64::MAX;

/// A take's file size and the free space where it goes, looked up every
/// [`DISK_CHECK`] on a thread of its own and read from there at once: a disk
/// that does not answer — a network share whose server went away, a drive
/// waking — holds only that thread, never the show's frames, as asking on
/// the engine thread would. Dropped, it stops.
pub struct DiskWatch {
    file: std::sync::mpsc::Sender<PathBuf>,
    seen: std::sync::Arc<Seen>,
}

#[derive(Default)]
struct Seen {
    bytes: std::sync::atomic::AtomicU64,
    free: std::sync::atomic::AtomicU64,
    /// The take stopped: nothing more is looked up for it.
    stopped: std::sync::atomic::AtomicBool,
}

impl DiskWatch {
    /// Watch the disk holding `folder`.
    pub fn start(folder: PathBuf) -> Self {
        Self::start_with(folder, DISK_CHECK, |folder, file| {
            let bytes = file
                .and_then(|p| std::fs::metadata(p).ok())
                .map_or(0, |m| m.len());
            (bytes, free_space(folder))
        })
    }

    /// Watch with `look`, which gives the size of the take's file (none
    /// yet: 0) and the free space, every `every`.
    fn start_with(
        folder: PathBuf,
        every: std::time::Duration,
        look: impl Fn(&Path, Option<&Path>) -> (u64, Option<u64>) + Send + 'static,
    ) -> Self {
        use std::sync::atomic::Ordering::{Acquire, Relaxed};
        use std::sync::mpsc::{RecvTimeoutError, TryRecvError};
        let (file, files) = std::sync::mpsc::channel::<PathBuf>();
        let seen = std::sync::Arc::new(Seen {
            bytes: 0.into(),
            free: UNKNOWN.into(),
            stopped: false.into(),
        });
        let found = std::sync::Arc::clone(&seen);
        let spawned = std::thread::Builder::new()
            .name("disk-watch".into())
            .spawn(move || {
                let mut current: Option<PathBuf> = None;
                // Each look only for a take still going: one that hangs
                // after it stopped would hold this thread for nothing.
                while !found.stopped.load(Acquire) {
                    let (bytes, free) = look(&folder, current.as_deref());
                    found.bytes.store(bytes, Relaxed);
                    found.free.store(free.unwrap_or(UNKNOWN), Relaxed);
                    match files.recv_timeout(every) {
                        Ok(path) => current = Some(path),
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                    // The newest file sent, unless the take has stopped.
                    loop {
                        match files.try_recv() {
                            Ok(path) => current = Some(path),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => return,
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            log::warn!("The recording's disk is not watched: {e}");
        }
        Self { file, seen }
    }

    /// The take goes on in `path` now.
    pub fn file(&self, path: PathBuf) {
        let _ = self.file.send(path);
    }

    /// The take's file's size, as last found.
    pub fn bytes(&self) -> u64 {
        self.seen.bytes.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The free space, as last found.
    pub fn free(&self) -> Option<u64> {
        Some(self.seen.free.load(std::sync::atomic::Ordering::Relaxed)).filter(|&b| b != UNKNOWN)
    }
}

impl Drop for DiskWatch {
    fn drop(&mut self) {
        self.seen
            .stopped
            .store(true, std::sync::atomic::Ordering::Release);
    }
}

/// On macOS by `statfs`: `statvfs` there counts blocks in 32 bits, which
/// cannot say more than 16 TiB free.
#[cfg(target_os = "macos")]
fn available_on(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: a NUL-terminated path, and a valid place for the answer.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statfs` succeeded, so it filled the answer in.
    let stat = unsafe { stat.assume_init() };
    Some(stat.f_bavail.saturating_mul(u64::from(stat.f_bsize)))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn available_on(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: a NUL-terminated path, and a valid place for the answer.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: `statvfs` succeeded, so it filled the answer in.
    let stat = unsafe { stat.assume_init() };
    // The field types differ between platforms; both fit in a u64.
    #[allow(clippy::useless_conversion, clippy::unnecessary_cast)]
    Some(u64::from(stat.f_bavail).saturating_mul(stat.f_frsize as u64))
}

#[cfg(not(unix))]
fn available_on(path: &Path) -> Option<u64> {
    let disks = sysinfo::Disks::new_with_refreshed_list();
    disks
        .list()
        .iter()
        .filter(|d| path.starts_with(d.mount_point()))
        .max_by_key(|d| d.mount_point().as_os_str().len())
        .map(sysinfo::Disk::available_space)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_takes_reserve_distinct_absolute_files_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("new folder").join("Clean.mp4");
        std::fs::create_dir_all(configured.parent().unwrap()).unwrap();
        std::fs::write(&configured, b"existing recording").unwrap();
        let first = OutputRecordingFile::reserve(&configured).unwrap();
        let second = OutputRecordingFile::reserve(&configured).unwrap();
        assert!(first.path().is_absolute());
        assert_ne!(first.path(), second.path());
        assert_eq!(first.path().extension().unwrap(), "mp4");
        assert_eq!(std::fs::read(&configured).unwrap(), b"existing recording");
        let path = first.path().to_path_buf();
        first.keep();
        assert!(path.exists(), "successful starts retain their reservation");
        let abandoned = second.path().to_path_buf();
        drop(second);
        assert!(
            !abandoned.exists(),
            "failed starts remove their reservation"
        );
    }

    #[test]
    fn output_take_creates_parents_and_rejects_empty_paths() {
        let dir = tempfile::tempdir().unwrap();
        let file = OutputRecordingFile::reserve(&dir.path().join("new/takes/PGM.mov")).unwrap();
        assert!(file.path().exists());
        assert!(OutputRecordingFile::reserve(Path::new("")).is_err());
    }

    #[test]
    fn recording_names_are_safe_and_old_settings_default_to_nida() {
        assert_eq!(recording_name("  Žinių studija. ", "Nida"), "Žinių studija");
        assert_eq!(recording_name("../bad\\name:*?", "Nida"), "_bad_name___");
        assert_eq!(recording_name(" . ", "Nida"), "Nida");
        let options: RecordOptions = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(options.name, "Nida");
        let dir = tempfile::tempdir().unwrap();
        let path = file_path(dir.path(), "Clean feed", "2026-10-18 21-30-00", 1, "mp4");
        assert_eq!(
            path.file_name().unwrap(),
            "Clean feed 2026-10-18 21-30-00.mp4"
        );
        let saved = serde_json::to_value(RecordOptions {
            name: "Lietuviškas įrašas".into(),
            ..RecordOptions::default()
        })
        .unwrap();
        assert_eq!(
            serde_json::from_value::<RecordOptions>(saved).unwrap().name,
            "Lietuviškas įrašas"
        );
    }

    /// The free space where a recording goes is found at once — the frame
    /// it is asked in must not come late — and found for a folder not made
    /// yet, by the folder it will go in.
    #[test]
    fn free_space_is_found_at_once_even_before_the_folder_is_made() {
        let dir = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        for _ in 0..100 {
            assert!(free_space(dir.path()).is_some_and(|b| b > 0));
        }
        let each = started.elapsed() / 100;
        assert!(each < std::time::Duration::from_millis(2), "{each:?} each");
        let later = dir.path().join("takes").join("Nida");
        assert_eq!(free_space(&later).map(|b| b > 0), Some(true));
    }

    /// While the disk does not answer, the take's size and the free space
    /// are read at once, as last found — the watch's own thread waits, not
    /// the show's frame that asks — and a new file of the take is looked at
    /// once the disk answers again.
    #[test]
    fn a_disk_that_does_not_answer_holds_only_its_watch() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use std::sync::{mpsc, Arc, Mutex};
        use std::time::{Duration, Instant};
        let (answer, answers) = mpsc::channel::<()>();
        let answers = Mutex::new(answers);
        let looks = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&looks);
        let watch = DiskWatch::start_with(
            PathBuf::from("/Volumes/Share"),
            Duration::from_millis(5),
            move |_, file| {
                // The first look is answered; each after waits for an answer,
                // two seconds at most.
                if counted.fetch_add(1, SeqCst) > 0 {
                    let _ = answers.lock().unwrap().recv_timeout(Duration::from_secs(2));
                }
                (file.map_or(0, |_| 7_000), Some(42))
            },
        );
        let until = Instant::now() + Duration::from_secs(5);
        while looks.load(SeqCst) < 2 {
            assert!(Instant::now() < until, "the watch never looked again");
            std::thread::yield_now();
        }
        // The second look hangs now.
        let asked = Instant::now();
        watch.file(PathBuf::from("/Volumes/Share/Nida.mp4"));
        for _ in 0..1_000 {
            assert_eq!((watch.bytes(), watch.free()), (0, Some(42)));
        }
        assert!(asked.elapsed() < Duration::from_millis(50), "{:?}", asked.elapsed());
        // Answered, the next look finds the file.
        while watch.bytes() != 7_000 {
            let _ = answer.send(());
            assert!(Instant::now() < until, "the new file was never looked at");
            std::thread::sleep(Duration::from_millis(1));
        }
        drop(watch);
        drop(answer);
    }

    /// A take stopped while the disk is being looked at is not looked at
    /// again — not even for the file it had just gone on in: a disk that
    /// stopped answering must not hold a thread for a take long gone.
    #[test]
    fn a_stopped_take_is_not_looked_at_again() {
        use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
        use std::sync::{mpsc, Arc, Mutex};
        use std::time::{Duration, Instant};
        let (answer, answers) = mpsc::channel::<()>();
        let answers = Mutex::new(answers);
        let looks = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&looks);
        let watch = DiskWatch::start_with(
            PathBuf::from("/Volumes/Share"),
            Duration::from_millis(5),
            move |_, _| {
                counted.fetch_add(1, SeqCst);
                let _ = answers.lock().unwrap().recv_timeout(Duration::from_secs(2));
                (0, Some(1))
            },
        );
        let until = Instant::now() + Duration::from_secs(5);
        while looks.load(SeqCst) < 1 {
            assert!(Instant::now() < until, "the watch never looked");
            std::thread::yield_now();
        }
        // While the first look waits, the take goes on in a file, and stops.
        watch.file(PathBuf::from("/Volumes/Share/Nida.mp4"));
        drop(watch);
        answer.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(looks.load(SeqCst), 1, "looked again after the take stopped");
    }

    #[test]
    fn prores_goes_in_mov_and_numbers_stay_in_range() {
        let o = RecordOptions {
            codec: RecordCodec::ProRes,
            format: RecordFormat::Mp4,
            video_kbps: Some(10),
            size: Some((1921, 99_999)),
            ..RecordOptions::default()
        }
        .normalized();
        assert_eq!(o.format, RecordFormat::Mov);
        assert_eq!(o.video_kbps, Some(1_000));
        assert_eq!(o.size, Some((1920, 4320)));
    }

    #[test]
    fn the_automatic_bit_rate_follows_size_frame_rate_and_codec() {
        let h264 = RecordOptions::default();
        assert_eq!(h264.effective_kbps((1920, 1080), 30), 12_000);
        let at_4k60 = h264.effective_kbps((3840, 2160), 60);
        assert!((66_000..=70_000).contains(&at_4k60), "{at_4k60}");
        let hevc = RecordOptions {
            codec: RecordCodec::Hevc,
            ..RecordOptions::default()
        };
        assert_eq!(hevc.effective_kbps((1920, 1080), 30), 7_200);
        let set = RecordOptions {
            video_kbps: Some(25_000),
            ..RecordOptions::default()
        };
        assert_eq!(
            set.effective_kbps((3840, 2160), 60),
            25_000,
            "a set rate wins"
        );
    }

    #[test]
    fn mp4_is_written_to_survive_a_crash() {
        let args = RecordOptions::default()
            .output_args((1920, 1080), 50)
            .join(" ");
        assert!(args.contains("+frag_keyframe+empty_moov"), "{args}");
        assert!(
            args.contains("-r 50") && args.contains("-g 50"),
            "a keyframe a second: {args}"
        );
        let mkv = RecordOptions {
            format: RecordFormat::Mkv,
            fps: Some(25),
            ..RecordOptions::default()
        }
        .output_args((1920, 1080), 50)
        .join(" ");
        assert!(
            mkv.contains("-f matroska") && mkv.contains("-r 25"),
            "{mkv}"
        );
    }

    #[test]
    fn prores_carries_uncompressed_sound_and_the_rest_aac() {
        let prores = RecordOptions {
            codec: RecordCodec::ProResHq,
            ..RecordOptions::default()
        }
        .normalized();
        assert_eq!(prores.audio_codec_args(), ["-c:a", "pcm_s16le"]);
        let args = prores.output_args((1920, 1080), 25).join(" ");
        assert!(args.contains("prores") && args.contains("-f mov"), "{args}");
        assert_eq!(
            RecordOptions::default().audio_codec_args(),
            ["-c:a", "aac", "-b:a", "256k"]
        );
    }

    #[test]
    fn files_are_named_by_time_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let first = file_path(dir.path(), "Nida", "2026-09-26 14-05-09", 1, "mp4");
        assert_eq!(first.file_name().unwrap(), "Nida 2026-09-26 14-05-09.mp4");
        std::fs::write(&first, b"x").unwrap();
        let again = file_path(dir.path(), "Nida", "2026-09-26 14-05-09", 1, "mp4");
        assert_eq!(again.file_name().unwrap(), "Nida 2026-09-26 14-05-09 2.mp4");
        let part = file_path(dir.path(), "Nida", "2026-09-26 14-05-09", 2, "mp4");
        assert_eq!(
            part.file_name().unwrap(),
            "Nida 2026-09-26 14-05-09 (2).mp4"
        );
    }

    #[test]
    fn the_utc_fallback_writes_a_real_date() {
        // 2026-09-26 12:34:56 UTC.
        assert_eq!(utc_timestamp(1_790_426_096), "2026-09-26 12-34-56");
        let now = timestamp_now();
        assert_eq!(now.len(), "2026-09-26 12-34-56".len(), "{now}");
    }
}
