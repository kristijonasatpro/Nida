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
    CaptureTargetKind, MockBackend, PermissionState, ScreenCaptureBackend, TargetIdentity,
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
    thread: Option<std::thread::JoinHandle<()>>,
    /// Frames put in its texture: none yet, and its texture shows nothing.
    uploaded: u64,
    /// Of those, the ones from the capture running now — none yet after it
    /// replaced another, though the texture still shows that one's last.
    fresh: u64,
    /// When the last frame was put in its texture: macOS sends a capture's
    /// frames only as what it shows changes, so a still slide sends none.
    last_picture: Option<std::time::Instant>,
}

/// Manages capture-target enumeration, capture sessions, and shared textures.
pub struct ScreenCaptureManager {
    targets: Vec<CaptureTargetInfo>,
    active: HashMap<CaptureId, ActiveCapture>,
    next_id: CaptureId,
    permission: PermissionState,
    disabled: bool,
}

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
        }
    }

    /// Enabled, as `new`, without asking the platform for permission or
    /// targets.
    #[cfg(test)]
    fn enabled_for_tests() -> Self {
        Self {
            targets: Vec::new(),
            active: HashMap::new(),
            next_id: 0,
            permission: PermissionState::NotRequired,
            disabled: false,
        }
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
    pub fn find_target(&self, identity: &TargetIdentity) -> Option<&CaptureTargetInfo> {
        if let Some(exact) = self.targets.iter().find(|t| &t.identity() == identity) {
            return Some(exact);
        }
        match identity {
            TargetIdentity::Display { .. } => None,
            TargetIdentity::Window { app, .. } => {
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

        if let Some(shared) = self.share(target) {
            return Ok(shared);
        }

        let config = config.sanitized();
        let session = platform::open(target, &config)?;
        self.start_session(target, session, config, device)
    }

    /// The session already capturing `target` — the same window, not only
    /// one of its app and title — with one more holder.
    fn share(&mut self, target: &CaptureTargetInfo) -> Option<(CaptureId, u32, u32)> {
        let identity = target.identity();
        let (id, active) = self
            .active
            .iter_mut()
            .find(|(_, a)| a.identity == identity && a.platform_id == target.platform_id)?;
        active.ref_count += 1;
        Some((*id, active.width, active.height))
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
            .find(|(_, a)| a.identity == identity && a.platform_id == target.platform_id)
            .map(|(id, a)| {
                let same = a.target_size == (target.width, target.height)
                    && a.backend_size == session.resolution()
                    && a.config.lock().is_ok_and(|c| *c == config);
                (*id, same)
            });
        match running {
            Some((_, true)) if share => {
                if let Some(shared) = self.share(target) {
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
        self.start_session(target, session, config, device)
    }

    /// Capture `id` from `session` from now on, set to `config`: its thread
    /// stopped and one started for `session`, the frames going where they
    /// went. Its texture follows the new frames' size (see `upload_frame`).
    fn replace_session(
        &mut self,
        id: CaptureId,
        target: &CaptureTargetInfo,
        mut session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
        let backend_size = session.resolution();
        let Some(active) = self.active.get_mut(&id) else {
            return Err(CaptureError::Backend(format!("capture {id} is gone")));
        };
        active.stop_flag.store(true, Ordering::Relaxed);
        if let Some(thread) = active.thread.take() {
            let _ = thread.join();
        }
        if let Ok(mut guard) = active.config.lock() {
            *guard = config;
        }
        // What the one replaced sent and is not yet shown is not this one's.
        if let Ok(mut picker) = active.frame_data.lock() {
            picker.discard_waiting();
        }
        active.fresh = 0;
        let stop_flag = Arc::new(AtomicBool::new(false));
        let frame_tx = Arc::clone(&active.frame_data);
        let config_rx = Arc::clone(&active.config);
        let stop_rx = Arc::clone(&stop_flag);
        let connected_tx = Arc::clone(&active.connected);
        let spawned = std::thread::Builder::new()
            .name(format!("screen-capture-{id}"))
            .spawn(move || {
                capture_loop(
                    session.as_mut(),
                    id,
                    &frame_tx,
                    &config_rx,
                    &stop_rx,
                    &connected_tx,
                );
            });
        let thread = match spawned {
            Ok(thread) => thread,
            Err(e) => {
                // Its thread stopped and none to follow: gone, not shared.
                self.active.remove(&id);
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
        Ok((id, active.width, active.height))
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
        if let Some(shared) = self.share(target) {
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
        mut session: Box<dyn ScreenCaptureBackend>,
        config: CaptureConfig,
        device: &wgpu::Device,
    ) -> Result<(CaptureId, u32, u32), CaptureError> {
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

        let (texture, texture_view) =
            Self::create_texture(device, id, &target.label, width, height, format);

        let mut picker = FramePicker::keeping(PICKER_FRAMES);
        if let Some(first) = first {
            let let_go = picker.push(frame_time(&first), first);
            drop(let_go);
        }
        let frame_data = Arc::new(Mutex::new(picker));
        let config = Arc::new(Mutex::new(config));
        let stop_flag = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));

        let frame_tx = Arc::clone(&frame_data);
        let config_rx = Arc::clone(&config);
        let stop_rx = Arc::clone(&stop_flag);
        let connected_tx = Arc::clone(&connected);

        let thread = std::thread::Builder::new()
            .name(format!("screen-capture-{id}"))
            .spawn(move || {
                capture_loop(
                    session.as_mut(),
                    id,
                    &frame_tx,
                    &config_rx,
                    &stop_rx,
                    &connected_tx,
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
                width,
                height,
                format,
                ref_count: 1,
                frame_data,
                config,
                stop_flag,
                connected,
                thread: Some(thread),
                uploaded: 0,
                fresh: 0,
                last_picture: None,
            },
        );

        Ok((id, width, height))
    }

    fn create_texture(
        device: &wgpu::Device,
        id: CaptureId,
        label: &str,
        width: u32,
        height: u32,
        format: CapturePixelFormat,
    ) -> (wgpu::Texture, wgpu::TextureView) {
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
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
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
        removed.stop_flag.store(true, Ordering::Relaxed);
        if let Some(t) = removed.thread.take() {
            let _ = t.join();
        }
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
            removed.stop_flag.store(true, Ordering::Relaxed);
            if let Some(t) = removed.thread.take() {
                let _ = t.join();
            }
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

    pub fn config(&self, id: CaptureId) -> Option<CaptureConfig> {
        let active = self.active.get(&id)?;
        active.config.lock().ok().map(|c| c.clone())
    }

    /// Update a live capture's config. The capture thread picks it up on its
    /// next tick; no session restart.
    pub fn set_config(&mut self, id: CaptureId, config: CaptureConfig) {
        let Some(active) = self.active.get_mut(&id) else {
            return;
        };
        if let Ok(mut guard) = active.config.lock() {
            *guard = config.sanitized();
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
        for (id, active) in &mut self.active {
            if needed.contains(id) {
                Self::upload_frame(*id, active, context, show_at);
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
        for (id, active) in &mut self.active {
            Self::upload_frame(*id, active, context, show_at);
        }
    }

    fn upload_frame(id: CaptureId, active: &mut ActiveCapture, context: &GpuContext, show_at: f64) {
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
            let (texture, view) =
                Self::create_texture(device, id, &active.label, w, h, frame.format);
            active.texture = texture;
            active.texture_view = view;
            active.width = w;
            active.height = h;
            active.format = frame.format;
        }

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
fn capture_loop(
    session: &mut dyn ScreenCaptureBackend,
    id: CaptureId,
    frame_data: &Mutex<FramePicker<CaptureFrame>>,
    config: &Mutex<CaptureConfig>,
    stop: &AtomicBool,
    connected: &AtomicBool,
) {
    let self_paced = session.is_self_paced();
    let mut applied = config.lock().ok().map(|c| c.clone()).unwrap_or_default();
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

        // Every frame come, each with when it was captured: the render
        // thread picks the one for each show frame. A self-paced backend may
        // have several waiting; a polled one makes one a call.
        while let Some(frame) = session.next_frame() {
            connected.store(true, Ordering::SeqCst);
            // Frames let go give their surfaces back to the capture here,
            // the picker no longer held: the show's frame waits for it.
            let let_go = frame_data
                .lock()
                .map(|mut picker| picker.push(frame_time(&frame), frame))
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

#[cfg(test)]
mod tests {
    use super::backend::{
        mock_targets, CaptureConfig, CaptureError, CaptureTargetInfo, CropRect, MockBackend,
        ScreenCaptureBackend, TargetIdentity,
    };
    use super::{poll_interval, ScreenCaptureManager, SELF_PACED_MAX_POLL, SELF_PACED_MIN_POLL};
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
        };
        assert!(mgr.find_target(&missing).is_none());
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
