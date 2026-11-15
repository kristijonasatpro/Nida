//! Device-agnostic screen/window capture backend abstraction.
//!
//! A `ScreenCaptureBackend` produces RGBA/BGRA frames from an OS display or an
//! application window. The manager owns one backend per open capture and polls
//! it on a dedicated capture thread (see [`super::ScreenCaptureManager`]),
//! mirroring the `DepthBackend` / `CameraManager` split.
//!
//! Concrete backends live in [`super::platform`] and are selected by target OS
//! behind the default-on `screen-capture` cargo feature. A [`MockBackend`] is
//! always compiled so the manager, deck integration, persistence, API, and UI
//! can be built and tested with no display server and no permissions.
//!
//! See spec/screen-capture.md.

use std::fmt;

/// Whether a capture target is a whole display or a single window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureTargetKind {
    Display,
    Window,
}

impl CaptureTargetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Display => "display",
            Self::Window => "window",
        }
    }
}

/// A capturable display or window, as reported by a platform enumeration.
///
/// `platform_id` is the OS handle (CoreGraphics display id, window number, …).
/// It is deliberately **not** persisted — handles are ephemeral across restarts,
/// so scenes match on `label` / `(app, title)` instead. See spec/screen-capture.md
/// § Configuration and Persistence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureTargetInfo {
    pub kind: CaptureTargetKind,
    pub platform_id: u64,
    /// Human-readable name for UI and for display-target persistence matching.
    pub label: String,
    /// Owning application (bundle id or process name). Windows only.
    pub app: Option<String>,
    /// Window title at enumeration time. Windows only.
    pub title: Option<String>,
    pub width: u32,
    pub height: u32,
    /// This target belongs to Varda's own process.
    pub is_varda: bool,
    /// A display's own identity where the platform gives one — on a Mac its
    /// UUID, kept across restarts and whichever other screens are plugged in.
    /// Its label ("Display 2") is only its place among them as they are now:
    /// a third screen plugged in made a show's "Display 1" another screen
    /// (an M5 Max, 2026-11-09).
    pub uuid: Option<String>,
}

impl CaptureTargetInfo {
    /// Stable-ish identity used to detect "this is the same target" across a
    /// rescan, and to match a persisted scene back onto a live target.
    pub fn identity(&self) -> TargetIdentity {
        match self.kind {
            CaptureTargetKind::Display => TargetIdentity::Display {
                label: self.label.clone(),
                uuid: self.uuid.clone(),
            },
            CaptureTargetKind::Window => TargetIdentity::Window {
                app: self.app.clone().unwrap_or_default(),
                title: self.title.clone().unwrap_or_default(),
            },
        }
    }
}

/// The persisted, handle-free identity of a capture target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetIdentity {
    /// A display: by its own identity where one is known (see
    /// [`CaptureTargetInfo::uuid`]), else by the name it was listed under.
    Display { label: String, uuid: Option<String> },
    Window { app: String, title: String },
}

impl TargetIdentity {
    /// What a deck keeps of this, picked now, once `found` is opened for it
    /// — an input added, or its screen picked again. A display picked as
    /// itself — or by its name alone, as the window lists it and an API
    /// client may send no more — is kept as the one found: named as it is
    /// listed now, and by its identity from here on. One stood in for by
    /// another of its name, its own not plugged in, stays as it was, to be
    /// found again; and a window, whose title may be another by then.
    #[must_use]
    pub fn bound_to(&self, found: &CaptureTargetInfo) -> Self {
        match self {
            Self::Display { uuid, .. }
                if found.kind == CaptureTargetKind::Display
                    && (uuid.is_none() || *uuid == found.uuid) =>
            {
                found.identity()
            }
            _ => self.clone(),
        }
    }

    /// What a deck keeps of this, read from a show, once `found` is opened
    /// for it. A display found as itself is named as it is listed now. One
    /// the show has by its name alone — saved before displays had an
    /// identity — stays so: which screen had that name when it was picked
    /// is not known, and the one that has it now may be another. Kept as
    /// that one, a show's two "Display 1" inputs, made of the 4K screen,
    /// were a capture box's own screen for good once the show was opened
    /// and closed with the box plugged in (a copy of a show, an M5 Max,
    /// 2026-11-10). It goes by its name until its screen is picked again
    /// (see [`Self::bound_to`]).
    #[must_use]
    pub fn restored_to(&self, found: &CaptureTargetInfo) -> Self {
        match self {
            Self::Display { uuid: Some(_), .. } => self.bound_to(found),
            _ => self.clone(),
        }
    }

    /// Whether this is a display kept by its name alone: its place among
    /// the screens plugged in, not the screen itself.
    #[must_use]
    pub fn is_display_by_name(&self) -> bool {
        matches!(self, Self::Display { uuid: None, .. })
    }
}

/// The display among `targets` that a show's `label` and `uuid` mean: the one
/// of that identity — of two with one (identical screens), the one so named —
/// and only when none of it is plugged in, or none was kept, the one of that
/// name: a show made on another computer still finds a screen.
pub fn find_display<'a>(
    targets: &'a [CaptureTargetInfo],
    label: &str,
    uuid: Option<&str>,
) -> Option<&'a CaptureTargetInfo> {
    let displays = || {
        targets
            .iter()
            .filter(|t| t.kind == CaptureTargetKind::Display)
    };
    let named = || displays().find(|t| t.label == label);
    let Some(uuid) = uuid else {
        return named();
    };
    let same = || displays().filter(|t| t.uuid.as_deref() == Some(uuid));
    same().find(|t| t.label == label).or_else(|| same().next()).or_else(|| {
        let stand_in = named()?;
        log::warn!(
            "Screen capture: the screen kept as '{label}' is not plugged in — \
             the one now listed as '{label}' is captured in its place"
        );
        Some(stand_in)
    })
}

/// A normalized crop rectangle within the captured target (0.0–1.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CropRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Default for CropRect {
    fn default() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            w: 1.0,
            h: 1.0,
        }
    }
}

impl CropRect {
    /// Clamp into a valid sub-rectangle: origin in 0..1, extent positive, and
    /// `x + w <= 1` / `y + h <= 1`. A zero-or-negative extent collapses to the
    /// full frame rather than producing an empty capture.
    #[must_use]
    pub fn clamped(self) -> Self {
        let x = self.x.clamp(0.0, 1.0);
        let y = self.y.clamp(0.0, 1.0);
        let w = if self.w <= 0.0 {
            1.0 - x
        } else {
            self.w.min(1.0 - x)
        };
        let h = if self.h <= 0.0 {
            1.0 - y
        } else {
            self.h.min(1.0 - y)
        };
        Self {
            x,
            y,
            w: w.max(f32::EPSILON),
            h: h.max(f32::EPSILON),
        }
    }

    pub fn is_full_frame(self) -> bool {
        self.x <= 0.0 && self.y <= 0.0 && self.w >= 1.0 && self.h >= 1.0
    }
}

/// Lowest and highest capture rates accepted from the parameter router.
pub const MIN_CAPTURE_RATE: f32 = 1.0;
pub const MAX_CAPTURE_RATE: f32 = 120.0;
/// Default capture rate. Deliberately below the render rate — see
/// spec/screen-capture.md § Self-Capture and Feedback Safety.
pub const DEFAULT_CAPTURE_RATE: f32 = 30.0;

/// One of a capture's settings an input's parameters set (see
/// spec/screen-capture.md § Parameters). Its cap is not among them: it is
/// the show's (see `ScreenCaptureManager::cap_to`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSetting {
    Rate,
    CropX,
    CropY,
    CropW,
    CropH,
    Cursor,
    ExcludeVarda,
}

impl CaptureSetting {
    /// The setting the capture parameter `name` sets.
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "rate" => Self::Rate,
            "crop_x" => Self::CropX,
            "crop_y" => Self::CropY,
            "crop_w" => Self::CropW,
            "crop_h" => Self::CropH,
            "cursor" => Self::Cursor,
            "exclude_varda" => Self::ExcludeVarda,
            _ => return None,
        })
    }
}

/// A setting an input of a capture was given, for the capture to be given.
/// `at` orders these across the inputs of one screen or window, which share
/// its capture: that is left as the last one given says, whichever input it
/// was given to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CaptureGiven {
    pub at: u64,
    pub setting: CaptureSetting,
    /// Normalized (0.0–1.0), as its parameter was given it.
    pub value: f32,
}

/// How big a capture's picture is drawn in the show's frame
/// ([`CaptureConfig::scale_to`]), which is the size it comes at: as many
/// pixels as it is drawn with, where its screen or window has them (see
/// `resample::drawn_within`). In times the size that fits within the frame,
/// and its width and height in times the frame's — not one number: these
/// sizes differ by the picture's shape, which only its capture knows to the
/// pixel. It comes at the biggest of the three.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Drawn {
    /// Times the size that fits within the frame, the picture whole: 1 as an
    /// input fitted shows it, 2 zoomed into twice over.
    pub fitted: f32,
    /// Its width in times the frame's: 1 as an input that fills the frame
    /// or is stretched to it shows it, 2 such a one zoomed into twice over.
    /// 0: none shows it so.
    pub across: f32,
    /// Its height in times the frame's, likewise. An input that fills the
    /// frame has the two alike — the picture comes at the size that fills
    /// it, the bigger — one stretched and scaled apart has not.
    pub down: f32,
}

impl Default for Drawn {
    /// Fitted within the frame: how every capture came before its size
    /// followed how it is drawn.
    fn default() -> Self {
        Self {
            fitted: 1.0,
            across: 0.0,
            down: 0.0,
        }
    }
}

impl Drawn {
    /// The most times any of its sizes a capture is asked for: past the 25
    /// a Transform scales by (5, and 5 again an axis). What it comes at is
    /// never more than its screen or window has.
    pub const MOST: f32 = 32.0;

    /// Each within 0 and [`Self::MOST`]; drawn at no size at all, as
    /// fitted.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let part = |times: f32| {
            if times.is_finite() {
                times.clamp(0.0, Self::MOST)
            } else {
                0.0
            }
        };
        let (fitted, across, down) = (part(self.fitted), part(self.across), part(self.down));
        if fitted <= 0.0 && across <= 0.0 && down <= 0.0 {
            return Self::default();
        }
        Self {
            fitted,
            across,
            down,
        }
    }

    /// `times` as big, each: what a capture is asked for to have room —
    /// more pixels than it is drawn with, where its screen or window has
    /// them, for the show to make smaller itself (see
    /// `ScreenCaptureManager::size_as_drawn`). Never smaller than drawn;
    /// within bounds, as [`Self::sanitized`].
    #[must_use]
    pub fn with_room(self, times: f32) -> Self {
        let times = if times.is_finite() { times.max(1.0) } else { 1.0 };
        let drawn = self.sanitized();
        Self {
            fitted: drawn.fitted * times,
            across: drawn.across * times,
            down: drawn.down * times,
        }
        .sanitized()
    }

    /// The bigger of each: what a capture two inputs show is sized for.
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self {
            fitted: self.fitted.max(other.fitted),
            across: self.across.max(other.across),
            down: self.down.max(other.down),
        }
    }
}

/// Per-capture tunables. Shared with the capture thread and re-read each tick,
/// so router-driven changes take effect without restarting the session.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptureConfig {
    pub rate: f32,
    pub crop: CropRect,
    pub show_cursor: bool,
    /// Exclude Nida's own windows from the capture. Display targets only.
    pub exclude_varda: bool,
    /// Ask the OS to scale at capture time. Set to the deck render resolution so
    /// a 4K display does not move 33 MB per frame.
    pub scale_to: Option<(u32, u32)>,
    /// How big its picture is drawn in that frame: the capture's own to
    /// follow (`ScreenCaptureManager::size_as_drawn`), which its backend is
    /// told with the rest. Not a setting an input has or gives: fitted, in
    /// every settings but those a capture's thread runs by.
    pub drawn: Drawn,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            rate: DEFAULT_CAPTURE_RATE,
            crop: CropRect::default(),
            show_cursor: false,
            exclude_varda: true,
            scale_to: None,
            drawn: Drawn::default(),
        }
    }
}

impl CaptureConfig {
    /// Normalize user- or router-supplied values into the accepted ranges.
    #[must_use]
    pub fn sanitized(mut self) -> Self {
        self.rate = if self.rate.is_finite() {
            self.rate.clamp(MIN_CAPTURE_RATE, MAX_CAPTURE_RATE)
        } else {
            DEFAULT_CAPTURE_RATE
        };
        self.crop = self.crop.clamped();
        self.drawn = self.drawn.sanitized();
        self
    }

    /// These settings for an input that captured `was`, pointed at `now`:
    /// its rate, crop and cursor stay. Nida's own windows are left out of a
    /// display and not of a window, as when one is added, once it becomes
    /// the other — a window's input pointed at a screen would show Nida's
    /// own window on that screen, itself within itself.
    #[must_use]
    pub fn repointed(mut self, was: &TargetIdentity, now: &CaptureTargetInfo) -> Self {
        let display = now.kind == CaptureTargetKind::Display;
        if matches!(was, TargetIdentity::Display { .. }) != display {
            self.exclude_varda = display;
        }
        self
    }

    /// Give these one setting from a normalized value (0.0–1.0), as an
    /// input's parameter does; the whole is then within its ranges — a
    /// crop moved right is narrowed to what is left of the frame. A capture
    /// given what its inputs were, in the order they were, runs as if each
    /// had been set on it there and then (see
    /// `VardaApp::send_capture_settings`).
    pub fn give(&mut self, setting: CaptureSetting, value: f32) {
        let v = value.clamp(0.0, 1.0);
        match setting {
            CaptureSetting::Rate => {
                self.rate = MIN_CAPTURE_RATE + v * (MAX_CAPTURE_RATE - MIN_CAPTURE_RATE);
            }
            CaptureSetting::CropX => self.crop.x = v,
            CaptureSetting::CropY => self.crop.y = v,
            CaptureSetting::CropW => self.crop.w = v,
            CaptureSetting::CropH => self.crop.h = v,
            // Bucketed so a MIDI fader can drive it like every other toggle.
            CaptureSetting::Cursor => self.show_cursor = v > 0.5,
            CaptureSetting::ExcludeVarda => self.exclude_varda = v > 0.5,
        }
        *self = self.clone().sanitized();
    }

    /// These settings as their capture `runs`: its rate, crop and cursor,
    /// which every input of one screen or window shares. The cap stays: it
    /// is the show's, kept by each input.
    #[must_use]
    pub fn as_runs(&self, runs: &Self) -> Self {
        Self {
            scale_to: self.scale_to,
            ..runs.clone()
        }
    }

    /// Frame interval implied by `rate`.
    pub fn frame_interval(&self) -> std::time::Duration {
        std::time::Duration::from_secs_f32(1.0 / self.rate.max(MIN_CAPTURE_RATE))
    }
}

/// Pixel layout of a delivered frame. Backends report their native layout so
/// the manager can pick a matching texture format instead of swizzling on CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapturePixelFormat {
    Rgba8UnormSrgb,
    Bgra8UnormSrgb,
}

impl CapturePixelFormat {
    pub fn wgpu_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8UnormSrgb,
            Self::Bgra8UnormSrgb => wgpu::TextureFormat::Bgra8UnormSrgb,
        }
    }

    /// The format that reads the same texture as it was coded: its sRGB
    /// code values as they are, not made light.
    pub fn coded_format(self) -> wgpu::TextureFormat {
        match self {
            Self::Rgba8UnormSrgb => wgpu::TextureFormat::Rgba8Unorm,
            Self::Bgra8UnormSrgb => wgpu::TextureFormat::Bgra8Unorm,
        }
    }
}

/// One captured frame, tightly packed at `width * 4` bytes per row.
pub struct CaptureFrame {
    /// The pixels in memory — empty when they are in `surface` instead.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub format: CapturePixelFormat,
    /// macOS: the GPU surface the pixels were captured into, copied into the
    /// capture's texture on the GPU rather than through memory (see
    /// `screen_capture::surface`).
    #[cfg(target_os = "macos")]
    pub surface: Option<super::surface::SurfaceFrame>,
    /// macOS: when the window server showed what the frame captured, host
    /// seconds (see `crate::host_clock`) — what each show frame's capture
    /// frame is picked by (see `crate::frame_picker`). Elsewhere frames are
    /// timed as they arrive.
    #[cfg(target_os = "macos")]
    pub captured_at: Option<f64>,
}

/// Whether the platform will let us capture at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionState {
    /// Capture is permitted.
    Granted,
    /// The user explicitly refused. Requires a trip to system settings.
    Denied,
    /// Never asked. A capture attempt will raise the OS prompt.
    NotDetermined,
    /// This platform has no capture permission gate.
    NotRequired,
}

impl PermissionState {
    pub fn allows_capture(self) -> bool {
        matches!(self, Self::Granted | Self::NotRequired)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::NotDetermined => "not_determined",
            Self::NotRequired => "not_required",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureError {
    /// The `screen-capture` feature is off, the manager is disabled, or this
    /// platform has no backend.
    Unavailable(String),
    /// The OS refused; the user must grant Screen Recording access.
    PermissionDenied,
    /// The requested display or window no longer exists.
    TargetNotFound(String),
    /// The platform backend failed for some other reason.
    Backend(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(why) => write!(f, "screen capture unavailable: {why}"),
            Self::PermissionDenied => write!(
                f,
                "screen recording permission denied — grant access in System Settings and restart Nida"
            ),
            Self::TargetNotFound(what) => write!(f, "capture target not found: {what}"),
            Self::Backend(why) => write!(f, "screen capture backend error: {why}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// A live capture session. Implementations run on the capture thread and are
/// never touched from the render thread.
pub trait ScreenCaptureBackend: Send {
    /// Human-readable label of the captured target (logs / UI).
    fn label(&self) -> &str;
    /// Current output resolution `(width, height)`.
    fn resolution(&self) -> (u32, u32);
    /// Poll the next frame. `None` means no new frame is ready yet — for a
    /// static desktop that is the common case and is not an error.
    fn next_frame(&mut self) -> Option<CaptureFrame>;
    /// Native pixel layout of the frames this backend delivers.
    ///
    /// Declared rather than probed: a push-based backend has nothing ready at
    /// `open`, so probing there always guesses, allocates the shared texture in
    /// the wrong format, and throws it away on the first real frame. The
    /// manager still reallocates if a frame disagrees, so a wrong answer here
    /// costs a texture, not correctness.
    fn pixel_format(&self) -> CapturePixelFormat {
        CapturePixelFormat::Rgba8UnormSrgb
    }
    /// Whether the OS already paces delivery at [`CaptureConfig::rate`].
    ///
    /// Push-based backends (`ScreenCaptureKit`, Windows Graphics Capture,
    /// `PipeWire`) are handed the rate and deliver on the compositor's clock.
    /// The capture loop must then poll *faster* than the rate rather than
    /// matching it: two independent clocks of the same nominal frequency drift
    /// against each other, so ticks periodically find nothing while a delivered
    /// frame is overwritten before it is taken. The delivered cadence stutters
    /// even though the average rate looks correct, and in a self-capture
    /// feedback loop that irregularity is exactly what reads as flicker.
    ///
    /// Polled backends (X11, the mock) produce a frame only when asked, so the
    /// loop owns their pacing and this stays `false`.
    fn is_self_paced(&self) -> bool {
        false
    }
    /// Keep up with the target's own size: a window resized, a screen given
    /// another resolution. Called every tick of the capture thread, which
    /// the backend paces itself; by default, nothing to follow.
    fn follow_target(&mut self) {}
    /// Apply a live config change (rate, crop, cursor).
    ///
    /// # Errors
    ///
    /// Returns an error if the platform rejects the new configuration.
    fn set_config(&mut self, config: &CaptureConfig) -> Result<(), CaptureError>;
}

/// A synthetic capture backend, always compiled.
///
/// Emits a moving diagonal-bar pattern with a distinct solid marker in the
/// top-left texel so tests can assert *which* target they are looking at after a
/// scale or crop, not merely that some pixels arrived.
pub struct MockBackend {
    label: String,
    width: u32,
    height: u32,
    config: CaptureConfig,
    frame: u64,
    marker: [u8; 3],
    /// A size its target takes, found when it next follows it (see
    /// `ScreenCaptureBackend::follow_target`): a window resized.
    resized: Option<(u32, u32)>,
}

impl MockBackend {
    pub fn new(label: impl Into<String>, width: u32, height: u32, config: CaptureConfig) -> Self {
        let label = label.into();
        // Derive a stable per-target marker colour from the label so two mock
        // captures in one test are distinguishable in the readback.
        let mut h: u32 = 2_166_136_261;
        for b in label.as_bytes() {
            h = (h ^ u32::from(*b)).wrapping_mul(16_777_619);
        }
        let marker = [
            (h & 0xFF) as u8 | 0x40,
            ((h >> 8) & 0xFF) as u8 | 0x40,
            ((h >> 16) & 0xFF) as u8 | 0x40,
        ];
        Self {
            label,
            width,
            height,
            config: config.sanitized(),
            frame: 0,
            marker,
            resized: None,
        }
    }

    /// Its target resized to `width × height`, as a window is: taken when
    /// the capture next follows its target.
    pub fn resize_target(&mut self, width: u32, height: u32) {
        self.resized = Some((width, height));
    }

    /// Output size after `scale_to`, which is what a real backend would deliver.
    /// As the real captures size it: the part of the target its crop keeps,
    /// as big as it is drawn in `scale_to` (a bound, not an exact size: see
    /// `platform::macos::stream_size`), never enlarged.
    fn output_size(&self) -> (u32, u32) {
        let fitted = super::resample::Geometry::resolve(self.width, self.height, &self.config);
        (fitted.out_w, fitted.out_h)
    }

    pub fn marker(&self) -> [u8; 3] {
        self.marker
    }
}

impl ScreenCaptureBackend for MockBackend {
    fn label(&self) -> &str {
        &self.label
    }

    fn resolution(&self) -> (u32, u32) {
        self.output_size()
    }

    fn next_frame(&mut self) -> Option<CaptureFrame> {
        self.frame = self.frame.wrapping_add(1);
        let (width, height) = self.output_size();
        let crop = self.config.crop.clamped();
        let phase = (self.frame % 64) as f32 / 64.0;
        let mut data = vec![0u8; (width as usize) * (height as usize) * 4];
        for y in 0..height {
            for x in 0..width {
                // Map into the cropped sub-rectangle so a crop visibly changes
                // the content rather than only the reported size.
                let u = crop.x + (x as f32 / width as f32) * crop.w;
                let v = crop.y + (y as f32 / height as f32) * crop.h;
                let bar = ((u + v + phase) * 8.0).fract();
                let texel = ((y * width + x) * 4) as usize;
                data[texel] = (bar * 255.0) as u8;
                data[texel + 1] = (u * 255.0) as u8;
                data[texel + 2] = (v * 255.0) as u8;
                data[texel + 3] = 255;
            }
        }
        // Identity marker, top-left texel.
        data[0] = self.marker[0];
        data[1] = self.marker[1];
        data[2] = self.marker[2];
        data[3] = 255;
        Some(CaptureFrame {
            data,
            width,
            height,
            format: CapturePixelFormat::Rgba8UnormSrgb,
            #[cfg(target_os = "macos")]
            surface: None,
            #[cfg(target_os = "macos")]
            captured_at: None,
        })
    }

    fn follow_target(&mut self) {
        if let Some((width, height)) = self.resized.take() {
            self.width = width;
            self.height = height;
        }
    }

    fn set_config(&mut self, config: &CaptureConfig) -> Result<(), CaptureError> {
        self.config = config.clone().sanitized();
        Ok(())
    }
}

/// Synthetic targets reported when the mock provider is in use.
pub fn mock_targets() -> Vec<CaptureTargetInfo> {
    vec![
        CaptureTargetInfo {
            kind: CaptureTargetKind::Display,
            platform_id: 1,
            label: "Mock Display 1".into(),
            app: None,
            title: None,
            width: 1920,
            height: 1080,
            is_varda: false,
            uuid: None,
        },
        CaptureTargetInfo {
            kind: CaptureTargetKind::Display,
            platform_id: 2,
            label: "Mock Display 2".into(),
            app: None,
            title: None,
            width: 1280,
            height: 720,
            is_varda: false,
            uuid: None,
        },
        CaptureTargetInfo {
            kind: CaptureTargetKind::Window,
            platform_id: 100,
            label: "Mock App — Untitled".into(),
            app: Some("com.example.mock".into()),
            title: Some("Untitled".into()),
            width: 800,
            height: 600,
            is_varda: false,
            uuid: None,
        },
        CaptureTargetInfo {
            kind: CaptureTargetKind::Window,
            platform_id: 101,
            label: "Nida — Main".into(),
            app: Some("com.varda.app".into()),
            title: Some("Main".into()),
            width: 1920,
            height: 1080,
            is_varda: true,
            uuid: None,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crop_clamps_into_the_unit_square() {
        let c = CropRect {
            x: -0.5,
            y: 0.2,
            w: 5.0,
            h: 0.5,
        }
        .clamped();
        assert!((c.x - 0.0).abs() < f32::EPSILON);
        assert!((c.y - 0.2).abs() < f32::EPSILON);
        // w is capped so x + w never exceeds 1.0.
        assert!(c.x + c.w <= 1.0 + f32::EPSILON);
        assert!(c.y + c.h <= 1.0 + f32::EPSILON);
    }

    #[test]
    fn zero_extent_crop_collapses_to_full_frame_not_empty() {
        let c = CropRect {
            x: 0.0,
            y: 0.0,
            w: 0.0,
            h: -1.0,
        }
        .clamped();
        assert!(c.w > 0.9, "zero width should expand, got {}", c.w);
        assert!(c.h > 0.9, "negative height should expand, got {}", c.h);
    }

    #[test]
    fn config_sanitize_clamps_rate_and_rejects_nan() {
        assert!(
            (CaptureConfig {
                rate: 1000.0,
                ..Default::default()
            }
            .sanitized()
            .rate
                - MAX_CAPTURE_RATE)
                .abs()
                < f32::EPSILON
        );
        assert!(
            (CaptureConfig {
                rate: 0.0,
                ..Default::default()
            }
            .sanitized()
            .rate
                - MIN_CAPTURE_RATE)
                .abs()
                < f32::EPSILON
        );
        assert!(
            (CaptureConfig {
                rate: f32::NAN,
                ..Default::default()
            }
            .sanitized()
            .rate
                - DEFAULT_CAPTURE_RATE)
                .abs()
                < f32::EPSILON
        );
    }

    /// An input pointed at another screen or window keeps its settings;
    /// Nida's own windows are left out of a screen and not of a window once
    /// it becomes the other, and as it was set while it stays what it was.
    #[test]
    fn an_input_pointed_elsewhere_keeps_its_settings_but_for_nidas_own_windows() {
        let targets = mock_targets();
        let (screen, other_screen) = (&targets[0], &targets[1]);
        let (window, other_window) = (&targets[2], &targets[3]);
        let shown = CaptureConfig {
            rate: 24.0,
            crop: CropRect { x: 0.25, y: 0.0, w: 0.5, h: 1.0 },
            show_cursor: true,
            exclude_varda: false,
            scale_to: Some((1920, 1080)),
            drawn: Drawn::default(),
        };
        let left_out = CaptureConfig { exclude_varda: true, ..shown.clone() };
        // A screen to another screen: Nida's windows shown, as it was set.
        assert_eq!(shown.clone().repointed(&screen.identity(), other_screen), shown);
        // A window to another window: left out, as it was set.
        assert_eq!(left_out.clone().repointed(&window.identity(), other_window), left_out);
        // A window to a screen: Nida's own windows left out, the rest kept.
        assert_eq!(shown.clone().repointed(&window.identity(), screen), left_out);
        // A screen to a window: not left out, as a window added is not.
        assert_eq!(left_out.clone().repointed(&screen.identity(), window), shown);
    }

    /// A setting given is set as its parameter sets it — a rate across its
    /// range, a side of the crop, a switch past halfway — each by its
    /// parameter's name, the cap by none; and the whole is kept within its
    /// ranges: a crop moved right is narrowed to what is left of the frame.
    #[test]
    fn a_setting_given_is_set_as_its_parameter_sets_it() {
        let each = [
            ("rate", CaptureSetting::Rate),
            ("crop_x", CaptureSetting::CropX),
            ("crop_y", CaptureSetting::CropY),
            ("crop_w", CaptureSetting::CropW),
            ("crop_h", CaptureSetting::CropH),
            ("cursor", CaptureSetting::Cursor),
            ("exclude_varda", CaptureSetting::ExcludeVarda),
        ];
        for (name, setting) in each {
            assert_eq!(CaptureSetting::named(name), Some(setting), "{name}");
        }
        assert_eq!(CaptureSetting::named("scale_to"), None, "not an input's");

        let base = CaptureConfig {
            scale_to: Some((1920, 1080)),
            ..CaptureConfig::default()
        };
        let given = |from: &CaptureConfig, setting, value| {
            let mut config = from.clone();
            config.give(setting, value);
            config
        };
        let crop = |x, y, w, h| CaptureConfig {
            crop: CropRect { x, y, w, h },
            ..base.clone()
        };
        let rate = given(&base, CaptureSetting::Rate, 0.5).rate;
        assert!((rate - 60.5).abs() < 1e-4, "halfway from 1 to 120: {rate}");
        let rate = given(&base, CaptureSetting::Rate, 7.0).rate;
        assert!((rate - MAX_CAPTURE_RATE).abs() < 1e-4, "all the way: {rate}");
        assert_eq!(
            given(&base, CaptureSetting::CropX, 0.25),
            crop(0.25, 0.0, 0.75, 1.0),
            "narrowed to what is left"
        );
        assert_eq!(
            given(&base, CaptureSetting::CropY, 0.25),
            crop(0.0, 0.25, 1.0, 0.75)
        );
        assert_eq!(
            given(&base, CaptureSetting::CropW, 0.5),
            crop(0.0, 0.0, 0.5, 1.0)
        );
        assert_eq!(
            given(&base, CaptureSetting::CropH, 0.5),
            crop(0.0, 0.0, 1.0, 0.5)
        );
        let shown = given(&base, CaptureSetting::Cursor, 0.6);
        assert_eq!(
            shown,
            CaptureConfig {
                show_cursor: true,
                ..base.clone()
            }
        );
        assert_eq!(given(&shown, CaptureSetting::Cursor, 0.4), base, "hidden again");
        let seen = given(&base, CaptureSetting::ExcludeVarda, 0.4);
        assert_eq!(
            seen,
            CaptureConfig {
                exclude_varda: false,
                ..base.clone()
            }
        );
        assert_eq!(given(&seen, CaptureSetting::ExcludeVarda, 0.6), base);
    }

    /// What two inputs of one capture were given, given the capture in the
    /// order they were: a width, then a left edge, leave both as given —
    /// the edge narrows the width only past what is left of the frame.
    #[test]
    fn settings_given_in_turn_leave_each_as_given() {
        let mut runs = CaptureConfig::default();
        runs.give(CaptureSetting::CropW, 0.5);
        runs.give(CaptureSetting::CropX, 0.25);
        assert_eq!(
            runs.crop,
            CropRect {
                x: 0.25,
                y: 0.0,
                w: 0.5,
                h: 1.0
            }
        );
        runs.give(CaptureSetting::CropX, 0.75);
        assert_eq!(
            runs.crop,
            CropRect {
                x: 0.75,
                y: 0.0,
                w: 0.25,
                h: 1.0
            },
            "narrowed to what is left"
        );
    }

    /// An input takes its capture's settings, but for its cap: the show's.
    #[test]
    fn an_input_takes_its_captures_settings_but_for_its_cap() {
        let runs = CaptureConfig {
            rate: 24.0,
            crop: CropRect {
                x: 0.25,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            },
            show_cursor: true,
            exclude_varda: false,
            scale_to: Some((1920, 1080)),
            drawn: Drawn::default(),
        };
        let its = CaptureConfig {
            scale_to: Some((640, 360)),
            ..CaptureConfig::default()
        };
        assert_eq!(
            its.as_runs(&runs),
            CaptureConfig {
                scale_to: Some((640, 360)),
                ..runs
            }
        );
    }

    #[test]
    fn default_config_excludes_varda_and_hides_cursor() {
        let c = CaptureConfig::default();
        assert!(
            c.exclude_varda,
            "display captures must exclude Nida by default"
        );
        assert!(!c.show_cursor);
    }

    #[test]
    fn mock_backend_frame_matches_reported_resolution() {
        let mut b = MockBackend::new("Mock Display 1", 1920, 1080, CaptureConfig::default());
        assert_eq!(b.resolution(), (1920, 1080));
        let f = b.next_frame().expect("frame");
        assert_eq!((f.width, f.height), (1920, 1080));
        assert_eq!(f.data.len(), 1920 * 1080 * 4);
    }

    #[test]
    fn mock_backend_honours_scale_to() {
        let cfg = CaptureConfig {
            scale_to: Some((320, 180)),
            ..Default::default()
        };
        let mut b = MockBackend::new("Mock", 1920, 1080, cfg);
        assert_eq!(b.resolution(), (320, 180));
        let f = b.next_frame().expect("frame");
        assert_eq!((f.width, f.height), (320, 180));
        assert_eq!(f.data.len(), 320 * 180 * 4);
    }

    /// As the real captures: the crop taken first, then fitted within the
    /// bound — a quarter of the target at its own pixels where the bound
    /// takes them.
    #[test]
    fn mock_backend_crops_then_fits_within_scale_to() {
        let quarter = |scale_to| CaptureConfig {
            scale_to,
            crop: CropRect {
                x: 0.0,
                y: 0.0,
                w: 0.5,
                h: 0.5,
            },
            ..Default::default()
        };
        let sized = |scale_to| {
            MockBackend::new("Mock", 1920, 1080, quarter(scale_to)).resolution()
        };
        assert_eq!(sized(Some((960, 540))), (960, 540));
        assert_eq!(sized(Some((1920, 1080))), (960, 540), "never enlarged");
        assert_eq!(sized(Some((480, 270))), (480, 270));
    }

    /// Drawn bigger than fitted — filling its frame, zoomed into — the mock
    /// comes so, as the real captures do: never bigger than its target.
    #[test]
    fn mock_backend_comes_as_big_as_it_is_drawn() {
        let sized = |drawn| {
            let config = CaptureConfig {
                scale_to: Some((960, 540)),
                drawn,
                ..Default::default()
            };
            MockBackend::new("Mock", 1920, 1080, config).resolution()
        };
        let zoomed = |fitted| Drawn {
            fitted,
            ..Drawn::default()
        };
        assert_eq!(sized(Drawn::default()), (960, 540));
        assert_eq!(sized(zoomed(1.5)), (1440, 810));
        assert_eq!(sized(zoomed(2.0)), (1920, 1080));
        assert_eq!(sized(zoomed(4.0)), (1920, 1080), "never enlarged");
        let mut live = MockBackend::new("Mock", 1920, 1080, CaptureConfig::default());
        live.set_config(&CaptureConfig {
            scale_to: Some((960, 540)),
            drawn: zoomed(1.5),
            ..Default::default()
        })
        .expect("set");
        assert_eq!(live.resolution(), (1440, 810), "told while it runs");
    }

    /// With room a capture is asked for so many times what it is drawn at,
    /// each of its sizes, within the same bounds — never less than drawn,
    /// and drawn at no size, as one fitted with room.
    #[test]
    fn drawn_with_room_is_so_many_times_as_big() {
        let apart = |fitted, across, down| Drawn {
            fitted,
            across,
            down,
        };
        assert_eq!(Drawn::default().with_room(2.0), apart(2.0, 0.0, 0.0));
        assert_eq!(apart(1.5, 1.0, 0.5).with_room(2.0), apart(3.0, 2.0, 1.0));
        assert_eq!(apart(0.0, 0.75, 0.0).with_room(4.0), apart(0.0, 3.0, 0.0));
        assert_eq!(
            apart(20.0, 1.0, 0.0).with_room(2.0),
            apart(Drawn::MOST, 2.0, 0.0),
            "within bounds"
        );
        assert_eq!(
            apart(0.0, 0.0, 0.0).with_room(2.0),
            apart(2.0, 0.0, 0.0),
            "at no size: as fitted"
        );
        assert_eq!(
            apart(f32::NAN, 0.0, -1.0).with_room(1.5),
            apart(1.5, 0.0, 0.0)
        );
        for none in [1.0, 0.5, 0.0, -2.0, f32::NAN, f32::INFINITY] {
            assert_eq!(
                apart(1.5, 1.0, 0.0).with_room(none),
                apart(1.5, 1.0, 0.0),
                "room {none}: as drawn"
            );
        }
    }

    /// Each format a capture's pixels come in has one that reads the same
    /// texture as it was coded: the same bytes in the same order, not made
    /// light.
    #[test]
    fn a_captures_format_has_one_that_reads_it_as_coded() {
        use wgpu::TextureFormat::{Bgra8Unorm, Bgra8UnormSrgb, Rgba8Unorm, Rgba8UnormSrgb};
        for (format, light, coded) in [
            (CapturePixelFormat::Rgba8UnormSrgb, Rgba8UnormSrgb, Rgba8Unorm),
            (CapturePixelFormat::Bgra8UnormSrgb, Bgra8UnormSrgb, Bgra8Unorm),
        ] {
            assert_eq!(format.wgpu_format(), light);
            assert_eq!(format.coded_format(), coded);
            assert_eq!(coded.add_srgb_suffix(), light, "the same texture's");
        }
    }

    /// How big a capture is drawn is kept within bounds — a number no size
    /// comes of taken as none — and two inputs' are the bigger of each.
    #[test]
    fn drawn_is_kept_within_bounds_and_the_bigger_of_two() {
        // Fitted so many times, or filling the frame so many times over.
        let drawn = |fitted, fills| Drawn {
            fitted,
            across: fills,
            down: fills,
        };
        let apart = |fitted, across, down| Drawn {
            fitted,
            across,
            down,
        };
        assert_eq!(Drawn::default(), drawn(1.0, 0.0));
        assert_eq!(drawn(2.0, 0.5).sanitized(), drawn(2.0, 0.5));
        assert_eq!(drawn(0.0, 1.5).sanitized(), drawn(0.0, 1.5), "filling alone");
        assert_eq!(drawn(100.0, -3.0).sanitized(), drawn(Drawn::MOST, 0.0));
        assert_eq!(drawn(f32::NAN, 2.0).sanitized(), drawn(0.0, 2.0));
        assert_eq!(drawn(f32::INFINITY, 2.0).sanitized(), drawn(0.0, 2.0));
        // As far as a Transform scales: 5, and 5 again an axis.
        assert_eq!(drawn(25.0, 25.0).sanitized(), drawn(25.0, 25.0));
        // Each of its sizes by itself.
        assert_eq!(
            apart(1.0, f32::NAN, 99.0).sanitized(),
            apart(1.0, 0.0, Drawn::MOST)
        );
        assert_eq!(
            apart(1.0, 99.0, -1.0).sanitized(),
            apart(1.0, Drawn::MOST, 0.0)
        );
        assert_eq!(apart(0.0, 0.0, 0.5).sanitized(), apart(0.0, 0.0, 0.5), "its height alone");
        assert_eq!(apart(0.0, 0.5, 0.0).sanitized(), apart(0.0, 0.5, 0.0), "its width alone");
        for none in [drawn(0.0, 0.0), drawn(-1.0, f32::NAN), drawn(f32::NAN, f32::NAN)] {
            assert_eq!(none.sanitized(), Drawn::default(), "{none:?}");
        }
        assert_eq!(drawn(2.0, 0.0).or(drawn(1.0, 1.5)), drawn(2.0, 1.5));
        assert_eq!(drawn(1.0, 3.0).or(drawn(1.0, 1.0)), drawn(1.0, 3.0));
        assert_eq!(
            apart(1.0, 2.0, 0.5).or(apart(1.0, 0.5, 3.0)),
            apart(1.0, 2.0, 3.0)
        );
        assert_eq!(
            apart(1.0, 0.5, 3.0).or(apart(1.0, 2.0, 0.5)),
            apart(1.0, 2.0, 3.0)
        );
        let wild = CaptureConfig {
            drawn: drawn(f32::NAN, 99.0),
            ..Default::default()
        };
        assert_eq!(wild.sanitized().drawn, drawn(0.0, Drawn::MOST));
    }

    #[test]
    fn mock_backend_is_polled_not_self_paced() {
        // It synthesizes a frame per call, so the capture loop owns its rate.
        let b = MockBackend::new("Mock", 8, 8, CaptureConfig::default());
        assert!(!b.is_self_paced());
    }

    #[test]
    fn mock_backend_marker_is_stable_per_label_and_distinct_across_labels() {
        let a = MockBackend::new("Display A", 8, 8, CaptureConfig::default());
        let a2 = MockBackend::new("Display A", 8, 8, CaptureConfig::default());
        let b = MockBackend::new("Display B", 8, 8, CaptureConfig::default());
        assert_eq!(a.marker(), a2.marker());
        assert_ne!(a.marker(), b.marker());
    }

    #[test]
    fn crop_changes_frame_content_not_just_size() {
        let full = MockBackend::new("M", 64, 64, CaptureConfig::default())
            .next_frame()
            .expect("frame");
        let cropped = MockBackend::new(
            "M",
            64,
            64,
            CaptureConfig {
                crop: CropRect {
                    x: 0.5,
                    y: 0.5,
                    w: 0.5,
                    h: 0.5,
                },
                ..Default::default()
            },
        )
        .next_frame()
        .expect("frame");
        // As the real captures: the crop's part, at the crop's size — and
        // its content is that part of the whole (the marker texel aside).
        assert_eq!((cropped.width, cropped.height), (32, 32));
        let texel = |f: &CaptureFrame, x: u32, y: u32| {
            let at = ((y * f.width + x) * 4) as usize;
            f.data[at..at + 4].to_vec()
        };
        for y in 0..32 {
            for x in 0..32 {
                if (x, y) != (0, 0) {
                    assert_eq!(
                        texel(&cropped, x, y),
                        texel(&full, x + 32, y + 32),
                        "({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn permission_state_gates_capture() {
        assert!(PermissionState::Granted.allows_capture());
        assert!(PermissionState::NotRequired.allows_capture());
        assert!(!PermissionState::Denied.allows_capture());
        assert!(!PermissionState::NotDetermined.allows_capture());
    }

    #[test]
    fn target_identity_ignores_ephemeral_platform_handles() {
        let a = CaptureTargetInfo {
            kind: CaptureTargetKind::Window,
            platform_id: 7,
            label: "X".into(),
            app: Some("com.a".into()),
            title: Some("T".into()),
            width: 1,
            height: 1,
            is_varda: false,
            uuid: None,
        };
        let b = CaptureTargetInfo {
            platform_id: 999_999,
            ..a.clone()
        };
        assert_eq!(a.identity(), b.identity());
    }

    #[test]
    fn mock_targets_include_a_varda_owned_window() {
        let t = mock_targets();
        assert!(t.iter().any(|t| t.is_varda));
        assert!(t.iter().any(|t| t.kind == CaptureTargetKind::Display));
        assert!(t.iter().any(|t| t.kind == CaptureTargetKind::Window));
    }
}
