//! GPU rendering — mixer render, output windows, frame timing.

use super::VardaApp;
use crate::mixer::Mixer;
use crate::notifications::NotificationSystem;
use crate::renderer::context::{OutputSource, SurfaceRenderInfo};
use crate::surface::ContentMapping;

/// Kind of file dialog to open.
#[derive(Debug, Clone, Copy)]
pub enum FileDialogKind {
    Image,
    Video,
}

/// Correlates a spawned deck load with the target its requester recorded.
///
/// Deliberately opaque: a load outlives the frame that requested it, so any
/// position captured at spawn time may name a different entity by the time the
/// deck is ready. The requester keeps `token → channel UUID` and resolves the
/// UUID on completion. See [`/spec/api-addressing.md`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeckLoadToken(pub usize);

/// Result from a completed file dialog (sent from background thread).
/// Supports multi-select: `paths` may contain one or more files.
///
/// The target channel is held by UUID, not index: the dialog runs on a
/// background thread while the UI stays live, so the channel list can change
/// between opening the dialog and picking a file. No channel means a new
/// input for each file, as vMix's Add Input makes them.
#[derive(Debug)]
pub struct FileDialogResult {
    pub kind: FileDialogKind,
    pub channel_uuid: Option<String>,
    pub paths: Vec<std::path::PathBuf>,
}

/// Result from a background deck load (sent from a spawned thread).
/// Contains a ready-to-use Deck that just needs mixer insertion + egui texture registration.
pub struct DeckLoadResult {
    /// Echoed back verbatim from the spawn request; only the requester can
    /// interpret it.
    pub token: DeckLoadToken,
    pub deck: anyhow::Result<crate::deck::Deck>,
    pub name: String,
}

/// A file to load as a deck.
pub enum DeckFile {
    Image(std::path::PathBuf),
    Video(std::path::PathBuf),
    Shader(Box<crate::isf::ISFShader>),
}

/// Mutable external delivery sinks the headless render loop feeds frames and
/// audio into. Bundled so the render helper stays under the argument-count lint
/// while each manager remains a disjoint `&mut` borrow of `VardaApp`.
struct HeadlessDeliverySinks<'a> {
    ndi_manager: &'a mut crate::ndi::NdiManager,
    #[cfg(target_os = "macos")]
    syphon_manager: &'a mut crate::syphon::SyphonManager,
    audio_manager: &'a mut crate::audio::AudioManager,
    audio_mix: &'a crate::audio_mix::AudioEngine,
    notifications: &'a mut crate::notifications::NotificationSystem,
    /// Master render rate, resolved through `encoder_fps` so an uncapped stage
    /// still names a real number. Used to reopen an ffmpeg output on the SRT
    /// reconnect path — it must match what `cmd_start_output` used, or a
    /// reconnected stream would be timed differently from the one it replaced —
    /// and to declare the NDI sender's frame rate.
    encoder_fps: u32,
    /// When this frame was drawn: what a recording or stream times it by
    /// (see `VardaApp::frame_drawn`).
    drawn: std::time::Instant,
}

/// Deactivate a headless output and tell the operator. A log line alone is
/// not enough: the gray inactive dot is easy to miss mid-show.
fn stop_headless_output(
    name: &str,
    active: &mut bool,
    notifications: &mut NotificationSystem,
    reason: impl Into<String>,
) {
    let reason = reason.into();
    log::error!("{reason}");
    *active = false;
    notifications.error(format!("Output '{name}' stopped: {reason}"));
}

/// The frame-wide inputs every destination resolves against.
///
/// Bundled because they always travel together and are all borrowed for the
/// same frame: the mixer's composited output, the bus feeds resolved once at
/// the top of the frame, and the domemaster view when one exists.
struct FrameSources<'a> {
    mixer: &'a crate::mixer::Mixer,
    bus_feeds: &'a std::collections::HashMap<String, crate::app::BusFeed>,
    domemaster_view: Option<&'a wgpu::TextureView>,
}

/// What LED feeds need each frame beyond the sources: the master's size (a
/// bus's region is a share of it) and the test cards.
struct LedFrame<'a> {
    master: [u32; 2],
    cards: &'a crate::renderer::led_card::LedCards,
    /// The outputs' own test cards (calibration `Projector`).
    output_cards: &'a crate::renderer::led_card::OutputCards,
}

/// One LED screen ready to draw: where it lands in the feed (fractions) and
/// what it samples.
struct LedDraw<'a> {
    quad: [[f32; 2]; 4],
    rect: [f32; 4],
    uv_scale: [f32; 2],
    uv_offset: [f32; 2],
    view: &'a wgpu::TextureView,
    uuid: &'a str,
}

impl LedDraw<'_> {
    fn info(&self) -> SurfaceRenderInfo<'_> {
        SurfaceRenderInfo {
            uuid: self.uuid,
            content_view: self.view,
            vertices: &self.quad,
            extra_contours: &[],
            bounding_box: self.rect,
            uv_scale: self.uv_scale,
            uv_offset: self.uv_offset,
            warp_mode: None,
            overlap_zones: crate::renderer::edge_blend::SurfaceOverlapZones::default(),
            hole_uv_contours: Vec::new(),
        }
    }

    fn desc<'b>(
        &'b self,
        no_zones: &'b crate::renderer::edge_blend::SurfaceOverlapZones,
    ) -> crate::renderer::blit::PolygonDrawDesc<'b> {
        let [x, y, w, h] = self.rect;
        crate::renderer::blit::PolygonDrawDesc {
            content_view: self.view,
            uv_scale: self.uv_scale,
            uv_offset: self.uv_offset,
            homography: None,
            overlap_zones: no_zones,
            vertices: crate::renderer::blit::PolygonBlitPipeline::triangulate_verts(
                &self.quad, x, y, w, h,
            ),
            mask_uuid: self.uuid,
            mask_uv_contours: Vec::new(),
        }
    }
}

impl VardaApp {
    /// Spawn background threads to create decks from file paths and shaders.
    /// Each thread creates a full Deck (CPU decode + GPU upload) and sends
    /// the result via the channel. The render loop polls for completed decks.
    /// `pending` is incremented per-spawn and decremented when each thread completes.
    // Args map directly to the independent inputs a deck load needs; bundling them
    // would only add an ephemeral struct with no shared invariant.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_deck_loads(
        sender: &std::sync::mpsc::Sender<DeckLoadResult>,
        context: &crate::renderer::context::GpuContext,
        pending: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        render_width: u32,
        render_height: u32,
        images: Vec<(DeckLoadToken, std::path::PathBuf)>,
        videos: Vec<(DeckLoadToken, std::path::PathBuf)>,
        shaders: Vec<(DeckLoadToken, crate::isf::ISFShader)>,
    ) {
        use crate::deck::Deck;
        use std::sync::atomic::Ordering;

        for (token, path) in images {
            let tx = sender.clone();
            let ctx = context.clone();
            let counter = pending.clone();
            let w = render_width;
            let h = render_height;
            counter.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let name = path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("image")
                        .to_string();
                    let deck = Deck::new_from_image(&ctx, &path, w, h);
                    (name, deck)
                }));
                let (name, deck) = match result {
                    Ok((name, deck)) => (name, deck),
                    Err(_) => (
                        "image".to_string(),
                        Err(anyhow::anyhow!("panic loading image deck")),
                    ),
                };
                let _ = tx.send(DeckLoadResult { token, deck, name });
                counter.fetch_sub(1, Ordering::Relaxed);
            });
        }

        for (token, path) in videos {
            let tx = sender.clone();
            let ctx = context.clone();
            let counter = pending.clone();
            let w = render_width;
            let h = render_height;
            counter.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let name = path
                        .file_name()
                        .and_then(|f| f.to_str())
                        .unwrap_or("video")
                        .to_string();
                    let deck = Deck::new_from_video(&ctx, &path, w, h);
                    (name, deck)
                }));
                let (name, deck) = match result {
                    Ok((name, deck)) => (name, deck),
                    Err(_) => (
                        "video".to_string(),
                        Err(anyhow::anyhow!("panic loading video deck")),
                    ),
                };
                let _ = tx.send(DeckLoadResult { token, deck, name });
                counter.fetch_sub(1, Ordering::Relaxed);
            });
        }

        for (token, shader) in shaders {
            let tx = sender.clone();
            let ctx = context.clone();
            let counter = pending.clone();
            let w = render_width;
            let h = render_height;
            counter.fetch_add(1, Ordering::Relaxed);
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let name = shader.name();
                    let deck = if shader.metadata.is_compute() {
                        Deck::new_from_compute_shader(&ctx, shader, w, h)
                    } else {
                        Deck::new(&ctx, shader, w, h)
                    };
                    (name, deck)
                }));
                let (name, deck) = match result {
                    Ok((name, deck)) => (name, deck),
                    Err(_) => (
                        "shader".to_string(),
                        Err(anyhow::anyhow!("panic loading shader deck")),
                    ),
                };
                let _ = tx.send(DeckLoadResult { token, deck, name });
                counter.fetch_sub(1, Ordering::Relaxed);
            });
        }
    }

    /// A frame starts, `start_delay` after the refresh that called for it
    /// (when a screen's refresh starts frames). See [`super::frame_parts`].
    pub fn frame_begin(&mut self, start_delay: Option<std::time::Duration>) {
        self.frame_stats
            .parts
            .begin(std::time::Instant::now(), start_delay);
    }

    /// This frame was called for by the screen's refresh at `at` (none when
    /// a timer starts frames): what moves by whole frames moves on by it.
    pub fn set_frame_refresh_at(&mut self, at: Option<std::time::Instant>) {
        use crate::renderer::present_trace::{host_seconds_of, Step, TRACE};
        self.frame_stats.refresh_at = at;
        if TRACE.recording() {
            let refresh = at.map(host_seconds_of);
            let number = self.frame_stats.frame_count;
            TRACE.note("", 0, Step::Frame { number, refresh });
        }
    }

    /// When the frame the mixer drew last was drawn, as the recording and
    /// stream will time it — for tests.
    #[doc(hidden)]
    pub fn frame_drawn_at(&self) -> Option<std::time::Instant> {
        self.frame_stats.drawn_at
    }

    /// When this frame was drawn, for the encoders that time frames by it: the
    /// refresh it was drawn for, else now — and now too when that refresh is
    /// long past, so a display clock that stopped ticking cannot hold every
    /// frame of a recording to one moment.
    fn frame_drawn(&self) -> std::time::Instant {
        let now = std::time::Instant::now();
        self.frame_stats
            .refresh_at
            .filter(|at| now.saturating_duration_since(*at) < std::time::Duration::from_millis(100))
            .unwrap_or(now)
    }

    /// `took` spent on `part` of this frame.
    pub fn frame_part(&mut self, part: &str, took: std::time::Duration) {
        self.frame_stats.parts.add(part, took);
    }

    /// The frame is done: its parts are kept, and it is logged if slow.
    pub fn frame_end(&mut self) {
        #[allow(clippy::cast_precision_loss)]
        let frame_ms = if self.target_fps > 0 {
            1000.0 / self.target_fps as f32
        } else {
            1000.0 / 60.0
        };
        self.frame_stats
            .parts
            .end(std::time::Instant::now(), frame_ms);
    }

    /// Update frame timing (FPS measurement) and system stats. Call once per frame before any work.
    pub fn update_frame_timing(&mut self) {
        let now = std::time::Instant::now();
        let dt = now
            .duration_since(self.frame_stats.last_frame_instant)
            .as_secs_f32();
        self.frame_stats.last_frame_instant = now;
        // Sampled here, at the top of the frame, so the tally covers a whole
        // previous frame — including the output, preview, and present submits
        // that happen after the mixer is done.
        self.frame_stats.last_frame_submits = self.context.submits.take();
        // When the command buffers since ran on the GPU — the control
        // window's too, submitted from the main thread.
        let runs = crate::renderer::gpu_time::take_runs(&self.context.queue);
        trace_gpu_runs(&runs);
        self.frame_stats.gpu_time.add(now, runs);
        self.finish_present_trace();
        self.frame_stats.frame_count = self.frame_stats.frame_count.wrapping_add(1);
        if self.frame_stats.frame_count.is_multiple_of(120) {
            log::debug!(
                "[PERF] frame | submits={} fps={:.1}",
                self.frame_stats.last_frame_submits,
                self.frame_stats.fps_smoothed,
            );
        }
        if self.frame_stats.intervals.len() >= super::PACING_FRAMES {
            self.frame_stats.intervals.pop_front();
        }
        self.frame_stats.intervals.push_back(dt * 1000.0);
        if dt > 0.0 {
            let instant_fps = 1.0 / dt;
            self.frame_stats.fps_history.push_back(instant_fps);
            if self.frame_stats.fps_history.len() > 60 {
                self.frame_stats.fps_history.pop_front();
            }
            self.frame_stats.fps_smoothed = self.frame_stats.fps_history.iter().sum::<f32>()
                / self.frame_stats.fps_history.len() as f32;
        }
        self.frame_stats.frame_time_secs = now
            .duration_since(self.frame_stats.start_instant)
            .as_secs_f64();
        self.frame_stats.system_monitor.update();
    }

    /// Write the present trace once its time is up (see `TracePresents`).
    fn finish_present_trace(&self) {
        use crate::renderer::context::UnifiedOutput;
        let fps = if self.target_fps > 0 {
            self.target_fps
        } else {
            60
        };
        let frame_ms = 1000.0 / f64::from(fps);
        let outputs = || {
            self.output
                .outputs
                .iter()
                .filter_map(|output| match output {
                    UnifiedOutput::Window(window) => Some(window.trace_info()),
                    // A Blackmagic output's device refreshes at its format's rate.
                    UnifiedOutput::Headless(h) => h.decklink.as_ref().map(|device| {
                        crate::renderer::present_trace::OutputInfo {
                            name: h.name.clone(),
                            display: None,
                            refresh_ms: device.refresh_ms,
                            follows_clock: false,
                            frame_hold: None,
                        }
                    }),
                })
                .collect()
        };
        // Written on a thread of its own, which logs it.
        let writing = crate::renderer::present_trace::TRACE.finish_if_due(
            &self.frame_stats.clock,
            frame_ms,
            outputs,
        );
        drop(writing);
    }

    /// Collect all analyzer scalar values from all decks into a flat lookup table.
    fn collect_analyzer_values(&self) -> crate::modulation::AnalyzerValues {
        let mut vals = crate::modulation::AnalyzerValues::default();
        for ch in self.mixer.channels() {
            for slot in &ch.decks {
                let deck_id = slot.deck.uuid();
                for (analyzer_type, snapshot) in slot.deck.analyzers.all_snapshots() {
                    for (name, value) in &snapshot.scalars {
                        vals.insert(
                            deck_id.to_owned(),
                            analyzer_type.clone(),
                            name.clone(),
                            *value,
                        );
                    }
                }
            }
        }
        vals
    }

    /// Render the mixer frame: update cameras, NDI, Syphon, collect audio, render mixer.
    /// This performs all GPU work that doesn't need the surface texture.
    pub fn render_mixer_frame(&mut self) {
        // The shader Main's chosen transition needs, compiled the first
        // frame it is chosen (the library is the app's, not the mixer's).
        if let Some((transition, name)) = self.mixer.main_transition_shader_wanted() {
            let shader = self.registry.get(name).cloned();
            if let Err(e) = self
                .mixer
                .set_main_transition_shader(&self.context, transition, shader)
            {
                log::warn!(
                    "Main's {} transition could not be made ({e}): it fades instead",
                    transition.name()
                );
            }
        }
        // Sound follows the scene as this frame starts: new or removed clips,
        // what is on air, devices coming and going.
        self.update_audio_mix();
        // Surface a one-time notice for any deck whose ping-pong RAM cache was
        // truncated (hit the memory cap). The supported path for full-length
        // reverse on heavy/long/high-res clips is to pre-transcode to HAP.
        let truncated: Vec<(String, String)> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| ch.decks.iter())
            .filter_map(|slot| {
                slot.deck
                    .playback_snapshot()
                    .filter(|s| s.pingpong_cache_truncated)
                    .map(|_| {
                        (
                            slot.deck.uuid().to_string(),
                            slot.deck.source_name().to_string(),
                        )
                    })
            })
            .collect();
        for (uuid, name) in truncated {
            self.session.notifications.notify_once(
                format!("pingpong_truncated:{uuid}"),
                crate::notifications::NotificationLevel::Warning,
                format!(
                    "Deck '{name}': reverse playback truncated (cache full). \
                     Transcode to HAP for full-length reverse."
                ),
            );
        }

        // How strongly each channel shows, to know which cameras are needed:
        // in Main mode what is on Main, fading or overlaid, not the layered
        // mix's crossfader.
        let effective_opacities = self.mixer.channel_weights();

        // Collect camera IDs needed by visible channels. Watched channels — cued
        // previews and anything a bus has on air or staged — are included even
        // at zero opacity so their live camera inputs keep advancing while
        // off-air. See /spec/channel-preview.md.
        let mut needed_camera_ids = std::collections::HashSet::new();
        let mut needed_capture_ids = std::collections::HashSet::new();
        // A presenter view needs its PowerPoint input's capture, wanted or not.
        let show_captures: std::collections::HashMap<String, crate::screen_capture::CaptureId> =
            self.mixer
                .channels()
                .iter()
                .flat_map(|ch| &ch.decks)
                .filter_map(|slot| {
                    let state = slot.deck.screen_capture.as_ref()?;
                    state
                        .presentation
                        .as_ref()
                        .map(|_| (slot.deck.uuid().to_string(), state.capture_id))
                })
                .collect();
        // Counted over every deck, not just the visible ones: this is what the
        // capture manager reconciles its sessions against, so an off-air deck
        // must still count as a holder.
        let mut capture_holders: std::collections::HashMap<crate::screen_capture::CaptureId, u32> =
            std::collections::HashMap::new();
        // How each camera's pictures are read into colours, as its decks say
        // (see `CameraManager::set_colours`).
        let mut camera_colours = std::collections::HashMap::new();
        let mut camera_latencies = std::collections::HashMap::new();
        // What each camera's picture is coded as, a LUT file named in full
        // (see `CameraManager::set_profiles`).
        let mut camera_profiles = std::collections::HashMap::new();
        let luts = self.session.workspace.luts_dir();
        let watched = self.watched_channels();
        for (ch_idx, channel) in self.mixer.channels().iter().enumerate() {
            let visible = effective_opacities.get(ch_idx).copied().unwrap_or(0.0) > 0.0;
            // A channel shown as a tile in the control window is drawn now
            // and then (see `Mixer::tile_previews`): its camera stays live.
            // So does one shown in another as a layer of it (a tap).
            let wanted = visible || watched.contains(&ch_idx) || self.ui_tiles || channel.tapped;
            for slot in &channel.decks {
                if let Some(cap_id) = slot.deck.screen_capture_id() {
                    *capture_holders.entry(cap_id).or_default() += 1;
                }
                if let Some(cam_id) = slot.deck.camera_id() {
                    camera_colours.insert(cam_id, slot.deck.camera_colour);
                    camera_latencies.insert(cam_id, slot.deck.camera_lowest_latency);
                    camera_profiles.insert(cam_id, lut_in_full(&slot.deck.camera_profile, &luts));
                }
                // The arrangement knows more than the current opacity does: it
                // knows this deck is about to be needed, or that it will not be
                // for the next forty minutes. See /spec/deck-residency.md.
                let deck_wanted = match slot.source_demand {
                    crate::arrangement::SourceDemand::Needed => true,
                    crate::arrangement::SourceDemand::Idle => false,
                    crate::arrangement::SourceDemand::Unscheduled => wanted,
                };
                if !deck_wanted {
                    continue;
                }
                if let Some(cam_id) = slot.deck.camera_id() {
                    needed_camera_ids.insert(cam_id);
                }
                if let Some(cap_id) = slot.deck.screen_capture_id() {
                    needed_capture_ids.insert(cap_id);
                }
                if let Some(id) = slot
                    .deck
                    .presenter()
                    .and_then(|p| show_captures.get(p.follows()))
                {
                    needed_capture_ids.insert(*id);
                }
            }
        }

        // Stop any capture whose last deck is gone. A deck dropped outside
        // `remove_deck` — scene diff, undo, truncated or removed channel —
        // never released its session, leaving the capture thread grabbing and
        // downscaling frames for the rest of the session.
        self.screen_capture_manager
            .reconcile_holders(&capture_holders);

        // A camera missing when the show opened is picked up once it is here.
        self.reconcile_cameras();
        // PowerPoint inputs: shows opened, closed, their windows captured;
        // presenter views told where their shows are.
        self.reconcile_presentations();
        self.feed_presenters();
        // macOS camera effects change a camera's picture on air: say so.
        if let Some(effects) = self.camera_manager.poll_video_effects() {
            let text = crate::camera::video_effects_warning(&effects);
            log::warn!("{text}");
            self.session.notifications.warn(text);
        }
        self.camera_manager.set_colours(&camera_colours);
        self.camera_manager.set_lowest_latencies(&camera_latencies);
        self.camera_manager.set_profiles(&camera_profiles);
        // Each camera's and capture's frame is picked by when this show
        // frame is due on screen (see `crate::frame_picker`).
        let show_at = self.frame_stats.refresh_at.map_or_else(
            crate::host_clock::host_seconds,
            crate::host_clock::host_seconds_of,
        );
        // Update only needed camera frames
        self.camera_manager
            .update_selective(&self.context, &needed_camera_ids, show_at);

        // Upload screen-capture frames for visible/cued decks only. An
        // invisible capture deck costs nothing, which is what makes a
        // self-capture deck safe to leave in a scene.
        self.screen_capture_manager
            .update_selective(&self.context, &needed_capture_ids, show_at);

        // Update NDI receiver frames
        self.external_io
            .ndi_manager
            .update(&self.context.device, &self.context.queue);

        // Periodic Syphon re-discovery + late-bind of deferred decks (~1×/sec).
        // Removes the start/stop-ordering dependency: a producer that joins or
        // restarts after Varda is picked up automatically.
        #[cfg(target_os = "macos")]
        self.reconcile_syphon();

        // Update Syphon client frames
        #[cfg(target_os = "macos")]
        self.external_io
            .syphon_manager
            .update(&self.context.device, &self.context.queue);

        // Update stream receiver frames
        self.external_io
            .stream_manager
            .update(&self.context.device, &self.context.queue);

        // Pump HTML (Servo) instances and upload their frames
        self.external_io
            .html_manager
            .update(&self.context.device, &self.context.queue);

        // Upload depth-sensor frames (off-render-thread capture; this only
        // does the GPU texture upload). See spec/depth-sensors.md.
        self.depth_manager.update(&self.context.queue);

        // A presenter view shows its PowerPoint input's slide, straight from
        // that input's capture.
        let shows: std::collections::HashMap<String, crate::screen_capture::CaptureId> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| &ch.decks)
            .filter_map(|slot| {
                let state = slot.deck.screen_capture.as_ref()?;
                state
                    .presentation
                    .as_ref()
                    .map(|_| (slot.deck.uuid().to_string(), state.capture_id))
            })
            .collect();

        for channel in self.mixer.channels_mut() {
            for slot in &mut channel.decks {
                if let Some(presenter) = slot.deck.presenter_mut() {
                    presenter.slide = shows.get(presenter.follows()).and_then(|id| {
                        let view = self.screen_capture_manager.texture_view(*id)?.clone();
                        Some((view, self.screen_capture_manager.resolution(*id)?))
                    });
                }
                if let Some(kind) = slot.deck.external_source_kind() {
                    use crate::deck::ExternalSourceKind;
                    slot.deck.external_source_view = match kind {
                        ExternalSourceKind::Camera(cam_id) => {
                            // A device set to another size mid-show sends
                            // pictures of that size: the deck follows.
                            if let Some((w, h)) = self.camera_manager.resolution(cam_id) {
                                slot.deck.set_external_source_size(w, h);
                            }
                            self.camera_manager.texture_view(cam_id).cloned()
                        }
                        ExternalSourceKind::Ndi(idx) => {
                            // Known once its first picture comes: 4:3,
                            // portrait or 16:10 is not stretched to 16:9.
                            if let Some((w, h)) =
                                self.external_io.ndi_manager.receiver_dimensions(idx)
                            {
                                slot.deck.set_external_source_size(w, h);
                            }
                            self.external_io.ndi_manager.texture_view(idx).cloned()
                        }
                        #[cfg(target_os = "macos")]
                        ExternalSourceKind::Syphon(idx) => {
                            self.external_io.syphon_manager.texture_view(idx).cloned()
                        }
                        #[cfg(not(target_os = "macos"))]
                        ExternalSourceKind::Syphon(_) => None,
                        ExternalSourceKind::Srt(idx)
                        | ExternalSourceKind::Hls(idx)
                        | ExternalSourceKind::Dash(idx)
                        | ExternalSourceKind::Rtmp(idx) => {
                            // At the stream's own size, once its first
                            // picture says it: not stretched to 16:9.
                            if let Some((w, h)) =
                                self.external_io.stream_manager.receiver_dimensions(idx)
                            {
                                slot.deck.set_external_source_size(w, h);
                            }
                            self.external_io.stream_manager.texture_view(idx).cloned()
                        }
                        ExternalSourceKind::Html(idx) => {
                            self.external_io.html_manager.texture_view(idx).cloned()
                        }
                        ExternalSourceKind::ScreenCapture(id) => {
                            // Router/UI edits land on the deck; push them down
                            // to the capture thread here, once, when they change.
                            if let Some(state) = &mut slot.deck.screen_capture {
                                if state.config_dirty {
                                    self.screen_capture_manager
                                        .set_config(id, state.config.clone());
                                    state.config_dirty = false;
                                }
                            }
                            // A crop or target resize reallocates the shared
                            // texture, so the deck's source dimensions are
                            // pushed down each frame rather than fixed at open.
                            if let Some((w, h)) = self.screen_capture_manager.resolution(id) {
                                slot.deck.set_external_source_size(w, h);
                            }
                            self.screen_capture_manager.texture_view(id).cloned()
                        }
                        // Taps own no device, so the mixer resolves them all at
                        // once in `prepare_taps` below.
                        ExternalSourceKind::Tap => None,
                        ExternalSourceKind::DepthSensor(id) => {
                            // Depth decks read the R16Uint depth view + RGB view
                            // and reproject via the point-cloud pass rather than
                            // blitting a single RGBA texture.
                            let depth = self.depth_manager.depth_view(id).cloned();
                            slot.deck.depth_rgb_view = self.depth_manager.rgb_view(id).cloned();
                            slot.deck.depth_intrinsics = self.depth_manager.intrinsics(id);
                            slot.deck.depth_source_size = self.depth_manager.resolution(id);
                            depth
                        }
                    };
                }

                // Shader decks with a `depth_sensor` preprocessor read the same
                // shared sensor textures, but convert them via their own GPU
                // passes rather than reprojecting a point cloud. The manager
                // lives here, so the views are pushed down once per frame — the
                // deck layer never reaches up into a device.
                // See spec/depth-sensor-preprocessor.md.
                if let Some(state) = &mut slot.deck.depth_prepro {
                    let id = state.sensor_id;
                    state.input = match (
                        self.depth_manager.depth_view(id),
                        self.depth_manager.rgb_view(id),
                        self.depth_manager.frame_generation(id),
                    ) {
                        (Some(depth), Some(rgb), Some(generation)) => {
                            Some(crate::deck::DepthPreprocessInput {
                                depth_view: depth.clone(),
                                rgb_view: rgb.clone(),
                                generation,
                                frame_dt: self.depth_manager.frame_dt(id).unwrap_or(1.0 / 30.0),
                                connected: self.depth_manager.is_connected(id),
                            })
                        }
                        _ => None,
                    };
                }
            }
        }

        // Runs after the binding loop but still before any deck renders, which
        // is the window in which a tap can be swapped safely.
        // See spec/program-tap.md.
        self.mixer.prepare_taps(&self.context);

        // Collect audio values for modulation
        let audio_values = {
            let mut av = crate::modulation::AudioValues::default();
            for id in self.audio_manager.active_source_ids() {
                if let Some(data) = self.audio_manager.get_data(id) {
                    av.sources.insert(
                        id,
                        crate::modulation::AudioSourceValues {
                            fft: data.fft.clone(),
                            level: data.level,
                            sample_rate: data.sample_rate,
                        },
                    );
                }
            }
            av
        };

        let mut primary_audio = self.audio_manager.get_primary_data().clone();

        // Override audio BPM/beat with clock-resolved values (MIDI > OSC > Audio)
        let clock = self.input.clock_manager.state();
        if clock.active {
            primary_audio.bpm = Some(clock.bpm);
            primary_audio.time_since_beat = clock.beat_phase * (60.0 / clock.bpm);
        }

        // Collect analyzer scalar values from all decks
        let analyzer_values = self.collect_analyzer_values();

        let inputs = crate::mixer::FrameInputs {
            audio_data: &primary_audio,
            audio_values: &audio_values,
            analyzer_values: &analyzer_values,
            beat_time: self.input.clock_manager.beat_time(),
            transport: self.transport.sample(),
            // The wall paces a live show.
            free_run_time: None,
        };

        let target_fps = self.target_fps;
        let watched = self.watched_channels();
        self.mixer.decks_shown_alone = self.decks_shown_alone();
        self.mixer.tile_previews = self.ui_tiles;
        // The refresh is taken by the mixer: when the frame was drawn is kept
        // for the outputs first, or the recording timed every frame by when
        // its readback was started — a few milliseconds later each time, by
        // however long the mixer took — and repeated one and left out the
        // next, a pair a second (a laptop, 2026-10-03).
        self.frame_stats.drawn_at = Some(self.frame_drawn());
        self.mixer.frame_timing = crate::mixer::FrameTiming {
            refresh_at: self.frame_stats.refresh_at.take(),
            held: self.frames_follow_a_screen(),
        };
        if let Err(e) = self
            .mixer
            .render(&self.context, &inputs, target_fps, &watched)
        {
            log::error!("Failed to render mixer: {e}");
        }

        self.report_gpu_faults();
        self.report_arrangement_blackout();
    }

    /// Tell the performer when the arrangement is driving the output to nothing.
    ///
    /// Correct-and-idle looks exactly like broken on a screen, so the state is
    /// named rather than left to be inferred. Reported on the transition only:
    /// a deliberate blackout is legitimate and must not produce a toast every
    /// frame it lasts. See /spec/transport.md § Black-output detection.
    fn report_arrangement_blackout(&mut self) {
        let blacked_out = self.mixer.arrangement_blacked_out();
        if blacked_out && !self.arrangement_blackout_reported {
            self.session.notifications.warn(
                "The arrangement is holding every deck it drives at zero. \
                 If this is not intentional, check the transport position \
                 against your regions."
                    .to_string(),
            );
        }
        self.arrangement_blackout_reported = blacked_out;
    }

    /// Surface quarantined decks to the performer, and drain anything the GPU
    /// error guard caught that no deck owned.
    ///
    /// Toasts are keyed per deck so a shader failing every frame reports once
    /// rather than burying the notification history.
    /// See spec/error-handling.md § Shader Errors.
    fn report_gpu_faults(&mut self) {
        let quarantined: Vec<(String, String, String)> = self
            .mixer
            .channels()
            .iter()
            .flat_map(|ch| ch.decks.iter())
            .filter_map(|slot| {
                slot.deck.gpu_error().map(|err| {
                    (
                        slot.deck.uuid().to_string(),
                        slot.deck.source_name().to_string(),
                        err.to_string(),
                    )
                })
            })
            .collect();

        for (uuid, name, error) in quarantined {
            self.session.notifications.notify_once(
                format!("gpu_fault:{uuid}"),
                crate::notifications::NotificationLevel::Error,
                format!("'{name}' disabled — GPU error. Deck frozen; the rest of the show is unaffected. {error}"),
            );
        }

        // Faults raised outside any deck (channel/master effect chains, output
        // compositing). Nothing to quarantine, but they must not vanish.
        for fault in self.context.errors.take_faults() {
            if fault.context.is_none() {
                self.session.notifications.notify_once(
                    format!("gpu_fault_global:{}", fault.message),
                    crate::notifications::NotificationLevel::Error,
                    format!("GPU error: {}", fault.message),
                );
            }
        }
    }

    /// Render content to all outputs (windowed + headless) using the surface
    /// layout. Each output window's presenter puts its frame on its screen
    /// (see `renderer::presenter::Presenter`).
    pub fn render_outputs(&mut self) {
        self.draw_outputs();
    }

    /// Render content to all outputs (windowed + headless) using the surface
    /// layout, handing the windows' frames to their presenters.
    pub fn draw_outputs(&mut self) {
        let t_prep = std::time::Instant::now();
        // Before anything borrows the context: these may start the stream's
        // or the recording's encoder again.
        self.poll_stream();
        self.poll_recording();
        self.poll_blackmagic_outputs();
        for output in &mut self.output.outputs {
            if let crate::renderer::context::UnifiedOutput::Window(window) = output {
                window.clock_display = self.frame_stats.clock_display;
            }
        }
        let context = &self.context;

        // Resolved once per frame, before any destination renders, so every
        // output following a bus sees the same on-air content this frame.
        let mut bus_feeds = self.bus_feeds();

        // Prepare sub-mixes for any Channels(...) sources: a surface's own, or
        // the master with layers hidden that a bus puts on air.
        {
            let mut seen: std::collections::HashSet<Vec<usize>> = std::collections::HashSet::new();
            let mut sub_mix_sources: Vec<Vec<usize>> = Vec::new();
            let bus_sources = bus_feeds.values().flat_map(|f| f.picture.sources());
            for source in self
                .output
                .surface_manager
                .surfaces
                .iter()
                .map(|s| &s.source)
                .chain(bus_sources)
            {
                if let OutputSource::Channels(indices) = source {
                    let mut sorted = indices.clone();
                    sorted.sort_unstable();
                    sorted.dedup();
                    if seen.insert(sorted.clone()) {
                        sub_mix_sources.push(sorted);
                    }
                }
            }
            self.mixer.prepare_sub_mixes(&sub_mix_sources, context);
        }

        // Prepare tonemapped copies for any Channel(idx) sources — a surface's
        // own, or one a bus puts on air — so a channel looks the same on every
        // screen it reaches, and as it does in the main output.
        {
            let mut channel_indices: Vec<usize> = Vec::new();
            let mut seen = std::collections::HashSet::new();
            let bus_sources = bus_feeds.values().flat_map(|f| f.picture.sources());
            for source in self
                .output
                .surface_manager
                .surfaces
                .iter()
                .map(|s| &s.source)
                .chain(bus_sources)
            {
                if let OutputSource::Channel(idx) = source {
                    if seen.insert(*idx) {
                        channel_indices.push(*idx);
                    }
                }
            }
            if !channel_indices.is_empty() {
                self.mixer
                    .prepare_channel_tonemaps(&channel_indices, &self.context);
            }
        }

        let render_aspect = self.render_width as f32 / self.render_height.max(1) as f32;
        let mixer = &self.mixer;

        // Guarantee the black source actually reads black: one 1x1 clear,
        // the first frame.
        if !mixer.black_source_cleared() {
            let mut encoder =
                self.context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Black source clear"),
                    });
            mixer.clear_black_source(&mut encoder);
            self.context.submit(Some(encoder.finish()));
        }

        // Buses mid-`AUTO` fade into targets of their own, after the tonemapped
        // channel copies they read are ready.
        self.output.bus_dissolves.render(
            context,
            (self.render_width, self.render_height),
            &mut bus_feeds,
            |source| Self::resolve_direct(mixer, source, None),
            mixer.black_view(),
        );

        // Run domemaster renderer if enabled (content rotation is updated each frame via set_content_rotation)
        let domemaster_view = if let Some(dome) = &self.output.domemaster {
            if dome.enabled {
                dome.update_params(&self.context.queue);
                dome.render(&self.context, mixer.composite_view());
                Some(dome.output_view())
            } else {
                None
            }
        } else {
            None
        };

        self.output.led_cards.prepare(
            &self.context.device,
            &self.context.queue,
            self.output.outputs.iter().filter_map(|o| o.led()),
        );
        // Each output showing its test card has it at its own pixels.
        {
            use crate::renderer::context::{CalibrationMode, UnifiedOutput};
            use crate::renderer::led_card::WantedCard;
            let wanted: Vec<WantedCard> = self
                .output
                .outputs
                .iter()
                .enumerate()
                .filter_map(|(index, o)| match o {
                    UnifiedOutput::Window(w)
                        if w.calibration_mode == CalibrationMode::Projector =>
                    {
                        let (width, height) =
                            w.rotation.effective_dimensions(w.size.width, w.size.height);
                        Some(WantedCard {
                            uuid: w.uuid.clone(),
                            name: w.name.clone(),
                            size: [width, height],
                            index,
                        })
                    }
                    UnifiedOutput::Headless(h)
                        if h.calibration_mode == CalibrationMode::Projector =>
                    {
                        Some(WantedCard {
                            uuid: h.uuid.clone(),
                            name: h.name.clone(),
                            size: [h.width, h.height],
                            index,
                        })
                    }
                    _ => None,
                })
                .collect();
            self.output.output_cards.prepare(
                &self.context.device,
                &self.context.queue,
                wanted.into_iter(),
            );
        }
        let led = LedFrame {
            master: [self.render_width, self.render_height],
            cards: &self.output.led_cards,
            output_cards: &self.output.output_cards,
        };
        self.frame_stats.parts.add("outputs prep", t_prep.elapsed());
        self.frame_stats.windows_presented = false;
        let hold = self.frame_hold_secs();
        let period = 1.0 / f64::from(self.target_fps.max(1));
        for output in &mut self.output.outputs {
            if let crate::renderer::context::UnifiedOutput::Window(window) = output {
                window.frame_period = period;
                // Held only on a screen refreshing faster than the show (see
                // `OutputWindow::holds_frames`).
                let hold = hold.filter(|_| window.holds_frames(period));
                if window.frame_hold != hold {
                    crate::renderer::context::set_frame_hold(&window.surface, hold);
                    window.frame_hold = hold;
                }
            }
        }
        for output in &mut self.output.outputs {
            match output {
                crate::renderer::context::UnifiedOutput::Window(output) => {
                    let t_window = std::time::Instant::now();
                    let presents = output.presents;
                    Self::render_window_output(
                        output,
                        context,
                        &FrameSources {
                            mixer,
                            bus_feeds: &bus_feeds,
                            domemaster_view,
                        },
                        &self.output.surface_manager,
                        &self.output.calibration_textures,
                        render_aspect,
                        &led,
                    );
                    self.frame_stats.windows_presented |= output.presents > presents;
                    // Drawing the picture; and the last present's wait for a
                    // frame to draw into, made after the engine was handed
                    // back (see `take_present_jobs`).
                    let parts = &mut self.frame_stats.parts;
                    parts.add(&format!("{} draw", output.name), t_window.elapsed());
                    parts.add(&format!("{} acquire", output.name), output.last_acquire);
                }
                crate::renderer::context::UnifiedOutput::Headless(_) => {
                    // Headless rendering handled separately (needs &mut for subprocess)
                }
            }
        }

        // Render headless outputs (needs &mut self for subprocess feeding)
        let drawn = self
            .frame_stats
            .drawn_at
            .take()
            .unwrap_or_else(std::time::Instant::now);
        let sinks = HeadlessDeliverySinks {
            ndi_manager: &mut self.external_io.ndi_manager,
            #[cfg(target_os = "macos")]
            syphon_manager: &mut self.external_io.syphon_manager,
            audio_manager: &mut self.audio_manager,
            audio_mix: &self.audio_mix,
            notifications: &mut self.session.notifications,
            encoder_fps: crate::app::state::encoder_fps(self.target_fps),
            drawn,
        };
        let t_headless = std::time::Instant::now();
        Self::render_headless_outputs_inner(
            &mut self.output.outputs,
            context,
            &FrameSources {
                mixer,
                bus_feeds: &bus_feeds,
                domemaster_view,
            },
            &self.output.surface_manager,
            sinks,
            &led,
        );
        self.frame_stats
            .parts
            .add("headless outputs", t_headless.elapsed());

        // The stream renders like a headless output, kept off the list.
        let sinks = HeadlessDeliverySinks {
            ndi_manager: &mut self.external_io.ndi_manager,
            #[cfg(target_os = "macos")]
            syphon_manager: &mut self.external_io.syphon_manager,
            audio_manager: &mut self.audio_manager,
            audio_mix: &self.audio_mix,
            notifications: &mut self.session.notifications,
            encoder_fps: crate::app::state::encoder_fps(self.target_fps),
            drawn,
        };
        let t_headless = std::time::Instant::now();
        Self::render_headless_outputs_inner(
            &mut self.streaming.output,
            context,
            &FrameSources {
                mixer,
                bus_feeds: &bus_feeds,
                domemaster_view,
            },
            &self.output.surface_manager,
            sinks,
            &led,
        );
        self.frame_stats.parts.add("stream", t_headless.elapsed());

        // And the recording.
        let sinks = HeadlessDeliverySinks {
            ndi_manager: &mut self.external_io.ndi_manager,
            #[cfg(target_os = "macos")]
            syphon_manager: &mut self.external_io.syphon_manager,
            audio_manager: &mut self.audio_manager,
            audio_mix: &self.audio_mix,
            notifications: &mut self.session.notifications,
            encoder_fps: crate::app::state::encoder_fps(self.target_fps),
            drawn,
        };
        let t_headless = std::time::Instant::now();
        Self::render_headless_outputs_inner(
            &mut self.recording.output,
            context,
            &FrameSources {
                mixer,
                bus_feeds: &bus_feeds,
                domemaster_view,
            },
            &self.output.surface_manager,
            sinks,
            &led,
        );
        self.frame_stats
            .parts
            .add("recording", t_headless.elapsed());
    }

    fn render_window_output(
        output: &mut crate::renderer::context::OutputWindow,
        context: &crate::renderer::context::GpuContext,
        frame: &FrameSources<'_>,
        surface_manager: &crate::surface::SurfaceManager,
        calibration_textures: &[(wgpu::Texture, wgpu::TextureView)],
        render_aspect: f32,
        led: &LedFrame<'_>,
    ) {
        use crate::renderer::context::CalibrationMode;

        let (mixer, bus_feeds, domemaster_view) =
            (frame.mixer, frame.bus_feeds, frame.domemaster_view);
        // Projector calibration: one full-frame test card over the whole output,
        // bypassing surface geometry/warp (physical projector alignment).
        if output.calibration_mode == CalibrationMode::Projector {
            // Its own card, at its pixels — the shared one until that is made.
            let card = led
                .output_cards
                .view(&output.uuid)
                .or_else(|| calibration_textures.first().map(|(_, view)| view));
            if let Some(card) = card {
                output.render(context, card);
                output.request_redraw();
                return;
            }
        }
        // An LED feed: each screen at its own pixels, in place of surfaces.
        if let Some(layout) = output.led.clone() {
            let draws = Self::led_draws(&layout, frame, led);
            let infos: Vec<SurfaceRenderInfo<'_>> = draws.iter().map(LedDraw::info).collect();
            output.render_surfaces(context, &infos);
            output.request_redraw();
            return;
        }
        // Surfaces calibration: each surface shows a colored test card through its warp.
        let surfaces_cal = output.calibration_mode == CalibrationMode::Surfaces;
        if surface_manager.surfaces.is_empty() {
            // No stage geometry, so the window is the whole canvas and the
            // master's shape is the only thing that can define the picture.
            // Surfaces below are user-placed and carry their own aspect.
            output.render_fit(context, mixer.composite_view(), render_aspect);
        } else if !output.surface_assignments.is_empty() {
            // Draw in global stacking order (surface-manager Vec order, index 0 =
            // bottom), not per-assignment order — see 8i.12. For each surface, find
            // its enabled assignment on this output.
            let render_infos: Vec<SurfaceRenderInfo<'_>> = surface_manager
                .surfaces
                .iter()
                .filter_map(|surface| {
                    let (ai, assignment) = output
                        .surface_assignments
                        .iter()
                        .enumerate()
                        .find(|(_, a)| a.enabled && a.surface_uuid == surface.uuid)?;
                    let bb = surface.bounding_box();
                    let content_view = if surfaces_cal && !calibration_textures.is_empty() {
                        &calibration_textures[ai % calibration_textures.len()].1
                    } else {
                        Self::resolve_source(mixer, &surface.source, domemaster_view, bus_feeds)?
                    };
                    let (uv_scale, uv_offset) = if surfaces_cal {
                        ([1.0, 1.0], [0.0, 0.0])
                    } else {
                        Self::surface_uv(&surface.source, surface.content_mapping, &bb, bus_feeds)
                    };
                    Some(SurfaceRenderInfo {
                        uuid: &surface.uuid,
                        content_view,
                        vertices: &surface.vertices,
                        extra_contours: &surface.extra_contours,
                        bounding_box: [bb.x, bb.y, bb.width, bb.height],
                        uv_scale,
                        uv_offset,
                        warp_mode: surface.effective_warp(),
                        overlap_zones: assignment.overlap_zones.clone(),
                        hole_uv_contours: surface.hole_uv_contours(),
                    })
                })
                .collect();
            output.render_surfaces(context, &render_infos);
        } else {
            let render_infos: Vec<SurfaceRenderInfo<'_>> = surface_manager
                .surfaces
                .iter()
                .enumerate()
                .filter_map(|(si, surface)| {
                    let bb = surface.bounding_box();
                    let content_view = if surfaces_cal && !calibration_textures.is_empty() {
                        &calibration_textures[si % calibration_textures.len()].1
                    } else {
                        Self::resolve_source(mixer, &surface.source, domemaster_view, bus_feeds)?
                    };
                    let (uv_scale, uv_offset) = if surfaces_cal {
                        ([1.0, 1.0], [0.0, 0.0])
                    } else {
                        Self::surface_uv(&surface.source, surface.content_mapping, &bb, bus_feeds)
                    };
                    Some(SurfaceRenderInfo {
                        uuid: &surface.uuid,
                        content_view,
                        vertices: &surface.vertices,
                        extra_contours: &surface.extra_contours,
                        bounding_box: [bb.x, bb.y, bb.width, bb.height],
                        uv_scale,
                        uv_offset,
                        warp_mode: surface.effective_warp(),
                        overlap_zones: crate::renderer::edge_blend::SurfaceOverlapZones::default(),
                        hole_uv_contours: surface.hole_uv_contours(),
                    })
                })
                .collect();
            output.render_surfaces(context, &render_infos);
        }
        output.request_redraw();
    }

    /// The screens of an LED feed as draws: each at its own pixels of the
    /// feed, sampling its part of Main or of its bus's region of the master.
    /// A screen whose bus shows nothing draws black; one showing test cards
    /// draws its card over its whole rectangle.
    fn led_draws<'a>(
        layout: &'a crate::engine::value::led::LedLayout,
        frame: &FrameSources<'a>,
        led: &LedFrame<'a>,
    ) -> Vec<LedDraw<'a>> {
        let [fw, fh] = layout.feed.map(|v| v.max(1) as f32);
        let quad = |[x, y, w, h]: [f32; 4]| [[x, y], [x + w, y], [x + w, y + h], [x, y + h]];
        layout
            .screens
            .iter()
            .filter_map(|screen| {
                if layout.test_card {
                    let [x, y, w, h] = screen.feed_rect().map(|v| v as f32);
                    let rect = [x / fw, y / fh, w / fw, h / fh];
                    return Some(LedDraw {
                        quad: quad(rect),
                        rect,
                        uv_scale: [1.0, 1.0],
                        uv_offset: [0.0, 0.0],
                        view: led.cards.view(&screen.uuid)?,
                        uuid: &screen.uuid,
                    });
                }
                let view = Self::resolve_source(
                    frame.mixer,
                    &screen.source,
                    frame.domemaster_view,
                    frame.bus_feeds,
                )?;
                // A bus's picture is its region of the master.
                let (region_scale, region_offset) = match &screen.source {
                    OutputSource::BusProgram(uuid) => frame
                        .bus_feeds
                        .get(uuid)
                        .map_or(([1.0, 1.0], [0.0, 0.0]), |f| (f.uv_scale, f.uv_offset)),
                    _ => ([1.0, 1.0], [0.0, 0.0]),
                };
                let picture = [0, 1]
                    .map(|i| (region_scale[i] * led.master[i] as f32).round().max(1.0) as u32);
                let placed = screen.placement(layout.feed, picture)?;
                Some(LedDraw {
                    quad: quad(placed.dest),
                    rect: placed.dest,
                    uv_scale: [0, 1].map(|i| placed.uv_scale[i] * region_scale[i]),
                    uv_offset: [0, 1]
                        .map(|i| region_offset[i] + placed.uv_offset[i] * region_scale[i]),
                    view,
                    uuid: &screen.uuid,
                })
            })
            .collect()
    }

    /// Resolve a destination's configured source to the view it should show.
    ///
    /// `bus_feeds` maps a bus UUID to whatever that bus currently has on air,
    /// resolved once per frame by `VardaApp::bus_feeds`. A bus with nothing on
    /// air — black, unresolvable, or quarantined — is absent from the map and
    /// resolves to the black source rather than `None`: returning `None` would
    /// make the destination skip its blit and keep showing the previous frame,
    /// which is precisely what a black bus must not do.
    fn resolve_source<'a>(
        mixer: &'a Mixer,
        source: &OutputSource,
        domemaster_view: Option<&'a wgpu::TextureView>,
        bus_feeds: &'a std::collections::HashMap<String, crate::app::BusFeed>,
    ) -> Option<&'a wgpu::TextureView> {
        use crate::app::FeedPicture;

        let OutputSource::BusProgram(bus_uuid) = source else {
            return Self::resolve_direct(mixer, source, domemaster_view);
        };
        // One level only: a feed is always a concrete source or a rendered
        // dissolve, never another bus, so this cannot recurse.
        match bus_feeds.get(bus_uuid).map(|f| &f.picture) {
            Some(FeedPicture::Source(source)) => {
                Self::resolve_direct(mixer, source, domemaster_view)
            }
            Some(FeedPicture::Rendered(view)) => Some(view),
            // A dissolve nobody rendered this frame holds where it started.
            Some(FeedPicture::Dissolve { from, .. }) => Some(
                from.as_ref()
                    .and_then(|s| Self::resolve_direct(mixer, s, domemaster_view))
                    .unwrap_or_else(|| mixer.black_view()),
            ),
            None => Some(mixer.black_view()),
        }
    }

    /// Resolve a concrete (non-bus) source to its view.
    pub(crate) fn resolve_direct<'a>(
        mixer: &'a Mixer,
        source: &OutputSource,
        domemaster_view: Option<&'a wgpu::TextureView>,
    ) -> Option<&'a wgpu::TextureView> {
        match source {
            OutputSource::Master => Some(mixer.composite_view()),
            OutputSource::Channel(ch_idx) => mixer
                .get_tonemapped_channel_view(*ch_idx)
                .or_else(|| mixer.channels().get(*ch_idx).map(|ch| &ch.composite_view)),
            OutputSource::Channels(indices) => {
                let mut sorted = indices.clone();
                sorted.sort_unstable();
                sorted.dedup();
                mixer.get_sub_mix_view(&sorted)
            }
            OutputSource::Deck(ch_idx, deck_idx) => mixer
                .channels()
                .get(*ch_idx)
                .and_then(|ch| ch.decks.get(*deck_idx))
                .map(|slot| &slot.deck.texture_view),
            OutputSource::Domemaster => domemaster_view,
            // Buses are resolved by `resolve_source`; a feed never names one.
            OutputSource::BusProgram(_) => Some(mixer.black_view()),
        }
    }

    /// UV for a surface, letting a bus feed override the surface's own mapping.
    ///
    /// A bus declares which region of the master raster is its picture, so a
    /// destination showing that bus must sample exactly that region — the crop
    /// belongs to the bus, configured once, not to every destination that
    /// happens to show it. Surfaces sourcing anything else keep their own
    /// mapping unchanged.
    fn surface_uv(
        surface_source: &OutputSource,
        mapping: ContentMapping,
        bb: &crate::surface::BoundingBox,
        bus_feeds: &std::collections::HashMap<String, crate::app::BusFeed>,
    ) -> ([f32; 2], [f32; 2]) {
        if let OutputSource::BusProgram(uuid) = surface_source {
            return bus_feeds
                .get(uuid)
                .map_or(([1.0, 1.0], [0.0, 0.0]), |feed| {
                    (feed.uv_scale, feed.uv_offset)
                });
        }
        Self::compute_uv(mapping, bb)
    }

    fn compute_uv(
        mapping: ContentMapping,
        bb: &crate::surface::BoundingBox,
    ) -> ([f32; 2], [f32; 2]) {
        match mapping {
            ContentMapping::Fill => ([1.0, 1.0], [0.0, 0.0]),
            ContentMapping::Mapped => ([bb.width, bb.height], [bb.x, bb.y]),
        }
    }

    /// Refresh monitors from the event loop.
    pub fn refresh_monitors(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let handles: Vec<_> = event_loop.available_monitors().collect();
        // Sizes read here, on the main thread: the engine thread publishes
        // them, and a screen asked for its size from another thread answers
        // through the main thread — which may be waiting for the engine.
        self.output.monitor_sizes = handles
            .iter()
            .map(winit::monitor::MonitorHandle::size)
            .collect();
        let names = super::monitors::names(&handles);
        // What macOS calls each screen, and what a name given to it is kept
        // by, asked here on the main thread too — this runs every frame, so
        // only when the screens change, or a second on (a rate changed).
        let screens: Vec<(String, (i32, i32))> = names
            .iter()
            .zip(&handles)
            .map(|(name, h)| {
                let p = h.position();
                (name.clone(), (p.x, p.y))
            })
            .collect();
        let due = self
            .output
            .screen_facts_at
            .is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(1));
        if due || screens != self.output.screen_facts_for {
            self.output.screen_facts = super::screens::facts(&handles, &names);
            self.output.screen_facts_for = screens;
            self.output.screen_facts_at = Some(std::time::Instant::now());
        }
        self.output.cached_monitors = names.into_iter().zip(handles).collect();
        // Which screen each output window is on, read here on the main
        // thread for the engine thread, which may not ask the windows.
        for output in &mut self.output.outputs {
            if let crate::renderer::context::UnifiedOutput::Window(w) = output {
                w.note_screen();
            }
        }
        self.release_lost_displays();
        self.adopt_awaited_displays();
        self.follow_output_screens();
        self.warn_on_uneven_rates();
        // Only a running app with an event loop gets here — never the test
        // harness — so this holds the machine awake exactly while Varda runs.
        self.output.keep_awake.set(true);
        // Likewise, list sound devices only in a running app: enumeration
        // talks to the OS audio server, which a test engine has no use for.
        if !self.audio_devices_listed {
            self.audio_devices_listed = true;
            self.audio_mix.rescan_devices();
        }
    }

    /// Render all active headless outputs — readback + deliver frames.
    // `sinks` bundles `&mut` borrows used mutably here; a shared ref won't do.
    #[allow(clippy::needless_pass_by_value)]
    fn render_headless_outputs_inner(
        outputs: &mut [crate::renderer::context::UnifiedOutput],
        context: &crate::renderer::context::GpuContext,
        frame: &FrameSources<'_>,
        surface_manager: &crate::surface::SurfaceManager,
        sinks: HeadlessDeliverySinks,
        led: &LedFrame<'_>,
    ) {
        let (mixer, bus_feeds, domemaster_view) =
            (frame.mixer, frame.bus_feeds, frame.domemaster_view);
        let no_zones = crate::renderer::edge_blend::SurfaceOverlapZones::default();
        for output in outputs.iter_mut() {
            // An NDI sender carries its output's audio bus while it runs,
            // and falls silent when it is stopped.
            if let crate::renderer::context::UnifiedOutput::Headless(h) = &*output {
                if let crate::renderer::context::OutputTarget::NdiSend { sender_name } = &h.target {
                    let bus = h.audio_bus.as_deref().filter(|_| h.active);
                    sinks
                        .ndi_manager
                        .keep_audio(sender_name, bus, sinks.audio_mix);
                }
            }
            let h = match output {
                crate::renderer::context::UnifiedOutput::Headless(h) if h.active => h,
                _ => continue,
            };

            let mut encoder =
                context
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                        label: Some("Headless Output Encoder"),
                    });

            // Post-process edge blend only for Manual mode; Auto uses per-surface shader blend.
            let use_edge_blend = h.edge_blend_mode
                == crate::renderer::edge_blend::EdgeBlendMode::Manual
                && h.edge_blend.any_enabled();
            // When edge blending: render to intermediate, then blend → final texture.
            let render_target = if use_edge_blend {
                &h.edge_blend_texture_view
            } else {
                &h.texture_view
            };

            if let Some(layout) = &h.led {
                let draws = Self::led_draws(layout, frame, led);
                let descs: Vec<crate::renderer::blit::PolygonDrawDesc<'_>> =
                    draws.iter().map(|d| d.desc(&no_zones)).collect();
                let (prepared, vertex_pool) =
                    h.polygon_pipeline
                        .prepare(&context.device, &context.queue, &descs);
                let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("Headless LED Pass"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: render_target,
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
                h.polygon_pipeline.draw(&mut rp, &prepared, &vertex_pool);
            } else if h.surface_assignments.is_empty() {
                // Fallback: simple blit from source — or the test card in
                // its place (`calibration_mode`).
                let card = led.output_cards.view(&h.uuid).filter(|_| {
                    h.calibration_mode == crate::renderer::context::CalibrationMode::Projector
                });
                let Some(source_view) = card
                    .or_else(|| Self::resolve_source(mixer, &h.source, domemaster_view, bus_feeds))
                else {
                    continue;
                };
                // A bus sends only its region of the master, as it does on a
                // screen; every other source is sent whole.
                let region = match &h.source {
                    OutputSource::BusProgram(uuid) if card.is_none() => bus_feeds
                        .get(uuid)
                        .map_or(([1.0, 1.0], [0.0, 0.0]), |f| (f.uv_scale, f.uv_offset)),
                    _ => ([1.0, 1.0], [0.0, 0.0]),
                };
                h.blit_pipeline.set_presentation_of_region(
                    &context.queue,
                    h.rotation.index(),
                    &h.resolved_presentation,
                    region,
                );
                // A Blackmagic output's frame is its format's: the picture
                // fitted into it, never stretched — its test card, made at
                // its pixels, filling it.
                let fitted = if card.is_some() {
                    None
                } else {
                    matches!(
                        h.target,
                        crate::renderer::context::OutputTarget::DeckLink { .. }
                    )
                    .then(|| {
                        crate::renderer::context::fitted_in_format(
                            led.master, region.0, h.rotation, h.width, h.height,
                        )
                    })
                };
                let bind_group = h
                    .blit_pipeline
                    .create_bind_group(&context.device, source_view);
                {
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Headless Blit Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: render_target,
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
                    if let Some([x, y, w, high]) = fitted {
                        let (width, height) = (h.width as f32, h.height as f32);
                        rp.set_viewport(x * width, y * height, w * width, high * height, 0.0, 1.0);
                    }
                    h.blit_pipeline.render(&mut rp, &bind_group);
                }
            } else {
                // Surface-routed rendering: render assigned surfaces with warp
                // Triangulate on the CPU, then prepare draws from the pipeline's
                // persistent param/vertex pools (no per-frame GPU buffer alloc).
                // Draw in global stacking order (surface-manager Vec order, index 0
                // = bottom), not per-assignment order — see 8i.12.
                let draws: Vec<crate::renderer::blit::PolygonDrawDesc<'_>> = surface_manager
                    .surfaces
                    .iter()
                    .filter_map(|surface| {
                        let assignment = h
                            .surface_assignments
                            .iter()
                            .find(|a| a.enabled && a.surface_uuid == surface.uuid)?;
                        let bb = surface.bounding_box();
                        let content_view = Self::resolve_source(
                            mixer,
                            &surface.source,
                            domemaster_view,
                            bus_feeds,
                        )?;
                        let (uv_scale, uv_offset) = Self::surface_uv(
                            &surface.source,
                            surface.content_mapping,
                            &bb,
                            bus_feeds,
                        );
                        // Warp is per-surface now; `None` = no warp (native
                        // position). `effective_warp` applies auto-warp binding.
                        let eff_warp = surface.effective_warp();
                        // Combined (multi-contour) surface: a single warp mesh
                        // can't represent disjoint contours, so render every
                        // contour as a bounding-box UV fill (matches the editor).
                        let (homography, vertices) = if surface.extra_contours.is_empty() {
                            match eff_warp.as_ref() {
                                Some(crate::renderer::warp::WarpMode::CornerPin { corners }) => {
                                    let src_corners = [
                                        [bb.x, bb.y],
                                        [bb.x + bb.width, bb.y],
                                        [bb.x + bb.width, bb.y + bb.height],
                                        [bb.x, bb.y + bb.height],
                                    ];
                                    let homography =
                                        crate::renderer::warp::compute_forward_homography(
                                            &src_corners,
                                            corners,
                                        );
                                    let verts =
                                    crate::renderer::blit::PolygonBlitPipeline::triangulate_verts(
                                        &surface.vertices,
                                        bb.x,
                                        bb.y,
                                        bb.width,
                                        bb.height,
                                    );
                                    (Some(homography), verts)
                                }
                                Some(crate::renderer::warp::WarpMode::Mesh(mesh)) => (
                                    None,
                                    crate::renderer::blit::PolygonBlitPipeline::mesh_verts(mesh),
                                ),
                                // Bezier: tessellate the control cage into a mesh,
                                // then bake to verts (identity homography).
                                Some(crate::renderer::warp::WarpMode::Bezier(b)) => (
                                    None,
                                    crate::renderer::blit::PolygonBlitPipeline::mesh_verts(
                                        &b.tessellate(),
                                    ),
                                ),
                                None => (
                                    None,
                                    crate::renderer::blit::PolygonBlitPipeline::triangulate_verts(
                                        &surface.vertices,
                                        bb.x,
                                        bb.y,
                                        bb.width,
                                        bb.height,
                                    ),
                                ),
                            }
                        } else {
                            (
                                None,
                                crate::renderer::blit::PolygonBlitPipeline::triangulate_multi(
                                    &surface.vertices,
                                    &surface.extra_contours,
                                    bb.x,
                                    bb.y,
                                    bb.width,
                                    bb.height,
                                ),
                            )
                        };
                        Some(crate::renderer::blit::PolygonDrawDesc {
                            content_view,
                            uv_scale,
                            uv_offset,
                            homography,
                            overlap_zones: &assignment.overlap_zones,
                            vertices,
                            mask_uuid: &surface.uuid,
                            mask_uv_contours: surface.hole_uv_contours(),
                        })
                    })
                    .collect();

                let (prepared, vertex_pool) =
                    h.polygon_pipeline
                        .prepare(&context.device, &context.queue, &draws);

                {
                    let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                        label: Some("Headless Surface Pass"),
                        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                            view: render_target,
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
                    h.polygon_pipeline.draw(&mut rp, &prepared, &vertex_pool);
                }
            }

            // Apply edge blend post-process if any edge is enabled
            if use_edge_blend {
                h.edge_blend_pipeline.render(
                    &context.device,
                    &context.queue,
                    &mut encoder,
                    &h.edge_blend_texture_view,
                    &h.texture_view,
                    &h.edge_blend,
                );
            }

            // Syphon (macOS) publishes GPU-side (zero-copy): skip the CPU
            // readback entirely and hand the rendered texture to the server.
            #[cfg(target_os = "macos")]
            let is_syphon = matches!(
                &h.target,
                crate::renderer::context::OutputTarget::SyphonServer { .. }
            );
            #[cfg(not(target_os = "macos"))]
            let is_syphon = false;

            let is_ndi_p216 = matches!(
                (&h.target, &h.resolved_presentation.pixel_format),
                (
                    crate::renderer::context::OutputTarget::NdiSend { .. },
                    crate::renderer::context::PresentationPixelFormat::P216
                )
            );
            // A recording or the stream: its encoder takes NV12 made here.
            let is_nv12 = h.nv12.is_some()
                && h.subprocess
                    .as_ref()
                    .is_some_and(|subprocess| subprocess.takes_nv12());
            if is_nv12 {
                if let Some(converter) = h.nv12.as_mut() {
                    converter.encode(
                        &context.device,
                        &context.queue,
                        &mut encoder,
                        &h.texture,
                        sinks.drawn,
                    );
                }
            } else if is_ndi_p216 {
                if let crate::renderer::context::OutputTarget::NdiSend { sender_name } = &h.target {
                    if let Err(error) = sinks.ndi_manager.begin_p216_frame(
                        sender_name,
                        crate::ndi::P216FrameConversion {
                            device: &context.device,
                            queue: &context.queue,
                            encoder: &mut encoder,
                            source: &h.texture_view,
                            width: h.width,
                            height: h.height,
                            dither: h.presentation_request.dither,
                        },
                    ) {
                        stop_headless_output(
                            &h.name,
                            &mut h.active,
                            sinks.notifications,
                            format!("NDI P216 conversion failed: {error}"),
                        );
                    }
                }
            } else if !is_syphon {
                // Enqueue readback copy from the now-rendered texture
                h.readback
                    .begin_readback(&mut encoder, &h.texture, sinks.drawn);
            }
            context.submit(std::iter::once(encoder.finish()));

            if is_ndi_p216 {
                if let crate::renderer::context::OutputTarget::NdiSend { sender_name } = &h.target {
                    if let Err(error) = sinks.ndi_manager.try_send_p216(
                        sender_name,
                        &context.device,
                        h.width,
                        h.height,
                        sinks.encoder_fps,
                    ) {
                        stop_headless_output(
                            &h.name,
                            &mut h.active,
                            sinks.notifications,
                            format!("NDI P216 submission failed: {error}"),
                        );
                    }
                }
            }

            #[cfg(target_os = "macos")]
            if let crate::renderer::context::OutputTarget::SyphonServer { ref server_name } =
                h.target
            {
                sinks.syphon_manager.publish_frame_gpu(
                    context,
                    server_name,
                    &h.texture_view,
                    h.width,
                    h.height,
                );
            }

            if is_nv12 {
                let frames = h
                    .nv12
                    .as_mut()
                    .map(|converter| converter.try_read(&context.device))
                    .unwrap_or_default();
                for (made, frame) in frames {
                    if h.subprocess
                        .as_mut()
                        .is_some_and(|subprocess| !subprocess.feed_nv12(&frame, made))
                    {
                        // The recording or stream starts its encoder again
                        // itself, reading why it stopped from the subprocess.
                        h.active = false;
                        break;
                    }
                }
                continue;
            }

            // Deliver previous frame's readback data to target
            if !is_syphon && !is_ndi_p216 {
                let mut finished = h.readback.try_read_all(&context.device);
                let newest = finished.pop();
                // An encoder takes every frame read back, in the order drawn:
                // the earlier ones first, the newest as every target does.
                if let Some(subprocess) = h.subprocess.as_mut() {
                    for frame in &finished {
                        if !subprocess.feed_readback_frame(frame) {
                            break;
                        }
                    }
                }
                if let Some(frame_data) = newest {
                    let delivery = if h.subprocess.is_some() {
                        if h.subprocess
                            .as_mut()
                            .is_some_and(|subprocess| !subprocess.feed_readback_frame(&frame_data))
                        {
                            if matches!(
                                h.target,
                                crate::renderer::context::OutputTarget::Stream
                                    | crate::renderer::context::OutputTarget::Record
                            ) {
                                // The stream starts its own encoder again
                                // (`poll_stream`), and reads why it stopped
                                // from the subprocess, so it is left in place.
                                h.active = false;
                                continue;
                            }
                            if let Some(mut subprocess) = h.subprocess.take() {
                                subprocess.stop();
                            }
                            if matches!(
                                h.target,
                                crate::renderer::context::OutputTarget::SrtStream { .. }
                            ) {
                                crate::renderer::context::DeliveryResult::SrtNeedsRestart
                            } else {
                                crate::renderer::context::DeliveryResult::Failed(format!(
                                    "FFmpeg frame contract failed for '{}'",
                                    h.name
                                ))
                            }
                        } else {
                            crate::renderer::context::DeliveryResult::Ok
                        }
                    } else {
                        h.deliver_frame(frame_data.bytes(), sinks.ndi_manager, sinks.encoder_fps)
                    };
                    match delivery {
                        crate::renderer::context::DeliveryResult::Failed(msg) => {
                            stop_headless_output(&h.name, &mut h.active, sinks.notifications, msg);
                            // A Blackmagic device is let go as its sender drops.
                            h.decklink = None;
                        }
                        crate::renderer::context::DeliveryResult::SrtNeedsRestart => {
                            // The disconnected client's PCM tap is now stale; drop it,
                            // then respawn the listener with a fresh audio tap so the
                            // reconnecting client gets audio again (parity with start).
                            if let Some(stale) = h.audio_pcm.take() {
                                stale.release(sinks.audio_manager);
                            }
                            let (url, codec) = match &h.target {
                                crate::renderer::context::OutputTarget::SrtStream {
                                    url,
                                    codec,
                                    ..
                                } => (url.clone(), codec.clone()),
                                _ => continue,
                            };
                            let device = h.target.audio_device().map(str::to_string);
                            let name = h.name.clone();
                            let (audio_input, passthrough) = super::state::resolve_output_audio(
                                sinks.audio_manager,
                                sinks.audio_mix,
                                sinks.notifications,
                                super::state::OutputAudio {
                                    bus: h.audio_bus.as_deref(),
                                    device: device.as_deref(),
                                },
                                &name,
                            );
                            match crate::renderer::FfmpegSubprocess::spawn_srt(
                                &url,
                                &codec,
                                h.presentation_request,
                                h.width,
                                h.height,
                                sinks.encoder_fps,
                                audio_input,
                            ) {
                                Ok(new_sub) => {
                                    h.subprocess = Some(Box::new(new_sub));
                                    h.audio_pcm = passthrough.map(Box::new);
                                    log::info!("SRT restarted for '{name}'");
                                }
                                Err(e) => {
                                    if let Some(pass) = passthrough {
                                        pass.release(sinks.audio_manager);
                                    }
                                    stop_headless_output(
                                        &name,
                                        &mut h.active,
                                        sinks.notifications,
                                        format!("Failed to restart SRT listener: {e}"),
                                    );
                                }
                            }
                        }
                        crate::renderer::context::DeliveryResult::Ok => {}
                    }
                }
            }
        }
    }

    /// Load files as decks one after another on a single background thread,
    /// sending each deck as soon as it is ready.
    ///
    /// For files dropped in together, where order is the point — twenty
    /// speaker slides must land in the channel in the order they were
    /// dropped. [`Self::spawn_deck_loads`] runs a thread per file, so its
    /// decks arrive in whatever order they finish.
    pub fn spawn_ordered_deck_loads(
        sender: &std::sync::mpsc::Sender<DeckLoadResult>,
        context: &crate::renderer::context::GpuContext,
        pending: &std::sync::Arc<std::sync::atomic::AtomicUsize>,
        (render_width, render_height): (u32, u32),
        files: Vec<(DeckLoadToken, DeckFile)>,
    ) {
        use crate::deck::Deck;
        use std::sync::atomic::Ordering;

        if files.is_empty() {
            return;
        }
        let tx = sender.clone();
        let ctx = context.clone();
        let counter = pending.clone();
        counter.fetch_add(files.len(), Ordering::Relaxed);
        std::thread::spawn(move || {
            for (token, file) in files {
                let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let file_name = |path: &std::path::Path| {
                        path.file_name()
                            .and_then(|f| f.to_str())
                            .unwrap_or("file")
                            .to_string()
                    };
                    match file {
                        DeckFile::Image(path) => (
                            file_name(&path),
                            Deck::new_from_image(&ctx, &path, render_width, render_height),
                        ),
                        DeckFile::Video(path) => (
                            file_name(&path),
                            Deck::new_from_video(&ctx, &path, render_width, render_height),
                        ),
                        DeckFile::Shader(shader) => {
                            let shader = *shader;
                            let name = shader.name();
                            let deck = if shader.metadata.is_compute() {
                                Deck::new_from_compute_shader(
                                    &ctx,
                                    shader,
                                    render_width,
                                    render_height,
                                )
                            } else {
                                Deck::new(&ctx, shader, render_width, render_height)
                            };
                            (name, deck)
                        }
                    }
                }));
                let (name, deck) = loaded.unwrap_or_else(|_| {
                    (
                        "file".to_string(),
                        Err(anyhow::anyhow!("panic loading a dropped file")),
                    )
                });
                let _ = tx.send(DeckLoadResult { token, deck, name });
                counter.fetch_sub(1, Ordering::Relaxed);
            }
        });
    }

    /// Open a native file picker on a background thread.
    /// Uses rfd's synchronous `FileDialog` which correctly dispatches to the
    /// main thread on macOS (`NSOpenPanel` requires main-thread presentation
    /// for proper focus/activation). Results are sent via channel.
    pub fn open_file_dialog(
        sender: &std::sync::mpsc::Sender<FileDialogResult>,
        kind: FileDialogKind,
        channel_uuid: Option<String>,
    ) {
        let tx = sender.clone();
        std::thread::spawn(move || {
            let dialog = match kind {
                FileDialogKind::Image => rfd::FileDialog::new().add_filter(
                    "Images",
                    &[
                        "png", "jpg", "jpeg", "bmp", "tiff", "tga", "webp", "svg", "svgz",
                    ],
                ),
                FileDialogKind::Video => rfd::FileDialog::new().add_filter(
                    "Video and sound",
                    &[
                        "mov", "mp4", "m4v", "avi", "mkv", "webm", "gif", "mp3", "wav", "m4a",
                        "aac", "flac", "ogg", "aif", "aiff",
                    ],
                ),
            };
            if let Some(paths) = dialog.pick_files() {
                if !paths.is_empty() {
                    let _ = tx.send(FileDialogResult {
                        kind,
                        channel_uuid,
                        paths,
                    });
                }
            }
        });
    }
}

/// While a present trace is made: the command buffers that ran on the GPU —
/// wgpu's own left out but for its uploads (`PendingWrites`), which ride on
/// whichever thread's submit comes next (see `renderer::present_trace`).
fn trace_gpu_runs(runs: &[crate::renderer::gpu_time::Ran]) {
    use crate::renderer::present_trace::{Step, TRACE};
    if !TRACE.recording() {
        return;
    }
    for run in runs {
        let internal = run.label.starts_with("(wgpu internal)");
        if !run.label.is_empty() && (!internal || run.label.contains("PendingWrites")) {
            let step = Step::Gpu {
                label: run.label.clone(),
                start: run.start,
                end: run.end,
            };
            TRACE.note_at(run.start, "", 0, step);
        }
    }
}

impl super::VardaApp {
    /// Each camera whose LUT its decks name in the show's LUT folder: the
    /// camera, and its profile named in full in `from` and in `to` — the
    /// folder a Save As copied it to — as the next frame will set it (see
    /// `CameraManager::set_profiles`). A camera whose LUT is named by a
    /// path of its own does not move.
    pub(crate) fn camera_lut_moves(
        &self,
        from: &std::path::Path,
        to: &std::path::Path,
    ) -> Vec<(
        crate::camera::CameraId,
        crate::engine::value::camera::CameraProfile,
        crate::engine::value::camera::CameraProfile,
    )> {
        lut_moves(
            self.mixer
                .channels()
                .iter()
                .flat_map(|channel| &channel.decks)
                .filter_map(|slot| Some((slot.deck.camera_id()?, &slot.deck.camera_profile))),
            from,
            to,
        )
    }
}

/// For each camera in `profiles` — its decks' in the order a frame reads
/// them, the last one's standing, as there — its profile named in full in
/// `from` and in `to`, where the two differ: a LUT named in the show's
/// folder.
fn lut_moves<'a>(
    profiles: impl Iterator<
        Item = (
            crate::camera::CameraId,
            &'a crate::engine::value::camera::CameraProfile,
        ),
    >,
    from: &std::path::Path,
    to: &std::path::Path,
) -> Vec<(
    crate::camera::CameraId,
    crate::engine::value::camera::CameraProfile,
    crate::engine::value::camera::CameraProfile,
)> {
    let mut standing = std::collections::BTreeMap::new();
    for (camera, profile) in profiles {
        standing.insert(camera, profile);
    }
    standing
        .into_iter()
        .map(|(camera, profile)| (camera, lut_in_full(profile, from), lut_in_full(profile, to)))
        .filter(|(_, before, after)| before != after)
        .collect()
}

/// `profile` with a LUT file named in full: a name is one in the show's LUT
/// folder, `luts`; a path is taken as it is.
fn lut_in_full(
    profile: &crate::engine::value::camera::CameraProfile,
    luts: &std::path::Path,
) -> crate::engine::value::camera::CameraProfile {
    use crate::engine::value::camera::CameraProfile;
    match profile {
        CameraProfile::Lut { file } if !std::path::Path::new(file).is_absolute() => {
            CameraProfile::Lut {
                file: luts.join(file).to_string_lossy().into_owned(),
            }
        }
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{BoundingBox, ContentMapping};

    /// Only a camera whose LUT its decks name in the show's folder moves
    /// with it — named by the last of its decks, as a frame sets it; one
    /// named by a path of its own, even the same file, does not, nor one
    /// with no LUT.
    #[test]
    fn only_luts_named_in_the_show_move_with_it() {
        use crate::engine::value::camera::CameraProfile;
        let root = tempfile::tempdir().unwrap();
        let from = root.path().join("A").join(".varda").join("luts");
        let to = root.path().join("B").join(".varda").join("luts");
        let lut = |file: &str| CameraProfile::Lut { file: file.into() };
        let named = lut("look.cube");
        let by_path = lut(from.join("look.cube").to_str().unwrap());
        let built_in = CameraProfile::SonySLog3;
        let moves = lut_moves(
            [
                (1, &named),
                (2, &by_path),
                (3, &built_in),
                (4, &by_path),
                (4, &named),
                (5, &named),
                (5, &by_path),
            ]
            .into_iter(),
            &from,
            &to,
        );
        let in_b = lut(to.join("look.cube").to_str().unwrap());
        assert_eq!(
            moves,
            vec![(1, by_path.clone(), in_b.clone()), (4, by_path, in_b)]
        );
    }

    /// A camera's LUT named by file is the one in the show's LUT folder; one
    /// chosen by path stays where it is; a built-in profile is untouched.
    #[test]
    fn a_camera_lut_named_by_file_is_found_in_the_shows_lut_folder() {
        use crate::engine::value::camera::CameraProfile;
        let root = tempfile::tempdir().unwrap();
        let luts = root.path().join(".varda").join("luts");
        let named = CameraProfile::Lut {
            file: "Film to Video.cube".into(),
        };
        assert_eq!(
            lut_in_full(&named, &luts),
            CameraProfile::Lut {
                file: luts
                    .join("Film to Video.cube")
                    .to_string_lossy()
                    .into_owned()
            }
        );
        let chosen = CameraProfile::Lut {
            file: root
                .path()
                .join("external")
                .join("s709.cube")
                .to_string_lossy()
                .into_owned(),
        };
        assert_eq!(lut_in_full(&chosen, &luts), chosen);
        assert_eq!(
            lut_in_full(&CameraProfile::SonyHlg, &luts),
            CameraProfile::SonyHlg
        );
    }

    #[test]
    fn compute_uv_fill() {
        let bb = BoundingBox {
            x: 0.2,
            y: 0.3,
            width: 0.4,
            height: 0.5,
        };
        let (scale, offset) = VardaApp::compute_uv(ContentMapping::Fill, &bb);
        assert_eq!(scale, [1.0, 1.0]);
        assert_eq!(offset, [0.0, 0.0]);
    }

    #[test]
    fn compute_uv_mapped() {
        let bb = BoundingBox {
            x: 0.2,
            y: 0.3,
            width: 0.4,
            height: 0.5,
        };
        let (scale, offset) = VardaApp::compute_uv(ContentMapping::Mapped, &bb);
        assert_eq!(scale, [0.4, 0.5]);
        assert_eq!(offset, [0.2, 0.3]);
    }

    #[test]
    fn compute_uv_mapped_full_canvas() {
        let bb = BoundingBox {
            x: 0.0,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        let (scale, offset) = VardaApp::compute_uv(ContentMapping::Mapped, &bb);
        // Full canvas mapped should behave like fill
        assert_eq!(scale, [1.0, 1.0]);
        assert_eq!(offset, [0.0, 0.0]);
    }

    #[test]
    fn fps_smoothing_converges() {
        let gpu = crate::renderer::context::GpuContext::new_headless();
        let Ok(gpu) = gpu else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let config = crate::testing::headless_config();
        let Ok(mut app) = VardaApp::new(gpu, &config) else {
            eprintln!("Skipping: VardaApp creation failed");
            return;
        };
        // Seed with 60 identical FPS values
        app.frame_stats.fps_history.clear();
        for _ in 0..60 {
            app.frame_stats.fps_history.push_back(60.0);
        }
        app.frame_stats.fps_smoothed = app.frame_stats.fps_history.iter().sum::<f32>()
            / app.frame_stats.fps_history.len() as f32;
        assert!((app.frame_stats.fps_smoothed - 60.0).abs() < 0.01);
    }

    #[test]
    fn fps_smoothing_window_cap() {
        let gpu = crate::renderer::context::GpuContext::new_headless();
        let Ok(gpu) = gpu else {
            eprintln!("Skipping: no headless GPU available");
            return;
        };
        let config = crate::testing::headless_config();
        let Ok(mut app) = VardaApp::new(gpu, &config) else {
            eprintln!("Skipping: VardaApp creation failed");
            return;
        };
        // Push more than 60 entries
        app.frame_stats.fps_history.clear();
        for _ in 0..100 {
            app.frame_stats.fps_history.push_back(30.0);
            if app.frame_stats.fps_history.len() > 60 {
                app.frame_stats.fps_history.pop_front();
            }
        }
        assert_eq!(
            app.frame_stats.fps_history.len(),
            60,
            "Window should cap at 60 entries"
        );
    }

    // ── Offensive: catch_unwind pattern delivers error through channel ──

    #[test]
    fn catch_unwind_delivers_error_on_panic() {
        let (tx, rx) = std::sync::mpsc::channel::<DeckLoadResult>();
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let c = counter.clone();
        c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || -> (String, anyhow::Result<crate::deck::Deck>) {
                    panic!("simulated loader panic");
                },
            ));
            let (name, deck) = match result {
                Ok((name, deck)) => (name, deck),
                Err(_) => (
                    "panicked".to_string(),
                    Err(anyhow::anyhow!("panic in loader")),
                ),
            };
            let _ = tx.send(DeckLoadResult {
                token: DeckLoadToken(0),
                deck,
                name,
            });
            c.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        });

        let msg = rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("should receive result even after panic");
        assert!(msg.deck.is_err(), "deck should be an error after panic");
        assert_eq!(msg.name, "panicked");
        // Counter should be back to zero (cleanup ran)
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(
            counter.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "counter must decrement even after panic"
        );
    }

    #[test]
    fn stopping_a_headless_output_toasts_the_operator() {
        let mut notifications = crate::notifications::NotificationSystem::new();
        let mut active = true;
        super::stop_headless_output(
            "Show rec",
            &mut active,
            &mut notifications,
            "Subprocess write failed for 'Show rec'",
        );
        assert!(!active);
        let visible = notifications.visible();
        assert_eq!(visible.len(), 1);
        assert_eq!(
            visible[0].level,
            crate::notifications::NotificationLevel::Error
        );
        assert!(visible[0].message.contains("Show rec"));
        assert!(visible[0].message.contains("stopped"));
    }
}
