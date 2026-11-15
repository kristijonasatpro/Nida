//! Screen/window capture manager — shared capture sessions for N deck consumers.
//!
//! One `ScreenCaptureManager` owns all capture sessions. Each captured display
//! or window produces one shared GPU texture that any number of decks read from,
//! exactly like `CameraManager`: capture runs on a dedicated thread per target,
//! publishes into an `Arc<Mutex<Option<CaptureFrame>>>`, and the render thread
//! only does a non-blocking `try_lock` + `write_texture`.
//!
//! Sessions are keyed by an opaque [`CaptureId`] minted at `open`, **not** by an
//! index into the enumerated target list — a rescan reorders that list, and an
//! open capture must survive it.
//!
//! See spec/screen-capture.md.

pub mod backend;
pub mod platform;
pub mod resample;
#[cfg(target_os = "macos")]
pub mod surface;

use backend::{
    CaptureConfig, CaptureError, CaptureFrame, CapturePixelFormat, CaptureTargetInfo,
    CaptureTargetKind, Drawn, MockBackend, PermissionState, ScreenCaptureBackend, TargetIdentity,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::frame_picker::FramePicker;
use crate::renderer::context::GpuContext;

/// Frames of a capture kept waiting for the show at most: each holds a
/// surface the capture needs back (see `platform::macos`).
const PICKER_FRAMES: usize = 4;

/// When a frame was captured, host seconds: as macOS says, or as it came.
fn frame_time(frame: &CaptureFrame) -> f64 {
    #[cfg(target_os = "macos")]
    if let Some(at) = frame.captured_at {
        return at;
    }
    let _ = frame;
    crate::host_clock::host_seconds()
}

/// Opaque handle to an open capture session.
pub type CaptureId = u32;

/// Sentinel for a capture deck that has no live session — the target named by a
/// restored scene is not currently on screen. The deck keeps its effect chain,
/// opacity, and MIDI mappings and renders black until repointed, rather than
/// being dropped. The manager never mints this id, so it can never collide with
/// a real session. See spec/screen-capture.md § Configuration and Persistence.
pub const UNBOUND_CAPTURE_ID: CaptureId = CaptureId::MAX;

/// An active capture session with its shared GPU texture.
struct ActiveCapture {
    /// Handle-free identity, used to dedupe `open` onto an existing session.
    identity: TargetIdentity,
    /// The OS handle of what it captures: two windows of one app and title
    /// (a show started again) are two sessions.
    platform_id: u64,
    /// What it captures' size when opened, as the platform gives it.
    target_size: (u32, u32),
    /// The size its backend delivers, as it said when opened: another for
    /// the same window when the window went to a screen of other pixels.
    backend_size: (u32, u32),
    label: String,
    texture: wgpu::Texture,
    texture_view: wgpu::TextureView,
    /// The same texture read as it was coded, where captures are drawn so
    /// (see [`ScreenCaptureManager::coded_view`]).
    coded_view: Option<wgpu::TextureView>,
    width: u32,
    height: u32,
    format: CapturePixelFormat,
    /// How many decks are using this capture.
    ref_count: u32,
    /// Frames come — the capture thread puts them in with when each was
    /// captured, and the render thread picks the one for each show frame
    /// (see `crate::frame_picker`).
    frame_data: Arc<Mutex<FramePicker<CaptureFrame>>>,
    /// Live config, re-read by the capture thread each tick.
    config: Arc<Mutex<CaptureConfig>>,
    stop_flag: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    /// Which of its threads is the one running now: moved on when it is
    /// replaced, so a thread replaced, still ending, puts no frame in (see
    /// [`retire`]). Each thread has a `connected` of its own.
    current: Arc<std::sync::atomic::AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Frames put in its texture: none yet, and its texture shows nothing.
    uploaded: u64,
    /// Which picture its texture holds (see `ScreenCaptureManager::picture`).
    picture: u64,
    /// Of those, the ones from the capture running now — none yet after it
    /// replaced another, though the texture still shows that one's last.
    fresh: u64,
    /// When the last frame was put in its texture: macOS sends a capture's
    /// frames only as what it shows changes, so a still slide sends none.
    last_picture: Option<std::time::Instant>,
    /// A presentation's (a PowerPoint show's window, on air or in a Slides
    /// input), taken through `adopt`: never shared with an input's own
    /// capture of the same window, whose cap and crop would undo the show's
    /// (its cap is the picture its slide is shown at, its crop cuts the
    /// title bar off).
    presentation: bool,
    /// When it was last given another size for how its picture is drawn
    /// (see [`ScreenCaptureManager::size_as_drawn`]): not yet, when opened.
    sized: Option<std::time::Instant>,
    /// Since when its picture has been drawn smaller than it is sized for.
    smaller_since: Option<std::time::Instant>,
}

/// The least between two sizes given a capture as its picture is drawn
/// bigger: a zoom dragged up changes its stream twice a second — as often
/// as a window resized does (`FOLLOW_EVERY` on macOS) — not at every frame.
const SIZED_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

/// How long a capture's picture is drawn smaller before the capture is made
/// smaller: a zoom taken off and put back, or dragged down, changes its
/// stream once.
const SMALLER_AFTER: std::time::Duration = std::time::Duration::from_secs(2);

/// Opens the platform's capture of a target: [`platform::open`], but in
/// tests, whose managers open a mock of it or nothing and never ask macOS
/// for a screen (see `ScreenCaptureManager::enabled_for_tests`).
type Opens =
    fn(&CaptureTargetInfo, &CaptureConfig) -> Result<Box<dyn ScreenCaptureBackend>, CaptureError>;

/// A test's capture of `target`: a mock of its size, set as asked. It
/// cannot fail; the platform's, whose shape it has, can.
#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
fn open_a_mock(
    target: &CaptureTargetInfo,
    config: &CaptureConfig,
) -> Result<Box<dyn ScreenCaptureBackend>, CaptureError> {
    Ok(Box::new(MockBackend::new(
        target.label.clone(),
        target.width,
        target.height,
        config.clone(),
    )))
}

/// A test's capture that does not open: a window closed since it was
/// listed, a screen macOS will not let be captured.
#[cfg(test)]
fn open_nothing(
    target: &CaptureTargetInfo,
    _config: &CaptureConfig,
) -> Result<Box<dyn ScreenCaptureBackend>, CaptureError> {
    Err(CaptureError::Backend(format!(
        "'{}' did not open",
        target.label
    )))
}

/// Manages capture-target enumeration, capture sessions, and shared textures.
pub struct ScreenCaptureManager {
    targets: Vec<CaptureTargetInfo>,
    active: HashMap<CaptureId, ActiveCapture>,
    next_id: CaptureId,
    permission: PermissionState,
    disabled: bool,
    /// Capture threads let go of and still ending — closing their session,
    /// which macOS may never finish (see [`retire`]).
    retiring: Vec<std::thread::JoinHandle<()>>,
    /// Opens a target nothing captures yet (see [`Opens`]).
    opens: Opens,
    /// How many times the size its picture is drawn at a capture is asked
    /// for (see [`Self::size_as_drawn`], [`room_of`]).
    room: f32,
    /// Whether inputs draw a capture from its view as coded (see
    /// [`Self::coded_view`], [`coded_of`]).
    coded: bool,
}

/// The room a capture is given when nothing says: twice the size it is
/// drawn at.
const ROOM: f32 = 2.0;

/// The room `NIDA_CAPTURE_ROOM` says (`said`), read once as the app starts:
/// from 1 — each capture asked for as it is drawn, as it was before it was
/// given room — to 4; [`ROOM`] when it says nothing or no such number.
fn room_of(said: Option<&str>) -> f32 {
    said.and_then(|s| s.trim().parse::<f32>().ok())
        .filter(|times| times.is_finite() && (1.0..=4.0).contains(times))
        .unwrap_or(ROOM)
}

/// Whether captures are drawn from their code values, as
/// `NIDA_CAPTURES_CODED` says (`said`), read once as the app starts: they
/// are, but for `0` — each filtered as light, as it was before.
fn coded_of(said: Option<&str>) -> bool {
    said.is_none_or(|s| s.trim() != "0")
}

/// Capture threads still ending past which it is said, at each such many
/// more: macOS not letting go of them. Captures still start — a talk's
/// window refused would leave its screen black — each such thread left
/// behind costing a thread and its session.
const RETIRING_SAID_PAST: usize = 8;

impl Default for ScreenCaptureManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenCaptureManager {
    pub fn new() -> Self {
        let mut mgr = Self {
            targets: Vec::new(),
            active: HashMap::new(),
            next_id: 0,
            permission: platform::permission_state(),
            disabled: false,
            retiring: Vec::new(),
            opens: platform::open,
            room: room_of(std::env::var("NIDA_CAPTURE_ROOM").ok().as_deref()),
            coded: coded_of(std::env::var("NIDA_CAPTURES_CODED").ok().as_deref()),
        };
        mgr.scan_targets();
        mgr
    }

    /// Explicit no-op mode for the `--no-screen-capture` CLI flag.
    pub fn new_disabled() -> Self {
        Self {
            targets: Vec::new(),
            active: HashMap::new(),
            next_id: 0,
            permission: PermissionState::NotRequired,
            disabled: true,
            retiring: Vec::new(),
            opens: platform::open,
            room: 1.0,
            coded: true,
        }
    }

    /// Disabled (nothing opens), knowing `targets`: what a show restores
    /// against, without a screen to capture.
    #[cfg(test)]
    pub(crate) fn disabled_with_targets(targets: Vec<CaptureTargetInfo>) -> Self {
        let mut manager = Self::new_disabled();
        manager.targets = targets;
        manager
    }

    /// Enabled, as `new`, without asking the platform for permission or
    /// targets — or for a capture: a target nothing captures yet opens as
    /// a mock of it, or not at all (see `opening_fails`).
    #[cfg(test)]
    pub(crate) fn enabled_for_tests() -> Self {
        Self {
            targets: Vec::new(),
            active: HashMap::new(),
            next_id: 0,
            permission: PermissionState::NotRequired,
            disabled: false,
            retiring: Vec::new(),
            opens: open_a_mock,
            room: 1.0,
            coded: true,
        }
    }

    /// Each capture asked for `times` the size it is drawn at from here on
    /// (see [`Self::size_as_drawn`]): a test's managers give none (1), the
    /// app's [`ROOM`].
    #[cfg(test)]
    pub(crate) fn give_room(&mut self, times: f32) {
        self.room = times;
    }

    /// Inputs draw captures from their code values, or — not `coded` —
    /// from their light: each capture opened from here on, and each
    /// running one from its next size on (see [`Self::coded_view`]).
    #[cfg(test)]
    pub(crate) fn draw_coded(&mut self, coded: bool) {
        self.coded = coded;
    }

    /// As it is, on a GPU that can do what `flags` say. One that reads a
    /// texture in its own format alone (OpenGL ES: no `VIEW_FORMATS`) makes
    /// no texture asked for a second one, so a capture has no view as
    /// coded there, and inputs draw it from its light (see
    /// [`Self::coded_view`]).
    #[must_use]
    pub fn on_gpu(mut self, flags: wgpu::DownlevelFlags) -> Self {
        self.coded &= flags.contains(wgpu::DownlevelFlags::VIEW_FORMATS);
        self
    }

    /// Enabled, knowing `targets`: the screens of a desk, to open by
    /// [`ScreenCaptureManager::open_mock`].
    #[cfg(test)]
    pub(crate) fn enabled_with_targets(targets: Vec<CaptureTargetInfo>) -> Self {
        let mut manager = Self::enabled_for_tests();
        manager.targets = targets;
        manager
    }

    /// From here on nothing opens: what is picked has gone since it was
    /// listed. A capture running already is still shared.
    #[cfg(test)]
    pub(crate) fn opening_fails(&mut self) {
        self.opens = open_nothing;
    }

    /// How many hold capture `id`: none when it is not running.
    #[cfg(test)]
    pub(crate) fn holders(&self, id: CaptureId) -> u32 {
        self.active.get(&id).map_or(0, |active| active.ref_count)
    }

    /// Whether a capture can actually be opened.
    ///
    /// Gated on a real backend, not on the cargo feature: the feature is
    /// default-on everywhere but only macOS has an implementation, so keying
    /// off the feature would have Linux and Windows advertise capture in the
    /// library panel and then fail every open.
    pub fn is_available(&self) -> bool {
        !self.disabled && platform::backend_name() != platform::unsupported::backend_name()
    }

    pub fn backend_name(&self) -> &'static str {
        if self.disabled {
            "disabled"
        } else {
            platform::backend_name()
        }
    }

    pub fn permission_state(&self) -> PermissionState {
        self.permission
    }

    /// Ask the OS for capture permission and refresh the cached state.
    pub fn request_permission(&mut self) {
        if self.disabled {
            return;
        }
        platform::request_permission();
        self.permission = platform::permission_state();
    }

    /// Re-enumerate displays and windows. Manual, never polled — window lists
    /// churn constantly and polling would thrash the library panel.
    pub fn scan_targets(&mut self) {
        if self.disabled {
            self.targets.clear();
            return;
        }
        self.permission = platform::permission_state();
        match platform::enumerate() {
            Ok(targets) => {
                log::info!(
                    "Screen capture scan: {} target(s) via {}",
                    targets.len(),
                    platform::backend_name()
                );
                self.targets = targets;
            }
            Err(e) => {
                log::warn!("Screen capture enumeration failed: {e}");
                self.targets.clear();
            }
        }
    }

    pub fn targets(&self) -> &[CaptureTargetInfo] {
        &self.targets
    }

    /// Find an enumerated target by its handle-free identity. This is how a
    /// restored scene rebinds onto a live target.
    ///
    /// Windows match on `(app, title)` first, then fall back to a **unique**
    /// match on `app` alone — an editor or browser that was retitled is the
    /// common case, but if two windows of the same app are open there is no
    /// principled way to choose, so the fallback declines.
    ///
    /// A display is found by its own identity, and by its name only when
    /// that is not plugged in or was never kept (see
    /// [`backend::find_display`]).
    pub fn find_target(&self, identity: &TargetIdentity) -> Option<&CaptureTargetInfo> {
        match identity {
            TargetIdentity::Display { label, uuid } => {
                backend::find_display(&self.targets, label, uuid.as_deref())
            }
            TargetIdentity::Window { app, .. } => {
                if let Some(exact) = self.targets.iter().find(|t| &t.identity() == identity) {
                    return Some(exact);
                }
                let mut matches = self
                    .targets
                    .iter()
                    .filter(|t| t.kind == CaptureTargetKind::Window)
                    .filter(|t| t.app.as_deref() == Some(app.as_str()));
                let first = matches.next()?;
                if matches.next().is_some() {
                    None
                } else {
                    Some(first)
                }
            }
        }
    }

    /// Whether `identity` is a display kept by its name alone here, where
    /// displays have an identity to be kept by: it shows whichever screen
    /// has that name now, until its screen is picked again (see
    /// [`TargetIdentity::restored_to`]).
    #[must_use]
    pub fn goes_by_name(&self, identity: &TargetIdentity) -> bool {
        identity.is_display_by_name()
            && self
                .targets
                .iter()
                .any(|t| t.kind == CaptureTargetKind::Display && t.uuid.is_some())
    }

    /// Open a capture and start its thread. If an identical target is already
    /// captured, increments the ref count and returns the existing session.
    ///
    /// Returns `(id, width, height)` of the shared texture.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::Unavailable`] when disabled or unsupported,
    /// [`CaptureError::PermissionDenied`] when the OS refuses, or
    /// [`CaptureError::Backend`] if the session or its thread cannot start.
    pub fn open(
        &mut self,
        target: &CaptureTargetInfo,
        config: CaptureConfig,
        device: &wgpu::Device,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        if self.disabled {
            return Err(CaptureError::Unavailable(
                "screen capture disabled via --no-screen-capture".into(),
            ));
        }
        if !self.permission.allows_capture() {
            // Re-check rather than trusting a state cached at startup: the user
            // may have granted access since, and on macOS that is exactly the
            // flow (prompt → System Settings → return).
            self.permission = platform::permission_state();
            if !self.permission.allows_capture() {
                return Err(CaptureError::PermissionDenied);
            }
        }

        if let Some(shared) = self.share(target, &config, false) {
            return Ok(shared);
        }

        let config = config.sanitized();
        let session = (self.opens)(target, &config)?;
        self.start_session(target, session, config, device)
    }

    /// Keep `thread`, let go of and still ending, clearing those that have
    /// ended; saying so when macOS has not let go of many.
    fn keep_retiring(&mut self, thread: Option<std::thread::JoinHandle<()>>) {
        self.retiring.retain(|thread| !thread.is_finished());
        let Some(thread) = thread else {
            return;
        };
        self.retiring.push(thread);
        let ending = self.retiring.len();
        if ending >= RETIRING_SAID_PAST && ending.is_multiple_of(RETIRING_SAID_PAST) {
            log::warn!(
                "macOS has not let go of {ending} screen captures: their threads are still ending"
            );
        }
    }

    /// The session already capturing `target` — the same window, not only
    /// one of its app and title — with one more holder.
    fn share(
        &mut self,
        target: &CaptureTargetInfo,
        config: &CaptureConfig,
        presentation: bool,
    ) -> Option<(CaptureId, u32, u32)> {
        let identity = target.identity();
        let (id, active) = self.active.iter_mut().find(|(_, a)| {
            a.identity == identity
                && a.platform_id == target.platform_id
                && a.presentation == presentation
        })?;
        active.ref_count += 1;
        let id = *id;
        let opened = (id, active.width, active.height);
        // At least as large as its new opener asks — a show opened at 4K
        // while one at 1080p captured the same screen — never smaller: what
        // is on air from it does not blur for an input added. Its other
        // settings are left as they are.
        let raised = self.config(id).and_then(|running| match (running.scale_to, config.scale_to) {
            (Some((w, h)), Some((bw, bh))) => Some((w.max(bw), h.max(bh))),
            _ => None,
        });
        self.cap_to(id, raised);
        Some(opened)
    }

    /// Capture `id` capped at `bound` (the show's size) from its next tick,
    /// its other settings — another input's crop on a shared one — kept.
    pub fn cap_to(&mut self, id: CaptureId, bound: Option<(u32, u32)>) {
        self.update_config(id, |config| config.scale_to = bound);
    }

    /// Capture `id` sized as its picture is `drawn` in the show's frame: the
    /// most among the inputs showing it (see `Deck::capture_drawn`) — one
    /// that fills a frame of another shape, or is zoomed into, wants more
    /// of its pixels than one fitted. Capped at the size that fits whatever
    /// it was drawn at — how it was — a 4K screen zoomed into twice over in
    /// a 1080p show came 1920 × 1080 and was enlarged; so was one filling
    /// an upright frame. It never comes bigger than its screen or window is
    /// (see `resample::drawn_within`).
    ///
    /// With room: asked for twice the size it is drawn at, where its screen
    /// or window has as much (`NIDA_CAPTURE_ROOM`, see [`room_of`]) — a 4K
    /// screen fitted into a 1080p show comes whole, a 5K one at 3840 × 2160
    /// — and drawn smaller from that by the show, which averages what each
    /// pixel covers exactly (`scaled_sample.wgsl`). Asked for at the size it
    /// is drawn, macOS made it smaller, and soft: a 4K screen's text at
    /// exactly half its size came with seven tenths of its contrast.
    ///
    /// Bigger at once, though not within [`SIZED_EVERY`] of the last time;
    /// smaller when its picture has been drawn smaller for
    /// [`SMALLER_AFTER`] at `now`. Its other settings are kept, as
    /// [`Self::cap_to`] keeps them. A presentation's capture is left as its
    /// presentation has it.
    pub fn size_as_drawn(&mut self, id: CaptureId, drawn: Drawn, now: std::time::Instant) {
        let room = self.room;
        let Some(active) = self.active.get_mut(&id) else {
            return;
        };
        if active.presentation {
            return;
        }
        let drawn = drawn.with_room(room);
        let Ok(mut config) = active.config.lock() else {
            return;
        };
        let runs = config.drawn;
        if drawn == runs {
            active.smaller_since = None;
            return;
        }
        let bigger =
            drawn.fitted > runs.fitted || drawn.across > runs.across || drawn.down > runs.down;
        let due = if bigger {
            active.smaller_since = None;
            active
                .sized
                .is_none_or(|at| now.saturating_duration_since(at) >= SIZED_EVERY)
        } else {
            let since = *active.smaller_since.get_or_insert(now);
            now.saturating_duration_since(since) >= SMALLER_AFTER
        };
        if due {
            config.drawn = drawn;
            active.sized = Some(now);
            active.smaller_since = None;
        }
    }

    /// Take a capture of `target` opened elsewhere — the slow part, done off
    /// the render thread (a PowerPoint show's window) — set to `config`.
    ///
    /// That window already captured, the same size and so set: shared. Opened
    /// again at a new size or with new settings (a show's window resized):
    /// put in the running one's place, under its id, so what shows it carries
    /// on — the running one sizes its frames from the window as it was.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::Unavailable`] when disabled, or
    /// [`CaptureError::Backend`] if the session's thread cannot start.
    pub fn adopt(
        &mut self,
        target: &CaptureTargetInfo,
        session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
        device: &wgpu::Device,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        self.adopt_with(target, session, config, device, true)
    }

    /// [`Self::adopt`], but never shared: the window already captured has
    /// the running capture's place taken by this one, under its id — one
    /// opened again because the running one never sent a picture.
    ///
    /// # Errors
    ///
    /// As [`Self::adopt`].
    pub fn adopt_afresh(
        &mut self,
        target: &CaptureTargetInfo,
        session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
        device: &wgpu::Device,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        self.adopt_with(target, session, config, device, false)
    }

    fn adopt_with(
        &mut self,
        target: &CaptureTargetInfo,
        mut session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
        device: &wgpu::Device,
        share: bool,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        if self.disabled {
            return Err(CaptureError::Unavailable(
                "screen capture disabled via --no-screen-capture".into(),
            ));
        }
        let config = config.sanitized();
        let identity = target.identity();
        let running = self
            .active
            .iter()
            .find(|(_, a)| {
                a.identity == identity && a.platform_id == target.platform_id && a.presentation
            })
            .map(|(id, a)| {
                let same = a.target_size == (target.width, target.height)
                    && a.backend_size == session.resolution()
                    && a.config.lock().is_ok_and(|c| *c == config);
                (*id, same)
            });
        match running {
            Some((_, true)) if share => {
                if let Some(shared) = self.share(target, &config, true) {
                    return Ok(shared);
                }
            }
            Some((id, _)) => {
                if let Err(e) = session.set_config(&config) {
                    log::warn!(
                        "Screen capture of '{}': settings not taken: {e}",
                        target.label
                    );
                }
                return self.replace_session(id, target, session, config);
            }
            None => {}
        }
        // Opened to the opener's settings: the thread only follows changes
        // made after it starts.
        if let Err(e) = session.set_config(&config) {
            log::warn!(
                "Screen capture of '{}': settings not taken: {e}",
                target.label
            );
        }
        let started = self.start_session(target, session, config, device)?;
        if let Some(active) = self.active.get_mut(&started.0) {
            active.presentation = true;
        }
        Ok(started)
    }

    /// Capture `id` from `session` from now on, set to `config`: its thread
    /// stopped and one started for `session`, the frames going where they
    /// went. Its texture follows the new frames' size (see `upload_frame`).
    fn replace_session(
        &mut self,
        id: CaptureId,
        target: &CaptureTargetInfo,
        session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        let mut session = session;
        let backend_size = session.resolution();
        let Some(active) = self.active.get_mut(&id) else {
            return Err(CaptureError::Backend(format!("capture {id} is gone")));
        };
        let retired = retire(active);
        let opened = config.clone();
        if let Ok(mut guard) = active.config.lock() {
            *guard = config;
        }
        // What the one replaced sent and is not yet shown is not this one's.
        if let Ok(mut picker) = active.frame_data.lock() {
            picker.discard_waiting();
        }
        active.fresh = 0;
        let stop_flag = Arc::new(AtomicBool::new(false));
        // Its own: the one replaced, still ending, says nothing of this one.
        active.connected = Arc::new(AtomicBool::new(false));
        let frame_tx = Arc::clone(&active.frame_data);
        let config_rx = Arc::clone(&active.config);
        let stop_rx = Arc::clone(&stop_flag);
        let connected_tx = Arc::clone(&active.connected);
        let current = Arc::clone(&active.current);
        let mine = current.load(Ordering::SeqCst);
        let spawned = std::thread::Builder::new()
            .name(format!("screen-capture-{id}"))
            .spawn(move || {
                capture_loop(
                    session.as_mut(),
                    id,
                    &frame_tx,
                    opened,
                    &config_rx,
                    &stop_rx,
                    (&connected_tx, &current, mine),
                );
            });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(e) => {
                // Its thread stopped and none to follow: gone, not shared.
                self.active.remove(&id);
                self.keep_retiring(retired);
                return Err(CaptureError::Backend(format!(
                    "failed to spawn capture thread: {e}"
                )));
            }
        };
        active.stop_flag = stop_flag;
        active.thread = Some(thread);
        active.target_size = (target.width, target.height);
        active.backend_size = backend_size;
        active.ref_count += 1;
        log::info!(
            "Screen capture {id}: '{}' opened again at {}x{}",
            target.label,
            target.width,
            target.height
        );
        let opened = (id, active.width, active.height);
        self.keep_retiring(retired);
        Ok(opened)
    }

    /// Open a synthetic capture. Always available — used by tests, benches, and
    /// the headless CI path, where no display server or permission exists.
    ///
    /// # Errors
    ///
    /// Returns [`CaptureError::Backend`] if the capture thread cannot spawn.
    pub fn open_mock(
        &mut self,
        target: &CaptureTargetInfo,
        config: CaptureConfig,
        device: &wgpu::Device,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        if let Some(shared) = self.share(target, &config, false) {
            return Ok(shared);
        }
        let config = config.sanitized();
        let session: Box<dyn ScreenCaptureBackend> = Box::new(MockBackend::new(
            target.label.clone(),
            target.width,
            target.height,
            config.clone(),
        ));
        self.start_session(target, session, config, device)
    }

    fn start_session(
        &mut self,
        target: &CaptureTargetInfo,
        session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
        device: &wgpu::Device,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        let mut session = session;
        let (width, height) = {
            let (w, h) = session.resolution();
            (w.max(1), h.max(1))
        };
        // The texture is allocated in the backend's own layout so no CPU
        // swizzle is ever needed. A frame that has already arrived is
        // authoritative; otherwise take the backend's declared format, which is
        // the only answer a push-based backend can give at `open`.
        let first = session.next_frame();
        let format = first
            .as_ref()
            .map_or_else(|| session.pixel_format(), |f| f.format);

        let id = self.next_id;
        // Skip the unbound sentinel so a restored-but-unbound deck can never be
        // mistaken for a live session.
        self.next_id = self.next_id.wrapping_add(1);
        if self.next_id == UNBOUND_CAPTURE_ID {
            self.next_id = 0;
        }

        let (texture, texture_view, coded_view) =
            Self::create_texture(device, id, &target.label, width, height, format, self.coded);

        let mut picker = FramePicker::keeping(PICKER_FRAMES);
        if let Some(first) = first {
            let let_go = picker.push(frame_time(&first), first);
            drop(let_go);
        }
        let frame_data = Arc::new(Mutex::new(picker));
        let opened = config.clone();
        let config = Arc::new(Mutex::new(config));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let current = Arc::new(std::sync::atomic::AtomicU64::new(0));

        let frame_tx = Arc::clone(&frame_data);
        let config_rx = Arc::clone(&config);
        let stop_rx = Arc::clone(&stop_flag);
        let connected_tx = Arc::clone(&connected);
        let current_rx = Arc::clone(&current);

        let thread = std::thread::Builder::new()
            .name(format!("screen-capture-{id}"))
            .spawn(move || {
                capture_loop(
                    session.as_mut(),
                    id,
                    &frame_tx,
                    opened,
                    &config_rx,
                    &stop_rx,
                    (&connected_tx, &current_rx, 0),
                );
            })
            .map_err(|e| CaptureError::Backend(format!("failed to spawn capture thread: {e}")))?;

        log::info!(
            "Opened screen capture {id}: '{}' {width}x{height} ({:?})",
            target.label,
            format
        );

        self.active.insert(
            id,
            ActiveCapture {
                identity: target.identity(),
                platform_id: target.platform_id,
                target_size: (target.width, target.height),
                backend_size: (width, height),
                label: target.label.clone(),
                texture,
                texture_view,
                coded_view,
                width,
                height,
                format,
                ref_count: 1,
                frame_data,
                config,
                stop_flag,
                connected,
                current,
                thread: Some(thread),
                uploaded: 0,
                picture: 0,
                fresh: 0,
                last_picture: None,
                presentation: false,
                sized: None,
                smaller_since: None,
            },
        );

        Ok((id, width, height))
    }

    /// A capture's texture, with its view as light — sRGB, which the GPU
    /// decodes as it is read — and, where captures are drawn as `coded`,
    /// its view as it was coded (see [`Self::coded_view`]).
    fn create_texture(
        device: &wgpu::Device,
        id: CaptureId,
        label: &str,
        width: u32,
        height: u32,
        format: CapturePixelFormat,
        coded: bool,
    ) -> (wgpu::Texture, wgpu::TextureView, Option<wgpu::TextureView>) {
        // Read as the code values it holds, too — asked for only where it
        // is drawn so: not every GPU makes a texture read two ways (see
        // [`Self::on_gpu`]).
        let twin = coded.then(|| format.coded_format());
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(&format!("ScreenCapture {id} ({label})")),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: format.wgpu_format(),
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: twin.as_slice(),
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let coded = twin.map(|twin| {
            texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("ScreenCapture as coded"),
                format: Some(twin),
                ..Default::default()
            })
        });
        (texture, view, coded)
    }

    /// Release a capture reference. Stops the thread when the count hits zero.
    pub fn release(&mut self, id: CaptureId) {
        let Some(active) = self.active.get_mut(&id) else {
            return;
        };
        active.ref_count = active.ref_count.saturating_sub(1);
        if active.ref_count > 0 {
            return;
        }
        log::info!("Closing screen capture {id} (no more references)");
        let Some(mut removed) = self.active.remove(&id) else {
            return;
        };
        self.keep_retiring(retire(&mut removed));
    }

    /// Reconcile the live sessions against the decks that actually hold them.
    ///
    /// `holders` maps a capture id to the number of decks referencing it right
    /// now. Any session no deck holds is stopped, and surviving sessions have
    /// their ref count reset to the real holder count.
    ///
    /// Ref counting alone is not enough: `open`/`release` are only paired on the
    /// explicit remove-deck path, but a deck can be dropped without passing
    /// through it — a scene diff rebuilding a slot, an undo, a truncated
    /// channel, a removed channel, a whole mixer replaced on scene load. Each of
    /// those left the capture thread grabbing and downscaling frames for the
    /// rest of the session, off-screen and invisible in the UI. Deriving the
    /// count from the decks that exist makes it correct by construction instead
    /// of relying on every future call site to release.
    ///
    /// Safe to call once per frame: capture sessions are opened and attached to
    /// a deck within a single command, never across a frame boundary.
    pub fn reconcile_holders(&mut self, holders: &HashMap<CaptureId, u32>) {
        let orphans: Vec<CaptureId> = self
            .active
            .keys()
            .copied()
            .filter(|id| !holders.contains_key(id))
            .collect();
        for id in orphans {
            let Some(mut removed) = self.active.remove(&id) else {
                continue;
            };
            log::warn!(
                "Screen capture {id} ('{}') outlived its last deck — stopping it",
                removed.label
            );
            self.keep_retiring(retire(&mut removed));
        }
        for (id, active) in &mut self.active {
            if let Some(&count) = holders.get(id) {
                active.ref_count = count;
            }
        }
    }

    /// Shared texture view for a capture (decks read from this).
    pub fn texture_view(&self, id: CaptureId) -> Option<&wgpu::TextureView> {
        self.active.get(&id).map(|a| &a.texture_view)
    }

    /// The same texture read as it was coded: its sRGB code values as they
    /// are, not the light they stand for. An input draws its capture from
    /// this one, filtering the code values and making them light after
    /// (`light_of_coded`, `scaled_sample.wgsl`) — as every picture viewer,
    /// browser and macOS itself shrink a picture. Filtered as light, dark
    /// text on a light page drawn smaller came out pale and thin: the
    /// light round a stroke counts for more than the stroke. None with
    /// `NIDA_CAPTURES_CODED=0` (see [`coded_of`]) and on a GPU that reads
    /// a texture one way (see [`Self::on_gpu`]): drawn from
    /// [`Self::texture_view`] then, as it was.
    pub fn coded_view(&self, id: CaptureId) -> Option<&wgpu::TextureView> {
        self.active.get(&id).and_then(|a| a.coded_view.as_ref())
    }

    pub fn resolution(&self, id: CaptureId) -> Option<(u32, u32)> {
        self.active.get(&id).map(|a| (a.width, a.height))
    }

    pub fn label(&self, id: CaptureId) -> Option<&str> {
        self.active.get(&id).map(|a| a.label.as_str())
    }

    pub fn identity(&self, id: CaptureId) -> Option<&TargetIdentity> {
        self.active.get(&id).map(|a| &a.identity)
    }

    pub fn is_active(&self, id: CaptureId) -> bool {
        self.active.contains_key(&id)
    }

    /// Whether a frame has been put in the capture's texture: until then it
    /// shows nothing — a talk starting waits for it.
    pub fn has_picture(&self, id: CaptureId) -> bool {
        self.active.get(&id).is_some_and(|a| a.uploaded > 0)
    }

    /// Which picture the capture's texture holds: another number each time
    /// a frame is put in it or it is made anew, no two the same across all
    /// captures — a screen or window where nothing moves sends no frame,
    /// and the number stands. `None` until it has a picture. What is drawn
    /// from the texture need not be drawn again while this is the same (see
    /// `Channel::keep_still_inputs`).
    pub fn picture(&self, id: CaptureId) -> Option<u64> {
        self.active
            .get(&id)
            .filter(|a| a.uploaded > 0)
            .map(|a| a.picture)
    }

    /// Whether the capture sent a new frame within `within`: a screen not
    /// there sends none, nor one whose picture is not changing.
    pub fn receiving(&self, id: CaptureId, within: std::time::Duration) -> bool {
        self.active
            .get(&id)
            .and_then(|a| a.last_picture)
            .is_some_and(|at| at.elapsed() <= within)
    }

    /// Whether the capture running now has sent a picture — not only one
    /// it replaced, whose last its texture may still show.
    pub fn has_fresh_picture(&self, id: CaptureId) -> bool {
        self.active.get(&id).is_some_and(|a| a.fresh > 0)
    }

    /// Whether the capture has delivered at least one frame and is still healthy.
    pub fn is_connected(&self, id: CaptureId) -> bool {
        self.active
            .get(&id)
            .is_some_and(|a| a.connected.load(Ordering::SeqCst))
    }

    pub fn active_ids(&self) -> Vec<CaptureId> {
        let mut ids: Vec<CaptureId> = self.active.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    /// A live capture's settings, as its inputs have them: how big it is
    /// drawn, which is its own and no input's, apart (see [`Self::drawn`]).
    pub fn config(&self, id: CaptureId) -> Option<CaptureConfig> {
        let active = self.active.get(&id)?;
        active.config.lock().ok().map(|c| CaptureConfig {
            drawn: Drawn::default(),
            ..c.clone()
        })
    }

    /// How big a live capture's picture is drawn, as it is sized for now
    /// (see [`Self::size_as_drawn`]).
    pub fn drawn(&self, id: CaptureId) -> Option<Drawn> {
        let active = self.active.get(&id)?;
        active.config.lock().ok().map(|c| c.drawn)
    }

    /// Update a live capture's config. The capture thread picks it up on its
    /// next tick; no session restart. How big it is drawn stays as it is
    /// sized: settings given it — an input's, a show's as saved — say
    /// nothing of that.
    pub fn set_config(&mut self, id: CaptureId, config: CaptureConfig) {
        let Some(active) = self.active.get_mut(&id) else {
            return;
        };
        if let Ok(mut guard) = active.config.lock() {
            let drawn = guard.drawn;
            *guard = CaptureConfig {
                drawn,
                ..config.sanitized()
            };
        }
    }

    /// Mutate one field of a live capture's config.
    pub fn update_config(&mut self, id: CaptureId, f: impl FnOnce(&mut CaptureConfig)) {
        let Some(active) = self.active.get_mut(&id) else {
            return;
        };
        if let Ok(mut guard) = active.config.lock() {
            f(&mut guard);
            *guard = guard.clone().sanitized();
        }
    }

    /// Upload frames only for captures whose IDs are in the set. Captures not
    /// in the set skip the GPU upload entirely, so an invisible capture deck
    /// costs nothing.
    ///
    /// `show_at` is when the show frame being drawn is due on screen, host
    /// seconds: each capture's frame for it is picked by that.
    pub fn update_selective(
        &mut self,
        context: &GpuContext,
        needed: &HashSet<CaptureId>,
        show_at: f64,
    ) {
        self.retiring.retain(|thread| !thread.is_finished());
        let coded = self.coded;
        for (id, active) in &mut self.active {
            if needed.contains(id) {
                Self::upload_frame(*id, active, context, show_at, coded);
            }
        }
    }

    /// Each capture's label and how long before its show frame's time the
    /// frame shown last was captured, seconds.
    pub fn picture_delays(&self) -> Vec<(String, f64)> {
        self.active
            .values()
            .filter_map(|active| {
                let latency = active.frame_data.try_lock().ok()?.latency()?;
                Some((active.label.clone(), latency))
            })
            .collect()
    }

    /// Upload frames for every active capture, for the show frame due at
    /// `show_at`.
    pub fn update(&mut self, context: &GpuContext, show_at: f64) {
        self.retiring.retain(|thread| !thread.is_finished());
        let coded = self.coded;
        for (id, active) in &mut self.active {
            Self::upload_frame(*id, active, context, show_at, coded);
        }
    }

    fn upload_frame(
        id: CaptureId,
        active: &mut ActiveCapture,
        context: &GpuContext,
        show_at: f64,
        coded: bool,
    ) {
        use crate::renderer::present_trace::{Step, TRACE};
        let device = &context.device;
        // Waits only while a frame is put in — moments: a show frame that
        // went without, the picker busy as a frame came, showed the one
        // before twice and skipped one the next (see the camera's `take`).
        let picked = active.frame_data.lock().ok().map(|mut picker| {
            let (frame, what) = picker.pick(show_at);
            let step = TRACE.recording().then(|| Step::Picked {
                input: format!("capture {id}"),
                show_at,
                captured: frame.as_ref().and(picker.shown()),
                skipped: match what {
                    crate::frame_picker::Picked::Skipped(n) => n,
                    _ => 0,
                },
                repeated: what == crate::frame_picker::Picked::Repeated,
                delay: picker.delay(),
                waiting: picker.waiting(),
            });
            (frame, step)
        });
        let Some((frame, step)) = picked else {
            return;
        };
        if let Some(step) = step {
            TRACE.note("", 0, step);
        }
        let Some(frame) = frame else {
            return;
        };

        // A crop or a target resize changes the delivered size mid-session.
        // Reallocate rather than dropping the frame, so crop is usable live.
        if frame.width != active.width
            || frame.height != active.height
            || frame.format != active.format
        {
            let (w, h) = (frame.width.max(1), frame.height.max(1));
            let (texture, view, coded) =
                Self::create_texture(device, id, &active.label, w, h, frame.format, coded);
            active.texture = texture;
            active.texture_view = view;
            active.coded_view = coded;
            active.width = w;
            active.height = h;
            active.format = frame.format;
        }
        // Another picture from here on: the texture made anew, a frame put
        // in it (one that fails to go in costs a drawing, no more).
        active.picture = crate::deck::next_picture();

        // In the GPU surface it was captured into: copied there, on the GPU.
        #[cfg(target_os = "macos")]
        if let Some(surface) = frame.surface {
            if let Err(e) = surface::copy_into(context, surface, &active.texture) {
                log::warn!("Screen capture {id}: frame not copied: {e}");
                return;
            }
            active.uploaded += 1;
            active.fresh += 1;
            active.last_picture = Some(std::time::Instant::now());
            return;
        }

        let expected = (active.width as usize) * (active.height as usize) * 4;
        if frame.data.len() < expected {
            return;
        }

        context.queue.write_texture(
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
        active.uploaded += 1;
        active.fresh += 1;
        active.last_picture = Some(std::time::Instant::now());
    }
}

impl Drop for ScreenCaptureManager {
    fn drop(&mut self) {
        for (_, mut active) in self.active.drain() {
            active.stop_flag.store(true, Ordering::Relaxed);
            if let Some(t) = active.thread.take() {
                let _ = t.join();
            }
        }
    }
}

/// Shortest and longest gap between polls of a self-paced backend. The OS owns
/// the delivery rate there, so this only bounds how long a delivered frame sits
/// unclaimed; it is oversampling, not capturing.
const SELF_PACED_MIN_POLL: std::time::Duration = std::time::Duration::from_millis(1);
const SELF_PACED_MAX_POLL: std::time::Duration = std::time::Duration::from_millis(4);

/// How long the capture thread sleeps between polls.
///
/// A polled backend produces a frame per call, so the loop *is* the clock and
/// sleeps a full frame interval. A self-paced backend already delivers at the
/// requested rate, so the loop must oversample it: polling at the same nominal
/// rate on a second, drifting clock silently drops and doubles frames. See
/// [`ScreenCaptureBackend::is_self_paced`].
fn poll_interval(frame_interval: std::time::Duration, self_paced: bool) -> std::time::Duration {
    if !self_paced {
        return frame_interval;
    }
    (frame_interval / 8).clamp(SELF_PACED_MIN_POLL, SELF_PACED_MAX_POLL)
}

/// Background capture loop — one per active target.
///
/// Delivery is paced by `config.rate`, which is deliberately decoupled from the
/// render rate: a self-capture at 60 fps would compound render cost every frame.
/// See spec/screen-capture.md § Self-Capture and Feedback Safety.
// The thread's inputs, each distinct; bundling them would only rename them.
#[allow(clippy::too_many_arguments)]
fn capture_loop(
    session: &mut dyn ScreenCaptureBackend,
    id: CaptureId,
    frame_data: &Mutex<FramePicker<CaptureFrame>>,
    opened: CaptureConfig,
    config: &Mutex<CaptureConfig>,
    stop: &AtomicBool,
    (connected, current, mine): (&AtomicBool, &std::sync::atomic::AtomicU64, u64),
) {
    // Still the capture's thread, not one replaced still ending.
    let still_mine = || current.load(Ordering::SeqCst) == mine;
    let self_paced = session.is_self_paced();
    // What the backend was opened with, not what the settings say by the time
    // this thread starts: a cap raised meanwhile (another input sharing it)
    // would read as already applied, and never reach the backend.
    let mut applied = opened;
    let mut interval = poll_interval(applied.frame_interval(), self_paced);
    log::debug!(
        "Screen capture {id} thread started ('{}', self_paced={self_paced}, poll={interval:?})",
        session.label()
    );

    while !stop.load(Ordering::Relaxed) {
        let tick = std::time::Instant::now();

        if let Ok(current) = config.lock() {
            if *current != applied {
                applied = current.clone();
                interval = poll_interval(applied.frame_interval(), self_paced);
                drop(current);
                if let Err(e) = session.set_config(&applied) {
                    log::warn!("Screen capture {id}: config change rejected: {e}");
                }
            }
        }

        session.follow_target();

        // Every frame come, each with when it was captured: the render
        // thread picks the one for each show frame. A self-paced backend may
        // have several waiting; a polled one makes one a call.
        while let Some(frame) = session.next_frame() {
            connected.store(true, Ordering::SeqCst);
            // Frames let go give their surfaces back to the capture here,
            // the picker no longer held: the show's frame waits for it.
            // Asked under the picker's lock: once replaced (the count moved
            // on, then the picker emptied, under it) nothing more goes in.
            let let_go = frame_data
                .lock()
                .map(|mut picker| {
                    if still_mine() {
                        picker.push(frame_time(&frame), frame)
                    } else {
                        vec![frame]
                    }
                })
                .unwrap_or_default();
            drop(let_go);
            if !self_paced {
                break;
            }
        }

        // A backend that blocks internally until a frame is ready will already
        // have consumed the budget, in which case this is a no-op.
        if let Some(rest) = interval.checked_sub(tick.elapsed()) {
            std::thread::sleep(rest);
        }
    }

    connected.store(false, Ordering::SeqCst);
    log::debug!("Screen capture {id} thread stopped");
}

/// Stop the capture's thread and let it end on its own: it puts nothing
/// more in and closes its session — macOS stopping the capture, which can
/// take a while with the window server busy — on its own thread. Waited
/// for here, it held the show's thread up as a talk's window was let go.
fn retire(active: &mut ActiveCapture) -> Option<std::thread::JoinHandle<()>> {
    active.stop_flag.store(true, Ordering::Relaxed);
    active.current.fetch_add(1, Ordering::SeqCst);
    // Not waited for: it ends once it sees the stop.
    active.thread.take()
}

#[cfg(test)]
mod tests {
    use super::backend::{
        mock_targets, CaptureConfig, CaptureError, CaptureTargetInfo, CaptureTargetKind, CropRect,
        Drawn, MockBackend,
        ScreenCaptureBackend, TargetIdentity,
    };
    use super::{poll_interval, ScreenCaptureManager, SELF_PACED_MAX_POLL, SELF_PACED_MIN_POLL};
    use super::{capture_loop, CaptureFrame, FramePicker, PICKER_FRAMES};
    use super::{coded_of, room_of, ROOM};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    // ── Capture-thread pacing ───────────────────────────────────────

    #[test]
    fn polled_backend_is_paced_by_the_loop_at_exactly_the_frame_interval() {
        let interval = Duration::from_millis(33);
        assert_eq!(poll_interval(interval, false), interval);
    }

    /// The bug this pins: polling a self-paced backend at its own rate puts two
    /// independent clocks of the same frequency in series, which drops and
    /// doubles frames as they drift. Oversampling is the fix.
    #[test]
    fn self_paced_backend_is_oversampled_not_matched() {
        for fps in [1.0_f32, 15.0, 30.0, 60.0, 120.0] {
            let interval = Duration::from_secs_f32(1.0 / fps);
            let poll = poll_interval(interval, true);
            assert!(
                poll < interval,
                "{fps} fps: poll {poll:?} must be shorter than the frame interval {interval:?}"
            );
            assert!(
                poll >= SELF_PACED_MIN_POLL && poll <= SELF_PACED_MAX_POLL,
                "{fps} fps: poll {poll:?} outside the allowed bounds"
            );
        }
    }

    #[test]
    fn self_paced_poll_never_busy_spins() {
        // A degenerate interval must still sleep, or the capture thread would
        // burn a core.
        assert!(poll_interval(Duration::ZERO, true) >= SELF_PACED_MIN_POLL);
    }

    #[test]
    fn disabled_manager_reports_unavailable_and_enumerates_nothing() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        assert!(!mgr.is_available());
        assert!(mgr.targets().is_empty());
        mgr.scan_targets();
        assert!(mgr.targets().is_empty());
        assert_eq!(mgr.backend_name(), "disabled");
    }

    #[test]
    fn accessors_on_unknown_id_return_none_not_panic() {
        let mgr = ScreenCaptureManager::new_disabled();
        assert!(mgr.texture_view(0).is_none());
        assert!(mgr.resolution(7).is_none());
        assert!(mgr.label(7).is_none());
        assert!(mgr.config(7).is_none());
        assert!(!mgr.is_active(7));
        assert!(!mgr.is_connected(7));
        assert!(mgr.active_ids().is_empty());
    }

    #[test]
    fn release_of_unknown_id_is_a_noop() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.release(123);
        assert!(mgr.active_ids().is_empty());
    }

    #[test]
    fn find_target_matches_window_by_app_when_title_changed() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = mock_targets();
        // Exact identity hit.
        let exact = TargetIdentity::Window {
            app: "com.example.mock".into(),
            title: "Untitled".into(),
        };
        assert!(mgr.find_target(&exact).is_some());
        // Retitled window still resolves via the unique-app fallback.
        let retitled = TargetIdentity::Window {
            app: "com.example.mock".into(),
            title: "Something Else".into(),
        };
        assert!(mgr.find_target(&retitled).is_some());
    }

    #[test]
    fn find_target_declines_ambiguous_app_fallback() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        let mut targets = mock_targets();
        // Two windows of the same app — no principled way to pick one.
        let mut second = targets[2].clone();
        second.platform_id = 200;
        second.title = Some("Second".into());
        targets.push(second);
        mgr.targets = targets;

        let ambiguous = TargetIdentity::Window {
            app: "com.example.mock".into(),
            title: "Gone".into(),
        };
        assert!(
            mgr.find_target(&ambiguous).is_none(),
            "ambiguous app fallback must decline rather than guess"
        );
    }

    #[test]
    fn find_target_does_not_fall_back_for_displays() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = mock_targets();
        let missing = TargetIdentity::Display {
            label: "Nonexistent".into(),
            uuid: None,
        };
        assert!(mgr.find_target(&missing).is_none());
    }

    /// The screens of a desk as macOS lists them: each `(uuid, width)` under
    /// "Display N" by its place.
    fn listed(screens: &[(&str, u32)]) -> Vec<CaptureTargetInfo> {
        screens
            .iter()
            .zip(1u32..)
            .map(|((uuid, width), n)| CaptureTargetInfo {
                kind: CaptureTargetKind::Display,
                platform_id: u64::from(n) + 10,
                label: format!("Display {n}"),
                app: None,
                title: None,
                width: *width,
                height: width * 9 / 16,
                is_varda: false,
                uuid: Some((*uuid).to_string()),
            })
            .collect()
    }

    fn display(label: &str, uuid: Option<&str>) -> TargetIdentity {
        TargetIdentity::Display {
            label: label.into(),
            uuid: uuid.map(str::to_string),
        }
    }

    /// A show captured the 4K screen as "Display 1"; a third screen plugged
    /// in, macOS lists the 4K as "Display 2" and another as "Display 1" (an
    /// M5 Max, 2026-11-09). The show still finds the 4K, and keeps it under
    /// the name it is listed by now.
    #[test]
    fn a_display_is_found_by_its_identity_whatever_number_it_is_listed_under() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = listed(&[("capture", 1920), ("4k", 3840), ("second", 1920)]);
        let saved = display("Display 1", Some("4k"));
        let found = mgr.find_target(&saved).expect("the 4K screen");
        assert_eq!((found.width, found.label.as_str()), (3840, "Display 2"));
        assert_eq!(saved.bound_to(found), display("Display 2", Some("4k")));
    }

    /// A display picked by its name alone — in the window, or by an API
    /// client — is found by it, and kept by the identity of the one found
    /// from then on.
    #[test]
    fn a_display_picked_by_name_alone_is_found_by_it_and_then_kept_by_identity() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = listed(&[("4k", 3840), ("second", 1920)]);
        let picked = display("Display 1", None);
        let found = mgr.find_target(&picked).expect("the first screen");
        assert_eq!(found.width, 3840);
        assert_eq!(picked.bound_to(found), display("Display 1", Some("4k")));
        assert!(mgr.find_target(&display("Display 3", None)).is_none());
    }

    /// A show saved before displays were kept by identity has only the name.
    /// Which screen had it then is not known — a capture box plugged in
    /// since is "Display 1" now, the show's 4K "Display 2" (an M5 Max,
    /// 2026-11-10) — so read from the show it stays by its name, and is not
    /// made the screen that has the name today. One the show has by its
    /// identity is named as it is listed now; one stood in for stays its own.
    #[test]
    fn a_display_a_show_has_by_name_alone_stays_by_name() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = listed(&[("capture", 1920), ("4k", 3840)]);
        let saved = display("Display 1", None);
        let found = mgr.find_target(&saved).expect("the screen of that name now");
        assert_eq!(found.uuid.as_deref(), Some("capture"));
        assert_eq!(saved.restored_to(found), saved, "not the capture box's for good");
        assert!(saved.is_display_by_name());
        assert!(mgr.goes_by_name(&saved));

        let kept = display("Display 1", Some("4k"));
        let found = mgr.find_target(&kept).expect("the 4K");
        assert_eq!(kept.restored_to(found), display("Display 2", Some("4k")));
        assert!(!kept.is_display_by_name());
        assert!(!mgr.goes_by_name(&kept));

        let away = display("Display 2", Some("led"));
        let found = mgr.find_target(&away).expect("the one of its name");
        assert_eq!(away.restored_to(found), away, "still the LED wall's");

        let window = TargetIdentity::Window {
            app: "com.example.mock".into(),
            title: "Talk".into(),
        };
        assert!(!window.is_display_by_name());
        assert!(!mgr.goes_by_name(&window));
    }

    /// Where displays have no identity to be kept by, one kept by its name
    /// is kept as well as it can be: nothing to pick again, nothing said.
    #[test]
    fn a_display_by_name_is_all_there_is_where_displays_have_no_identity() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = listed(&[("a", 1920), ("b", 1920)]);
        for target in &mut mgr.targets {
            target.uuid = None;
        }
        let saved = display("Display 1", None);
        assert!(saved.is_display_by_name());
        assert!(!mgr.goes_by_name(&saved));
    }

    /// The screen a show kept is not plugged in — or the show came from
    /// another computer: the one of its name stands in, and the show goes on
    /// keeping its own, found again when it is back.
    #[test]
    fn a_display_not_plugged_in_is_stood_in_for_by_the_one_of_its_name() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = listed(&[("laptop", 1728), ("second", 1920)]);
        let saved = display("Display 2", Some("4k"));
        let found = mgr.find_target(&saved).expect("the screen of its name");
        assert_eq!(found.uuid.as_deref(), Some("second"));
        assert_eq!(saved.bound_to(found), saved, "still the 4K's");
        assert!(mgr.find_target(&display("Display 3", Some("4k"))).is_none());

        mgr.targets = listed(&[("laptop", 1728), ("second", 1920), ("4k", 3840)]);
        let found = mgr.find_target(&saved).expect("the 4K, back");
        assert_eq!(found.width, 3840);
    }

    /// Two identical screens may share an identity: the one of the name kept
    /// is taken, and neither name fitting, the first.
    #[test]
    fn of_two_displays_of_one_identity_the_one_of_the_name_kept_is_found() {
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.targets = listed(&[("laptop", 1728), ("twin", 1920), ("twin", 1920)]);
        let found = mgr.find_target(&display("Display 3", Some("twin")));
        assert_eq!(found.map(|t| t.label.as_str()), Some("Display 3"));
        let found = mgr.find_target(&display("Display 1", Some("twin")));
        assert_eq!(found.map(|t| t.label.as_str()), Some("Display 2"));
    }

    /// A window is kept as it was saved, whichever window was found for it:
    /// one of its app found by another title is not that title's from then on.
    #[test]
    fn a_window_found_stays_as_it_was_kept() {
        let targets = mock_targets();
        let saved = TargetIdentity::Window {
            app: "com.example.mock".into(),
            title: "Something Else".into(),
        };
        assert_eq!(saved.bound_to(&targets[2]), saved);
    }

    #[test]
    fn disabled_manager_refuses_to_open() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return; // No adapter in this environment — skip, per project convention.
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let err = mgr
            .open(&target, CaptureConfig::default(), &gpu.device)
            .expect_err("disabled manager must refuse");
        assert!(matches!(err, CaptureError::Unavailable(_)));
    }

    #[test]
    fn open_mock_is_refcounted_and_survives_release_of_one_holder() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);

        let (id, w, h) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock open");
        assert_eq!((w, h), (1920, 1080));

        // Second deck on the same target reuses the session.
        let (id2, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock reopen");
        assert_eq!(id, id2, "same target must share one capture session");
        assert_eq!(mgr.active_ids().len(), 1);

        // One holder leaving must not tear down the shared session.
        mgr.release(id);
        assert!(
            mgr.is_active(id),
            "session must survive while a holder remains"
        );
        mgr.release(id);
        assert!(!mgr.is_active(id), "session must stop at zero references");
        assert!(mgr.active_ids().is_empty());
    }

    #[test]
    fn open_mock_delivers_frames_and_uploads_to_the_shared_texture() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(2); // 800x600 window
        let cfg = CaptureConfig {
            scale_to: Some((160, 120)),
            ..Default::default()
        };
        let (id, w, h) = mgr.open_mock(&target, cfg, &gpu.device).expect("mock open");
        assert_eq!(
            (w, h),
            (160, 120),
            "scale_to must determine the shared texture size"
        );
        assert!(mgr.texture_view(id).is_some());

        let mut needed = std::collections::HashSet::new();
        needed.insert(id);
        mgr.update_selective(&gpu, &needed, crate::host_clock::host_seconds());
        assert_eq!(mgr.resolution(id), Some((160, 120)));
        mgr.release(id);
    }

    /// A test's manager opens a mock of a target nothing captures yet, set
    /// as asked — never the platform's capture, which would ask macOS for a
    /// screen — and nothing once told so: no capture left behind, though
    /// one running already is still shared.
    #[test]
    fn a_tests_manager_opens_a_mock_or_nothing() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let mut targets = mock_targets();
        let window = targets.remove(2);
        let screen = targets.remove(0);
        let asked = CaptureConfig {
            rate: 24.0,
            ..Default::default()
        };
        let (id, _, _) = mgr.open(&screen, asked.clone(), &gpu.device).expect("open");
        assert_eq!(mgr.label(id), Some("Mock Display 1"));
        assert_eq!(mgr.config(id), Some(asked.clone()), "as asked");

        mgr.opening_fails();
        let refused = mgr.open(&window, asked.clone(), &gpu.device);
        assert!(
            matches!(refused, Err(CaptureError::Backend(_))),
            "nothing opens"
        );
        assert_eq!(mgr.active_ids(), vec![id], "nothing left behind");
        let (again, _, _) = mgr.open(&screen, asked, &gpu.device).expect("shared");
        assert_eq!(again, id);
        mgr.release(id);
        mgr.release(id);
    }

    /// A capture opened again — a show opened at 4K while one at 1080p
    /// captured the same screen — takes the new opener's larger cap, never a
    /// smaller one, and keeps the crop set on it.
    #[test]
    fn a_shared_capture_takes_its_new_openers_cap() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let target = mock_targets().remove(0);
        let at = |w, h| CaptureConfig {
            scale_to: Some((w, h)),
            ..Default::default()
        };
        let (id, _, _) = mgr.open_mock(&target, at(1920, 1080), &gpu.device).expect("open");
        mgr.update_config(id, |c| c.crop.w = 0.5);
        let (again, _, _) = mgr.open_mock(&target, at(3840, 2160), &gpu.device).expect("open");
        assert_eq!(again, id, "shared");
        let config = mgr.config(id).expect("live");
        assert_eq!(config.scale_to, Some((3840, 2160)));
        assert!((config.crop.w - 0.5).abs() < 1e-6, "its crop kept");
        // Never lowered: what is on air from it stays as sharp.
        let (_, _, _) = mgr.open_mock(&target, at(1920, 1080), &gpu.device).expect("open");
        assert_eq!(mgr.config(id).expect("live").scale_to, Some((3840, 2160)));
        mgr.release(id);
        mgr.release(id);
        mgr.release(id);
    }

    #[test]
    fn a_window_shaped_unlike_the_deck_leaves_the_scaling_mode_something_to_do() {
        // Regression: the deck size was passed straight through as scale_to, so
        // a 4:3 window arrived as a 16:9 texture (letterboxed by the OS) and the
        // deck's Scale control did nothing — source and target dimensions were
        // identical, which every mode resolves to identity UVs.
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(2); // 800x600 window
        let (deck_w, deck_h) = (1920, 1080);
        let cfg = CaptureConfig {
            scale_to: Some(crate::screen_capture::resample::fit_within(
                target.width,
                target.height,
                deck_w,
                deck_h,
            )),
            ..Default::default()
        };
        let (id, w, h) = mgr.open_mock(&target, cfg, &gpu.device).expect("mock open");
        assert_eq!((w, h), (800, 600), "a small window must not be blown up");

        let (fill, _) = crate::deck::ScalingMode::Fill.compute_uv_transform(w, h, deck_w, deck_h);
        let (fit, _) = crate::deck::ScalingMode::Fit.compute_uv_transform(w, h, deck_w, deck_h);
        assert!(
            (fill[1] - 1.0).abs() > 0.01,
            "Fill must crop vertically, got {fill:?}"
        );
        assert!(
            (fit[0] - 1.0).abs() > 0.01,
            "Fit must pillarbox, got {fit:?}"
        );
        mgr.release(id);
    }

    #[test]
    fn update_selective_skips_captures_not_in_the_needed_set() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let (id, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock open");

        // Empty set: no upload, and crucially no panic on the absent entry.
        mgr.update_selective(
            &gpu,
            &std::collections::HashSet::new(),
            crate::host_clock::host_seconds(),
        );
        assert!(mgr.is_active(id));
        mgr.release(id);
    }

    /// The leak this pins: a deck dropped without going through `remove_deck`
    /// left its capture thread running for the rest of the session.
    #[test]
    fn reconcile_stops_a_session_no_deck_holds_any_more() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let (id, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock open");

        // Still held: reconciling must leave it alone.
        mgr.reconcile_holders(&std::collections::HashMap::from([(id, 1)]));
        assert!(mgr.is_active(id));

        // The holding deck is gone without a release.
        mgr.reconcile_holders(&std::collections::HashMap::new());
        assert!(
            !mgr.is_active(id),
            "an unheld session must be stopped, not left running"
        );
    }

    #[test]
    fn reconcile_makes_the_deck_count_authoritative_over_the_ref_count() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let (id, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock open");
        let (id2, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock reopen");
        assert_eq!(id, id2);

        // One of the two decks vanished without releasing, so the count is
        // stale at 2. After reconciling, the one remaining release must be
        // enough to close the session.
        mgr.reconcile_holders(&std::collections::HashMap::from([(id, 1)]));
        mgr.release(id);
        assert!(
            !mgr.is_active(id),
            "a stale ref count must not keep the session alive"
        );
    }

    #[test]
    fn a_window_opened_again_at_a_new_size_is_captured_anew_under_its_id() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let target = mock_targets().remove(0);
        let mock = |t: &CaptureTargetInfo, c: &CaptureConfig| -> Box<dyn ScreenCaptureBackend> {
            Box::new(MockBackend::new(
                t.label.clone(),
                t.width,
                t.height,
                c.clone(),
            ))
        };
        let config = CaptureConfig::default();
        let (id, _, _) = mgr
            .adopt(&target, mock(&target, &config), config.clone(), &gpu.device)
            .expect("adopted");
        // The same window, size and settings: shared.
        let (again, _, _) = mgr
            .adopt(&target, mock(&target, &config), config.clone(), &gpu.device)
            .expect("shared");
        assert_eq!(again, id);
        // Resized, its top cut off anew: the new capture, under that id.
        let mut resized = target.clone();
        resized.width /= 2;
        let cropped = CaptureConfig {
            crop: CropRect {
                x: 0.0,
                y: 0.1,
                w: 1.0,
                h: 0.9,
            },
            ..config
        };
        let (swapped, _, _) = mgr
            .adopt(
                &resized,
                mock(&resized, &cropped),
                cropped.clone(),
                &gpu.device,
            )
            .expect("replaced");
        assert_eq!(swapped, id);
        assert_eq!(
            mgr.config(id).map(|c| c.crop),
            Some(cropped.clone().sanitized().crop)
        );
        // The same window, size and settings from a screen of twice the
        // pixels: its own capture, not the one sized for the other screen.
        let sharp = |t: &CaptureTargetInfo, c: &CaptureConfig| -> Box<dyn ScreenCaptureBackend> {
            Box::new(MockBackend::new(
                t.label.clone(),
                t.width * 2,
                t.height * 2,
                c.clone(),
            ))
        };
        let (retina, _, _) = mgr
            .adopt(
                &resized,
                sharp(&resized, &cropped),
                cropped.clone(),
                &gpu.device,
            )
            .expect("replaced again");
        assert_eq!(retina, id);
        // Its frames come at the new size.
        let want = sharp(&resized, &cropped).resolution();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while mgr.resolution(id) != Some(want) && std::time::Instant::now() < deadline {
            mgr.update(&gpu, f64::MAX);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(mgr.resolution(id), Some(want));
        for _ in 0..4 {
            mgr.release(id);
        }
        assert!(!mgr.is_active(id));
    }

    /// A session that takes a second to close, as macOS's capture can with
    /// the window server busy.
    struct SlowToClose(MockBackend);

    impl ScreenCaptureBackend for SlowToClose {
        fn label(&self) -> &str {
            self.0.label()
        }
        fn resolution(&self) -> (u32, u32) {
            self.0.resolution()
        }
        fn next_frame(&mut self) -> Option<CaptureFrame> {
            self.0.next_frame()
        }
        fn set_config(&mut self, config: &CaptureConfig) -> Result<(), CaptureError> {
            self.0.set_config(config)
        }
    }

    impl Drop for SlowToClose {
        fn drop(&mut self) {
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    /// Letting a capture go — released, replaced, or outliving its decks —
    /// never waits for its session to close: that closes on its own thread.
    #[test]
    fn letting_a_capture_go_never_waits_for_its_session_to_close() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let target = mock_targets().remove(0);
        let config = CaptureConfig::default();
        let slow = |t: &CaptureTargetInfo| -> Box<dyn ScreenCaptureBackend> {
            Box::new(SlowToClose(MockBackend::new(
                t.label.clone(),
                t.width,
                t.height,
                CaptureConfig::default(),
            )))
        };
        let quick = |what: &str, started: std::time::Instant| {
            assert!(
                started.elapsed() < Duration::from_millis(300),
                "{what} waited for the session to close: {:?}",
                started.elapsed()
            );
        };
        // Released.
        let (id, _, _) = mgr.adopt(&target, slow(&target), config.clone(), &gpu.device).unwrap();
        let started = std::time::Instant::now();
        mgr.release(id);
        quick("release", started);
        assert!(!mgr.is_active(id));
        // Replaced: the window at another size.
        let (id, _, _) = mgr.adopt(&target, slow(&target), config.clone(), &gpu.device).unwrap();
        let mut resized = target.clone();
        resized.width /= 2;
        let started = std::time::Instant::now();
        let (again, _, _) = mgr.adopt(&resized, slow(&resized), config.clone(), &gpu.device).unwrap();
        quick("replacing", started);
        assert_eq!(again, id);
        // Outliving its last deck.
        let started = std::time::Instant::now();
        mgr.reconcile_holders(&HashMap::new());
        quick("reconciling", started);
        assert!(!mgr.is_active(id));
    }

    /// A session macOS does not let go of until the test says.
    struct HeldOnClose(MockBackend, Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl ScreenCaptureBackend for HeldOnClose {
        fn label(&self) -> &str {
            self.0.label()
        }
        fn resolution(&self) -> (u32, u32) {
            self.0.resolution()
        }
        fn next_frame(&mut self) -> Option<CaptureFrame> {
            self.0.next_frame()
        }
        fn set_config(&mut self, config: &CaptureConfig) -> Result<(), CaptureError> {
            self.0.set_config(config)
        }
    }

    impl Drop for HeldOnClose {
        fn drop(&mut self) {
            let (held, let_go) = &*self.1;
            let mut held = held.lock().unwrap();
            while *held {
                held = let_go.wait(held).unwrap();
            }
        }
    }

    /// Lets held sessions go when dropped.
    struct LetGo(Arc<(Mutex<bool>, std::sync::Condvar)>);

    impl Drop for LetGo {
        fn drop(&mut self) {
            *self.0 .0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = false;
            self.0 .1.notify_all();
        }
    }

    /// Captures macOS does not let go of still let new ones start — a
    /// talk's window refused would show black — and are kept until they
    /// end, then cleared.
    #[test]
    fn captures_still_ending_never_stop_new_ones() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let target = mock_targets().remove(0);
        let config = CaptureConfig::default();
        let held = Arc::new((Mutex::new(true), std::sync::Condvar::new()));
        // Let go however the test ends, before the manager is dropped (it
        // waits for its captures as Nida quits).
        let _let_go = LetGo(Arc::clone(&held));
        let session = |t: &CaptureTargetInfo| -> Box<dyn ScreenCaptureBackend> {
            Box::new(HeldOnClose(
                MockBackend::new(t.label.clone(), t.width, t.height, CaptureConfig::default()),
                Arc::clone(&held),
            ))
        };
        let many = super::RETIRING_SAID_PAST + 2;
        for _ in 0..many {
            let started = std::time::Instant::now();
            let (id, _, _) = mgr.adopt(&target, session(&target), config.clone(), &gpu.device).unwrap();
            mgr.release(id);
            assert!(started.elapsed() < Duration::from_millis(300), "never waited");
        }
        assert_eq!(mgr.retiring.len(), many, "kept while still ending");
        // macOS lets go: cleared.
        *held.0.lock().unwrap() = false;
        held.1.notify_all();
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while !mgr.retiring.is_empty() {
            assert!(std::time::Instant::now() < until, "the ending captures ended");
            // As the show's frame does, needing none.
            mgr.update_selective(&gpu, &std::collections::HashSet::new(), f64::MAX);
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A capture's thread replaced, still ending, puts no frame in: the one
    /// now running is the capture. (Each thread says it is connected on a
    /// flag of its own.)
    #[test]
    fn a_replaced_capture_thread_puts_nothing_in() {
        let frames = Arc::new(Mutex::new(FramePicker::keeping(PICKER_FRAMES)));
        let config = Arc::new(Mutex::new(CaptureConfig::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let current = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let running = {
            let (frames, config, stop, connected, current) = (
                Arc::clone(&frames),
                Arc::clone(&config),
                Arc::clone(&stop),
                Arc::clone(&connected),
                Arc::clone(&current),
            );
            std::thread::spawn(move || {
                let mut session = MockBackend::new("Old".to_string(), 32, 18, CaptureConfig::default());
                capture_loop(
                    &mut session,
                    7,
                    &frames,
                    CaptureConfig::default(),
                    &config,
                    &stop,
                    (&connected, &current, 0),
                );
            })
        };
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while frames.lock().unwrap().waiting() == 0 {
            assert!(std::time::Instant::now() < until, "frames came");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(connected.load(Ordering::SeqCst));
        // Replaced, as `replace_session` does: the count moved on, the
        // picker emptied.
        current.fetch_add(1, Ordering::SeqCst);
        frames.lock().unwrap().discard_waiting();
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(frames.lock().unwrap().waiting(), 0, "the old thread put nothing in");
        stop.store(true, Ordering::Relaxed);
        running.join().unwrap();
    }

    /// A cap raised while its thread was starting — another input sharing
    /// the capture just then — still reaches the backend: the thread starts
    /// from what the backend was opened with, not from the settings as they
    /// are by the time it runs, which would read the raise as done.
    #[test]
    fn a_cap_raised_as_its_thread_starts_reaches_the_capture() {
        let frames = Arc::new(Mutex::new(FramePicker::new()));
        let opened = CaptureConfig {
            scale_to: Some((32, 18)),
            ..Default::default()
        };
        let raised = CaptureConfig {
            scale_to: Some((64, 36)),
            ..Default::default()
        };
        let config = Arc::new(Mutex::new(raised));
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let current = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let running = {
            let (frames, config, stop, connected, current, opened) = (
                Arc::clone(&frames),
                Arc::clone(&config),
                Arc::clone(&stop),
                Arc::clone(&connected),
                Arc::clone(&current),
                opened.clone(),
            );
            std::thread::spawn(move || {
                let mut session = MockBackend::new("Screen".to_string(), 64, 36, opened.clone());
                capture_loop(
                    &mut session,
                    7,
                    &frames,
                    opened,
                    &config,
                    &stop,
                    (&connected, &current, 0),
                );
            })
        };
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let size = loop {
            let newest = frames.lock().unwrap().newest().map(|f| (f.width, f.height));
            if newest == Some((64, 36)) {
                break newest;
            }
            if std::time::Instant::now() > until {
                break newest;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        stop.store(true, Ordering::Relaxed);
        running.join().unwrap();
        assert_eq!(size, Some((64, 36)), "the raised cap reached the capture");
    }

    /// A window resized while captured is followed: its frames come at its
    /// new size, not squeezed into the one it started at.
    #[test]
    fn a_resized_window_is_captured_at_its_new_size() {
        let frames = Arc::new(Mutex::new(FramePicker::new()));
        let opened = CaptureConfig::default();
        let config = Arc::new(Mutex::new(opened.clone()));
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let current = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let running = {
            let (frames, config, stop, connected, current, opened) = (
                Arc::clone(&frames),
                Arc::clone(&config),
                Arc::clone(&stop),
                Arc::clone(&connected),
                Arc::clone(&current),
                opened.clone(),
            );
            std::thread::spawn(move || {
                let mut session = MockBackend::new("Window".to_string(), 32, 18, opened.clone());
                session.resize_target(64, 36);
                capture_loop(
                    &mut session,
                    7,
                    &frames,
                    opened,
                    &config,
                    &stop,
                    (&connected, &current, 0),
                );
            })
        };
        let until = std::time::Instant::now() + Duration::from_secs(5);
        let size = loop {
            let newest = frames.lock().unwrap().newest().map(|f| (f.width, f.height));
            if newest == Some((64, 36)) || std::time::Instant::now() > until {
                break newest;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        stop.store(true, Ordering::Relaxed);
        running.join().unwrap();
        assert_eq!(size, Some((64, 36)), "the window's new size");
    }

    /// A PowerPoint show's capture and an input's own capture of the same
    /// window are two: capping the input's leaves the show's cap — its own,
    /// the picture its slide is shown at — as it is.
    #[test]
    fn a_presentations_capture_is_never_shared_with_an_inputs() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let target = mock_targets().remove(0);
        let shows = CaptureConfig {
            scale_to: Some((1920, 1146)),
            ..Default::default()
        };
        let session = Box::new(MockBackend::new(
            target.label.clone(),
            target.width,
            target.height,
            shows.clone(),
        ));
        let (show, _, _) = mgr.adopt(&target, session, shows, &gpu.device).expect("adopt");
        let input = CaptureConfig {
            scale_to: Some((1920, 1080)),
            ..Default::default()
        };
        let (own, _, _) = mgr.open_mock(&target, input, &gpu.device).expect("open");
        assert_ne!(own, show, "two captures");
        mgr.cap_to(own, Some((3840, 2160)));
        assert_eq!(mgr.config(show).expect("live").scale_to, Some((1920, 1146)));
        assert_eq!(mgr.config(own).expect("live").scale_to, Some((3840, 2160)));
        mgr.release(own);
        mgr.release(show);
    }

    /// A capture's size follows how big its picture is drawn: bigger at
    /// once — though not twice within `SIZED_EVERY` — and smaller only once
    /// drawn smaller for `SMALLER_AFTER` without a break.
    #[test]
    fn a_capture_is_sized_as_its_picture_is_drawn() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let set = CaptureConfig {
            scale_to: Some((960, 540)),
            show_cursor: true,
            ..Default::default()
        };
        let (id, _, _) = mgr.open_mock(&target, set.clone(), &gpu.device).expect("open");
        assert_eq!(mgr.drawn(id), Some(Drawn::default()), "fitted, as opened");
        let zoomed = |fitted| Drawn {
            fitted,
            ..Drawn::default()
        };
        let start = std::time::Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);

        mgr.size_as_drawn(id, zoomed(2.0), at(0));
        assert_eq!(mgr.drawn(id), Some(zoomed(2.0)), "bigger at once");
        // Bigger again too soon after: held, then taken.
        mgr.size_as_drawn(id, zoomed(3.0), at(499));
        assert_eq!(mgr.drawn(id), Some(zoomed(2.0)), "held");
        mgr.size_as_drawn(id, zoomed(3.0), at(500));
        assert_eq!(mgr.drawn(id), Some(zoomed(3.0)), "taken");
        // Smaller: once drawn so for long enough.
        mgr.size_as_drawn(id, zoomed(1.0), at(1000));
        mgr.size_as_drawn(id, zoomed(1.0), at(2999));
        assert_eq!(mgr.drawn(id), Some(zoomed(3.0)), "not yet");
        // Drawn as big again meanwhile: the wait starts over.
        mgr.size_as_drawn(id, zoomed(3.0), at(2999));
        mgr.size_as_drawn(id, zoomed(1.0), at(3000));
        mgr.size_as_drawn(id, zoomed(1.0), at(4999));
        assert_eq!(mgr.drawn(id), Some(zoomed(3.0)), "the wait started over");
        mgr.size_as_drawn(id, zoomed(1.0), at(5000));
        assert_eq!(mgr.drawn(id), Some(zoomed(1.0)), "smaller");
        // Bigger one way, smaller the other — fitted and zoomed into, then
        // filling its frame: what it needs now, at once.
        mgr.size_as_drawn(id, zoomed(2.0), at(6000));
        let fills = Drawn {
            fitted: 1.0,
            across: 1.0,
            down: 1.0,
        };
        mgr.size_as_drawn(id, fills, at(7000));
        assert_eq!(mgr.drawn(id), Some(fills));
        // Its width alone bigger, then its height alone — stretched, and
        // scaled one way: at once, each.
        let wider = Drawn { across: 2.0, ..fills };
        mgr.size_as_drawn(id, wider, at(7500));
        assert_eq!(mgr.drawn(id), Some(wider), "wider at once");
        let taller = Drawn {
            fitted: 1.0,
            across: 0.5,
            down: 3.0,
        };
        mgr.size_as_drawn(id, taller, at(8000));
        assert_eq!(mgr.drawn(id), Some(taller), "taller at once");
        // A number no size comes of is none.
        mgr.size_as_drawn(
            id,
            Drawn {
                fitted: f32::NAN,
                across: 4.0,
                down: 4.0,
            },
            at(8500),
        );
        let filled_twice = Drawn {
            fitted: 0.0,
            across: 4.0,
            down: 4.0,
        };
        assert_eq!(mgr.drawn(id), Some(filled_twice));

        // A capture that is not there: nothing.
        mgr.size_as_drawn(id + 1, zoomed(2.0), at(9000));
        assert_eq!(mgr.drawn(id + 1), None);
        mgr.release(id);
    }

    /// A capture's settings, as its inputs have them, say nothing of how
    /// big it is drawn: they are as they were set, however it is sized.
    #[test]
    fn a_captures_settings_say_nothing_of_how_big_it_is_drawn() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let set = CaptureConfig {
            scale_to: Some((960, 540)),
            show_cursor: true,
            ..Default::default()
        };
        let (id, _, _) = mgr.open_mock(&target, set.clone(), &gpu.device).expect("open");
        assert_eq!(mgr.config(id), Some(set.clone()), "as set");
        let twice = Drawn {
            fitted: 2.0,
            across: 1.5,
            down: 3.0,
        };
        mgr.size_as_drawn(id, twice, std::time::Instant::now());
        assert_eq!(mgr.drawn(id), Some(twice), "sized");
        assert_eq!(mgr.config(id), Some(set.clone()), "its settings as they were");
        mgr.cap_to(id, Some((1920, 1080)));
        assert_eq!(
            mgr.config(id),
            Some(CaptureConfig {
                scale_to: Some((1920, 1080)),
                ..set
            }),
            "capped anew, and no more said"
        );
        mgr.release(id);
    }

    /// Settings given a capture — an input's, a show's as saved — leave it
    /// sized as it is drawn, whatever they say of that themselves.
    #[test]
    fn settings_given_a_capture_keep_how_big_it_is_drawn() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let set = CaptureConfig {
            scale_to: Some((960, 540)),
            ..Default::default()
        };
        let (id, _, _) = mgr.open_mock(&target, set.clone(), &gpu.device).expect("open");
        let twice = Drawn {
            fitted: 2.0,
            across: 1.5,
            down: 3.0,
        };
        mgr.size_as_drawn(id, twice, std::time::Instant::now());
        // An input's settings: they say nothing of it.
        let given = CaptureConfig {
            rate: 24.0,
            ..set.clone()
        };
        mgr.set_config(id, given.clone());
        assert_eq!(mgr.drawn(id), Some(twice), "sized as it was");
        assert_eq!(mgr.config(id), Some(given.clone()), "the settings taken");
        // And settings that do say: still as it is drawn.
        mgr.set_config(
            id,
            CaptureConfig {
                drawn: Drawn {
                    fitted: 5.0,
                    ..Drawn::default()
                },
                ..given.clone()
            },
        );
        assert_eq!(mgr.drawn(id), Some(twice), "not as they said");
        mgr.cap_to(id, Some((1920, 1080)));
        assert_eq!(mgr.drawn(id), Some(twice), "capped anew, sized as it was");
        mgr.release(id);
    }

    /// Sized as it is drawn, a capture's frames — and the texture its inputs
    /// show — come so.
    #[test]
    fn a_capture_sized_as_drawn_comes_at_that_size() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0); // 1920x1080
        let fitted = CaptureConfig {
            scale_to: Some((960, 540)),
            ..Default::default()
        };
        let (id, w, h) = mgr.open_mock(&target, fitted, &gpu.device).expect("open");
        assert_eq!((w, h), (960, 540));
        let zoomed = Drawn {
            fitted: 1.5,
            ..Drawn::default()
        };
        mgr.size_as_drawn(id, zoomed, std::time::Instant::now());
        let needed = std::collections::HashSet::from([id]);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while mgr.resolution(id) != Some((1440, 810)) && std::time::Instant::now() < until {
            mgr.update_selective(&gpu, &needed, crate::host_clock::host_seconds());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(mgr.resolution(id), Some((1440, 810)), "as it is drawn");
        mgr.release(id);
    }

    /// Given room, a capture is asked for so many times the size it is
    /// drawn at and comes so — never bigger than its screen is — for the
    /// show to make smaller itself. With none (a test's manager), as drawn.
    #[test]
    fn a_capture_given_room_comes_bigger_than_it_is_drawn() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0); // 1920x1080
        let fitted = CaptureConfig {
            scale_to: Some((640, 360)),
            ..Default::default()
        };
        let (id, w, h) = mgr.open_mock(&target, fitted, &gpu.device).expect("open");
        assert_eq!((w, h), (640, 360));
        let zoomed = |fitted| Drawn {
            fitted,
            ..Drawn::default()
        };
        let start = std::time::Instant::now();
        let needed = std::collections::HashSet::from([id]);
        let comes = |mgr: &mut ScreenCaptureManager, size: (u32, u32)| {
            let until = std::time::Instant::now() + Duration::from_secs(5);
            while mgr.resolution(id) != Some(size) && std::time::Instant::now() < until {
                mgr.update_selective(&gpu, &needed, crate::host_clock::host_seconds());
                std::thread::sleep(Duration::from_millis(5));
            }
            mgr.resolution(id)
        };

        // No room: sized as it is drawn, and so it stays.
        mgr.size_as_drawn(id, zoomed(1.0), start);
        assert_eq!(mgr.drawn(id), Some(zoomed(1.0)), "as drawn");

        // Room for twice: fitted, it comes twice the size that fits.
        mgr.give_room(2.0);
        mgr.size_as_drawn(id, zoomed(1.0), start + Duration::from_secs(1));
        assert_eq!(mgr.drawn(id), Some(zoomed(2.0)), "twice what it is drawn at");
        assert_eq!(comes(&mut mgr, (1280, 720)), Some((1280, 720)));

        // Zoomed into twice over: four times fitted is more than the
        // screen has.
        mgr.size_as_drawn(id, zoomed(2.0), start + Duration::from_secs(2));
        assert_eq!(mgr.drawn(id), Some(zoomed(4.0)));
        assert_eq!(comes(&mut mgr, (1920, 1080)), Some((1920, 1080)), "all it has");
        mgr.release(id);
    }

    /// The room a capture is given, as `NIDA_CAPTURE_ROOM` says it: from 1
    /// to 4 times, twice when it says nothing or no such number.
    #[test]
    fn the_room_a_capture_is_given_is_as_said_or_twice() {
        assert_eq!(room_of(None), 2.0);
        assert_eq!(room_of(Some("1")), 1.0, "none: as it is drawn");
        assert_eq!(room_of(Some(" 1.5 ")), 1.5);
        assert_eq!(room_of(Some("4")), 4.0);
        for none in ["", "0", "0.99", "4.5", "-2", "nan", "inf", "twice"] {
            assert_eq!(room_of(Some(none)), ROOM, "{none:?}");
        }
    }

    /// Captures are drawn as coded but where `NIDA_CAPTURES_CODED` says 0.
    #[test]
    fn captures_are_drawn_as_coded_unless_told_0() {
        assert!(coded_of(None));
        assert!(coded_of(Some("1")));
        assert!(coded_of(Some("")));
        assert!(coded_of(Some("00")));
        assert!(!coded_of(Some("0")));
        assert!(!coded_of(Some(" 0 ")));
    }

    /// A capture's texture has a view that reads it as it was coded — the
    /// same pixels, in the format that does not make them light — for as
    /// long as it is captured, of the texture it has now.
    #[test]
    fn a_capture_has_a_view_of_it_as_coded() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        if !gpu.reads_a_texture_two_ways() {
            eprintln!("Skipping: this GPU reads a texture one way");
            return;
        }
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0); // 1920x1080
        let fitted = CaptureConfig {
            scale_to: Some((960, 540)),
            ..Default::default()
        };
        let (id, _, _) = mgr.open_mock(&target, fitted, &gpu.device).expect("open");
        assert!(mgr.coded_view(id + 1).is_none(), "no such capture");
        let sized = |mgr: &ScreenCaptureManager| {
            let light = mgr.texture_view(id).expect("its view");
            let coded = mgr.coded_view(id).expect("its view as coded");
            assert_ne!(coded, light, "another view");
            assert_eq!(coded.texture(), light.texture(), "of one texture");
            assert!(light.texture().format().is_srgb(), "light of itself");
            (coded.texture().width(), coded.texture().height())
        };
        assert_eq!(sized(&mgr), (960, 540));

        // Come at another size: a view of the texture it has now.
        let bigger = Drawn {
            fitted: 1.5,
            ..Drawn::default()
        };
        mgr.size_as_drawn(id, bigger, std::time::Instant::now());
        let needed = std::collections::HashSet::from([id]);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while mgr.resolution(id) != Some((1440, 810)) && std::time::Instant::now() < until {
            mgr.update_selective(&gpu, &needed, crate::host_clock::host_seconds());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sized(&mgr), (1440, 810), "of the texture it has now");
        mgr.release(id);
        assert!(mgr.coded_view(id).is_none(), "released");
    }

    /// Where captures are drawn from their light, one opened has its view
    /// alone: its texture is asked for read one way.
    #[test]
    fn a_capture_drawn_from_its_light_opens_with_no_view_as_coded() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        mgr.draw_coded(false);
        let target = mock_targets().remove(0);
        let (id, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("open");
        assert!(mgr.texture_view(id).is_some(), "its view");
        assert!(mgr.coded_view(id).is_none(), "drawn from its light");
        mgr.release(id);
    }

    /// ...and one running has none from its next size on.
    #[test]
    fn a_capture_drawn_from_its_light_resized_has_no_view_as_coded() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        if !gpu.reads_a_texture_two_ways() {
            eprintln!("Skipping: this GPU reads a texture one way");
            return;
        }
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0); // 1920x1080
        let fitted = CaptureConfig {
            scale_to: Some((960, 540)),
            ..Default::default()
        };
        let (id, _, _) = mgr.open_mock(&target, fitted, &gpu.device).expect("open");
        assert!(mgr.coded_view(id).is_some(), "as coded");

        mgr.draw_coded(false);
        let bigger = Drawn {
            fitted: 1.5,
            ..Drawn::default()
        };
        mgr.size_as_drawn(id, bigger, std::time::Instant::now());
        let needed = std::collections::HashSet::from([id]);
        let until = std::time::Instant::now() + Duration::from_secs(5);
        while mgr.resolution(id) != Some((1440, 810)) && std::time::Instant::now() < until {
            mgr.update_selective(&gpu, &needed, crate::host_clock::host_seconds());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(mgr.resolution(id), Some((1440, 810)), "its next size");
        assert_eq!(
            mgr.texture_view(id).map(|view| view.texture().width()),
            Some(1440),
            "its view"
        );
        assert!(mgr.coded_view(id).is_none(), "drawn from its light");
        mgr.release(id);
    }

    /// A GPU that reads a texture in its own format alone has captures
    /// drawn from their light; one that reads it in another too, as coded
    /// — where nothing said otherwise.
    #[test]
    fn a_gpu_reading_a_texture_one_way_has_captures_drawn_from_their_light() {
        use wgpu::DownlevelFlags;
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let target = mock_targets().remove(0);
        // Whether a capture opened has its view, and its view as coded.
        let views = |mut mgr: ScreenCaptureManager| {
            let (id, _, _) = mgr
                .open_mock(&target, CaptureConfig::default(), &gpu.device)
                .expect("open");
            let has = (mgr.texture_view(id).is_some(), mgr.coded_view(id).is_some());
            mgr.release(id);
            has
        };
        let on = |flags| ScreenCaptureManager::new_disabled().on_gpu(flags);
        // (Asked of a GPU that does: one that does not makes no such
        // texture, whatever it is said to do.)
        if gpu.reads_a_texture_two_ways() {
            assert_eq!(views(on(DownlevelFlags::all())), (true, true));
            assert_eq!(views(on(DownlevelFlags::VIEW_FORMATS)), (true, true));
        }
        let one_way = DownlevelFlags::all().difference(DownlevelFlags::VIEW_FORMATS);
        assert_eq!(views(on(one_way)), (true, false), "from its light");
        assert_eq!(views(on(DownlevelFlags::empty())), (true, false));
        // Told to draw them from their light, a GPU that could does not.
        let mut told = ScreenCaptureManager::new_disabled();
        told.draw_coded(false);
        assert_eq!(views(told.on_gpu(DownlevelFlags::all())), (true, false));
    }

    /// A presentation's capture is left as its presentation has it, however
    /// big its slide is drawn.
    #[test]
    fn a_presentations_capture_is_not_sized_as_drawn() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::enabled_for_tests();
        let target = mock_targets().remove(0);
        let shows = CaptureConfig {
            scale_to: Some((960, 540)),
            ..Default::default()
        };
        let session = Box::new(MockBackend::new(
            target.label.clone(),
            target.width,
            target.height,
            shows.clone(),
        ));
        let (show, _, _) = mgr.adopt(&target, session, shows, &gpu.device).expect("adopt");
        let (own, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("open");
        let bigger = Drawn {
            fitted: 2.0,
            across: 2.0,
            down: 2.0,
        };
        let now = std::time::Instant::now();
        mgr.size_as_drawn(show, bigger, now);
        mgr.size_as_drawn(own, bigger, now);
        assert_eq!(mgr.drawn(show), Some(Drawn::default()), "its presentation's");
        assert_eq!(mgr.drawn(own), Some(bigger), "an input's own of that window");
        mgr.release(own);
        mgr.release(show);
    }

    #[test]
    fn set_config_clamps_through_the_live_session() {
        let Ok(gpu) = crate::renderer::GpuContext::new_headless() else {
            return;
        };
        let mut mgr = ScreenCaptureManager::new_disabled();
        let target = mock_targets().remove(0);
        let (id, _, _) = mgr
            .open_mock(&target, CaptureConfig::default(), &gpu.device)
            .expect("mock open");

        mgr.update_config(id, |c| c.rate = 10_000.0);
        let rate = mgr.config(id).expect("config").rate;
        assert!(
            (rate - super::backend::MAX_CAPTURE_RATE).abs() < f32::EPSILON,
            "router-supplied rate must be clamped, got {rate}"
        );

        mgr.update_config(id, |c| {
            c.crop = CropRect {
                x: 0.9,
                y: 0.0,
                w: 0.5,
                h: 1.0,
            };
        });
        let crop = mgr.config(id).expect("config").crop;
        assert!(crop.x + crop.w <= 1.0 + f32::EPSILON);
        mgr.release(id);
    }
}
