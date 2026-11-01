//! Camera capture manager — shared camera sessions for N deck consumers.
//!
//! One `CameraManager` owns all camera capture sessions. Each physical camera
//! produces one shared GPU texture that any number of decks can read from.
//!
//! Capture runs on a dedicated thread per camera. On a Mac the devices come
//! straight from `AVFoundation` ([`avf`]); elsewhere from nokhwa, whose
//! YUV frames are turned into RGBA with SIMD-accelerated yuvutils-rs (MJPEG
//! with nokhwa's own decoder).

#[cfg(target_os = "macos")]
mod avf;
#[cfg(target_os = "macos")]
mod blackmagic;

use anyhow::{Context, Result};
#[cfg(not(target_os = "macos"))]
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::utils::{CameraFormat, CameraIndex, FrameFormat};
#[cfg(not(target_os = "macos"))]
use nokhwa::utils::{RequestedFormat, RequestedFormatType};
#[cfg(not(target_os = "macos"))]
use nokhwa::Camera;
use crate::engine::value::camera::{CameraColour, CameraMatrix, CameraProfile, CameraRange};
use crate::frame_picker::FramePicker;
use crate::renderer::camera_profile::{ConvertedPicture, ProfilePasses};
use crate::renderer::context::GpuContext;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use yuvutils_rs::YuvStandardMatrix;
#[cfg(not(target_os = "macos"))]
use yuvutils_rs::{
    yuv_nv12_to_rgba, yuyv422_to_rgba, YuvBiPlanarImage, YuvConversionMode, YuvPackedImage,
    YuvRange,
};

/// A camera, as Nida numbers it: given the first time the device is seen and
/// kept for it while Nida runs, however devices come and go around it.
pub type CameraId = u32;

/// The camera of a deck waiting for its device — not plugged in, or not
/// allowed yet — when the show was opened. It shows black until the device
/// is found by name and opened (see `VardaApp::reconcile_cameras`).
pub const UNBOUND_CAMERA: CameraId = CameraId::MAX;

pub use crate::engine::value::camera::CameraMode;

/// Information about a detected camera device.
#[derive(Debug, Clone)]
pub struct CameraDeviceInfo {
    pub id: CameraId,
    pub name: String,
    /// What tells this device from another of the same model: macOS's
    /// unique ID for it, the backend's device path elsewhere.
    pub unique_id: String,
    pub index: CameraIndex,
    /// Every format the device offers, as nokhwa lists them (a size and rate
    /// can come in several pixel formats), its rates cut down to whole
    /// numbers: what a camera is opened with, off a Mac. Empty on a Mac, and
    /// when they could not be read.
    pub formats: Vec<CameraFormat>,
    /// Every size, pixel format and rate on offer, at the rates the device
    /// really runs at.
    pub rates: Vec<Rate>,
    /// It takes the format that comes in, whatever it was opened at (a
    /// Blackmagic input that tells it): there is no format to choose.
    pub follows_signal: bool,
}

impl CameraDeviceInfo {
    /// The sizes and rates on offer, largest and fastest first.
    pub fn modes(&self) -> Vec<CameraMode> {
        Self::modes_of(&self.rates)
    }

    pub(crate) fn modes_of(rates: &[Rate]) -> Vec<CameraMode> {
        crate::engine::value::camera::sorted_modes(rates.iter().map(Rate::mode))
    }
}

/// A size and frame rate a device offers in one of its pixel formats, at
/// the rate it really runs at — 59.94, or 59.9999 for a 60 that is not
/// quite. The backend lists rates cut down to whole numbers and then refuses
/// one a whole frame off the device's own (59.9999 listed as 59, then
/// refused), so a camera is asked for its rate from these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rate {
    pub width: u32,
    pub height: u32,
    pub format: FrameFormat,
    pub fps: f64,
    /// The device's own code for its pixel format (a four-character code
    /// on a Mac); 0 where there is none.
    pub code: u32,
}

impl Rate {
    /// A backend's format, where there is nothing finer to read.
    #[cfg(not(target_os = "macos"))]
    fn of(format: &CameraFormat) -> Self {
        Self {
            width: format.resolution().width_x,
            height: format.resolution().height_y,
            format: format.format(),
            fps: f64::from(format.frame_rate()),
            code: 0,
        }
    }

    /// As a person reads it: "1920×1080 60p".
    #[cfg(target_os = "macos")]
    pub(crate) fn label(&self) -> String {
        self.mode().label()
    }

    /// As Nida lists it: see [`listed_fps`].
    fn mode(&self) -> CameraMode {
        CameraMode {
            width: self.width,
            height: self.height,
            fps: listed_fps(self.fps),
        }
    }

    /// What to ask the device for to get this: the rate to the nearest whole
    /// frame, which the backend takes within a frame of the device's own.
    #[cfg(any(not(target_os = "macos"), test))]
    fn request(&self) -> CameraFormat {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let fps = self.fps.round().max(1.0) as u32;
        CameraFormat::new(
            nokhwa::utils::Resolution::new(self.width, self.height),
            self.format,
            fps,
        )
    }
}

/// A rate as Nida lists and saves it: a whole number of frames when it is
/// one, near enough (59.9999 is 60), and cut down when it is a broadcast
/// rate — 59.94 is kept as 59 and shown as 59.94 (see
/// [`CameraMode::label`]).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn listed_fps(fps: f64) -> u32 {
    if (fps - fps.round()).abs() < 0.02 {
        fps.round().max(0.0) as u32
    } else {
        fps.trunc().max(0.0) as u32
    }
}

/// The rate to ask a device for to get `mode`: that size and rate — or,
/// when the device no longer has it (a camera set from 59.94 to 60 since
/// the show was saved), the nearest within a frame and a half — in the pixel
/// format cheapest to turn into a picture.
fn target(rates: &[Rate], mode: CameraMode) -> Option<Rate> {
    let cost = |r: &Rate| match r.format {
        FrameFormat::NV12 => 0,
        FrameFormat::YUYV => 1,
        FrameFormat::MJPEG => 2,
        _ => 3,
    };
    let sized = || {
        rates
            .iter()
            .filter(move |r| r.width == mode.width && r.height == mode.height)
    };
    sized()
        .filter(|r| listed_fps(r.fps) == mode.fps)
        .min_by_key(|r| cost(r))
        .or_else(|| {
            sized()
                .filter(|r| (r.fps - f64::from(mode.fps)).abs() <= 1.5)
                .min_by(|a, b| {
                    let off = |r: &Rate| (r.fps - f64::from(mode.fps)).abs();
                    off(a)
                        .total_cmp(&off(b))
                        .then_with(|| cost(a).cmp(&cost(b)))
                })
        })
        .copied()
}

/// The rate to open a camera at for `mode`: that one (see [`target`]), or
/// when there is none — no mode chosen, or one the device no longer offers —
/// the device's best.
pub(crate) fn choose(rates: &[Rate], mode: Option<CameraMode>) -> Option<Rate> {
    mode.and_then(|m| target(rates, m)).or_else(|| {
        crate::engine::value::camera::default_mode(&CameraDeviceInfo::modes_of(rates))
            .and_then(|m| target(rates, m))
    })
}

/// The rates to list from one of a device's ranges of rates: its fastest,
/// and where it runs at any rate between two, the usual rates between them
/// and its slowest when that is a rate anyone would choose.
#[cfg(any(target_os = "macos", test))]
pub(crate) fn rates_in_range(slowest: f64, fastest: f64) -> Vec<f64> {
    let mut rates = vec![fastest];
    if fastest - slowest > 1e-3 {
        rates.extend(
            [60.0, 50.0, 30.0, 25.0, 24.0]
                .into_iter()
                .filter(|usual| *usual > slowest + 1e-3 && *usual < fastest - 1e-3),
        );
        if slowest >= 10.0 {
            rates.push(slowest);
        }
    }
    rates
}

/// The colour matrix of a picture not tagged with one: BT.601 below 720
/// lines (SD), BT.709 from there.
pub(crate) fn untagged_matrix(height: u32) -> YuvStandardMatrix {
    if height < 720 {
        YuvStandardMatrix::Bt601
    } else {
        YuvStandardMatrix::Bt709
    }
}

/// The range a camera's picture is read in: the one set by hand in
/// `colour`, or else `said` — what its pixel format says.
pub(crate) fn picture_range(
    colour: CameraColour,
    said: yuvutils_rs::YuvRange,
) -> yuvutils_rs::YuvRange {
    match colour.range {
        CameraRange::Auto => said,
        CameraRange::Limited => yuvutils_rs::YuvRange::Limited,
        CameraRange::Full => yuvutils_rs::YuvRange::Full,
    }
}

/// The matrix a camera's colours are read with: the one set by hand in
/// `colour`, or else `said` — its tag, or what its size implies.
pub(crate) fn picture_matrix(colour: CameraColour, said: YuvStandardMatrix) -> YuvStandardMatrix {
    match colour.matrix {
        CameraMatrix::Auto => said,
        CameraMatrix::Bt601 => YuvStandardMatrix::Bt601,
        CameraMatrix::Bt709 => YuvStandardMatrix::Bt709,
        CameraMatrix::Bt2020 => YuvStandardMatrix::Bt2020,
    }
}

/// A camera's picture, RGBA, as its capture left it.
pub(crate) struct Frame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Where a camera's capture leaves its pictures for the render loop, each
/// with when it was captured, and hands back the memory of pictures done
/// with, so capture does not allocate a frame's worth every frame.
///
/// The render loop picks the picture for each show frame by when it was
/// captured (see `crate::frame_picker`): taken as the newest to have come,
/// a camera at the show's rate whose pictures came near the moment show
/// frames were drawn showed some twice and others never — a judder whenever
/// someone moved in front of it (an ATEM, a laptop, 2026-10-02).
pub(crate) struct Sink {
    pictures: Mutex<FramePicker<Frame>>,
    spare: Mutex<Vec<Vec<u8>>>,
    frames: AtomicU64,
    /// When its pictures came (see [`Arrivals`]).
    arrivals: Mutex<Arrivals>,
    /// The unique ID its device came back under, plugged into another
    /// port (see [`moved_device`]), till the camera manager takes it.
    moved_to: Mutex<Option<String>>,
    /// When its pictures last stopped and it was started again.
    stopped: Mutex<Option<Instant>>,
    /// Asked to close and open again (a Reset input), till its capture
    /// does.
    restart: AtomicBool,
    /// A format asked for since it was opened — `None` within, its best —
    /// taken as it opens again (see [`Sink::ask_mode`]): none asked, its
    /// best asked for, or a format.
    #[allow(clippy::option_option)]
    mode: Mutex<Option<Option<CameraMode>>>,
    /// A Blackmagic input's sound (see [`Sink::put_sound`]).
    sound: Mutex<Sound>,
    /// Show frames since it opened that showed its picture before again,
    /// and its pictures left out (see `CameraSignal::shown_again`).
    shown_again: AtomicU64,
    left_out: AtomicU64,
    /// This sink's own number: the mixer tells a camera opened anew by it.
    sound_key: u64,
    /// How the capture reads the camera's picture into colours (see
    /// [`CameraColour::to_bits`]), read with every frame.
    colour: AtomicU8,
    /// "camera N", for the trace.
    label: OnceLock<String>,
    /// Show the newest picture at each pick (see
    /// [`FramePicker::set_newest`]).
    newest: AtomicBool,
    /// What its pictures say of the signal, lately (see
    /// [`Sink::note_signal`]).
    signal: Mutex<Option<SignalSeen>>,
}

/// What a camera's pictures say of its signal, as its capture reads them
/// (see [`Sink::note_signal`]).
#[derive(Debug, Clone, Default)]
pub(crate) struct SignalSeen {
    pub width: u32,
    pub height: u32,
    /// The pixel format, as macOS names it.
    pub format: String,
    /// The colour matrix read with.
    pub matrix: String,
    /// Read as full range; none for RGB.
    pub full_range: Option<bool>,
    /// Brightness samples below video black, and all taken, lately: each
    /// look counts a tenth less than the one after it.
    pub below: f64,
    pub sampled: f64,
}

/// How much of a camera's brightness samples to keep at each new look (see
/// [`SignalSeen`]): a few seconds' worth of looks.
const SIGNAL_KEEP: f64 = 0.9;

/// Brightness under this, of 255, is well below video black (16): past the
/// footroom and noise video keeps there.
const VIDEO_BLACK_LOW: u8 = 8;

/// Of a picture's brightness, sampled on a grid of 64 × 36, how many samples
/// are well below video black (see [`VIDEO_BLACK_LOW`]), and how many were
/// taken: each pixel's brightness byte at `first`, a pixel every `step`
/// bytes, rows `stride` apart.
pub(crate) fn below_black(
    bytes: &[u8],
    stride: usize,
    (width, height): (usize, usize),
    (first, step): (usize, usize),
) -> (u32, u32) {
    let (mut below, mut taken) = (0u32, 0u32);
    if width == 0 || height == 0 {
        return (0, 0);
    }
    for gy in 0..36 {
        let y = (gy * 2 + 1) * height / 72;
        for gx in 0..64 {
            let x = (gx * 2 + 1) * width / 128;
            if let Some(&luma) = bytes.get(y * stride + x * step + first) {
                taken += 1;
                below += u32::from(luma < VIDEO_BLACK_LOW);
            }
        }
    }
    (below, taken)
}

/// How a capture called back with a picture, for the trace (see
/// `present_trace::Step::Arrived`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct CallBack {
    /// When it called back, host seconds.
    pub called: f64,
    /// The picture's time was its sample time, not when it came.
    pub stamped: bool,
    /// Its memory was new: none done with was there to take.
    pub fresh: bool,
}

/// A wait between pictures longer than this ends a run of them.
const RUN_GAP: Duration = Duration::from_millis(250);

/// A camera that has sent a run of pictures this long is streaming
/// steadily (see [`Arrivals::steady`]).
const STEADY_RUN: Duration = Duration::from_secs(2);

/// Times in a row a camera started again stays steady from its first
/// picture, with no run of [`STEADY_RUN`] between: an ATEM stalled by ATEM
/// Setup stalls again within two seconds of starting one time in four, and
/// was given the five seconds of a camera just opened (an M5 Max,
/// 2026-10-28). Past it, a camera that cannot keep going is given those.
const CARRIED_RESTARTS: u32 = 5;

/// A camera taken for one that sends as its picture changes, whose pictures
/// then come this long with no wait over [`RUN_GAP`], streams at its rate
/// after all: a pause as it warmed up, or under load, is no sign of the
/// other.
const SPARSE_CLEARED: Duration = Duration::from_secs(10);

/// How long a camera just cleared of sending as it changes is not kept
/// steady through being started again (see [`CARRIED_RESTARTS`]): a source
/// that does after all, at the end of a long animation, is started again
/// once, not over and over.
const CARRY_AFTER_CLEARED: Duration = Duration::from_secs(20);

/// When a camera's pictures came, since it was opened: what tells a camera
/// streaming at its rate — whose pictures stopping is a stall — from a
/// source that sends a picture only as it changes.
#[derive(Debug, Default, Clone, Copy)]
struct Arrivals {
    /// Its last picture since it was opened.
    last: Option<Instant>,
    /// The first picture of the run its last belongs to: pictures with no
    /// wait between them longer than [`RUN_GAP`].
    run_from: Option<Instant>,
    /// The rate it was opened at.
    fps: f64,
    /// It sent a picture after a wait longer than a steady camera of its
    /// rate is given (see [`steady_limit`]): it sends as its picture
    /// changes. Kept through being started again, till a run of
    /// [`SPARSE_CLEARED`].
    sparse: bool,
    /// When it was last opened.
    opened: Option<Instant>,
    /// How long its last opening took to bring its first picture.
    open_cost: Option<Duration>,
    /// It was steady when it was started again: steady again from its
    /// first picture (see [`CARRIED_RESTARTS`]).
    carried: bool,
    /// Times in a row it was started again steady, with no run of
    /// [`STEADY_RUN`] since.
    carried_in_a_row: u32,
    /// When it was last cleared of sending as it changes.
    cleared: Option<Instant>,
}

impl Arrivals {
    /// A picture came at `now`.
    fn saw(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let wait = now.saturating_duration_since(last);
            if wait > self.limit() + Duration::from_millis(300) {
                self.sparse = true;
            }
            if wait > RUN_GAP {
                self.run_from = Some(now);
            }
        } else {
            self.run_from = Some(now);
            self.open_cost = self.opened.map(|at| now.saturating_duration_since(at));
        }
        self.last = Some(now);
        if self.long_run() {
            self.carried_in_a_row = 0;
        }
        if self.sparse && self.run_of(SPARSE_CLEARED) {
            self.sparse = false;
            self.cleared = Some(now);
        }
    }

    /// It is being opened at `fps`, at `now`: no picture since. Steady as
    /// it is started again, it is steady again from its first picture —
    /// at most [`CARRIED_RESTARTS`] times in a row; whether it was is said.
    fn opening(&mut self, fps: f64, now: Instant) -> bool {
        let trusted = self
            .cleared
            .is_none_or(|at| now.saturating_duration_since(at) >= CARRY_AFTER_CLEARED);
        self.carried = self.steady() && trusted && self.carried_in_a_row < CARRIED_RESTARTS;
        if self.carried {
            self.carried_in_a_row += 1;
        }
        self.last = None;
        self.run_from = None;
        self.fps = fps;
        self.opened = Some(now);
        self.carried
    }

    /// Its pictures have been coming with no wait between them longer than
    /// [`RUN_GAP`] for [`STEADY_RUN`] up to its last.
    fn long_run(&self) -> bool {
        self.run_of(STEADY_RUN)
    }

    /// Its pictures have been coming with no wait between them longer than
    /// [`RUN_GAP`] for `span` up to its last.
    fn run_of(&self, span: Duration) -> bool {
        matches!((self.run_from, self.last), (Some(from), Some(last))
            if last.saturating_duration_since(from) >= span)
    }

    /// Whether it is streaming steadily: a long run of pictures (see
    /// [`Arrivals::long_run`]) — or a picture since it was started again
    /// steady — and never one after a long wait.
    fn steady(&self) -> bool {
        !self.sparse && (self.long_run() || (self.carried && self.last.is_some()))
    }

    /// How long it is given without a picture, steady (see
    /// [`steady_limit`]).
    fn limit(&self) -> Duration {
        steady_limit(self.fps, self.open_cost)
    }

    /// How long it is given without a picture as it is now: its limit,
    /// steady; [`STALL`] otherwise.
    fn stall_limit(&self) -> Duration {
        if self.steady() {
            self.limit()
        } else {
            STALL
        }
    }
}

/// A camera's own sound, come with its pictures — a Blackmagic input's.
#[derive(Default)]
struct Sound {
    /// Where it goes: the mixer's feed, once the mixer takes it.
    feed: Option<crossbeam_channel::Sender<crate::audio::PcmChunk>>,
    /// Channels to a frame, once some has come.
    channels: Option<u16>,
    /// Samples thrown away since the last chunk went, the feed full.
    lost: u64,
}

/// The rate a Blackmagic input's sound is taken at.
pub const OWN_SOUND_RATE: u32 = 48_000;

/// Pictures kept waiting for the show at most: 8 MB each at 1080p.
const SINK_PICTURES: usize = 4;

impl Default for Sink {
    fn default() -> Self {
        Self {
            pictures: Mutex::new(FramePicker::keeping(SINK_PICTURES)),
            spare: Mutex::default(),
            frames: AtomicU64::default(),
            arrivals: Mutex::default(),
            moved_to: Mutex::default(),
            stopped: Mutex::default(),
            restart: AtomicBool::new(false),
            mode: Mutex::default(),
            sound: Mutex::default(),
            shown_again: AtomicU64::default(),
            left_out: AtomicU64::default(),
            sound_key: {
                static NEXT: AtomicU64 = AtomicU64::new(1);
                NEXT.fetch_add(1, Ordering::Relaxed)
            },
            colour: AtomicU8::default(),
            label: OnceLock::new(),
            newest: AtomicBool::new(false),
            signal: Mutex::default(),
        }
    }
}

impl Sink {
    /// What a picture says of the signal: its size, pixel format, matrix
    /// and range as read, and `(below, sampled)` of its brightness (see
    /// [`below_black`]), counted with those of the pictures before it.
    pub(crate) fn note_signal(&self, mut seen: SignalSeen, (below, sampled): (u32, u32)) {
        if let Ok(mut signal) = self.signal.lock() {
            let before = signal.take().unwrap_or_default();
            seen.below = before.below * SIGNAL_KEEP + f64::from(below);
            seen.sampled = before.sampled * SIGNAL_KEEP + f64::from(sampled);
            *signal = Some(seen);
        }
    }

    /// What the camera sends, as its pictures say, at the rate its capture
    /// last opened at; none before its first picture is read.
    pub(crate) fn signal(&self) -> Option<crate::engine::value::camera::CameraSignal> {
        let seen = self.signal.lock().ok()?.clone()?;
        let fps = self
            .pictures
            .lock()
            .ok()
            .and_then(|pictures| pictures.period())
            .map_or(0.0, |period| 1.0 / period);
        #[allow(clippy::cast_possible_truncation)] // A share.
        let below_black = if seen.sampled > 0.0 {
            (seen.below / seen.sampled) as f32
        } else {
            0.0
        };
        Some(crate::engine::value::camera::CameraSignal {
            width: seen.width,
            height: seen.height,
            fps,
            opened_fps: self
                .arrivals
                .lock()
                .ok()
                .map(|arrivals| arrivals.fps)
                .filter(|fps| *fps > 0.0),
            format: seen.format,
            matrix: seen.matrix,
            full_range: seen.full_range,
            below_black,
            shown_again: self.shown_again.load(Ordering::Relaxed),
            left_out: self.left_out.load(Ordering::Relaxed),
        })
    }

    /// How the camera's pictures are read into colours from the next one.
    pub(crate) fn set_colour(&self, colour: CameraColour) {
        self.colour.store(colour.to_bits(), Ordering::Relaxed);
    }

    /// How the camera's pictures are read into colours.
    pub(crate) fn colour(&self) -> CameraColour {
        CameraColour::from_bits(self.colour.load(Ordering::Relaxed))
    }

    /// Memory for the next picture: one done with, or new.
    pub(crate) fn buffer(&self) -> Vec<u8> {
        self.spare
            .lock()
            .ok()
            .and_then(|mut spare| spare.pop())
            .unwrap_or_default()
    }

    /// Memory done with, for the next picture.
    pub(crate) fn recycle(&self, data: Vec<u8>) {
        if let Ok(mut spare) = self.spare.lock() {
            if spare.len() < 2 {
                spare.push(data);
            }
        }
    }

    /// Name the camera, for the trace.
    pub(crate) fn set_label(&self, label: String) {
        let _ = self.label.set(label);
    }

    /// Show the newest picture at each pick, from the next (see
    /// [`FramePicker::set_newest`]).
    pub(crate) fn set_lowest_latency(&self, on: bool) {
        self.newest.store(on, Ordering::Relaxed);
    }

    /// A picture captured at `at`, host seconds (see `crate::host_clock`).
    /// On a Mac the capture puts its pictures with [`Self::put_called`].
    #[cfg(any(test, not(target_os = "macos")))]
    pub(crate) fn put(&self, frame: Frame, at: f64) {
        self.put_called(frame, at, None);
    }

    /// [`Self::put`], with how the capture called back with it, for the trace.
    pub(crate) fn put_called(&self, frame: Frame, at: f64, call: Option<CallBack>) {
        use crate::renderer::present_trace::{Step, TRACE};
        // The pictures let go are dropped here, the pictures no longer held:
        // the show's frame waits for them (see `take`).
        let let_go = self
            .pictures
            .lock()
            .map(|mut pictures| pictures.push_came(at, crate::host_clock::host_seconds(), frame))
            .unwrap_or_default();
        drop(let_go);
        self.frames.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut arrivals) = self.arrivals.lock() {
            arrivals.saw(Instant::now());
        }
        if TRACE.recording() {
            let input = self.label.get().cloned().unwrap_or_default();
            TRACE.note(
                "",
                0,
                Step::Arrived {
                    input,
                    captured: at,
                    called: call.map(|c| c.called),
                    stamped: call.map(|c| c.stamped),
                    fresh: call.map(|c| c.fresh),
                },
            );
        }
    }

    /// The picture for the show frame due on screen at `show_at`, host
    /// seconds; none to keep showing the one before. Waits only while a
    /// picture is put in — moments: a show frame that went without, the
    /// pictures busy as one came, showed the one before twice and skipped a
    /// picture the next, more often the nearer the camera's frames came to
    /// the show's (a laptop, 2026-10-04).
    fn take(&self, show_at: f64, camera: CameraId) -> Option<Frame> {
        use crate::renderer::present_trace::{Step, TRACE};
        let mut pictures = self.pictures.lock().ok()?;
        pictures.set_newest(self.newest.load(Ordering::Relaxed));
        let (frame, what) = pictures.pick_drawn(show_at, crate::host_clock::host_seconds());
        match what {
            crate::frame_picker::Picked::Repeated => {
                self.shown_again.fetch_add(1, Ordering::Relaxed);
            }
            crate::frame_picker::Picked::Skipped(n) => {
                self.left_out.fetch_add(n as u64, Ordering::Relaxed);
            }
            crate::frame_picker::Picked::Next => {}
        }
        if TRACE.recording() {
            let skipped = match what {
                crate::frame_picker::Picked::Skipped(n) => n,
                _ => 0,
            };
            let step = Step::Picked {
                input: format!("camera {camera}"),
                show_at,
                captured: frame.as_ref().and(pictures.shown()),
                skipped,
                repeated: what == crate::frame_picker::Picked::Repeated,
                delay: pictures.delay(),
                waiting: pictures.waiting(),
            };
            drop(pictures);
            TRACE.note("", 0, step);
        }
        frame
    }

    /// How long before its show frame's time the picture shown last was
    /// captured, seconds. Never waits.
    pub(crate) fn latency(&self) -> Option<f64> {
        self.pictures.try_lock().ok()?.latency()
    }

    /// A copy of the newest picture not yet taken. Never waits.
    fn peek(&self) -> Option<(Vec<u8>, u32, u32)> {
        let pictures = self.pictures.try_lock().ok()?;
        pictures
            .newest()
            .map(|f| (f.data.clone(), f.width, f.height))
    }

    /// How many pictures have come.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// How long since the last picture came; `None` before the first.
    fn since_last_frame(&self) -> Option<Duration> {
        self.arrivals.lock().ok()?.last.map(|at| at.elapsed())
    }

    /// Ask its capture to close and open again (a Reset input).
    fn ask_restart(&self) {
        self.restart.store(true, Ordering::Relaxed);
    }

    /// Ask its capture to open again at `mode` — `None`, its best.
    fn ask_mode(&self, mode: Option<CameraMode>) {
        if let Ok(mut asked) = self.mode.lock() {
            *asked = Some(mode);
        }
        self.ask_restart();
    }

    /// The format asked for since this was last asked, if one was: its
    /// best (`Some(None)`) or a format.
    #[allow(clippy::option_option)]
    fn take_mode(&self) -> Option<Option<CameraMode>> {
        self.mode.lock().ok()?.take()
    }

    /// Sound come with the pictures: `samples` interleaved, `channels` to a
    /// frame, 32-bit, at [`OWN_SOUND_RATE`] — to the mixer, once it takes it;
    /// what a full feed throws away is counted, for it to put back as
    /// silence.
    pub(crate) fn put_sound(&self, samples: &[i32], channels: usize) {
        use crossbeam_channel::TrySendError;
        let Ok(mut sound) = self.sound.lock() else {
            return;
        };
        sound.channels = u16::try_from(channels).ok().filter(|c| *c > 0);
        let Some(feed) = sound.feed.as_ref() else {
            return;
        };
        #[allow(clippy::cast_precision_loss)]
        let chunk = crate::audio::PcmChunk {
            samples: samples
                .iter()
                .map(|&s| s as f32 / 2_147_483_648.0)
                .collect(),
            lost_before: sound.lost,
        };
        match feed.try_send(chunk) {
            Ok(()) => sound.lost = 0,
            Err(TrySendError::Full(_)) => sound.lost += samples.len() as u64,
            Err(TrySendError::Disconnected(_)) => {
                sound.feed = None;
                sound.lost = 0;
            }
        }
    }

    /// Its own sound's rate, channels and key, once some has come.
    fn sound_format(&self) -> Option<(u32, u16, u64)> {
        let channels = self.sound.lock().ok()?.channels?;
        Some((OWN_SOUND_RATE, channels, self.sound_key))
    }

    /// Where its own sound goes from now.
    fn set_sound_feed(&self, feed: crossbeam_channel::Sender<crate::audio::PcmChunk>) {
        if let Ok(mut sound) = self.sound.lock() {
            sound.feed = Some(feed);
            sound.lost = 0;
        }
    }

    /// Whether it was asked to close and open again, since this was last
    /// asked.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn take_restart(&self) -> bool {
        self.restart.swap(false, Ordering::Relaxed)
    }

    /// Its pictures stopped and it is being started again.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn note_stop(&self) {
        if let Ok(mut at) = self.stopped.lock() {
            *at = Some(Instant::now());
        }
    }

    /// Whether its pictures stopped within `within`.
    fn stopped_within(&self, within: Duration) -> bool {
        self.stopped
            .lock()
            .is_ok_and(|at| at.is_some_and(|at| at.elapsed() < within))
    }

    /// The unique ID its device came back under, plugged into another
    /// port, since this was last asked; `None` if it has not moved.
    fn take_move(&self) -> Option<String> {
        self.moved_to.lock().ok()?.take()
    }

    /// How long it is given without a picture as it is now (see
    /// [`Arrivals::stall_limit`]).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn stall_limit(&self) -> Duration {
        self.arrivals.lock().map_or(STALL, |a| a.stall_limit())
    }

    /// The camera is being opened, at `fps` — at first or started again:
    /// its pictures before are the capture's before, and the time it was
    /// out is no wait of its own. Whether it sends as its picture changes
    /// is kept. Whether it stays steady from its first picture is said
    /// (see [`Arrivals::opening`]).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn opening(&self, fps: f64) -> bool {
        // Counted afresh from here, under the pictures' lock: none let go
        // before is put down to this opening.
        if let Ok(mut pictures) = self.pictures.lock() {
            pictures.forget_lost();
            self.shown_again.store(0, Ordering::Relaxed);
            self.left_out.store(0, Ordering::Relaxed);
        }
        self.arrivals
            .lock()
            .is_ok_and(|mut arrivals| arrivals.opening(fps, Instant::now()))
    }
}

/// The camera, among `others` open in Nida, that holds the Blackmagic box
/// camera `name` failed to open on (`why`): the same box as macOS lists
/// it — named after it. None when more than one could be: which, cannot be
/// told.
fn held_by<'a>(name: &str, why: &str, others: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    if !name.ends_with(" (Blackmagic)") || !why.contains("in use") {
        return None;
    }
    let holders: Vec<&str> = others
        .filter(|other| {
            !other.ends_with(" (Blackmagic)") && crate::decklink::input::same_box(name, other)
        })
        .collect();
    match holders.as_slice() {
        [one] => Some(one),
        _ => None,
    }
}

/// How long opening a camera may take before it is given up on: a device
/// that starts at all starts well within it.
const OPEN_WAIT: Duration = Duration::from_secs(8);

/// How long to wait for a camera's thread to stop when its last deck goes. A
/// device stalled in the OS is left to its thread rather than hanging Nida.
const STOP_WAIT: Duration = Duration::from_secs(2);

/// A camera opened sending no picture for this long is started again: one
/// takes a moment to send its first.
const STALL: Duration = Duration::from_secs(5);

/// A camera whose pictures have been coming, gone quiet this many of its
/// frames — [`STEADY_STALL`] at the least — is started again. An ATEM
/// Mini's webcam stalled by ATEM Setup sends no picture until it is started
/// again, which takes it 0.15 s (an M5 Max, 2026-10-28, from macOS's own
/// camera log): each second waited is a second of frozen picture on air.
const STALL_FRAMES: f64 = 8.0;

/// The least a camera streaming steadily is given without a picture.
const STEADY_STALL: Duration = Duration::from_millis(250);

/// How long a camera streaming steadily at `fps`, whose last opening took
/// `open_cost` to bring its first picture, is given without a picture:
/// [`STALL_FRAMES`] of its frames, [`STEADY_STALL`] at least — and its
/// opening's time, for a camera slow to start a stall shorter than that is
/// better waited out — never more than [`STALL`].
fn steady_limit(fps: f64, open_cost: Option<Duration>) -> Duration {
    Duration::from_secs_f64(STALL_FRAMES / fps.max(1.0))
        .max(STEADY_STALL)
        .max(open_cost.unwrap_or_default())
        .min(STALL)
}

/// Whether a camera has stalled: `since_open` since it was opened,
/// `since_frame` since its last picture, given `limit` (see
/// [`Arrivals::stall_limit`]).
#[cfg(any(target_os = "macos", test))]
fn stalled(since_open: Duration, since_frame: Option<Duration>, limit: Duration) -> bool {
    since_frame.map_or(since_open, |since| since.min(since_open)) > limit
}

/// How often a camera gone quiet or unplugged is tried again.
#[cfg(target_os = "macos")]
const RETRY_EVERY: Duration = Duration::from_secs(2);

/// The formats to try opening a camera with, in turn: the one nearest the
/// rate wanted, then the rest fastest and largest first. Any that opens will
/// do — the rate wanted is set once it is open (see [`Rate::request`]).
#[cfg(any(not(target_os = "macos"), test))]
fn open_order(formats: &[CameraFormat], wanted: Option<&Rate>) -> Vec<CameraFormat> {
    let mut order = formats.to_vec();
    order.sort_by_key(|f| {
        let near = wanted.is_some_and(|w| {
            f.resolution().width_x == w.width
                && f.resolution().height_y == w.height
                && f.format() == w.format
                && (f64::from(f.frame_rate()) - w.fps).abs() < 1.0
        });
        (
            !near,
            std::cmp::Reverse(f.frame_rate()),
            std::cmp::Reverse(f.resolution().width_x * f.resolution().height_y),
        )
    });
    order.dedup();
    order
}

/// An active camera capture session with its shared GPU texture.
struct ActiveCamera {
    /// Shared GPU texture that decks read from.
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    width: u32,
    height: u32,
    /// How many decks are using this camera.
    ref_count: u32,
    /// Where its capture leaves its pictures.
    sink: Arc<Sink>,
    /// Signal to stop the capture thread.
    stop_flag: Arc<AtomicBool>,
    /// The size and rate it was opened at.
    mode: Option<CameraMode>,
    /// Capture thread handle.
    thread: Option<std::thread::JoinHandle<()>>,
    /// Closed when the capture thread ends.
    done: mpsc::Receiver<()>,
    /// What its picture is coded as, as its decks say (see
    /// [`CameraManager::set_profiles`]); a LUT file's path in full.
    profile: CameraProfile,
    /// Its picture converted for the show while a profile is set — what its
    /// decks read then.
    converted: Option<ConvertedPicture>,
    /// A profile that could not be set up (a LUT file that will not read):
    /// not tried again every frame, and said once.
    failed: Option<CameraProfile>,
    /// When its last new picture was put in its texture.
    last_picture: Option<std::time::Instant>,
}

/// `profile`, if it is `from`, named `to`.
fn relocate_lut(profile: &mut CameraProfile, from: &CameraProfile, to: &CameraProfile) {
    if profile == from {
        profile.clone_from(to);
    }
}

/// What a camera's thread runs: opens the device, says so (or why not)
/// through `opened`, and captures into the sink until told to stop.
type RunCamera = fn(
    &CameraDeviceInfo,
    Option<CameraMode>,
    &Arc<Sink>,
    &AtomicBool,
    &mpsc::Sender<Result<CameraMode>>,
);

/// Manages camera device enumeration, capture sessions, and shared textures.
pub struct CameraManager {
    /// What each camera's thread runs: [`Self::run_camera`], or a test's
    /// stand-in for a device.
    run: RunCamera,
    /// Detected camera devices (refreshed periodically).
    devices: Vec<CameraDeviceInfo>,
    /// Active camera sessions keyed by `CameraId`.
    active: HashMap<CameraId, ActiveCamera>,
    /// Cameras starting in the background (see [`Self::open_in_background`]).
    opening: HashMap<CameraId, Opening>,
    /// The number given each device seen, by its unique ID.
    ids: HashMap<String, CameraId>,
    next_id: CameraId,
    /// Whether nokhwa has been initialized.
    initialized: bool,
    /// Whether the OS refused the cameras, as of the last scan. Asking is a
    /// round trip to macOS's privacy service, and the control screen reads
    /// this every frame.
    denied: bool,
    /// The OS camera effects on for this app, as last looked at.
    effects: Vec<&'static str>,
    /// The effects last warned about while a camera was open.
    effects_warned: Vec<&'static str>,
    /// When the effects are next looked at while a camera is open.
    effects_due: Option<Instant>,
    /// The GPU pipelines converting cameras' pictures, made when a camera
    /// first has a profile.
    profile_passes: Option<ProfilePasses>,
    /// Blackmagic's devices as last looked for (see
    /// [`CameraManager::blackmagic_changed`]).
    blackmagic_seen: u64,
}

/// The OS camera effects on for this app: macOS Video Effects; none elsewhere.
fn read_video_effects() -> Vec<&'static str> {
    #[cfg(target_os = "macos")]
    {
        avf::video_effects_on()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Open the OS's camera Video Effects for this app (a Mac); nothing elsewhere.
pub fn show_video_effects() {
    #[cfg(target_os = "macos")]
    avf::show_video_effects();
}

/// What to say when OS camera `effects` are on while a camera is open.
/// A camera's frames watched for coming far slower than it was opened at —
/// a USB capture on a USB 2 link (the user's Magewell: 7.7 of 60 a second):
/// how many looks in a row found it slow, and whether that was said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RateWatch {
    pub slow_looks: u32,
    pub said: bool,
}

impl RateWatch {
    /// A look at its rate, `fps` against the `opened` one: true when it is
    /// to be said now — under six tenths of it three looks running, once a
    /// time; said again only after it has come back (nine tenths).
    pub fn look(&mut self, fps: f64, opened: Option<f64>) -> bool {
        let Some(opened) = opened.filter(|o| *o >= 20.0) else {
            return false;
        };
        if fps > 0.0 && fps < opened * 0.6 {
            self.slow_looks += 1;
            if self.slow_looks >= 3 && !self.said {
                self.said = true;
                return true;
            }
        } else if fps >= opened * 0.9 {
            *self = Self::default();
        } else {
            // Three looks running: a dip now and then is not one.
            self.slow_looks = 0;
        }
        false
    }
}

/// What is said of camera `name` whose frames come at `fps` of `opened`.
pub fn slow_camera_warning(name: &str, fps: f64, opened: f64) -> String {
    format!(
        "{name}: only {fps:.1} of {opened:.0} frames a second arrive — check its cable and \
         USB port (on a USB 2 link an HD capture cannot keep up)"
    )
}

pub fn video_effects_warning(effects: &[&str]) -> String {
    format!(
        "macOS camera effects are on for this app: {}. They change every camera's \
         picture, on air too. Switch them off in the menu bar's camera icon → \
         Video Effects.",
        effects.join(", ")
    )
}

/// `now`'s effects when they are to be warned about — some on, and not the
/// set warned about last — noting them as warned. All off clears the note,
/// so the same effects switched on again are said again.
fn news(now: &[&'static str], warned: &mut Vec<&'static str>) -> Option<Vec<&'static str>> {
    if now.is_empty() {
        warned.clear();
        return None;
    }
    if now == warned.as_slice() {
        return None;
    }
    *warned = now.to_vec();
    Some(now.to_vec())
}

/// See [`CameraManager::find`]. A saved unique ID not plugged in (the same
/// camera in another USB port has another) falls back to the name, but only
/// to a device not already showing: the other of two identical cameras is
/// not taken for this one.
/// The device a camera that is not plugged in under `old` is now, among the
/// cameras plugged in with its name, `candidates`: the first that no
/// capture holds (`open`) and that was not there already (`known`, the
/// others of its name plugged in while its pictures came). A USB camera's
/// unique ID holds the port it is plugged into, so one plugged back into
/// another port has another — and its capture, trying the old one, waited
/// for it for ever (an M5 Max, 2026-10-28). One of the same model plugged
/// in all along is another camera: never taken for it.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn moved_device(
    candidates: &[String],
    open: &HashMap<String, u32>,
    known: &std::collections::HashSet<String>,
    old: &str,
) -> Option<String> {
    candidates
        .iter()
        .find(|uid| {
            uid.as_str() != old && !open.contains_key(uid.as_str()) && !known.contains(uid.as_str())
        })
        .cloned()
}

/// The devices the cameras' captures hold, and how many hold each — one
/// each, but a capture starting and one finding its device moved could
/// meet — so a camera plugged back into another port does not take
/// another's. Held from before a capture opens.
#[cfg(target_os = "macos")]
static OPEN_DEVICES: std::sync::LazyLock<Mutex<HashMap<String, u32>>> =
    std::sync::LazyLock::new(Mutex::default);

/// How often a capture whose pictures come notes the others of its name
/// plugged in (see [`moved_device`]).
#[cfg(target_os = "macos")]
const NOTE_OTHERS_EVERY: Duration = Duration::from_secs(5);

/// A capture's hold on its device in [`OPEN_DEVICES`], let go as it ends,
/// and the others of its name it has seen plugged in.
#[cfg(target_os = "macos")]
struct Holding {
    unique_id: String,
    name: String,
    known: std::collections::HashSet<String>,
    noted: Option<Instant>,
}

#[cfg(target_os = "macos")]
fn hold(open: &mut HashMap<String, u32>, unique_id: &str) {
    *open.entry(unique_id.to_string()).or_default() += 1;
}

#[cfg(target_os = "macos")]
fn let_go(open: &mut HashMap<String, u32>, unique_id: &str) {
    if let Some(count) = open.get_mut(unique_id) {
        *count -= 1;
        if *count == 0 {
            open.remove(unique_id);
        }
    }
}

#[cfg(target_os = "macos")]
impl Holding {
    fn new(unique_id: &str, name: &str) -> Self {
        if let Ok(mut open) = OPEN_DEVICES.lock() {
            hold(&mut open, unique_id);
        }
        Self {
            unique_id: unique_id.to_string(),
            name: name.to_string(),
            known: std::collections::HashSet::new(),
            noted: None,
        }
    }

    /// Its pictures come: note the others of its name plugged in now,
    /// every [`NOTE_OTHERS_EVERY`].
    fn note_others(&mut self, now: Instant) {
        if self
            .noted
            .is_some_and(|at| now.saturating_duration_since(at) < NOTE_OTHERS_EVERY)
        {
            return;
        }
        self.noted = Some(now);
        self.known = avf::unique_ids_named(&self.name)
            .into_iter()
            .filter(|uid| *uid != self.unique_id)
            .collect();
    }

    /// It has just stopped: the others of its name plugged in now are
    /// noted too — one plugged in since the last note is another camera,
    /// while it, plugged back in, takes seconds to be listed again.
    fn note_others_as_lost(&mut self) {
        let others: Vec<String> = avf::unique_ids_named(&self.name)
            .into_iter()
            .filter(|uid| *uid != self.unique_id)
            .collect();
        self.known.extend(others);
    }

    /// The device, not plugged in under its unique ID, plugged in again
    /// under another — into another port — and its rates now: held
    /// instead.
    fn find_moved(&mut self) -> Option<Vec<Rate>> {
        let candidates = avf::unique_ids_named(&self.name);
        let mut open = OPEN_DEVICES.lock().ok()?;
        let moved = moved_device(&candidates, &open, &self.known, &self.unique_id)?;
        let rates = avf::rates_now(&moved)?;
        let_go(&mut open, &self.unique_id);
        hold(&mut open, &moved);
        self.unique_id = moved;
        Some(rates)
    }
}

#[cfg(target_os = "macos")]
impl Drop for Holding {
    fn drop(&mut self) {
        if let Ok(mut open) = OPEN_DEVICES.lock() {
            let_go(&mut open, &self.unique_id);
        }
    }
}

/// Cameras open and the devices they came back as (`moved`, by camera):
/// each new unique ID keeps its camera's number, so the camera stays the
/// one its inputs show — and the old one no longer does: another of the
/// model plugged in there later is another camera.
fn keep_moved_ids(ids: &mut HashMap<String, CameraId>, moved: &[(CameraId, String)]) {
    for (id, uid) in moved {
        ids.retain(|known, known_id| known_id != id || known == uid);
        ids.insert(uid.clone(), *id);
    }
}

/// The devices listed, as cameras open came back under other unique IDs
/// (`moved`): listed already under its number, a camera's entry takes the
/// new ID; listed under the new ID — a scan found it there before its
/// capture did — that entry takes the camera's number, so choosing it
/// shows the camera open, not a second capture of one device.
fn renumber_moved(devices: &mut [CameraDeviceInfo], moved: &[(CameraId, String)]) {
    for (id, uid) in moved {
        if let Some(device) = devices.iter_mut().find(|d| d.id == *id) {
            device.unique_id.clone_from(uid);
        } else if let Some(device) = devices.iter_mut().find(|d| d.unique_id == *uid) {
            device.id = *id;
        }
    }
}

fn find_in(
    devices: &[CameraDeviceInfo],
    showing: impl Fn(CameraId) -> bool,
    unique_id: Option<&str>,
    name: &str,
) -> Option<CameraId> {
    let by_id = unique_id.and_then(|u| devices.iter().find(|d| d.unique_id == u));
    let named = || devices.iter().filter(|d| d.name == name);
    by_id
        .or_else(|| named().find(|d| !showing(d.id)))
        .or_else(|| {
            if unique_id.is_some() {
                None
            } else {
                named().next()
            }
        })
        .map(|d| d.id)
}

/// A camera starting: its thread, and the word it sends once it runs.
struct Opening {
    name: String,
    sink: Arc<Sink>,
    stop_flag: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
    done: mpsc::Receiver<()>,
    opened: mpsc::Receiver<Result<CameraMode>>,
    since: Instant,
    /// The decks waiting for it, by UUID.
    decks: Vec<String>,
    /// Started for an input just added, whose operator is told if it fails
    /// — not for one waiting since its show opened, tried every two seconds.
    asked: bool,
    /// The format it was started at; none, its best.
    #[cfg_attr(not(test), allow(dead_code))]
    mode: Option<CameraMode>,
}

/// A camera started in the background that has run, or failed to: the
/// decks that were waiting for it, and its size or what went wrong.
pub struct Opened {
    pub id: CameraId,
    pub decks: Vec<String>,
    pub result: Result<(u32, u32)>,
    /// Started for an input just added (see [`CameraManager::open_in_background`]).
    pub asked: bool,
}

/// A device as a scan finds it.
struct Found {
    unique_id: String,
    name: String,
    index: CameraIndex,
    formats: Vec<CameraFormat>,
    rates: Vec<Rate>,
    /// It takes the format that comes in, whatever it was opened at.
    follows_signal: bool,
}

impl Default for CameraManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraManager {
    pub fn new() -> Self {
        let mut mgr = Self {
            run: Self::run_camera,
            devices: Vec::new(),
            active: HashMap::new(),
            opening: HashMap::new(),
            ids: HashMap::new(),
            next_id: 0,
            initialized: false,
            denied: false,
            effects: Vec::new(),
            effects_warned: Vec::new(),
            effects_due: None,
            profile_passes: None,
            blackmagic_seen: 0,
        };
        #[cfg(target_os = "macos")]
        avf::keep_center_stage_off();
        mgr.initialize();
        mgr.scan_devices();
        mgr
    }

    /// A manager that never touches the capture backend.
    ///
    /// For `--no-camera`. Enumeration is what raises the OS permission dialog,
    /// so the only way to guarantee no prompt is not to enumerate — checking a
    /// flag inside `scan_devices` would still have called the backend once
    /// during construction. `initialized` stays false, so every later operation
    /// finds no devices and does nothing.
    pub fn disabled() -> Self {
        log::info!("Camera sources disabled via --no-camera; not enumerating devices");
        Self {
            run: Self::run_camera,
            devices: Vec::new(),
            active: HashMap::new(),
            opening: HashMap::new(),
            ids: HashMap::new(),
            next_id: 0,
            initialized: false,
            denied: false,
            effects: Vec::new(),
            effects_warned: Vec::new(),
            effects_due: None,
            profile_passes: None,
            blackmagic_seen: 0,
        }
    }

    /// A manager listing `devices` (unique ID, name), whose cameras' threads
    /// run `run` in place of a device's — for tests.
    #[cfg(test)]
    pub(crate) fn with_devices(devices: &[(&str, &str)], run: RunCamera) -> Self {
        let mut mgr = Self::disabled();
        mgr.run = run;
        mgr.list(
            devices
                .iter()
                .map(|(unique_id, name)| Found {
                    unique_id: (*unique_id).into(),
                    name: (*name).into(),
                    index: CameraIndex::String((*unique_id).into()),
                    formats: Vec::new(),
                    rates: Vec::new(),
                    follows_signal: false,
                })
                .collect(),
        );
        mgr
    }

    fn initialize(&mut self) {
        if self.initialized {
            return;
        }
        // On macOS, requests AVFoundation camera permission.
        // On Linux, this is a no-op (V4L2 doesn't need explicit permission).
        nokhwa::nokhwa_initialize(|granted| {
            if granted {
                log::info!("Camera access granted");
            } else {
                log::warn!("Camera access denied by OS");
            }
        });
        self.initialized = true;
    }

    /// Scan for available camera devices, and the formats each offers. Each
    /// keeps the number it was first given, so an input stays on its camera
    /// however devices are plugged in and out around it. Off a Mac, a camera
    /// already capturing keeps the formats read before: reading them opens
    /// the device, which would reconfigure it mid-picture.
    pub fn scan_devices(&mut self) {
        if !self.initialized {
            return;
        }
        self.denied = !nokhwa::nokhwa_check();
        self.effects = read_video_effects();
        let Some(found) = self.find_devices() else {
            self.devices.clear();
            return;
        };
        self.list(found);
    }

    /// The devices a scan found, each numbered — with the number it had
    /// before, when it was seen before.
    fn list(&mut self, found: Vec<Found>) {
        let before = std::mem::take(&mut self.devices);
        self.devices = found
            .into_iter()
            .map(|f| {
                let id = *self.ids.entry(f.unique_id.clone()).or_insert_with(|| {
                    let id = self.next_id;
                    self.next_id += 1;
                    id
                });
                CameraDeviceInfo {
                    id,
                    name: f.name,
                    unique_id: f.unique_id,
                    index: f.index,
                    formats: f.formats,
                    rates: f.rates,
                    follows_signal: f.follows_signal,
                }
            })
            .collect();
        // Logged when the devices change: a deck waiting for its camera
        // looks every two seconds.
        let seen = |devices: &[CameraDeviceInfo]| -> Vec<(CameraId, String, Vec<CameraMode>)> {
            devices
                .iter()
                .map(|d| (d.id, d.name.clone(), d.modes()))
                .collect()
        };
        if seen(&before) == seen(&self.devices) {
            return;
        }
        log::info!("Camera scan: found {} device(s)", self.devices.len());
        for dev in &self.devices {
            let modes = dev.modes();
            log::info!(
                "  Camera {}: {} ({} formats, best {})",
                dev.id,
                dev.name,
                modes.len(),
                crate::engine::value::camera::default_mode(&modes)
                    .map_or_else(|| "unknown".to_string(), |m| m.label())
            );
        }
    }

    /// The devices plugged in, as `AVFoundation` lists them.
    #[cfg(target_os = "macos")]
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    fn find_devices(&self) -> Option<Vec<Found>> {
        Some(
            avf::devices()
                .into_iter()
                .map(|d| Found {
                    index: CameraIndex::String(d.unique_id.clone()),
                    unique_id: d.unique_id,
                    name: d.name,
                    formats: Vec::new(),
                    rates: d.rates,
                    follows_signal: false,
                })
                // Blackmagic inputs as their own API has them, beside the
                // same boxes as macOS lists them.
                .chain(blackmagic::found())
                .collect(),
        )
    }

    /// The devices plugged in, as nokhwa lists them. Without camera access
    /// the formats are not read: opening a device fails then anyway.
    #[cfg(not(target_os = "macos"))]
    fn find_devices(&self) -> Option<Vec<Found>> {
        let allowed = nokhwa::nokhwa_check();
        if !allowed {
            log::warn!("No camera access: camera formats are not read");
        }
        let cameras = nokhwa::query(nokhwa::utils::ApiBackend::Auto)
            .map_err(|e| log::warn!("Camera enumeration failed: {e}"))
            .ok()?;
        Some(
            cameras
                .iter()
                .enumerate()
                .map(|(i, info)| {
                    let name = info.human_name();
                    let unique_id = if info.misc().is_empty() {
                        format!("{name}#{i}")
                    } else {
                        info.misc()
                    };
                    let in_use = self
                        .devices
                        .iter()
                        .find(|d| d.unique_id == unique_id && self.active.contains_key(&d.id));
                    let (formats, rates) = if let Some(d) = in_use {
                        (d.formats.clone(), d.rates.clone())
                    } else if allowed {
                        Self::read_formats(info.index())
                    } else {
                        (Vec::new(), Vec::new())
                    };
                    Found {
                        unique_id,
                        name,
                        index: info.index().clone(),
                        formats,
                        rates,
                        follows_signal: false,
                    }
                })
                .collect(),
        )
    }

    /// The formats a device offers, read by opening it without starting it.
    #[cfg(not(target_os = "macos"))]
    fn read_formats(index: &CameraIndex) -> (Vec<CameraFormat>, Vec<Rate>) {
        let request = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::None);
        match Camera::new(index.clone(), request).and_then(|mut c| c.compatible_camera_formats()) {
            Ok(formats) => {
                let rates = formats.iter().map(Rate::of).collect();
                (formats, rates)
            }
            Err(e) => {
                log::warn!("Camera {index}: could not read its formats: {e}");
                (Vec::new(), Vec::new())
            }
        }
    }

    /// Whether the OS has refused this app the cameras (a Mac where Camera
    /// access is off for it), as of the last scan. False with cameras
    /// switched off.
    pub fn access_denied(&self) -> bool {
        self.initialized && self.denied
    }

    /// The OS camera effects on for this app, as of the last look (a scan,
    /// or [`Self::poll_video_effects`]).
    pub fn video_effects(&self) -> &[&'static str] {
        &self.effects
    }

    /// While a camera is open, look at the OS camera effects every five
    /// seconds; the effects when they have changed to a set not yet warned
    /// about, to warn once. Switched all off, nothing is said.
    pub fn poll_video_effects(&mut self) -> Option<Vec<&'static str>> {
        if self.active.is_empty() {
            return None;
        }
        let now = Instant::now();
        if self.effects_due.is_some_and(|due| now < due) {
            return None;
        }
        self.effects_due = Some(now + Duration::from_secs(5));
        self.effects = read_video_effects();
        news(&self.effects, &mut self.effects_warned)
    }

    /// Get the list of detected camera devices.
    pub fn devices(&self) -> &[CameraDeviceInfo] {
        &self.devices
    }

    /// The camera a saved input names: the device with its unique ID when
    /// that is plugged in, or else one of its name — one not already showing,
    /// when there are two of a model, so each input finds its own.
    pub fn find(&self, unique_id: Option<&str>, name: &str) -> Option<CameraId> {
        find_in(
            &self.devices,
            |id| self.active.contains_key(&id),
            unique_id,
            name,
        )
    }

    /// The unique ID of a camera, for an input to find it by next time.
    /// Have camera `id`'s capture close and open again, as it is (a Reset
    /// input): its device found afresh — in another USB port too — and its
    /// formats read again, at the format it was opened with; its inputs
    /// keep its last picture meanwhile. Whoever else holds it (another
    /// layer, stage detection) keeps it. Whether it is open.
    pub fn restart(&mut self, id: CameraId) -> bool {
        let Some(active) = self.active.get(&id) else {
            return false;
        };
        active.sink.ask_restart();
        true
    }

    /// The open cameras whose devices came back under another unique ID —
    /// plugged into another port — since this was last asked, each with
    /// its new one, kept under its number from now on.
    pub fn take_moves(&mut self) -> Vec<(CameraId, String)> {
        let moves: Vec<(CameraId, String)> = self
            .active
            .iter()
            .filter_map(|(id, active)| Some((*id, active.sink.take_move()?)))
            .collect();
        keep_moved_ids(&mut self.ids, &moves);
        renumber_moved(&mut self.devices, &moves);
        moves
    }

    pub fn unique_id(&self, id: CameraId) -> Option<&str> {
        self.devices
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.unique_id.as_str())
    }

    /// Read camera `id`'s pictures into colours this way from its next
    /// frame — capturing, or still starting. One not open is left alone.
    pub fn set_colour(&self, id: CameraId, colour: CameraColour) {
        if let Some(active) = self.active.get(&id) {
            active.sink.set_colour(colour);
        }
        if let Some(opening) = self.opening.get(&id) {
            opening.sink.set_colour(colour);
        }
    }

    /// Show camera `id`'s newest picture at each pick, or each in turn at
    /// the delay that keeps them even (see [`FramePicker::set_newest`]).
    pub fn set_lowest_latency(&self, id: CameraId, on: bool) {
        if let Some(active) = self.active.get(&id) {
            active.sink.set_lowest_latency(on);
        }
        if let Some(opening) = self.opening.get(&id) {
            opening.sink.set_lowest_latency(on);
        }
    }

    /// The format camera `id` was started at, while it starts — for tests.
    #[cfg(test)]
    pub(crate) fn opening_mode(&self, id: CameraId) -> Option<CameraMode> {
        self.opening.get(&id)?.mode
    }

    /// The format camera `id`'s capture was asked to open again at, taken —
    /// for tests.
    #[cfg(test)]
    #[allow(clippy::option_option)]
    pub(crate) fn take_asked_mode(&self, id: CameraId) -> Option<Option<CameraMode>> {
        self.active
            .get(&id)
            .map(|a| &a.sink)
            .or_else(|| self.opening.get(&id).map(|o| &o.sink))?
            .take_mode()
    }

    /// Camera `id`'s own sound — a Blackmagic input's — once some has come:
    /// its rate, channels, and a key that changes when it must be picked up
    /// afresh.
    pub fn sound_format(&self, id: CameraId) -> Option<(u32, u16, u64)> {
        self.active.get(&id)?.sink.sound_format()
    }

    /// Where camera `id`'s own sound goes from now.
    pub fn set_sound_feed(
        &self,
        id: CameraId,
        feed: crossbeam_channel::Sender<crate::audio::PcmChunk>,
    ) {
        if let Some(active) = self.active.get(&id) {
            active.sink.set_sound_feed(feed);
        }
    }

    /// Whether Blackmagic devices arrived or went since this was last asked:
    /// the cameras are to be looked for again. Blackmagic's discovery tells
    /// of the devices there a moment after it starts — after the first look
    /// for cameras, which missed them.
    pub fn blackmagic_changed(&mut self) -> bool {
        if !self.initialized {
            return false;
        }
        let now = crate::decklink::input::generation();
        std::mem::replace(&mut self.blackmagic_seen, now) != now
    }

    /// The Blackmagic inputs looked for again, the other cameras kept as
    /// they were: cheap enough for a show frame — Blackmagic's discovery has
    /// them in memory, and macOS is not asked for its cameras.
    pub fn refresh_blackmagic(&mut self) {
        if !self.initialized {
            return;
        }
        let kept = self
            .devices
            .iter()
            .filter(|d| !crate::decklink::input::is_blackmagic(&d.unique_id))
            .map(|d| Found {
                unique_id: d.unique_id.clone(),
                name: d.name.clone(),
                index: d.index.clone(),
                formats: d.formats.clone(),
                rates: d.rates.clone(),
                follows_signal: d.follows_signal,
            });
        #[cfg(target_os = "macos")]
        let found = kept.chain(blackmagic::found()).collect();
        #[cfg(not(target_os = "macos"))]
        let found = kept.collect();
        self.list(found);
    }

    /// The cameras that take the format that comes in, whatever they were
    /// opened at.
    pub fn following_signal(&self) -> Vec<CameraId> {
        self.devices
            .iter()
            .filter(|d| d.follows_signal)
            .map(|d| d.id)
            .collect()
    }

    /// The cameras whose sound comes with their pictures: Blackmagic inputs.
    pub fn with_own_sound(&self) -> Vec<CameraId> {
        self.devices
            .iter()
            .filter(|d| crate::decklink::input::is_blackmagic(&d.unique_id))
            .map(|d| d.id)
            .collect()
    }

    /// Open camera `id` again at `mode` — `None`, its best — its inputs
    /// keeping its last picture meanwhile; one still starting, as soon as it
    /// has. Whether it was open, or opening.
    pub fn set_mode(&self, id: CameraId, mode: Option<CameraMode>) -> bool {
        if let Some(active) = self.active.get(&id) {
            active.sink.ask_mode(mode);
            return true;
        }
        if let Some(opening) = self.opening.get(&id) {
            opening.sink.ask_mode(mode);
            return true;
        }
        false
    }

    /// [`Self::set_lowest_latency`] for each camera in `cameras`, as its
    /// decks say, every frame.
    pub fn set_lowest_latencies(&self, cameras: &HashMap<CameraId, bool>) {
        for (id, on) in cameras {
            self.set_lowest_latency(*id, *on);
        }
    }

    /// [`Self::set_colour`] for each camera in `colours`: what its decks
    /// say, every frame, so a camera opened late, started again or put
    /// back by an undo reads its pictures as they say.
    pub fn set_colours(&self, colours: &HashMap<CameraId, CameraColour>) {
        for (id, colour) in colours {
            self.set_colour(*id, *colour);
        }
    }

    /// How camera `id`'s pictures are read into colours now; `None` for one
    /// not open.
    pub fn colour(&self, id: CameraId) -> Option<CameraColour> {
        self.active.get(&id).map(|active| active.sink.colour())
    }

    /// Open a camera and start capturing on a dedicated thread.
    /// Returns the camera's resolution. If already open, just increments the ref count.
    ///
    /// # Errors
    ///
    /// Returns an error if no enumerated device matches `id`, if the underlying
    /// capture backend fails to open the device or start its stream, or if the
    /// dedicated capture thread cannot be spawned.
    ///
    /// Opened at `mode`, or when that is `None` (or not on offer) at
    /// [`default_mode`](crate::engine::value::camera::default_mode). A camera
    /// already open stays at the format it was opened at.
    pub fn open_camera(
        &mut self,
        id: CameraId,
        device: &wgpu::Device,
        mode: Option<CameraMode>,
    ) -> Result<(u32, u32)> {
        if let Some(active) = self.active.get_mut(&id) {
            active.ref_count += 1;
            return Ok((active.width, active.height));
        }

        // Starting already, in the background (an input just added): waited
        // for here, not started twice — the second start was overwritten as
        // the first ran, its capture left running unseen and its input
        // black. The decks waiting for the first are bound as cameras that
        // are open are (see `reconcile_cameras`).
        let opening = match self.opening.remove(&id) {
            Some(opening) => opening,
            None => self.start(id, mode, Vec::new())?,
        };
        let wait = OPEN_WAIT.saturating_sub(opening.since.elapsed());
        match opening.opened.recv_timeout(wait) {
            Ok(Ok(opened)) => Ok(self.finish(id, opening, opened, device)),
            Ok(Err(e)) => {
                let _ = opening.thread.join();
                Err(e.context(format!("Failed to open camera '{}'", opening.name)))
            }
            Err(_) => {
                // Left to finish on its own, and stop.
                opening.stop_flag.store(true, Ordering::Relaxed);
                anyhow::bail!(
                    "camera '{}' did not start within {} s",
                    opening.name,
                    OPEN_WAIT.as_secs()
                );
            }
        }
    }

    /// Start a camera for decks waiting for it, without waiting for it to
    /// run: it can take the OS half a second, and the render loop must not
    /// stand still for that. [`Self::poll_opening`] says when it has. A
    /// camera already open is left as it is; one starting takes these
    /// decks too. `asked`: for an input just added, whose operator is told
    /// if it fails.
    ///
    /// # Errors
    ///
    /// As [`Self::open_camera`], for what fails before the camera starts.
    pub fn open_in_background(
        &mut self,
        id: CameraId,
        mode: Option<CameraMode>,
        decks: Vec<String>,
        asked: bool,
    ) -> Result<()> {
        if self.active.contains_key(&id) {
            return Ok(());
        }
        if let Some(opening) = self.opening.get_mut(&id) {
            opening.decks.extend(decks);
            opening.asked |= asked;
            return Ok(());
        }
        let mut opening = self.start(id, mode, decks)?;
        opening.asked = asked;
        self.opening.insert(id, opening);
        Ok(())
    }

    /// Whether a camera is starting in the background.
    pub fn is_opening(&self, id: CameraId) -> bool {
        self.opening.contains_key(&id)
    }

    /// The cameras started in the background that have run (open now, one
    /// reference held for their decks) or failed since last asked. Never
    /// waits.
    pub fn poll_opening(&mut self, device: &wgpu::Device) -> Vec<Opened> {
        let ids: Vec<CameraId> = self.opening.keys().copied().collect();
        let mut done = Vec::new();
        for id in ids {
            let answer = match self.opening.get(&id).map(|o| o.opened.try_recv()) {
                Some(Ok(answer)) => Some(answer),
                Some(Err(mpsc::TryRecvError::Empty)) => {
                    let late = self
                        .opening
                        .get(&id)
                        .is_some_and(|o| o.since.elapsed() > OPEN_WAIT);
                    late.then(|| Err(anyhow::anyhow!("did not start in time")))
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    Some(Err(anyhow::anyhow!("its thread ended")))
                }
                None => None,
            };
            let Some(answer) = answer else {
                continue;
            };
            let Some(opening) = self.opening.remove(&id) else {
                continue;
            };
            let (decks, asked) = (opening.decks.clone(), opening.asked);
            let result = match answer {
                Ok(opened) => Ok(self.finish(id, opening, opened, device)),
                Err(e) => {
                    opening.stop_flag.store(true, Ordering::Relaxed);
                    // A Blackmagic box busy because Nida itself shows it, as
                    // macOS lists it, in another input: said, not "another
                    // app" (an M5 Max, 2026-10-29).
                    let others = self
                        .devices
                        .iter()
                        .filter(|d| d.id != id && self.active.contains_key(&d.id))
                        .map(|d| d.name.as_str());
                    let e = match held_by(&opening.name, &format!("{e:#}"), others) {
                        Some(other) => e.context(format!(
                            "its box is in use by the camera '{other}', which another input \
                             shows: remove that input, or switch it to this camera"
                        )),
                        None => e,
                    };
                    Err(e.context(format!("Failed to open camera '{}'", opening.name)))
                }
            };
            done.push(Opened {
                id,
                decks,
                result,
                asked,
            });
        }
        done
    }

    /// Start camera `id`'s thread, which opens it and says so.
    fn start(&self, id: CameraId, mode: Option<CameraMode>, decks: Vec<String>) -> Result<Opening> {
        let dev_info = self
            .devices
            .iter()
            .find(|d| d.id == id)
            .context("Camera device not found")?
            .clone();
        // Asked afresh: access may have been given since the last scan. A
        // Blackmagic input, read through Blackmagic's API, needs none.
        if self.initialized
            && !crate::decklink::input::is_blackmagic(&dev_info.unique_id)
            && !nokhwa::nokhwa_check()
        {
            anyhow::bail!(
                "no camera access for '{}': allow this app in System Settings → Privacy & \
                 Security → Camera (Terminal, when it is started from Terminal), then start \
                 it again",
                dev_info.name
            );
        }
        let name = dev_info.name.clone();
        let sink = Arc::new(Sink::default());
        sink.set_label(format!("camera {id}"));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let (opened_tx, opened) = mpsc::channel::<Result<CameraMode>>();
        let (done_tx, done) = mpsc::channel::<()>();
        let thread = {
            let sink = Arc::clone(&sink);
            let stop = Arc::clone(&stop_flag);
            let run = self.run;
            std::thread::Builder::new()
                .name(format!("camera-{id}"))
                .spawn(move || {
                    // Dropped as the thread ends, which is what `done` hears.
                    let _done = done_tx;
                    run(&dev_info, mode, &sink, &stop, &opened_tx);
                })
                .map_err(|e| anyhow::anyhow!("Failed to spawn camera thread: {e}"))?
        };
        Ok(Opening {
            name,
            sink,
            stop_flag,
            thread,
            done,
            opened,
            since: Instant::now(),
            decks,
            asked: false,
            mode,
        })
    }

    /// A started camera that runs: its texture made, and open with one
    /// reference. Its size.
    fn finish(
        &mut self,
        id: CameraId,
        opening: Opening,
        opened: CameraMode,
        device: &wgpu::Device,
    ) -> (u32, u32) {
        let (width, height) = (opened.width, opened.height);
        log::info!("Opened camera '{}': {}", opening.name, opened.label());
        let texture = Self::texture(device, &opening.name, width, height);
        let texture_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        self.active.insert(
            id,
            ActiveCamera {
                texture,
                texture_view,
                width,
                height,
                ref_count: 1,
                sink: opening.sink,
                stop_flag: opening.stop_flag,
                mode: Some(opened),
                thread: Some(opening.thread),
                done: opening.done,
                profile: CameraProfile::Normal,
                converted: None,
                failed: None,
                last_picture: None,
            },
        );
        (width, height)
    }

    fn texture(device: &wgpu::Device, name: &str, width: u32, height: u32) -> wgpu::Texture {
        device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("Camera {name} Texture")),
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
            // Read as the code values the camera sent by a picture profile
            // (see `renderer::camera_profile`).
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        })
    }

    /// A camera's thread on a Mac: opens it, says how, then keeps it going —
    /// started again when it stops sending pictures, and when it is
    /// unplugged, as soon as it is back — until told to stop.
    #[cfg(target_os = "macos")]
    fn run_camera(
        device: &CameraDeviceInfo,
        mode: Option<CameraMode>,
        sink: &Arc<Sink>,
        stop: &AtomicBool,
        opened: &mpsc::Sender<Result<CameraMode>>,
    ) {
        if crate::decklink::input::is_blackmagic(&device.unique_id) {
            blackmagic::run(device, mode, sink, stop, opened);
            return;
        }
        let name = &device.name;
        // Held before it opens: a camera plugged back into another port
        // meanwhile does not take it (see `moved_device`).
        let mut holding = Holding::new(&device.unique_id, name);
        let first = choose(&device.rates, mode)
            .with_context(|| format!("'{name}' offers no picture Nida can read"))
            .and_then(|rate| {
                sink.opening(rate.fps);
                avf::Capture::open(&device.unique_id, &rate, Arc::clone(sink))
            });
        let mut capture = match first {
            Ok(capture) => {
                let _ = opened.send(Ok(capture.rate.mode()));
                Some(capture)
            }
            Err(e) => {
                let _ = opened.send(Err(e));
                return;
            }
        };
        let mut started = Instant::now();
        let mut retry_at = started;
        let mut mode = mode;
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(50));
            let now = Instant::now();
            // A Reset input, or another format asked for: closed and opened
            // again at once.
            let asked = sink.take_restart();
            if asked {
                retry_at = now;
                if let Some(asked_mode) = sink.take_mode() {
                    mode = asked_mode;
                }
                if capture.take().is_some() {
                    log::info!("Camera '{name}' reset: opening it again");
                }
            }
            let lost = capture.as_ref().is_none_or(|c| {
                !c.running() || stalled(now - started, sink.since_last_frame(), sink.stall_limit())
            });
            if !lost {
                if sink
                    .since_last_frame()
                    .is_some_and(|quiet| quiet < Duration::from_millis(200))
                {
                    holding.note_others(now);
                }
                continue;
            }
            if now < retry_at {
                continue;
            }
            if capture.take().is_some() {
                // How long its picture stood still before it was started
                // again: what a stall cost on air.
                let quiet = sink.since_last_frame().map_or_else(
                    || "no picture since it was opened".to_string(),
                    |quiet| format!("{} ms without a picture", quiet.as_millis()),
                );
                log::warn!("Camera '{name}' stopped ({quiet}): trying it again");
                sink.note_stop();
                holding.note_others_as_lost();
            }
            retry_at = now + RETRY_EVERY;
            // Read afresh: a device plugged back in, or set to another rate,
            // offers what it offers now. Not plugged in under its unique ID,
            // it may be under another: plugged into another port.
            let rates = avf::rates_now(&holding.unique_id).or_else(|| {
                let rates = holding.find_moved()?;
                log::info!("Camera '{name}' is plugged into another port: using it there");
                if let Ok(mut moved) = sink.moved_to.lock() {
                    *moved = Some(holding.unique_id.clone());
                }
                Some(rates)
            });
            let Some(rate) = rates.and_then(|r| choose(&r, mode)) else {
                continue;
            };
            let carried = sink.opening(rate.fps);
            match avf::Capture::open(&holding.unique_id, &rate, Arc::clone(sink)) {
                Ok(c) => {
                    log::info!("Camera '{name}' is back, at {}", rate.label());
                    started = Instant::now();
                    // Started again steady: a stall soon after is started
                    // again as soon as it is seen, not two seconds on.
                    if carried {
                        retry_at = started;
                    }
                    capture = Some(c);
                }
                Err(e) => log::debug!("Camera '{name}' is not back yet: {e}"),
            }
        }
    }

    /// A camera's thread off a Mac: opens it with nokhwa, says how, then
    /// captures until told to stop.
    #[cfg(not(target_os = "macos"))]
    fn run_camera(
        device: &CameraDeviceInfo,
        mode: Option<CameraMode>,
        sink: &Arc<Sink>,
        stop: &AtomicBool,
        opened: &mpsc::Sender<Result<CameraMode>>,
    ) {
        let mut first = true;
        let mut mode = mode;
        while !stop.load(Ordering::Relaxed) {
            // Another format asked for: taken before each opening, those
            // that fail too.
            if let Some(asked_mode) = sink.take_mode() {
                mode = asked_mode;
            }
            match Self::open_nokhwa(device, mode) {
                Ok(camera) => {
                    if first {
                        let res = camera.resolution();
                        let _ = opened.send(Ok(CameraMode {
                            width: res.width_x,
                            height: res.height_y,
                            fps: camera.frame_rate(),
                        }));
                        first = false;
                    }
                    // Back from capturing: stopped, or asked to open again
                    // (a Reset input) — closed as `camera` drops, opened
                    // again as the loop comes round.
                    if !Self::capture_loop(camera, &device.name, sink, stop) {
                        return;
                    }
                    sink.take_restart();
                    log::info!("Camera '{}' reset: opening it again", device.name);
                }
                Err(e) if first => {
                    let _ = opened.send(Err(e));
                    return;
                }
                Err(e) => {
                    log::warn!("Camera '{}' not opened again: {e:#}", device.name);
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn open_nokhwa(dev_info: &CameraDeviceInfo, mode: Option<CameraMode>) -> Result<Camera> {
        let wanted = choose(&dev_info.rates, mode);
        // Opened with the first format the backend takes, then set to the
        // rate wanted: a rate it lists cut down, it refuses to open with.
        let mut refused = Vec::new();
        let mut opened = None;
        for format in open_order(&dev_info.formats, wanted.as_ref()) {
            let request = RequestedFormat::new::<RgbAFormat>(RequestedFormatType::Exact(format));
            match Camera::new(dev_info.index.clone(), request) {
                Ok(camera) => {
                    opened = Some(camera);
                    break;
                }
                Err(e) => refused.push(format!("{format}: {e}")),
            }
        }
        if !refused.is_empty() {
            log::info!(
                "Camera '{}' would not open with {}",
                dev_info.name,
                refused.join("; ")
            );
        }
        let mut camera = if let Some(camera) = opened {
            camera
        } else {
            let request =
                RequestedFormat::new::<RgbAFormat>(RequestedFormatType::AbsoluteHighestFrameRate);
            Camera::new(dev_info.index.clone(), request)
                .map_err(|e| anyhow::anyhow!("Failed to open camera '{}': {}", dev_info.name, e))?
        };
        if let Some(rate) = wanted {
            let request = rate.request();
            if camera.camera_format() != request {
                // Straight to the device, past the backend's own list.
                #[allow(deprecated)]
                if let Err(e) = camera.set_camera_format(request) {
                    log::warn!(
                        "Camera '{}' kept {} rather than {request}: {e}",
                        dev_info.name,
                        camera.camera_format()
                    );
                }
            }
        }
        camera
            .open_stream()
            .map_err(|e| anyhow::anyhow!("Failed to start camera stream: {e}"))?;
        Ok(camera)
    }

    /// Background capture loop — runs on a dedicated thread per camera.
    #[cfg(not(target_os = "macos"))]
    /// Capture from `camera` until told to stop — `false` — or asked to open
    /// it again (see [`Sink::ask_restart`]) — `true`.
    fn capture_loop(mut camera: Camera, name: &str, sink: &Sink, stop: &AtomicBool) -> bool {
        const MAX_BACKOFF_US: u64 = 500_000; // 500ms cap

        let res = camera.resolution();
        let (w, h) = (res.width_x, res.height_y);
        let expected_rgba = (w * h * 4) as usize;
        let mut backoff_us: u64 = 500; // 500µs initial backoff

        log::info!("Camera '{name}' capture thread started ({w}x{h})");

        let mut again = false;
        while !stop.load(Ordering::Relaxed) {
            if sink.restart.load(Ordering::Relaxed) {
                again = true;
                break;
            }
            let Ok(buf) = camera.frame() else {
                std::thread::sleep(std::time::Duration::from_micros(backoff_us));
                backoff_us = (backoff_us * 2).min(MAX_BACKOFF_US);
                continue;
            };

            let raw = buf.buffer();
            let mut rgba_buf = sink.buffer();
            rgba_buf.resize(expected_rgba, 0);
            // Limited range, and the matrix the size implies, unless set by
            // hand (see `CameraColour`): nokhwa gives no tags to go by.
            let colour = sink.colour();
            let range = picture_range(colour, YuvRange::Limited);
            let matrix = picture_matrix(colour, untagged_matrix(h));
            let ok = match buf.source_frame_format() {
                FrameFormat::NV12 => {
                    // SIMD NV12→RGBA via yuvutils-rs
                    let y_size = (w * h) as usize;
                    raw.len() >= y_size + y_size / 2 && {
                        let bi = YuvBiPlanarImage {
                            y_plane: &raw[..y_size],
                            y_stride: w,
                            uv_plane: &raw[y_size..],
                            uv_stride: w,
                            width: w,
                            height: h,
                        };
                        yuv_nv12_to_rgba(
                            &bi,
                            &mut rgba_buf,
                            w * 4,
                            range,
                            matrix,
                            YuvConversionMode::Balanced,
                        )
                        .is_ok()
                    }
                }
                FrameFormat::YUYV => {
                    // SIMD YUYV→RGBA via yuvutils-rs
                    let expected_yuyv = (w * h * 2) as usize;
                    raw.len() >= expected_yuyv && {
                        let packed = YuvPackedImage {
                            yuy: &raw[..expected_yuyv],
                            yuy_stride: w * 2,
                            width: w,
                            height: h,
                        };
                        yuyv422_to_rgba(&packed, &mut rgba_buf, w * 4, range, matrix).is_ok()
                    }
                }
                // Fallback for MJPEG/GRAY/etc
                _ => buf
                    .decode_image_to_buffer::<RgbAFormat>(&mut rgba_buf)
                    .is_ok(),
            };

            if ok {
                backoff_us = 500;
                sink.put(
                    Frame {
                        data: rgba_buf,
                        width: w,
                        height: h,
                    },
                    crate::host_clock::host_seconds(),
                );
            } else {
                sink.recycle(rgba_buf);
                std::thread::sleep(std::time::Duration::from_micros(backoff_us));
                backoff_us = (backoff_us * 2).min(MAX_BACKOFF_US);
            }
        }

        let _ = camera.stop_stream();
        log::info!("Camera '{name}' capture thread stopped");
        again
    }

    /// Release a camera reference. Stops capture thread when `ref_count` hits 0.
    /// Its thread is waited for away from the show: stopping takes the OS a
    /// moment — a device stalled in it, up to never — and the outputs drew
    /// nothing while the show waited. One that does not stop in
    /// [`STOP_WAIT`] is left to end on its own.
    pub fn release_camera(&mut self, id: CameraId) {
        if let Some(active) = self.active.get_mut(&id) {
            active.ref_count = active.ref_count.saturating_sub(1);
            if active.ref_count == 0 {
                log::info!("Closing camera {id} (no more references)");
                let Some(mut removed) = self.active.remove(&id) else {
                    log::warn!("Camera {id} not found in active map during release");
                    return;
                };
                removed.stop_flag.store(true, Ordering::Relaxed);
                let (thread, done) = (removed.thread.take(), removed.done);
                let waited = std::thread::Builder::new()
                    .name(format!("camera-{id}-stop"))
                    .spawn(move || match done.recv_timeout(STOP_WAIT) {
                        Err(mpsc::RecvTimeoutError::Timeout) => log::warn!(
                            "Camera {id} did not stop within {} s: left to stop on its own",
                            STOP_WAIT.as_secs()
                        ),
                        _ => {
                            if let Some(t) = thread {
                                let _ = t.join();
                            }
                        }
                    });
                if let Err(e) = waited {
                    log::warn!("Camera {id} left to stop on its own: {e}");
                }
            }
        }
    }

    /// Get the shared texture view for a camera (decks read from this).
    /// With a picture profile set, its converted picture.
    pub fn texture_view(&self, id: CameraId) -> Option<&wgpu::TextureView> {
        self.active.get(&id).map(|a| {
            a.converted
                .as_ref()
                .map_or(&a.texture_view, ConvertedPicture::view)
        })
    }

    /// What each camera in `profiles` is coded as, as its decks say — every
    /// frame, so a camera opened late, started again or put back by an undo
    /// is converted as they say. A LUT file is named by its full path here.
    pub fn set_profiles(&mut self, profiles: &HashMap<CameraId, CameraProfile>) {
        for (id, profile) in profiles {
            if let Some(active) = self.active.get_mut(id) {
                if active.profile != *profile {
                    active.profile.clone_from(profile);
                    active.failed = None;
                }
            }
        }
    }

    /// The show's LUT folder has moved — a Save As copied it — and with it
    /// the LUT each camera in `moves` is converted by: `(camera, from, to)`,
    /// the same file in the copy, named as its decks will name it next
    /// frame. Converted as `from`, it is kept as it is, named `to`, rather
    /// than read again, which a frame would wait for. One that failed as
    /// `from` is tried as `to`: the copy may hold a file the old folder did
    /// not.
    pub fn relocate_luts(&mut self, moves: &[(CameraId, CameraProfile, CameraProfile)]) {
        for (id, from, to) in moves {
            let Some(active) = self.active.get_mut(id) else {
                continue;
            };
            relocate_lut(&mut active.profile, from, to);
            if let Some(converted) = &mut active.converted {
                relocate_lut(&mut converted.profile, from, to);
            }
        }
    }

    /// What camera `id`'s picture is converted as now; `None` for one not
    /// open.
    pub fn profile(&self, id: CameraId) -> Option<&CameraProfile> {
        self.active.get(&id).map(|a| &a.profile)
    }

    /// The size and rate an active camera was opened at.
    pub fn active_mode(&self, id: CameraId) -> Option<CameraMode> {
        self.active.get(&id).and_then(|a| a.mode)
    }

    /// What an open camera sends, as its pictures say (see
    /// [`Sink::signal`]); none before its first picture is read.
    pub fn signal(&self, id: CameraId) -> Option<crate::engine::value::camera::CameraSignal> {
        self.active.get(&id)?.sink.signal()
    }

    /// Get the resolution of an active camera: the size of its latest
    /// picture, which a device set to another size mid-show changes.
    pub fn resolution(&self, id: CameraId) -> Option<(u32, u32)> {
        self.active.get(&id).map(|a| (a.width, a.height))
    }

    /// Whether a camera is sending pictures: one came within the last second.
    pub fn is_connected(&self, id: CameraId) -> bool {
        self.active.get(&id).is_some_and(|a| {
            a.sink
                .since_last_frame()
                .is_some_and(|d| d < Duration::from_secs(1))
        })
    }

    /// Whether a camera sent a new picture within `within`: a camera with no
    /// device, or no signal, sends none.
    pub fn receiving(&self, id: CameraId, within: std::time::Duration) -> bool {
        self.active
            .get(&id)
            .and_then(|a| a.last_picture)
            .is_some_and(|at| at.elapsed() <= within)
    }

    /// The names of the cameras open now.
    pub fn open_names(&self) -> Vec<String> {
        self.active.keys().map(|id| self.name_of(*id)).collect()
    }

    /// The names of the cameras open whose pictures stopped, and that were
    /// started again, within `within`.
    pub fn stopped_names(&self, within: Duration) -> Vec<String> {
        self.active
            .iter()
            .filter(|(_, active)| active.sink.stopped_within(within))
            .map(|(id, _)| self.name_of(*id))
            .collect()
    }

    /// A camera's name; "Camera N" if it is not listed.
    fn name_of(&self, id: CameraId) -> String {
        self.devices
            .iter()
            .find(|d| d.id == id)
            .map_or_else(|| format!("Camera {id}"), |d| d.name.clone())
    }

    /// Each open camera's name and how long before its show frame's time
    /// the picture shown last was captured, seconds.
    pub fn picture_delays(&self) -> Vec<(String, f64)> {
        self.active
            .iter()
            .filter_map(|(id, active)| {
                let name = self
                    .devices
                    .iter()
                    .find(|d| d.id == *id)
                    .map_or_else(|| format!("Camera {id}"), |d| d.name.clone());
                Some((name, active.sink.latency()?))
            })
            .collect()
    }

    /// Upload every camera's picture for the show frame due at `show_at`,
    /// host seconds. Non-blocking — just picks from what the background
    /// capture threads have produced since the last call. Call once per
    /// frame from the main loop.
    pub fn update(&mut self, context: &GpuContext, show_at: f64) {
        let ids: std::collections::HashSet<CameraId> = self.active.keys().copied().collect();
        self.update_selective(context, &ids, show_at);
    }

    /// Upload frames only for cameras whose IDs are in the provided set.
    /// Cameras not in the set skip the GPU upload, saving bandwidth.
    /// A camera with a picture profile has each new picture converted on
    /// the GPU, all of them in one submission after the uploads.
    pub fn update_selective(
        &mut self,
        context: &GpuContext,
        needed_ids: &std::collections::HashSet<CameraId>,
        show_at: f64,
    ) {
        let (device, queue) = (&context.device, &context.queue);
        let mut encoder: Option<wgpu::CommandEncoder> = None;
        for (id, active) in &mut self.active {
            if !needed_ids.contains(id) {
                continue;
            }
            let uploaded = Self::upload_frame(*id, active, device, queue, show_at);
            if uploaded {
                active.last_picture = Some(std::time::Instant::now());
            }
            if active.profile.is_normal() {
                active.converted = None;
                continue;
            }
            let ready = active.converted.as_ref().is_some_and(|c| {
                c.profile == active.profile && c.size == (active.width, active.height)
            });
            if !ready {
                active.converted = None;
                if active.failed.as_ref() == Some(&active.profile) {
                    continue;
                }
                let passes = match &mut self.profile_passes {
                    Some(passes) => passes,
                    passes @ None => match ProfilePasses::new(device) {
                        Ok(made) => passes.insert(made),
                        Err(e) => {
                            log::warn!("Camera picture profiles unavailable: {e}");
                            active.failed = Some(active.profile.clone());
                            continue;
                        }
                    },
                };
                let lut = match &active.profile {
                    CameraProfile::Lut { file } => Some(std::path::Path::new(file)),
                    _ => None,
                };
                match ConvertedPicture::new(device, queue, passes, &active.texture, &active.profile, lut) {
                    Ok(converted) => active.converted = Some(converted),
                    Err(e) => {
                        log::warn!(
                            "Camera picture not converted as {}: {e:#}",
                            active.profile.label()
                        );
                        active.failed = Some(active.profile.clone());
                        continue;
                    }
                }
            }
            // A new picture, or one not yet converted this way.
            if uploaded || !ready {
                if let (Some(converted), Some(passes)) = (&active.converted, &self.profile_passes) {
                    let encoder = encoder.get_or_insert_with(|| {
                        device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("Camera Profile Encoder"),
                        })
                    });
                    converted.encode(device, passes, encoder);
                }
            }
        }
        if let Some(encoder) = encoder {
            context.submit([encoder.finish()]);
        }
    }

    /// Upload the camera's picture for the show frame due at `show_at`;
    /// whether there was a new one.
    fn upload_frame(
        id: CameraId,
        active: &mut ActiveCamera,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        show_at: f64,
    ) -> bool {
        let Some(frame) = active.sink.take(show_at, id) else {
            return false;
        };
        // A device set to another size — or started again at one — sends
        // pictures of that size: the texture follows.
        if (frame.width, frame.height) != (active.width, active.height) {
            log::info!(
                "Camera picture is now {}×{} (was {}×{})",
                frame.width,
                frame.height,
                active.width,
                active.height
            );
            active.texture = Self::texture(device, "", frame.width, frame.height);
            active.texture_view = active
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            (active.width, active.height) = (frame.width, frame.height);
        }
        let expected = (active.width * active.height * 4) as usize;
        if frame.data.len() >= expected {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: &active.texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                &frame.data[..expected],
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(active.width * 4),
                    rows_per_image: Some(active.height),
                },
                wgpu::Extent3d {
                    width: active.width,
                    height: active.height,
                    depth_or_array_layers: 1,
                },
            );
        }
        active.sink.recycle(frame.data);
        true
    }

    /// Check if a camera is currently active (has an open capture session).
    pub fn is_active(&self, id: CameraId) -> bool {
        self.active.contains_key(&id)
    }

    /// Snapshot the current frame from an active camera without consuming it.
    /// Never waits, so it does not stall the render thread.
    /// Returns `Some((data, width, height))` if a frame is available, `None` otherwise.
    pub fn snapshot_frame(&self, id: CameraId) -> Option<(Vec<u8>, u32, u32)> {
        self.active.get(&id)?.sink.peek()
    }

    /// Returns the first active camera ID, if any.
    pub fn first_active_id(&self) -> Option<CameraId> {
        self.active.keys().next().copied()
    }

    /// Returns all active camera IDs as a sorted vec.
    pub fn active_ids(&self) -> Vec<CameraId> {
        let mut ids: Vec<CameraId> = self.active.keys().copied().collect();
        ids.sort_unstable();
        ids
    }
}

#[cfg(test)]
mod tests {
    /// Frames far slower than opened at: said after three looks running,
    /// once; said again only after coming back. Dips not in a row, or a
    /// camera of unknown or low rate, never.
    #[test]
    fn a_camera_far_below_its_rate_is_said_once_a_time() {
        let mut watch = super::RateWatch::default();
        assert!(!watch.look(7.7, Some(60.0)));
        assert!(!watch.look(7.7, Some(60.0)));
        assert!(watch.look(7.7, Some(60.0)), "the third slow look");
        assert!(!watch.look(7.7, Some(60.0)), "said once");
        assert!(!watch.look(45.0, Some(60.0)), "between: still slow, not said again");
        assert!(!watch.look(60.0, Some(60.0)), "back");
        for _ in 0..2 {
            assert!(!watch.look(10.0, Some(60.0)));
        }
        assert!(watch.look(10.0, Some(60.0)), "slow again: said again");
        // Dips now and then, not three running: never said.
        let mut dips = super::RateWatch::default();
        for _ in 0..5 {
            assert!(!dips.look(10.0, Some(60.0)));
            assert!(!dips.look(45.0, Some(60.0)));
        }
        let mut unknown = super::RateWatch::default();
        for _ in 0..5 {
            assert!(!unknown.look(5.0, None));
            assert!(!unknown.look(5.0, Some(15.0)));
        }
    }

    use super::stalled;

    /// A camera plugged back into another port is found by its name: the
    /// first device with it that no capture holds and that was not plugged
    /// in already while its pictures came, never its old ID.
    #[test]
    fn a_camera_in_another_port_is_a_new_one_of_its_name_not_held() {
        use super::moved_device;
        let open: std::collections::HashMap<String, u32> = [("usb-a".to_string(), 1)].into();
        let known: std::collections::HashSet<String> = ["usb-b".to_string()].into();
        let none = std::collections::HashSet::new();
        let ids = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(
            moved_device(&ids(&["usb-a", "usb-b", "usb-c"]), &open, &known, "usb-old").as_deref(),
            Some("usb-c"),
            "one held, one plugged in all along: the new one"
        );
        assert_eq!(
            moved_device(&ids(&["usb-b"]), &open, &known, "usb-old"),
            None,
            "the other of the model, plugged in all along, is another camera"
        );
        assert_eq!(
            moved_device(&ids(&["usb-old"]), &open, &none, "usb-old"),
            None
        );
        assert_eq!(
            moved_device(&ids(&["usb-a"]), &open, &none, "usb-old"),
            None,
            "held"
        );
        assert_eq!(moved_device(&[], &open, &none, "usb-old"), None);
    }

    /// A camera's new unique ID keeps its number, over one a scan gave it
    /// before its capture found it; its old one is let go.
    #[test]
    fn a_moved_camera_keeps_its_number() {
        let mut ids: std::collections::HashMap<String, super::CameraId> = [
            ("usb-old".to_string(), 3),
            ("usb-new".to_string(), 7),
            ("usb-other".to_string(), 4),
        ]
        .into();
        super::keep_moved_ids(&mut ids, &[(3, "usb-new".to_string())]);
        assert_eq!(ids.get("usb-new"), Some(&3));
        assert_eq!(ids.get("usb-old"), None, "the old port is free for another");
        assert_eq!(ids.get("usb-other"), Some(&4));
    }

    /// A moved camera's entry in the devices listed keeps its number with
    /// its new ID — found by number, or by the new ID a scan listed first.
    #[test]
    fn a_moved_camera_is_listed_under_its_number() {
        let device = |id, uid: &str| super::CameraDeviceInfo {
            id,
            name: "USB Capture HDMI".into(),
            unique_id: uid.into(),
            index: CameraIndex::Index(0),
            formats: Vec::new(),
            rates: Vec::new(),
            follows_signal: false,
        };
        let mut listed = vec![device(3, "usb-old"), device(5, "usb-x")];
        super::renumber_moved(&mut listed, &[(3, "usb-new".to_string())]);
        assert_eq!((listed[0].id, listed[0].unique_id.as_str()), (3, "usb-new"));
        let mut scanned = vec![device(7, "usb-new"), device(5, "usb-x")];
        super::renumber_moved(&mut scanned, &[(3, "usb-new".to_string())]);
        assert_eq!(
            (scanned[0].id, scanned[0].unique_id.as_str()),
            (3, "usb-new")
        );
        assert_eq!(scanned[1].id, 5);
    }

    /// A Reset input asks an open camera's capture to close and open
    /// again — once — keeping its references; a camera not open is not
    /// asked.
    #[test]
    fn a_reset_asks_an_open_cameras_capture_to_open_again() {
        static ASKED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn listens(
            _: &CameraDeviceInfo,
            _: Option<CameraMode>,
            sink: &Arc<Sink>,
            stop: &AtomicBool,
            opened: &mpsc::Sender<Result<CameraMode>>,
        ) {
            let _ = opened.send(Ok(CameraMode {
                width: 64,
                height: 36,
                fps: 60,
            }));
            while !stop.load(Ordering::Relaxed) {
                if sink.take_restart() {
                    ASKED.fetch_add(1, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let gpu = crate::renderer::context::GpuContext::new_headless().expect("GPU");
        let mut mgr = CameraManager::with_devices(&[("a", "ATEM")], listens);
        let id = mgr.devices()[0].id;
        assert!(!mgr.restart(id), "not open: nothing to ask");
        mgr.open_camera(id, &gpu.device, None).unwrap();
        mgr.open_camera(id, &gpu.device, None).unwrap();
        assert!(mgr.restart(id));
        let waited = Instant::now();
        while ASKED.load(Ordering::Relaxed) == 0 && waited.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(ASKED.load(Ordering::Relaxed), 1, "asked once");
        assert!(mgr.is_active(id), "still open");
        mgr.release_camera(id);
        assert!(mgr.is_active(id), "its other reference kept");
        mgr.release_camera(id);
    }

    /// A camera's stop is remembered for as long as asked.
    #[test]
    fn a_camera_that_stopped_lately_says_so() {
        let sink = super::Sink::default();
        let long = std::time::Duration::from_secs(60);
        assert!(!sink.stopped_within(long), "never stopped");
        sink.note_stop();
        assert!(sink.stopped_within(long));
        assert!(!sink.stopped_within(std::time::Duration::ZERO));
    }

    /// Holds are counted: two holding one device, one letting go leaves it
    /// held.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_device_held_twice_stays_held_till_both_let_go() {
        let mut open = std::collections::HashMap::new();
        super::hold(&mut open, "usb-a");
        super::hold(&mut open, "usb-a");
        super::let_go(&mut open, "usb-a");
        assert!(open.contains_key("usb-a"));
        super::let_go(&mut open, "usb-a");
        assert!(open.is_empty());
    }

    /// A steady camera is given 8 of its frames, a quarter second at least,
    /// and as long as its last opening took, five seconds at most; a camera
    /// has stalled once its picture has stood still longer than it is
    /// given, counted from its opening at most.
    #[test]
    fn a_steady_camera_is_given_a_quarter_second_or_its_opening() {
        use super::{steady_limit, STALL};
        let s = std::time::Duration::from_secs_f64;
        let near = |a: std::time::Duration, b: f64| (a.as_secs_f64() - b).abs() < 1e-6;
        assert!(
            near(steady_limit(60.0, None), 0.25),
            "60p: a quarter second"
        );
        assert!(near(steady_limit(25.0, None), 0.32), "25p: 8 frames");
        assert!(
            near(steady_limit(60.0, Some(s(0.15))), 0.25),
            "an ATEM back in 0.15 s"
        );
        assert!(
            near(steady_limit(60.0, Some(s(1.5))), 1.5),
            "a camera slow to start"
        );
        assert_eq!(steady_limit(1.0, None), STALL, "never over five seconds");
        assert_eq!(steady_limit(60.0, Some(s(9.0))), STALL);
        assert!(!stalled(s(20.0), Some(s(0.2)), s(0.25)), "a hiccup");
        assert!(stalled(s(20.0), Some(s(0.3)), s(0.25)));
        assert!(
            !stalled(s(3.0), None, STALL),
            "its first picture on its way"
        );
        assert!(stalled(s(5.5), None, STALL));
        assert!(
            !stalled(s(3.0), Some(s(10.0)), STALL),
            "the last before it opened"
        );
    }

    /// Pictures `every` apart from `from` for `span`; the time of the last.
    fn feed(
        arrivals: &mut super::Arrivals,
        from: std::time::Instant,
        every: std::time::Duration,
        span: std::time::Duration,
    ) -> std::time::Instant {
        let (mut t, mut last) = (from, from);
        while t <= from + span {
            arrivals.saw(t);
            last = t;
            t += every;
        }
        last
    }

    /// Steady is two seconds of pictures with no wait over a quarter
    /// second; a burst as it opens is not. Steady, it is given its limit;
    /// otherwise five seconds.
    #[test]
    fn a_camera_is_steady_after_two_seconds_of_pictures() {
        let ms = std::time::Duration::from_millis;
        let t0 = std::time::Instant::now();
        let mut a = super::Arrivals::default();
        assert!(!a.opening(60.0, t0), "opened at first: not steady");
        let t = feed(&mut a, t0, ms(17), ms(300));
        assert!(!a.steady(), "a burst as it opens");
        assert_eq!(a.stall_limit(), super::STALL);
        let t = feed(&mut a, t + ms(17), ms(17), ms(1800));
        assert!(a.steady(), "two seconds at its rate");
        assert_eq!(a.stall_limit(), ms(250));
        // A wait over a quarter second starts a new run.
        let t = feed(&mut a, t + ms(400), ms(17), ms(500));
        assert!(!a.steady(), "half a second since a wait");
        feed(&mut a, t + ms(17), ms(17), ms(2000));
        assert!(a.steady());
    }

    /// Started again steady, a camera is steady again from its first
    /// picture — five times in a row at most, till it has run two seconds
    /// again; started again not steady, it is not.
    #[test]
    fn a_camera_started_again_steady_is_steady_from_its_first_picture() {
        let ms = std::time::Duration::from_millis;
        let t0 = std::time::Instant::now();
        let mut a = super::Arrivals::default();
        a.opening(60.0, t0);
        let mut t = feed(&mut a, t0, ms(17), ms(2500));
        assert!(a.steady());
        for restart in 1..=5 {
            t += ms(1000);
            assert!(a.opening(60.0, t), "restart {restart}: carried");
            assert!(!a.steady(), "no picture yet");
            assert!(a.last.is_none());
            // Back in 0.15 s, a few pictures, then a stall again.
            t = feed(&mut a, t + ms(150), ms(17), ms(300));
            assert!(
                a.steady(),
                "restart {restart}: steady from its first picture"
            );
            assert_eq!(a.stall_limit(), ms(250), "back in 0.15 s: a quarter second");
        }
        t += ms(1000);
        assert!(!a.opening(60.0, t), "a sixth time in a row: not carried");
        t = feed(&mut a, t + ms(150), ms(17), ms(300));
        assert!(!a.steady(), "five seconds again");
        // Two seconds of pictures: steady, and carried again.
        t = feed(&mut a, t + ms(17), ms(17), ms(2000));
        assert!(a.steady());
        assert!(
            a.opening(60.0, t + ms(500)),
            "after a long run: carried again"
        );
        // Started again while not steady: not carried.
        let mut b = super::Arrivals::default();
        b.opening(60.0, t0);
        feed(&mut b, t0, ms(17), ms(500));
        assert!(!b.opening(60.0, t0 + ms(3000)));
    }

    /// The time an opening takes to bring the first picture sets how long a
    /// steady camera is given: a stall shorter than starting it again is
    /// waited out.
    #[test]
    fn a_camera_slow_to_open_is_given_longer() {
        let ms = std::time::Duration::from_millis;
        let t0 = std::time::Instant::now();
        let mut a = super::Arrivals::default();
        a.opening(60.0, t0);
        let t = feed(&mut a, t0 + ms(1500), ms(17), ms(2500));
        assert!(a.steady());
        assert_eq!(a.stall_limit(), ms(1500), "it took 1.5 s to open");
        a.opening(60.0, t + ms(500));
        feed(&mut a, t + ms(650), ms(17), ms(100));
        assert_eq!(a.stall_limit(), ms(250), "started again in 0.15 s");
    }

    /// A picture after a longer wait than a steady camera of its rate is
    /// given makes it a source sending as it changes, for good; a camera
    /// started again does not count the time it was out as a wait.
    #[test]
    fn a_long_wait_makes_a_camera_sparse_for_good() {
        let ms = std::time::Duration::from_millis;
        let t0 = std::time::Instant::now();
        let mut a = super::Arrivals::default();
        a.opening(30.0, t0);
        // The time it was out is not a wait of its own.
        let t = feed(&mut a, t0, ms(33), ms(2500));
        a.opening(30.0, t + ms(10_000));
        let t = feed(&mut a, t + ms(10_000), ms(33), ms(2500));
        assert!(a.steady(), "started again after ten seconds out");
        // A wait of half a second at 30 fps (given 0.27 s): a hiccup.
        a.saw(t + ms(500));
        let t = feed(&mut a, t + ms(533), ms(33), ms(2500));
        assert!(a.steady(), "a wait within its limit and a little");
        // 0.7 s: a source that sends as it changes.
        a.saw(t + ms(700));
        let t = feed(&mut a, t + ms(733), ms(33), ms(3000));
        assert!(!a.steady(), "sparse");
        a.opening(30.0, t + ms(100));
        let t = feed(&mut a, t + ms(100), ms(33), ms(3000));
        assert!(!a.steady(), "still sparse after it is started again");
        // Ten seconds at its rate: it streams after all.
        let t = feed(&mut a, t + ms(33), ms(33), ms(6000));
        assert!(!a.steady(), "nine seconds");
        let t = feed(&mut a, t + ms(33), ms(33), ms(1100));
        assert!(a.steady(), "ten seconds: steady again");
        // Just cleared, it is started again once without being kept
        // steady; twenty seconds on, it is kept steady again.
        assert!(!a.opening(30.0, t + ms(300)), "just cleared: not carried");
        let t = feed(&mut a, t + ms(400), ms(33), ms(21_000));
        assert!(a.opening(30.0, t + ms(300)), "twenty seconds on: carried");
    }

    use super::*;
    use nokhwa::utils::Resolution;

    /// A camera's LUT follows the show only if it is the one moved.
    #[test]
    fn a_lut_in_the_show_follows_it_when_it_moves() {
        let lut = |file: &str| CameraProfile::Lut { file: file.into() };
        let (from, to) = (
            lut("/shows/A/.varda/luts/slog3.cube"),
            lut("/shows/B/.varda/luts/slog3.cube"),
        );
        let mut inside = from.clone();
        relocate_lut(&mut inside, &from, &to);
        assert_eq!(inside, to);
        for other in [
            lut("/shows/A/.varda/luts/other.cube"),
            CameraProfile::default(),
        ] {
            let mut kept = other.clone();
            relocate_lut(&mut kept, &from, &to);
            assert_eq!(kept, other);
        }
    }

    /// The real macOS calls: every effect this Mac has is asked about (a
    /// missing one skipped by `respondsToSelector:`), and each answer is a
    /// known name. A wrong selector or return type would crash here.
    #[cfg(target_os = "macos")]
    #[test]
    fn this_macs_camera_effects_can_be_read() {
        let known = ["Portrait", "Studio Light", "Edge Light", "Background", "Reactions gestures"];
        for effect in read_video_effects() {
            assert!(known.contains(&effect), "{effect}");
        }
    }

    /// Camera effects are said once while they stay the same, again when
    /// they change, and not at all when all are off.
    #[test]
    fn camera_effects_are_warned_about_once_per_change() {
        let mut warned = Vec::new();
        assert_eq!(news(&["Portrait"], &mut warned), Some(vec!["Portrait"]));
        assert_eq!(news(&["Portrait"], &mut warned), None, "already said");
        assert_eq!(
            news(&["Portrait", "Reactions gestures"], &mut warned),
            Some(vec!["Portrait", "Reactions gestures"]),
            "one more came on"
        );
        assert_eq!(news(&[], &mut warned), None, "all off: nothing to say");
        assert_eq!(news(&["Portrait"], &mut warned), Some(vec!["Portrait"]), "on again");
        let text = video_effects_warning(&["Portrait", "Reactions gestures"]);
        assert!(text.contains("Portrait, Reactions gestures"), "{text}");
        assert!(text.contains("Video Effects"), "{text}");
    }

    /// The control screen reads `access_denied` every frame, so it answers
    /// from the last scan and never asks the OS's privacy service again.
    #[test]
    fn access_denied_answers_from_the_last_scan() {
        let mut mgr = CameraManager::disabled();
        assert!(!mgr.access_denied(), "cameras switched off");
        mgr.initialized = true;
        mgr.denied = false;
        assert!(!mgr.access_denied(), "allowed at the last scan");
        mgr.denied = true;
        assert!(mgr.access_denied(), "refused at the last scan");
    }

    fn f(width: u32, height: u32, format: FrameFormat, fps: u32) -> CameraFormat {
        CameraFormat::new(Resolution::new(width, height), format, fps)
    }

    fn rate(width: u32, height: u32, format: FrameFormat, fps: f64) -> Rate {
        Rate {
            width,
            height,
            format,
            fps,
            code: 0,
        }
    }

    fn mode(width: u32, height: u32, fps: u32) -> CameraMode {
        CameraMode { width, height, fps }
    }

    #[test]
    fn a_mode_is_asked_for_in_the_cheapest_pixel_format_on_offer() {
        let rates = [
            rate(1920, 1080, FrameFormat::MJPEG, 60.0),
            rate(1920, 1080, FrameFormat::NV12, 60.0),
            rate(1920, 1080, FrameFormat::YUYV, 30.0),
            rate(1280, 720, FrameFormat::MJPEG, 60.0),
        ];
        assert_eq!(
            target(&rates, mode(1920, 1080, 60)),
            Some(rate(1920, 1080, FrameFormat::NV12, 60.0))
        );
        assert_eq!(
            target(&rates, mode(1280, 720, 60)),
            Some(rate(1280, 720, FrameFormat::MJPEG, 60.0))
        );
        assert_eq!(target(&rates, mode(1280, 720, 30)), None);
    }

    /// The ATEM Mini Pro ISO since its update, set to 60p, says 59.9999: it
    /// is listed as 60, asked for as 60 — within a frame of what it says,
    /// where the backend's 59 was refused — and a show saved at 59.94 finds it.
    #[test]
    fn a_rate_a_hair_under_a_whole_number_is_that_number() {
        let atem = [
            rate(1920, 1080, FrameFormat::NV12, 59.9999),
            rate(1920, 1080, FrameFormat::NV12, 30.0),
        ];
        let device = CameraDeviceInfo {
            id: 0,
            name: "Blackmagic Design".into(),
            unique_id: "0x1100001edb4100".into(),
            index: CameraIndex::Index(0),
            formats: vec![
                f(1920, 1080, FrameFormat::NV12, 59),
                f(1920, 1080, FrameFormat::NV12, 30),
            ],
            rates: atem.to_vec(),
            follows_signal: false,
        };
        assert_eq!(
            device.modes(),
            vec![mode(1920, 1080, 60), mode(1920, 1080, 30)]
        );
        let sixty = target(&atem, mode(1920, 1080, 60)).unwrap();
        assert_eq!(sixty.request().frame_rate(), 60);
        let saved_at_59_94 = target(&atem, mode(1920, 1080, 59)).unwrap();
        assert!((saved_at_59_94.fps - 59.9999).abs() < 1e-9);
        assert_eq!(target(&atem, mode(1920, 1080, 50)), None, "not 50");
    }

    /// Broadcast rates stay what they are: 59.94 is kept as 59, shown as
    /// 59.94, and asked for as 60, within a frame of it.
    #[test]
    fn broadcast_rates_are_kept_and_asked_for_within_a_frame() {
        let old_atem = [
            rate(1920, 1080, FrameFormat::YUYV, 59.94),
            rate(1920, 1080, FrameFormat::YUYV, 29.97),
        ];
        let modes: Vec<CameraMode> = old_atem.iter().map(Rate::mode).collect();
        assert_eq!(modes, vec![mode(1920, 1080, 59), mode(1920, 1080, 29)]);
        assert_eq!(modes[0].label(), "1920×1080 59.94p");
        let asked = target(&old_atem, mode(1920, 1080, 29)).unwrap().request();
        assert_eq!(asked.frame_rate(), 30);
        for (fps, listed) in [
            (23.976, 23),
            (25.0, 25),
            (50.0, 50),
            (119.88, 119),
            (120.0, 120),
        ] {
            assert_eq!(listed_fps(fps), listed, "{fps}");
        }
    }

    /// Any format the backend takes opens the camera, the nearest the rate
    /// wanted first; the rest follow fastest first.
    #[test]
    fn the_format_nearest_the_rate_wanted_is_tried_first() {
        let formats = [
            f(1920, 1080, FrameFormat::NV12, 30),
            f(1280, 720, FrameFormat::NV12, 60),
            f(1920, 1080, FrameFormat::NV12, 59),
        ];
        let wanted = rate(1920, 1080, FrameFormat::NV12, 59.9999);
        let order = open_order(&formats, Some(&wanted));
        assert_eq!(order[0], f(1920, 1080, FrameFormat::NV12, 59));
        assert_eq!(
            order[1],
            f(1280, 720, FrameFormat::NV12, 60),
            "then the fastest"
        );
        assert_eq!(order.len(), 3);
    }

    /// Lists the formats of every camera plugged in. Touches real devices, so
    /// only by hand: `cargo test --lib real_cameras_list -- --ignored --nocapture`.
    #[cfg(not(target_os = "macos"))]
    #[test]
    #[ignore = "needs a camera plugged in"]
    fn real_cameras_list_their_formats() {
        let cameras = nokhwa::query(nokhwa::utils::ApiBackend::Auto).expect("cameras");
        for info in cameras {
            let (formats, rates) = CameraManager::read_formats(info.index());
            let device = CameraDeviceInfo {
                id: 0,
                name: info.human_name(),
                unique_id: info.misc(),
                index: info.index().clone(),
                formats,
                rates,
                follows_signal: false,
            };
            let modes = device.modes();
            println!("{}: {} formats", device.name, device.formats.len());
            for mode in &modes {
                println!("  {}", mode.label());
            }
            assert!(!modes.is_empty(), "{} lists no formats", device.name);
        }
    }

    fn found(unique_id: &str, name: &str) -> Found {
        Found {
            unique_id: unique_id.into(),
            name: name.into(),
            index: CameraIndex::String(unique_id.into()),
            formats: Vec::new(),
            rates: Vec::new(),
            follows_signal: false,
        }
    }

    fn ids(mgr: &CameraManager) -> Vec<(String, CameraId)> {
        mgr.devices()
            .iter()
            .map(|d| (d.unique_id.clone(), d.id))
            .collect()
    }

    /// A camera keeps its number however others are plugged in and out
    /// around it: an input numbered by list position would switch cameras.
    #[test]
    fn a_camera_keeps_its_number_as_others_come_and_go() {
        let mut mgr = CameraManager::disabled();
        mgr.list(vec![found("a", "FaceTime"), found("b", "ATEM")]);
        assert_eq!(ids(&mgr), [("a".into(), 0), ("b".into(), 1)]);
        mgr.list(vec![found("b", "ATEM")]);
        assert_eq!(ids(&mgr), [("b".into(), 1)], "the first unplugged");
        mgr.list(vec![
            found("c", "ATEM"),
            found("a", "FaceTime"),
            found("b", "ATEM"),
        ]);
        assert_eq!(
            ids(&mgr),
            [("c".into(), 2), ("a".into(), 0), ("b".into(), 1)],
            "back, with a new one first"
        );
    }

    /// Two ATEMs of one name: each saved input finds its own by unique ID;
    /// one saved before IDs were kept takes one not already showing.
    #[test]
    fn a_saved_camera_is_found_by_its_device_then_its_name() {
        let mut mgr = CameraManager::disabled();
        mgr.list(vec![
            found("a", "Blackmagic Design"),
            found("b", "Blackmagic Design"),
            found("c", "FaceTime HD"),
        ]);
        let devices = mgr.devices();
        let none = |_| false;
        let first = |id| id == 0;
        let both = |id| id <= 1;
        let name = "Blackmagic Design";
        assert_eq!(find_in(devices, none, Some("b"), name), Some(1));
        assert_eq!(
            find_in(devices, first, Some("b"), name),
            Some(1),
            "its own, even if showing"
        );
        assert_eq!(find_in(devices, none, None, name), Some(0));
        assert_eq!(
            find_in(devices, first, None, name),
            Some(1),
            "the other one"
        );
        assert_eq!(
            find_in(devices, both, None, name),
            Some(0),
            "unsaved: a second input of one"
        );
        assert_eq!(
            find_in(devices, first, Some("moved"), name),
            Some(1),
            "moved port: by name"
        );
        assert_eq!(
            find_in(devices, both, Some("moved"), name),
            None,
            "not the other's camera"
        );
        assert_eq!(find_in(devices, none, None, "Elgato"), None);
    }

    #[test]
    fn a_range_of_rates_lists_its_ends_and_the_usual_rates_between() {
        assert_eq!(rates_in_range(59.99988, 59.99988), vec![59.99988]);
        assert_eq!(rates_in_range(1.0, 30.0), vec![30.0, 25.0, 24.0]);
        assert_eq!(
            rates_in_range(15.0, 60.0),
            vec![60.0, 50.0, 30.0, 25.0, 24.0, 15.0]
        );
    }

    #[test]
    fn an_untagged_picture_is_read_as_sd_or_hd_by_its_size() {
        assert_eq!(untagged_matrix(480), YuvStandardMatrix::Bt601);
        assert_eq!(untagged_matrix(576), YuvStandardMatrix::Bt601);
        assert_eq!(untagged_matrix(720), YuvStandardMatrix::Bt709);
    }

    /// A show frame taking its picture while one is being put in waits for
    /// it rather than going without: going without showed the picture
    /// before twice and skipped one the next frame, more often the nearer
    /// the camera's frames came to the show's.
    #[test]
    fn a_picture_taken_while_one_is_put_in_is_not_missed() {
        let sink = Arc::new(Sink::default());
        sink.put(
            Frame {
                data: vec![1; 16],
                width: 2,
                height: 2,
            },
            10.0,
        );
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let putting = {
            let sink = Arc::clone(&sink);
            std::thread::spawn(move || {
                let pictures = sink.pictures.lock().expect("the pictures");
                held_tx.send(()).expect("the test waits");
                std::thread::sleep(std::time::Duration::from_millis(20));
                drop(pictures);
            })
        };
        held_rx.recv().expect("held");
        assert!(sink.take(10.05, 0).is_some(), "waited for the pictures, not gone without");
        putting.join().expect("the putting thread");
    }

    /// Memory for pictures goes round: capture takes a spare and the render
    /// loop hands back the memory of each picture it took.
    #[test]
    fn picture_memory_is_reused() {
        let sink = Sink::default();
        let mut data = sink.buffer();
        data.clear();
        data.resize(16, 1);
        sink.put(
            Frame {
                data,
                width: 2,
                height: 2,
            },
            10.0,
        );
        let taken = sink.take(10.05, 0).expect("a picture");
        assert_eq!(taken.data[0], 1);
        assert_eq!(sink.frames(), 1);
        sink.recycle(taken.data);
        assert_eq!(sink.buffer().capacity(), 16, "its memory back");
        assert!(sink.take(10.07, 0).is_none(), "nothing new");
        assert!(sink.since_last_frame().unwrap() < Duration::from_secs(1));
    }

    /// Blackmagic's devices told of afresh keep the other cameras as they
    /// were — numbered as before, macOS not asked again.
    #[test]
    fn a_blackmagic_refresh_keeps_the_other_cameras() {
        let mut mgr = CameraManager::with_devices(&[("a", "ATEM")], |_, _, _, _, _| {});
        mgr.initialized = true;
        let id = mgr.devices()[0].id;
        mgr.refresh_blackmagic();
        assert!(mgr
            .devices()
            .iter()
            .any(|d| d.id == id && d.unique_id == "a" && d.name == "ATEM"));
        assert!(mgr
            .devices()
            .iter()
            .all(|d| d.unique_id == "a" || crate::decklink::input::is_blackmagic(&d.unique_id)));
    }

    /// A Blackmagic box busy because another input shows it as macOS lists
    /// it is said to be held by that camera; any other failure, or another
    /// box, is not.
    #[test]
    fn a_blackmagic_box_busy_in_another_input_says_which() {
        let busy = "the device would not start its video input (in use by another app?)";
        let open = ["FaceTime HD", "Blackmagic UltraStudio Mini Recorder 12G"];
        let name = "UltraStudio Mini Recorder 12G (Blackmagic)";
        assert_eq!(
            held_by(name, busy, open.into_iter()),
            Some("Blackmagic UltraStudio Mini Recorder 12G")
        );
        assert_eq!(held_by(name, "not connected", open.into_iter()), None);
        assert_eq!(held_by(name, busy, ["FaceTime HD"].into_iter()), None);
        assert_eq!(held_by("FaceTime HD", busy, open.into_iter()), None);
        let longer = ["Blackmagic UltraStudio 4K Extreme"];
        assert_eq!(
            held_by("UltraStudio 4K (Blackmagic)", busy, longer.into_iter()),
            None,
            "another box"
        );
        let two = ["Blackmagic UltraStudio Mini Recorder 12G"; 2];
        assert_eq!(
            held_by(name, busy, two.into_iter()),
            None,
            "which, cannot be told"
        );
    }

    /// A show frame that shows the picture before again — none new having
    /// come — is counted in what the camera sends; opening it again starts
    /// the count afresh.
    #[test]
    fn a_picture_shown_again_is_counted() {
        let sink = Sink::default();
        sink.note_signal(SignalSeen::default(), (0, 0));
        let now = crate::host_clock::host_seconds();
        sink.put(
            Frame {
                data: vec![0; 16],
                width: 2,
                height: 2,
            },
            now,
        );
        for n in 1..=4 {
            let _ = sink.take(now + f64::from(n) / 60.0, 1);
        }
        let signal = sink.signal().expect("its signal");
        assert!(signal.shown_again >= 1, "{signal:?}");
        sink.opening(60.0);
        assert_eq!(sink.signal().expect("its signal").shown_again, 0);
    }

    /// Pictures let go before a camera is opened again are not put down to
    /// the new opening as left out.
    #[test]
    fn pictures_let_go_before_an_opening_are_not_counted_after() {
        let sink = Sink::default();
        sink.note_signal(SignalSeen::default(), (0, 0));
        let now = crate::host_clock::host_seconds();
        for n in 0..10 {
            sink.put(
                Frame {
                    data: vec![0; 16],
                    width: 2,
                    height: 2,
                },
                now + f64::from(n) * 1e-4,
            );
        }
        sink.opening(60.0);
        let _ = sink.take(now + 1.0 / 60.0, 1);
        let left_out = sink.signal().expect("its signal").left_out;
        assert!(left_out < 6, "{left_out} left out, those before counted");
    }

    /// A camera's own sound (a Blackmagic input's) is said once some has
    /// come, goes to the mixer once it takes it, as -1 to 1, and what a full
    /// feed throws away is counted for it to put back as silence; each
    /// camera opened has its own key.
    #[test]
    fn a_cameras_own_sound_goes_to_the_mixer_once_it_takes_it() {
        let sink = Sink::default();
        assert!(sink.sound_format().is_none(), "none come yet");
        sink.put_sound(&[0, 0], 2);
        let (rate, channels, key) = sink.sound_format().expect("some came");
        assert_eq!((rate, channels), (48_000, 2));
        let (tx, rx) = crossbeam_channel::bounded(1);
        sink.set_sound_feed(tx);
        sink.put_sound(&[1 << 30, -(1 << 30)], 2);
        let chunk = rx.try_recv().expect("a chunk");
        assert_eq!(chunk.samples, vec![0.5, -0.5]);
        assert_eq!(chunk.lost_before, 0);
        sink.put_sound(&[0; 4], 2);
        sink.put_sound(&[0; 4], 2);
        let _ = rx.try_recv();
        sink.put_sound(&[0; 2], 2);
        assert_eq!(rx.try_recv().expect("a chunk").lost_before, 4, "four lost");
        let other = Sink::default();
        other.put_sound(&[0, 0], 2);
        assert_ne!(other.sound_format().map(|f| f.2), Some(key));
    }

    /// Another format asked for a camera opens it again at that format:
    /// its capture is told to, and takes the format as it does, once.
    #[test]
    fn another_format_asked_for_opens_the_camera_again_at_it() {
        let sink = Sink::default();
        assert!(sink.take_mode().is_none());
        let fifty = CameraMode {
            width: 1920,
            height: 1080,
            fps: 50,
        };
        sink.ask_mode(Some(fifty));
        assert!(sink.take_restart(), "told to open again");
        assert_eq!(sink.take_mode(), Some(Some(fifty)));
        assert_eq!(sink.take_mode(), None, "once");
        sink.ask_mode(None);
        assert_eq!(sink.take_mode(), Some(None), "its best");
    }

    /// Brightness is sampled on a grid, at each pixel's brightness byte:
    /// those well below video black are counted.
    #[test]
    fn brightness_below_video_black_is_counted_on_a_grid() {
        // 128 × 72 'uyvy': brightness at odd bytes; the left half black (0),
        // the right half just under video black (10: footroom).
        let (w, h) = (128_usize, 72_usize);
        let stride = w * 2;
        let mut bytes = vec![128_u8; stride * h];
        for y in 0..h {
            for x in 0..w {
                bytes[y * stride + x * 2 + 1] = if x < w / 2 { 0 } else { 10 };
            }
        }
        let (below, taken) = below_black(&bytes, stride, (w, h), (1, 2));
        assert_eq!(taken, 64 * 36);
        assert_eq!(below, 32 * 36, "the left half");
        assert_eq!(below_black(&bytes, stride, (0, 0), (1, 2)), (0, 0));
    }

    /// A camera's signal, as its pictures say: the latest's size and
    /// format, its rate as its pictures come, and its darks counted over
    /// the latest looks — older ones counting less.
    #[test]
    fn a_cameras_signal_is_what_its_pictures_say_lately() {
        let sink = Sink::default();
        assert!(sink.signal().is_none(), "no picture read yet");
        let _ = sink.opening(60.0); // its capture opened at 60
        let seen = || SignalSeen {
            width: 1920,
            height: 1080,
            format: "2vuy".into(),
            matrix: "BT.709".into(),
            full_range: Some(false),
            ..SignalSeen::default()
        };
        sink.note_signal(seen(), (2304, 2304));
        for _ in 0..60 {
            sink.note_signal(seen(), (0, 2304));
        }
        for i in 0..120_u32 {
            sink.put(
                Frame {
                    data: vec![0; 16],
                    width: 2,
                    height: 2,
                },
                10.0 + f64::from(i) / 50.0,
            );
        }
        let signal = sink.signal().expect("a signal");
        assert_eq!(
            signal.opened_fps,
            Some(60.0),
            "the rate its capture opened at"
        );
        assert_eq!((signal.width, signal.format.as_str()), (1920, "2vuy"));
        assert!(
            signal.below_black < 0.001,
            "a dark look long past: {}",
            signal.below_black
        );
        assert!((signal.fps - 50.0).abs() < 0.5, "{} fps", signal.fps);
        assert_eq!(signal.other_rate().map(f64::round), Some(50.0));
    }

    /// Each picture is told to the picker with when it came, and each show
    /// frame with when it was drawn: an input whose pictures keep coming as
    /// show frames are drawn is known by it, and held a frame at the lowest
    /// latency (see `FramePicker::set_newest`).
    #[test]
    fn pictures_are_picked_knowing_when_they_came_and_were_drawn() {
        let sink = Sink::default();
        let now = crate::host_clock::host_seconds();
        sink.put(
            Frame {
                data: vec![0; 16],
                width: 2,
                height: 2,
            },
            now,
        );
        assert!(sink.take(now + 0.03, 0).is_some());
        let pictures = sink.pictures.lock().expect("pictures");
        assert_eq!(pictures.told_at_the_draw(), 1, "came as it was drawn");
    }

    /// A camera's pictures are taken by when they were captured, one per
    /// show frame in turn — not the newest to have come, which skipped some
    /// and showed others twice when they came near the moment frames were
    /// drawn.
    #[test]
    fn pictures_are_taken_in_turn_by_when_they_were_captured() {
        let sink = Sink::default();
        let period = 1.0 / 60.0;
        let mut shown = Vec::new();
        for k in 0..120_u8 {
            let at = 10.0 + f64::from(k) * period;
            sink.put(
                Frame {
                    data: vec![k; 4],
                    width: 1,
                    height: 1,
                },
                at,
            );
            // Two pictures come before some show frames and none before
            // others, as a camera's pictures come unevenly.
            if k % 2 == 1 {
                for show in [at - period, at] {
                    if let Some(frame) = sink.take(show + 0.040, 0) {
                        shown.push(frame.data[0]);
                    }
                }
            }
        }
        let settled = &shown[10..];
        assert!(
            settled.windows(2).all(|w| w[1] == w[0] + 1),
            "in turn: {shown:?}"
        );
    }

    /// A camera started for a waiting deck takes the OS a moment to run:
    /// asking after it never waits, and once it runs it is open, with its
    /// size and the decks that waited for it.
    #[test]
    fn a_camera_starting_in_the_background_never_holds_up_the_frame() {
        let gpu = crate::renderer::context::GpuContext::new_headless().expect("GPU");
        let mut mgr = CameraManager::disabled();
        let (tell, opened) = mpsc::channel();
        let (done_tx, done) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _done = done_tx;
            std::thread::sleep(Duration::from_millis(300));
            let _ = tell.send(Ok(CameraMode {
                width: 64,
                height: 36,
                fps: 60,
            }));
        });
        mgr.opening.insert(
            7,
            Opening {
                name: "ATEM".into(),
                sink: Arc::new(Sink::default()),
                stop_flag: Arc::new(AtomicBool::new(false)),
                thread,
                done,
                opened,
                since: Instant::now(),
                decks: vec!["deck-1".into()],
                asked: false,
                mode: None,
            },
        );
        assert!(mgr.is_opening(7));
        let start = Instant::now();
        let mut slowest = Duration::ZERO;
        let mut got = Vec::new();
        while got.is_empty() && start.elapsed() < Duration::from_secs(3) {
            let asked = Instant::now();
            got = mgr.poll_opening(&gpu.device);
            slowest = slowest.max(asked.elapsed());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            slowest < Duration::from_millis(50),
            "a frame waited {slowest:?}"
        );
        let opened = got.pop().expect("it ran");
        assert_eq!(opened.id, 7);
        assert_eq!(opened.decks, ["deck-1"]);
        assert_eq!(opened.result.unwrap(), (64, 36));
        assert!(mgr.is_active(7) && !mgr.is_opening(7));
        assert_eq!(mgr.resolution(7), Some((64, 36)));
        mgr.release_camera(7);
        assert!(!mgr.is_active(7));
    }

    /// A camera opened while it is still starting in the background — an
    /// input added, then undone and redone, or a show opened that uses it —
    /// is waited for, not started twice: the second start was overwritten as
    /// the first ran, its capture left running unseen and its input black.
    #[test]
    fn a_camera_opened_while_it_starts_is_started_once() {
        static STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        fn counted(
            _: &CameraDeviceInfo,
            _: Option<CameraMode>,
            _: &Arc<Sink>,
            stop: &AtomicBool,
            opened: &mpsc::Sender<Result<CameraMode>>,
        ) {
            STARTS.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(300));
            let _ = opened.send(Ok(CameraMode {
                width: 64,
                height: 36,
                fps: 60,
            }));
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        let gpu = crate::renderer::context::GpuContext::new_headless().expect("GPU");
        let mut mgr = CameraManager::with_devices(&[("a", "ATEM")], counted);
        let id = mgr.devices()[0].id;
        mgr.open_in_background(id, None, vec!["deck-1".into()], true)
            .unwrap();
        assert_eq!(mgr.open_camera(id, &gpu.device, None).unwrap(), (64, 36));
        assert_eq!(STARTS.load(Ordering::Relaxed), 1, "started once");
        assert!(mgr.is_active(id) && !mgr.is_opening(id));
        assert!(
            mgr.poll_opening(&gpu.device).is_empty(),
            "nothing to bind over it"
        );
        mgr.release_camera(id);
        assert!(!mgr.is_active(id), "its one reference let go");
    }

    /// Letting a camera go never waits for its thread: one stalled in the
    /// OS held every output still for up to two seconds as its input was
    /// removed.
    #[test]
    fn letting_a_camera_go_never_waits_for_its_thread() {
        fn deaf_to_stop(
            _: &CameraDeviceInfo,
            _: Option<CameraMode>,
            _: &Arc<Sink>,
            _: &AtomicBool,
            opened: &mpsc::Sender<Result<CameraMode>>,
        ) {
            let _ = opened.send(Ok(CameraMode {
                width: 64,
                height: 36,
                fps: 60,
            }));
            std::thread::sleep(Duration::from_secs(3));
        }
        let gpu = crate::renderer::context::GpuContext::new_headless().expect("GPU");
        let mut mgr = CameraManager::with_devices(&[("a", "ATEM")], deaf_to_stop);
        let id = mgr.devices()[0].id;
        assert_eq!(mgr.open_camera(id, &gpu.device, None).unwrap(), (64, 36));
        let asked = Instant::now();
        mgr.release_camera(id);
        let took = asked.elapsed();
        assert!(took < Duration::from_millis(100), "waited {took:?}");
        assert!(!mgr.is_active(id));
    }

    /// A camera's colour reaches its capture while it is still starting and
    /// stays once it runs; a camera not open is left alone.
    #[test]
    fn a_cameras_colour_reaches_its_capture_starting_or_running() {
        let gpu = crate::renderer::context::GpuContext::new_headless().expect("GPU");
        let mut mgr = CameraManager::disabled();
        let (tell, opened) = mpsc::channel();
        let (done_tx, done) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _done = done_tx;
            let _ = tell.send(Ok(CameraMode {
                width: 64,
                height: 36,
                fps: 60,
            }));
        });
        mgr.opening.insert(
            7,
            Opening {
                name: "ATEM".into(),
                sink: Arc::new(Sink::default()),
                stop_flag: Arc::new(AtomicBool::new(false)),
                thread,
                done,
                opened,
                since: Instant::now(),
                decks: vec!["deck-1".into()],
                asked: false,
                mode: None,
            },
        );
        let full = CameraColour {
            range: CameraRange::Full,
            matrix: CameraMatrix::Bt709,
        };
        mgr.set_colours(&HashMap::from([(7, full), (9, full)]));
        assert_eq!(mgr.opening[&7].sink.colour(), full, "while starting");
        let start = Instant::now();
        while !mgr.is_active(7) && start.elapsed() < Duration::from_secs(3) {
            let _ = mgr.poll_opening(&gpu.device);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(mgr.colour(7), Some(full), "once running");
        assert_eq!(mgr.colour(9), None, "not open: nothing to set");
        mgr.set_colour(7, CameraColour::default());
        assert_eq!(mgr.colour(7), Some(CameraColour::default()));
        mgr.release_camera(7);
    }

    /// With a picture profile set, a camera's decks read its converted
    /// picture, made from each new frame; back on Normal, its own again. A
    /// LUT file that will not read leaves the picture as it comes, and is not
    /// tried again every frame.
    #[test]
    fn a_camera_with_a_profile_is_read_converted() {
        let Ok(gpu) = crate::renderer::context::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = CameraManager::disabled();
        let (tell, opened) = mpsc::channel();
        let (done_tx, done) = mpsc::channel::<()>();
        let thread = std::thread::spawn(move || {
            let _done = done_tx;
            let _ = tell.send(Ok(CameraMode {
                width: 8,
                height: 4,
                fps: 60,
            }));
        });
        let sink = Arc::new(Sink::default());
        mgr.opening.insert(
            7,
            Opening {
                name: "Sony FX6".into(),
                sink: Arc::clone(&sink),
                stop_flag: Arc::new(AtomicBool::new(false)),
                thread,
                done,
                opened,
                since: Instant::now(),
                decks: vec!["deck-1".into()],
                asked: false,
                mode: None,
            },
        );
        let start = Instant::now();
        while !mgr.is_active(7) && start.elapsed() < Duration::from_secs(3) {
            let _ = mgr.poll_opening(&gpu.device);
            std::thread::sleep(Duration::from_millis(5));
        }
        let own = mgr.texture_view(7).expect("open").clone();
        let needed = std::collections::HashSet::from([7]);
        let frame = || Frame {
            data: vec![128; 8 * 4 * 4],
            width: 8,
            height: 4,
        };

        sink.put(frame(), 10.0);
        mgr.set_profiles(&HashMap::from([(7, CameraProfile::SonySLog3Cine)]));
        mgr.update_selective(&gpu, &needed, 10.05);
        assert!(mgr.active[&7].converted.is_some(), "converted");
        assert!(mgr.texture_view(7) != Some(&own), "decks read the converted picture");

        mgr.set_profiles(&HashMap::from([(7, CameraProfile::Normal)]));
        sink.put(frame(), 10.1);
        mgr.update_selective(&gpu, &needed, 10.15);
        assert!(mgr.active[&7].converted.is_none());
        assert!(mgr.texture_view(7) == Some(&own), "its own again");

        let missing = CameraProfile::Lut {
            file: "/no/such/folder/look.cube".into(),
        };
        mgr.set_profiles(&HashMap::from([(7, missing.clone())]));
        mgr.update_selective(&gpu, &needed, 10.2);
        assert!(mgr.texture_view(7) == Some(&own), "as it comes");
        assert_eq!(mgr.active[&7].failed.as_ref(), Some(&missing), "not tried again");
        mgr.release_camera(7);
    }

    /// A range or matrix set by hand wins over what the camera says; on
    /// Auto, what it says stands.
    #[test]
    fn what_is_set_by_hand_wins_over_what_the_camera_says() {
        let auto = CameraColour::default();
        let set = CameraColour {
            range: CameraRange::Full,
            matrix: CameraMatrix::Bt2020,
        };
        assert_eq!(
            picture_range(auto, yuvutils_rs::YuvRange::Limited),
            yuvutils_rs::YuvRange::Limited
        );
        assert_eq!(
            picture_range(set, yuvutils_rs::YuvRange::Limited),
            yuvutils_rs::YuvRange::Full
        );
        assert_eq!(
            picture_matrix(auto, YuvStandardMatrix::Bt601),
            YuvStandardMatrix::Bt601
        );
        assert_eq!(
            picture_matrix(set, YuvStandardMatrix::Bt601),
            YuvStandardMatrix::Bt2020
        );
    }

    #[test]
    fn snapshot_frame_on_empty_manager_returns_none() {
        let mgr = CameraManager::disabled();
        assert!(mgr.snapshot_frame(0).is_none());
        assert!(mgr.snapshot_frame(42).is_none());
    }

    #[test]
    fn first_active_id_on_empty_manager_returns_none() {
        let mgr = CameraManager::disabled();
        assert!(mgr.first_active_id().is_none());
    }

    #[test]
    fn active_ids_on_empty_manager_returns_empty() {
        let mgr = CameraManager::disabled();
        assert!(mgr.active_ids().is_empty());
    }
}
