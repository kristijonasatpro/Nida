//! `UIRunner` — windowed delivery layer for the Varda engine.
//!
//! Owns the window, egui state, blit pipeline, texture registrations, and `WindowSurface`.
//! The engine (`VardaApp`) is owned here and driven each frame.
//! For headless operation (HTTP API, CLI), this module is simply not used.

use crate::app::render::{DeckLoadResult, FileDialogKind, FileDialogResult};
use crate::app::{AppConfig, VardaApp};
use crate::renderer::blit::BlitPipeline;
use crate::renderer::context::{GpuContext, WindowSurface};
use crate::usecases::ui;

use winit::{
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

mod camera_detect;
mod deck_load;
mod detect;
mod drag_position;
mod engine_thread;
mod event_loop;
mod preview;
mod shows;
mod vsync;

pub(crate) use event_loop::WindowHost;

use preview::PreviewEncoder;

use deck_load::{apply_deck_texture_outcomes, register_deck_preview_texture, DeckLoadTargets};
use detect::{spawn_detect_thread, DetectRequest, DetectResponse};

// The flags are unrelated (a frame due, a detection under way, a gesture, the
// show saved on the way out), not one state enum.
#[allow(clippy::struct_excessive_bools)]
pub struct UIRunner {
    // ── Session config (CLI flags + workspace defaults) ──────────────
    config: AppConfig,

    // ── Window / egui state (delivery layer) ────────────────────────
    window: Option<&'static Window>,
    window_surface: Option<WindowSurface>,
    blit_pipeline: Option<BlitPipeline>,
    egui_ctx: egui::Context,
    egui_state: Option<egui_winit::State>,
    egui_renderer: Option<egui_wgpu::Renderer>,
    preview_encoder: Option<PreviewEncoder>,
    /// The textures this frame's UI draws, tessellated: the previews to
    /// encode (see `encode_previews`).
    textures_drawn: std::collections::HashSet<egui::TextureId>,
    /// The largest each texture was drawn at, in pixels: the size its
    /// preview is made (see `PreviewEncoder::target_size`).
    drawn_px: std::collections::HashMap<egui::TextureId, [u32; 2]>,
    deck_preview_textures: std::collections::HashMap<String, egui::TextureId>,
    /// A folder picker for recordings, open on its own thread.
    record_folder_pick: Option<std::sync::mpsc::Receiver<Option<std::path::PathBuf>>>,
    /// A Show menu dialog (Open…, Save As…), open on its own thread.
    show_pick: Option<shows::PendingPick>,
    /// Wakes the event loop from other threads (the engine thread, after
    /// each frame).
    proxy: Option<winit::event_loop::EventLoopProxy<()>>,
    /// The engine thread has drawn a frame the control window has not shown.
    ui_due: bool,
    /// When the control window last drew a frame.
    last_ui_frame: Option<std::time::Instant>,
    /// Where the control window's frames' time goes (see `app::frame_parts`).
    ui_parts: crate::app::frame_parts::FrameParts,
    /// Control window frames drawn.
    ui_frames: u32,
    /// What the operator did in a frame the engine could not be had in time
    /// to apply it: applied at the next frame.
    stashed_actions: Option<ui::UIActions>,
    /// When the engine was last not had in time, for a report now and then.
    engine_late_reported: Option<std::time::Instant>,
    /// Test hook: the control window freezes this long once a second —
    /// holding the engine, or not (see [`UiStall`]).
    ui_stall: Option<UiStall>,
    last_ui_stall: Option<std::time::Instant>,
    /// How long each frame is held on the control window, as last set on
    /// its surface.
    ui_frame_hold: Option<f64>,
    /// How evenly the control window's frames reached its screen.
    ui_on_screen: crate::renderer::on_screen::OnScreen,
    /// The control window's frames waiting to be shown, and the most seen
    /// since the last report.
    ui_queue: crate::renderer::on_screen::Waiting,
    ui_waiting_most: u64,
    /// The control window's screen held at the show's rate (see
    /// `crate::screen_rate`).
    screen_rate: crate::screen_rate::ScreenRate,
    night_shift: crate::night_shift::NightShift,
    /// Shows opened lately, for the Show menu.
    recent_shows: std::sync::Arc<Vec<std::path::PathBuf>>,
    /// The show folder the window is named after (see `show_changed`).
    named_after: std::path::PathBuf,
    /// Where the Recent list is kept (see `shows::recent_file`).
    recent_file: Option<std::path::PathBuf>,
    /// The saved layouts' writes on their way.
    layout_writes: LayoutWrites,
    channel_preview_textures: std::collections::HashMap<usize, egui::TextureId>,
    output_preview_textures: std::collections::HashMap<usize, egui::TextureId>,
    main_output_texture: Option<egui::TextureId>,
    dome_preview_renderer: Option<crate::renderer::dome_preview::DomePreviewRenderer>,
    dome_preview_texture: Option<egui::TextureId>,
    // Camera detection mode state
    camera_detect_texture: Option<egui::TextureId>,
    camera_detect_camera_id: Option<crate::camera::CameraId>,
    camera_detect_contours: Vec<crate::surface::detect::DetectedContour>,
    // Background detection thread channels
    detect_req_tx: std::sync::mpsc::Sender<DetectRequest>,
    detect_res_rx: std::sync::mpsc::Receiver<DetectResponse>,
    detect_in_flight: bool,
    main_window_id: Option<WindowId>,

    // ── UI-consumer-owned layout/selection state ─────────────────────
    layout: super::UILayoutState,
    /// What the selection was on, by UUID, as the last frame ended: found
    /// again as the next starts, wherever the API has moved it meanwhile.
    selection_anchor: super::state::SelectionAnchor,
    /// The channels this frame's UI was drawn from: what its clicks'
    /// positions name (see [`settle_selection`]).
    frame_order: super::state::ChannelOrder,
    /// The operator's saved screen layouts, kept for the user.
    saved_layouts: crate::usecases::ui::screen_layout::SavedLayoutStore,
    /// Choices made in windows, kept for the user across shows and runs.
    window_choices: crate::usecases::ui::window_choices::WindowChoiceStore,

    // ── File dialog channel (async, non-blocking) ─────────────────────
    file_dialog_tx: std::sync::mpsc::Sender<FileDialogResult>,
    file_dialog_rx: std::sync::mpsc::Receiver<FileDialogResult>,

    // ── Background deck loading channel (async, non-blocking) ────────
    deck_load_tx: std::sync::mpsc::Sender<DeckLoadResult>,
    deck_load_rx: std::sync::mpsc::Receiver<DeckLoadResult>,
    /// Number of deck loads currently in-flight on background threads
    pending_deck_loads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    deck_load_targets: DeckLoadTargets,
    /// Inputs made for files still loading, each removed again if its file
    /// fails to open.
    inputs_awaiting_files: std::collections::HashSet<String>,

    // ── Engine (created after GPU init in resumed()) ─────────────────
    /// The engine, while this thread holds it: always when headless; in the
    /// windowed app, only while the control window borrows it from the
    /// engine thread (see [`engine_thread`]).
    varda: Option<Box<VardaApp>>,
    /// The engine thread, in the windowed app.
    engine: Option<engine_thread::EngineThread>,
    /// The GPU, for the control window to draw with while the engine is on
    /// its own thread.
    gpu: Option<GpuContext>,

    // ── Deferred GPU init (avoids Metal dispatch-queue deadlock on Rosetta/Intel) ──
    gpu_init_handle: Option<std::thread::JoinHandle<anyhow::Result<(GpuContext, WindowSurface)>>>,
    startup_t0: Option<std::time::Instant>,

    // ── Undo/redo ─────────────────────────────────────────────────────
    // The undo/redo timeline itself lives on `VardaApp` (shared with the
    // HTTP/headless command bus). The runner only tracks gesture edges so a
    // continuous stage/warp drag collapses into one undo step.
    /// Previous frame's `gesture_active` flag, for detecting drag start vs.
    /// continuation so a continuous stage/warp drag collapses into one undo step.
    prev_gesture_active: bool,

    // ── Performance: gate publish_state to reduce snapshot overhead ──
    publish_counter: u32,

    // ── HTTP API server (background thread) ──────────────────────────
    api_handle: Option<crate::usecases::api::runner::ApiServerHandle>,

    // ── Adaptive frame pacing (windowed + headless) ────────────────
    /// The ideal start time for the next frame. Advances by `frame_budget`
    /// each frame to maintain a steady cadence. When a frame overshoots its
    /// budget, the anchor snaps forward to `now + budget` to avoid catch-up
    /// bursts.
    cadence_anchor: Option<std::time::Instant>,

    // ── Signal-driven shutdown (SIGINT/SIGTERM) ─────────────────────
    shutdown_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set once the windowed app has saved the show on its way out (see
    /// `shut_down`), so the window's close and the loop's end save it once.
    shut_down: bool,

    // ── Cached window geometry (avoids XGetGeometry round-trip per frame) ──
    // winit 0.30's Window::inner_size() on X11 issues a synchronous XGetGeometry
    // request every call. egui_winit::State::take_egui_input() calls inner_size()
    // unconditionally, causing a blocking X11 round-trip each frame. We cache
    // the size here, updated from Resized/ScaleFactorChanged events, and bypass
    // take_egui_input() to avoid the stall.
    egui_start_time: std::time::Instant,
    cached_screen_size: winit::dpi::PhysicalSize<u32>,
    cached_scale_factor: f32,
}

impl UIRunner {
    pub fn new(config: AppConfig) -> Self {
        let (file_dialog_tx, file_dialog_rx) = std::sync::mpsc::channel();
        let (deck_load_tx, deck_load_rx) = std::sync::mpsc::channel();
        let (detect_req_tx, detect_req_rx) = std::sync::mpsc::channel();
        let detect_res_rx = spawn_detect_thread(detect_req_rx);
        Self {
            config,
            window: None,
            window_surface: None,
            blit_pipeline: None,
            egui_ctx: {
                let ctx = egui::Context::default();
                ui::one_pass_a_frame(&ctx);
                ui::always_dark(&ctx);
                ctx
            },
            egui_state: None,
            egui_renderer: None,
            preview_encoder: None,
            textures_drawn: std::collections::HashSet::new(),
            drawn_px: std::collections::HashMap::new(),
            deck_preview_textures: std::collections::HashMap::new(),
            record_folder_pick: None,
            show_pick: None,
            proxy: None,
            ui_due: false,
            last_ui_frame: None,
            ui_parts: crate::app::frame_parts::FrameParts::default(),
            ui_frames: 0,
            stashed_actions: None,
            engine_late_reported: None,
            ui_stall: UiStall::from_env(),
            last_ui_stall: None,
            ui_frame_hold: None,
            ui_on_screen: crate::renderer::on_screen::OnScreen::default(),
            ui_queue: crate::renderer::on_screen::Waiting::default(),
            ui_waiting_most: 0,
            screen_rate: crate::screen_rate::ScreenRate::default(),
            night_shift: crate::night_shift::NightShift::default(),
            recent_shows: std::sync::Arc::default(),
            named_after: std::path::PathBuf::new(),
            // A test's runner never reads or writes this computer's Recent
            // list, saved layouts or window choices: a first frame names the
            // window after its scratch show, which put that show at the head
            // of the operator's list for Nida to reopen.
            recent_file: if cfg!(test) {
                None
            } else {
                crate::app::shows::recent_file()
            },
            layout_writes: LayoutWrites::default(),
            channel_preview_textures: std::collections::HashMap::new(),
            output_preview_textures: std::collections::HashMap::new(),
            main_output_texture: None,
            dome_preview_renderer: None,
            dome_preview_texture: None,
            camera_detect_texture: None,
            camera_detect_camera_id: None,
            camera_detect_contours: Vec::new(),
            detect_req_tx,
            detect_res_rx,
            detect_in_flight: false,
            main_window_id: None,
            layout: super::UILayoutState::default(),
            selection_anchor: super::state::SelectionAnchor::default(),
            frame_order: super::state::ChannelOrder::default(),
            saved_layouts: if cfg!(test) {
                crate::usecases::ui::screen_layout::SavedLayoutStore::default()
            } else {
                crate::usecases::ui::screen_layout::SavedLayoutStore::open_default()
            },
            window_choices: if cfg!(test) {
                crate::usecases::ui::window_choices::WindowChoiceStore::default()
            } else {
                crate::usecases::ui::window_choices::WindowChoiceStore::open_default()
            },
            file_dialog_tx,
            file_dialog_rx,
            deck_load_tx,
            deck_load_rx,
            pending_deck_loads: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            deck_load_targets: DeckLoadTargets::default(),
            inputs_awaiting_files: std::collections::HashSet::new(),
            varda: None,
            engine: None,
            gpu: None,
            gpu_init_handle: None,
            startup_t0: None,
            prev_gesture_active: false,
            publish_counter: 0,
            api_handle: None,
            cadence_anchor: None,
            shutdown_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            shut_down: false,
            egui_start_time: std::time::Instant::now(),
            cached_screen_size: winit::dpi::PhysicalSize::new(0, 0),
            cached_scale_factor: 1.0,
        }
    }

    /// A saved layout whose file did not land is not kept: the menu as the
    /// file has it, read again here, on the main thread with the engine
    /// not held — the outputs do not wait for it.
    fn take_lost_layouts(&mut self) {
        use std::sync::atomic::Ordering;
        // Not while one is on its way: it will say again when it lands.
        let writes = &self.layout_writes;
        if writes.on_way.load(Ordering::SeqCst) == 0 && writes.lost.swap(false, Ordering::SeqCst) {
            self.saved_layouts.reload();
        }
    }

    /// The panel arrangement the frame left, and saving or forgetting a
    /// named layout — which writes the user's layouts file, so it happens
    /// here rather than in the UI.
    fn apply_screen_layout_actions(&mut self, ui_actions: &mut ui::UIActions) {
        let session = &mut ui_actions.session;
        // A fold is kept with the workspace too.
        let mut changed = session.toggle_collapsed_channel.is_some();
        if let Some(screen) = session.screen_layout.take() {
            let old = &self.layout.screen;
            changed = screen.name != old.name
                || screen.locked != old.locked
                || !crate::usecases::ui::screen_layout::same_arrangement(&screen.dock, &old.dock);
            self.layout.screen = screen;
        }
        // The files below are written in the background, as a save is: the
        // outputs do not wait for the disk (see `persistence::writer`).
        // Said once the file is there.
        if let Some(name) = session.save_layout_as.take() {
            let varda = self.varda.as_deref_mut();
            let said = ui::screen_layout::valid_name(&name)
                .ok()
                .map(|saved| format!("Saved layout \"{saved}\""));
            let writes = self.layout_writes.clone();
            let result =
                self.saved_layouts
                    .save_as_with(&name, &self.layout.screen.dock, |path, text| {
                        let failed = ui::screen_layout::not_saved(path);
                        let then = Some(writes.landing());
                        keep_file(varda, "the saved layouts", path, text, said, failed, then)
                    });
            match result {
                Ok(saved) => {
                    self.layout.screen.name = saved;
                    changed = true;
                }
                Err(e) => {
                    if let Some(varda) = self.varda.as_mut() {
                        varda.notify_error(e);
                    }
                }
            }
        }
        if let Some(name) = session.delete_saved_layout.take() {
            let varda = self.varda.as_deref_mut();
            let said = Some(format!("Deleted layout \"{name}\""));
            let writes = self.layout_writes.clone();
            let result = self.saved_layouts.delete_with(&name, |path, text| {
                let failed = ui::screen_layout::not_saved(path);
                let then = Some(writes.landing());
                keep_file(varda, "the saved layouts", path, text, said, failed, then)
            });
            if let (Err(e), Some(varda)) = (result, self.varda.as_mut()) {
                varda.notify_error(e);
            }
        }
        if let Some(choices) = session.window_choices.take() {
            let varda = self.varda.as_deref_mut();
            let kept = self.window_choices.set_with(choices, |path, text| {
                let failed = crate::usecases::ui::window_choices::not_kept(path);
                keep_file(
                    varda,
                    "the windows' choices",
                    path,
                    text,
                    None,
                    failed,
                    None,
                )
            });
            if let Err(e) = kept {
                log::warn!("{e}");
                if let Some(varda) = self.varda.as_mut() {
                    varda.notify_error(e);
                }
            }
        }
        // A save through the API carries no layout; it uses this one.
        if changed {
            if let Some(varda) = self.varda.as_mut() {
                varda.remember_layout(&self.layout);
            }
        }
    }

    /// Run the UI event loop. Blocks until the window is closed.
    ///
    /// # Errors
    ///
    /// Returns an error if the winit event loop cannot be created (no display
    /// server, or one already exists on this thread), or if the event loop
    /// itself terminates with an error.
    pub fn run(mut self) -> anyhow::Result<()> {
        crate::streaming::try_encoders_meanwhile();
        // Install Ctrl-C handler for graceful shutdown (especially useful in headless)
        let flag = self.shutdown_flag.clone();
        let _ = ctrlc::set_handler(move || {
            log::info!("Received interrupt signal, shutting down...");
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        let event_loop = EventLoop::new()?;
        self.proxy = Some(event_loop.create_proxy());
        event_loop
            .run_app(&mut self)
            .map_err(|e| anyhow::anyhow!("Event loop error: {e:?}"))?;
        Ok(())
    }
}

/// Hand a file of Nida's own to the show's writer (see
/// `VardaApp::keep_file`) — or, with no engine to hand it to, write it now.
fn keep_file(
    varda: Option<&mut VardaApp>,
    label: &str,
    path: &std::path::Path,
    text: String,
    said: Option<String>,
    failed: String,
    then: Option<crate::persistence::writer::Then>,
) -> Result<(), String> {
    let Some(varda) = varda else {
        return path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .map_err(anyhow::Error::from)
            .and_then(|()| crate::persistence::atomic_write(path, &text))
            .map_err(|e| format!("{failed}: {e}"));
    };
    varda.keep_file(label, path, text, said, failed, then);
    Ok(())
}

/// The saved layouts' writes on their way: how many, and whether the last
/// to land did not — the menu is then read again from the file, once none
/// is on its way (see `UIRunner::take_lost_layouts`).
#[derive(Clone, Default)]
struct LayoutWrites {
    on_way: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl LayoutWrites {
    /// One more on its way; what to tell once it has landed. The last to
    /// land decides: one that failed leaves the file as it was; one that
    /// landed wrote the whole list as it is here, an earlier failure's
    /// change and all.
    fn landing(&self) -> crate::persistence::writer::Then {
        use std::sync::atomic::Ordering;
        self.on_way.fetch_add(1, Ordering::SeqCst);
        let writes = self.clone();
        Box::new(move |landed| {
            if writes.on_way.fetch_sub(1, Ordering::SeqCst) == 1 {
                writes
                    .lost
                    .store(!landed.errors.is_empty(), Ordering::SeqCst);
            }
        })
    }
}

/// Each `MoveChannel` among `commands`: the channel, where it is, and where
/// it will be once moved.
fn channel_moves(
    varda: &VardaApp,
    commands: &[crate::engine::EngineCommand],
) -> Vec<(String, usize, usize)> {
    let mut order: Vec<String> = varda
        .mixer_ref()
        .channels()
        .iter()
        .map(|c| c.uuid().to_string())
        .collect();
    let mut moves = Vec::new();
    for command in commands {
        if let crate::engine::EngineCommand::MoveChannel {
            channel_uuid,
            before_channel_uuid,
        } = command
        {
            if let Some((from, to)) =
                super::state::move_target(&order, channel_uuid, before_channel_uuid.as_deref())
            {
                let uuid = order.remove(from);
                order.insert(to, uuid);
                moves.push((channel_uuid.clone(), from, to));
            }
        }
    }
    moves
}

/// The selection kept on what the operator had selected — `selected`, by
/// UUID from the channels the frame was drawn from — wherever that is in
/// the show now. Held by position, an undo or redo, a layer moved into
/// another input, or the API reordering channels while the frame was drawn
/// left it on whichever channel took that place, and the next take sent
/// that one.
fn settle_selection(
    layout: &mut super::UILayoutState,
    selected: &super::state::SelectionAnchor,
    varda: &VardaApp,
) {
    layout.reanchor(
        selected,
        &super::state::ChannelOrder::of(varda.mixer_ref().channels()),
    );
}

/// Whether this frame's mutations deserve their own undo entry.
///
/// A held drag reports a mutation on every frame it moves, so only the frame
/// that *starts* the gesture takes a snapshot. Without this a single region drag
/// would fill the fifty-deep history by itself and undo would rewind the show a
/// pixel at a time.
fn wants_history_snapshot(
    prev_gesture_active: &mut bool,
    dirty: bool,
    gesture_active: bool,
) -> bool {
    let continuation = gesture_active && *prev_gesture_active;
    *prev_gesture_active = gesture_active;
    dirty && !continuation
}

impl UIRunner {
    /// Complete initialization once GPU context is available.
    /// Called from `resumed()` for headless or from `about_to_wait()` for windowed.
    fn finish_init(
        &mut self,
        gpu: GpuContext,
        win_surface: Option<WindowSurface>,
        startup_t0: std::time::Instant,
        event_loop: &ActiveEventLoop,
    ) {
        // Set up egui + blit pipeline for windowed mode
        if let (Some(window_static), Some(ws)) = (self.window, &win_surface) {
            self.cached_screen_size = window_static.inner_size();
            self.cached_scale_factor = window_static.scale_factor() as f32;
            self.egui_start_time = std::time::Instant::now();
            self.blit_pipeline = BlitPipeline::new(&gpu.device, ws.surface_config.format).ok();
            self.egui_state = Some(egui_winit::State::new(
                self.egui_ctx.clone(),
                egui::ViewportId::ROOT,
                window_static,
                Some(window_static.scale_factor() as f32),
                None,
                Some(2 * 1024),
            ));
            self.egui_renderer = Some(egui_wgpu::Renderer::new(
                &gpu.device,
                ws.surface_config.format,
                egui_wgpu::RendererOptions::default(),
            ));

            // Set the application icon on egui's viewport (controls dock/taskbar icon)
            {
                static ICON_BYTES: &[u8] = include_bytes!("../../../../assets/icon.png");
                if let Ok(img) = image::load_from_memory(ICON_BYTES) {
                    let rgba = img.into_rgba8();
                    let icon_data = egui::IconData {
                        rgba: rgba.as_raw().clone(),
                        width: rgba.width(),
                        height: rgba.height(),
                    };
                    self.egui_ctx
                        .send_viewport_cmd(egui::ViewportCommand::Icon(Some(std::sync::Arc::new(
                            icon_data,
                        ))));
                }
            }
        }
        if let Some(ws) = win_surface {
            self.window_surface = Some(ws);
        }

        // Create engine now that GPU is ready
        log::info!("[STARTUP] Creating engine (audio, MIDI, shaders, mixer)...");
        let mut varda = match VardaApp::new(gpu, &self.config) {
            Ok(mut v) => {
                self.share_shutdown(&mut v);
                Box::new(v)
            }
            Err(e) => {
                log::error!("Failed to initialize engine: {e}");
                event_loop.exit();
                return;
            }
        };
        log::info!(
            "[STARTUP] Engine ready: {} shaders ({:.0?})",
            varda.shader_count(),
            startup_t0.elapsed()
        );
        // What the first REC or STREAM draws with, made ahead (see
        // `VardaApp::make_recording_ahead`).
        varda.make_recording_ahead();

        // The show open last time, unless the command line names one: the
        // app bundle names the show to start with, not the one to keep.
        self.recent_shows = std::sync::Arc::new(
            self.recent_file
                .as_deref()
                .map_or_else(Vec::new, crate::app::shows::recent_shows_in),
        );
        if !self.config.headless && !workspace_on_command_line() {
            if let Some(last) = self.recent_shows.first().cloned() {
                if last != varda.show_folder() {
                    log::info!("[STARTUP] Reopening the last show: {}", last.display());
                    varda.start_in_show(&last);
                }
            }
        }

        // Load workspace (may replace default mixer with saved scene)
        log::info!("[STARTUP] Loading workspace...");
        let loaded = varda.load_workspace();
        if let Some(loaded_layout) = loaded.layout {
            self.layout = loaded_layout;
            self.saved_layouts.reconnect(&mut self.layout.screen);
        }
        // `load_workspace` clears the engine-owned undo/redo timeline.
        log::info!("[STARTUP] Workspace loaded ({:.0?})", startup_t0.elapsed());

        // Start HTTP API server on background thread
        if self.api_handle.is_none() {
            self.api_handle = crate::usecases::api::runner::start(
                self.config.api_port,
                varda.command_sender(),
                varda.state_reader(),
            );
            // The log says why, but an app opened from the Finder logs to a
            // file no one is watching: the window says it too, once.
            if self.api_handle.is_none() && !self.config.headless {
                varda.session.notifications.notify_once(
                    "api-off",
                    crate::notifications::NotificationLevel::Warning,
                    api_off(self.config.api_port),
                );
            }
        }

        self.gpu = Some(varda.gpu_context().clone());
        // The show it starts in moves nothing (see `show_moved`).
        self.named_after = varda.show_folder().to_path_buf();
        self.varda = Some(varda);

        // Register GPU textures with egui for previews (windowed only)
        if !self.config.headless {
            self.register_preview_textures();
            // A show named on the command line (a test, a one-off) is not
            // one to come back to next time.
            if !workspace_on_command_line() {
                self.show_changed();
            }
            // Every channel's picture is a tile in the control window: kept
            // drawn, now and then, while off air.
            if let Some(varda) = self.varda.as_mut() {
                varda.set_ui_tiles(true);
            }
            // From here the engine runs on a thread of its own, waking the
            // control window after each frame it draws.
            if let (Some(varda), Some(proxy), Some(gpu)) =
                (self.varda.take(), self.proxy.clone(), self.gpu.clone())
            {
                let proxy = std::sync::Mutex::new(proxy);
                self.engine = Some(engine_thread::EngineThread::start(varda, gpu, move || {
                    if let Ok(proxy) = proxy.lock() {
                        let _ = proxy.send_event(());
                    }
                }));
            }
        }
        log::info!(
            "[STARTUP] Initialization complete ({:.0?})",
            startup_t0.elapsed()
        );
    }

    /// Advance the cadence anchor after a frame completes (render or headless).
    ///
    /// The anchor represents the ideal start time for the next frame. Each call
    /// advances it by one frame budget to maintain a steady cadence. If the
    /// frame overshot its budget (anchor is already in the past), the anchor
    /// snaps forward to `now + budget` instead of trying to catch up — this
    /// prevents the burst-pause pattern where multiple short frames fire in
    /// rapid succession after one long frame.
    fn advance_cadence_anchor(&mut self, target_fps: u32) {
        self.cadence_anchor = engine_thread::next_cadence_anchor(
            self.cadence_anchor,
            target_fps,
            std::time::Instant::now(),
        );
    }

    /// The API's Shutdown and Ctrl-C raise the one flag the loop reads,
    /// windowed or headless (see `about_to_wait`).
    fn share_shutdown(&self, varda: &mut VardaApp) {
        varda.shutdown_requested = std::sync::Arc::clone(&self.shutdown_flag);
    }

    /// Headless render loop — engine processing without UI/egui.
    fn render_headless(&mut self, host: &dyn WindowHost) {
        // The frame's slot on the even cadence the loop sleeps to.
        let slot = self.cadence_anchor;
        let Some(varda) = self.varda.as_mut() else {
            return;
        };

        // Check for shutdown request (from API or SIGINT/SIGTERM)
        if varda
            .shutdown_requested
            .load(std::sync::atomic::Ordering::Relaxed)
            || self
                .shutdown_flag
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            log::info!("Shutdown requested, saving workspace and exiting...");
            if let Err(e) = varda.save_workspace(&self.layout) {
                log::error!("{e}");
            }
            varda.refuse_commands();
            if let Some(api) = self.api_handle.take() {
                api.shutdown();
            }
            if let Some(kept) = self.night_shift.give_back_now() {
                log::info!("Shutdown: {kept:?}");
            }
            host.exit();
            return;
        }

        varda.update_frame_timing();
        varda.set_frame_refresh_at(slot.filter(|_| varda.target_fps() > 0));
        varda.update_notifications();
        varda.process_commands();
        varda.process_inputs();
        // Per-frame state advances here: after commands, before any render
        // encode. See `VardaApp::advance_frame_state`.
        varda.advance_frame_state();

        // Create pending output windows (API-driven in headless)
        host.create_pending_outputs(varda);
        host.refresh_monitors(varda);
        // Night Shift off while an output is on a screen (see `night_shift`).
        if let Some(kept) = self
            .night_shift
            .keep(varda.outputs_on_screens(), varda.night_shift_off())
        {
            varda.note_night_shift(kept);
        }
        #[cfg(feature = "html")]
        host.create_pending_interactive(varda);

        // Publish the cued channel(s) so off-air previews render live (issue #72).
        varda.set_preview_channels(self.layout.preview_channels());
        varda.set_ui_selected_deck(self.layout.selected_deck);

        // GPU render (mixer compositing)
        varda.render_mixer_frame();

        // Push content rotation to domemaster renderer (headless path)
        let c_az = self
            .layout
            .dome_geometry
            .content_azimuth_degrees
            .to_radians();
        let c_el = self
            .layout
            .dome_geometry
            .content_elevation_degrees
            .to_radians();
        let c_roll = self.layout.dome_geometry.content_roll_degrees.to_radians();
        varda.set_domemaster_content_rotation(c_az, c_el, c_roll);

        // Render output windows + publish state
        varda.render_outputs();
        #[cfg(feature = "html")]
        varda.render_interactive();
        self.publish_counter += 1;
        if self.publish_counter.is_multiple_of(10) {
            varda.publish_state();
        }
    }

    /// One frame of the control window, on the main thread.
    ///
    /// The engine draws the show on its own thread (see [`engine_thread`]);
    /// this borrows it twice, briefly: before laying out the UI, to read
    /// what the UI shows, and after, to apply what the operator did. Laying
    /// out the UI and waiting for a frame of its own to draw into, it does
    /// without it — so neither holds up an output.
    fn render(&mut self, event_loop: &ActiveEventLoop) {
        self.take_lost_layouts();
        let started = std::time::Instant::now();
        self.last_ui_frame = Some(started);
        self.ui_parts.begin(started, None);
        // What the operator did in a frame the engine was not had for.
        if let Some(stashed) = self.stashed_actions.take() {
            let mut actions = Some(stashed);
            if self
                .with_engine(|this| this.ui_after(actions.take().expect("once")))
                .is_none()
            {
                self.stashed_actions = actions;
                return;
            }
        }
        let Some(Some(ui_data)) = self.with_engine(|this| this.ui_before(event_loop)) else {
            return;
        };
        let Some(window) = self.window else { return };
        self.stall_for_test();

        // 5. Run egui frame
        let t_egui = std::time::Instant::now();
        // Bypass take_egui_input() to avoid an XGetGeometry round-trip every frame.
        // winit 0.30's Window::inner_size() on X11 is a synchronous xcb request;
        // take_egui_input() calls it unconditionally. We replicate what it does
        // using cached values updated from Resized/ScaleFactorChanged events.
        let raw_input = {
            let Some(egui_state) = &mut self.egui_state else {
                return;
            };
            let display_scale = self.cached_scale_factor;
            let pixels_per_point = self.egui_ctx.zoom_factor() * display_scale;
            let w = self.cached_screen_size.width as f32 / pixels_per_point;
            let h = self.cached_screen_size.height as f32 / pixels_per_point;
            let input = egui_state.egui_input_mut();
            input.time = Some(self.egui_start_time.elapsed().as_secs_f64());
            if w > 0.0 && h > 0.0 {
                input.screen_rect = Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(w, h),
                ));
            }
            input.viewport_id = egui::ViewportId::ROOT;
            input
                .viewports
                .entry(egui::ViewportId::ROOT)
                .or_default()
                .native_pixels_per_point = Some(display_scale);
            // Files dragged in from another app: give egui the real pointer,
            // which the OS does not send while the drag is running.
            if !input.hovered_files.is_empty() || !input.dropped_files.is_empty() {
                if let Some((x, y)) = self.window.and_then(drag_position::pointer_in_window) {
                    let zoom = f64::from(self.egui_ctx.zoom_factor());
                    input.events.push(egui::Event::PointerMoved(egui::pos2(
                        (x / zoom) as f32,
                        (y / zoom) as f32,
                    )));
                }
            }
            input.take()
        };
        let mut ui_actions = ui::UIActions::new();
        let full_output = self.egui_ctx.run_ui(raw_input, |ui| {
            ui_actions = ui::panels::render_ui(ui, &ui_data);
        });
        {
            let Some(egui_state) = &mut self.egui_state else {
                return;
            };
            egui_state.handle_platform_output(window, full_output.platform_output);
        }
        self.part("ui", t_egui);
        // Tessellated before the previews are encoded: the meshes say which
        // textures this frame draws — on screen, not scrolled or clipped
        // away — and only those previews are encoded.
        let t_part = std::time::Instant::now();
        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        self.textures_drawn.clear();
        self.drawn_px.clear();
        let pixels_per_point = full_output.pixels_per_point;
        for job in &paint_jobs {
            if let egui::epaint::Primitive::Mesh(mesh) = &job.primitive {
                self.textures_drawn.insert(mesh.texture_id);
                let size = mesh.calc_bounds().size() * pixels_per_point;
                let size = [size.x.ceil() as u32, size.y.ceil() as u32];
                let largest = self.drawn_px.entry(mesh.texture_id).or_insert(size);
                *largest = [largest[0].max(size[0]), largest[1].max(size[1])];
            }
        }
        self.part("ui tessellate", t_part);

        let mut actions = Some(ui_actions);
        let shown = match self.with_engine(|this| this.ui_after(actions.take().expect("once"))) {
            Some(Some(shown)) => shown,
            Some(None) => return,
            // Kept for the next frame, not lost.
            None => {
                self.stashed_actions = actions;
                return;
            }
        };

        // The control window's own frame: the show's picture under the UI.
        let t_submit = std::time::Instant::now();
        self.submit_frame(
            &paint_jobs,
            full_output.pixels_per_point,
            full_output.textures_delta,
            &shown,
        );
        self.part("ui submit", t_submit);
        #[allow(clippy::cast_precision_loss)]
        let frame_ms = 1000.0 / shown.target_fps.max(1) as f32;
        self.ui_parts.end(std::time::Instant::now(), frame_ms);
    }

    /// The test hook's freeze, once a second (see [`UiStall`]).
    fn stall_for_test(&mut self) {
        let Some(stall) = self.ui_stall else { return };
        if self
            .last_ui_stall
            .is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(1))
        {
            return;
        }
        self.last_ui_stall = Some(std::time::Instant::now());
        if stall.holding_engine {
            let _ = self.with_engine(|_| std::thread::sleep(stall.freeze));
        } else {
            std::thread::sleep(stall.freeze);
        }
    }

    /// Borrow the engine for `f`: from the engine thread, waiting for it to
    /// finish a frame if it is in one — or as held here already (headless,
    /// tests, before the engine thread starts). Put back after.
    ///
    /// `None` when the engine thread keeps it past [`ENGINE_PATIENCE`]: this
    /// thread then goes back to macOS's event loop rather than wait on — the
    /// engine thread may be waiting for it (a call only the main thread can
    /// answer, which the engine should never make), and would never finish.
    fn with_engine<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> Option<R> {
        let borrowed = match (&self.engine, &self.varda) {
            (Some(engine), None) => {
                let t = std::time::Instant::now();
                let taken = engine.baton().take_within(Some(ENGINE_PATIENCE));
                self.ui_parts.add("engine wait", t.elapsed());
                match taken {
                    engine_thread::Taken::Engine(app) => self.varda = Some(app),
                    engine_thread::Taken::Late => {
                        self.report_engine_late();
                        return None;
                    }
                    engine_thread::Taken::Closed => {}
                }
                true
            }
            _ => false,
        };
        let result = f(self);
        if borrowed {
            if let (Some(engine), Some(app)) = (&self.engine, self.varda.take()) {
                engine.baton().put(app);
            }
        }
        Some(result)
    }

    /// Say — now and then — that the engine thread kept the engine too long,
    /// and where it was.
    fn report_engine_late(&mut self) {
        // Its first frames load the show and build shaders: long, and fine.
        if self.engine.as_ref().is_some_and(engine_thread::EngineThread::starting) {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .engine_late_reported
            .is_some_and(|at| now.duration_since(at) < std::time::Duration::from_secs(10))
        {
            return;
        }
        self.engine_late_reported = Some(now);
        let phase = self.engine.as_ref().map(engine_thread::EngineThread::phase);
        log::warn!(
            "The engine thread kept the show over {ENGINE_PATIENCE:?} (at {phase:?}): \
             the control window skipped a frame"
        );
    }

    /// Time `took` since `since` on `part` of the control window's frame.
    fn part(&mut self, part: &str, since: std::time::Instant) {
        self.ui_parts.add(part, since.elapsed());
    }

    /// Before the UI is laid out, with the engine: the control window's
    /// settings, preview textures, windows and screens, and what the UI
    /// shows.
    fn ui_before(&mut self, event_loop: &ActiveEventLoop) -> Option<crate::usecases::ui::UIData> {
        if let (Some(ws), Some(varda)) = (&mut self.window_surface, &self.varda) {
            ws.set_vsync(varda.gpu_context(), varda.control_vsync());
            let hold = varda.frame_hold_secs();
            if self.ui_frame_hold != hold {
                crate::renderer::context::set_frame_hold(&ws.surface, hold);
                self.ui_frame_hold = hold;
            }
        }

        // 2. Sync egui texture registrations
        let t_part = std::time::Instant::now();
        self.refresh_textures();
        self.part("textures", t_part);

        let window = self.window?;

        // 3. Create pending output windows + refresh monitors
        let t_part = std::time::Instant::now();
        {
            let varda = self.varda.as_mut()?;
            varda.create_pending_outputs(event_loop);
            varda.reopen_unshown_outputs(event_loop);
            varda.refresh_monitors(event_loop);
            varda.note_control_window(window.current_monitor());
            // The control window's screen at the show's rate (see
            // `screen_rate`) — with the output windows just made counted, and
            // every output window's screen asked now, not as last read.
            let display = || window.current_monitor().and_then(|m| display_id(&m));
            let outputs = || {
                varda
                    .output_screens_now()?
                    .iter()
                    .map(display_id)
                    .collect::<Option<Vec<u32>>>()
            };
            let kept = self.screen_rate.keep(
                display,
                outputs,
                varda.target_fps(),
                varda.control_screen_rate(),
            );
            if let Some(kept) = kept {
                varda.note_screen_rate(&kept);
            }
            // Night Shift off while an output is on a screen (see
            // `night_shift`).
            if let Some(kept) = self
                .night_shift
                .keep(varda.outputs_on_screens(), varda.night_shift_off())
            {
                varda.note_night_shift(kept);
            }
            #[cfg(feature = "html")]
            varda.create_pending_interactive(event_loop);
        }

        // 3b. Render dome preview if open (either dome_preview_open or dome_mode_active)
        if self.layout.dome_preview_open || self.layout.dome_mode_active {
            if let (Some(renderer), Some(varda)) = (&mut self.dome_preview_renderer, &self.varda) {
                let context = varda.gpu_context();

                // Update slice overlays when in dome mode
                if self.layout.dome_mode_active {
                    let setup = self
                        .layout
                        .dome_preset
                        .to_setup_with_geometry(self.layout.dome_geometry);
                    renderer.set_slice_overlays(&context.device, &setup);
                } else {
                    renderer.clear_slice_overlays();
                }

                // Use domemaster output if available, otherwise fall back to mixer composite
                let source_view = varda
                    .domemaster_view()
                    .unwrap_or_else(|| varda.mixer_ref().composite_view());
                let c_az = self
                    .layout
                    .dome_geometry
                    .content_azimuth_degrees
                    .to_radians();
                let c_el = self
                    .layout
                    .dome_geometry
                    .content_elevation_degrees
                    .to_radians();
                let c_roll = self.layout.dome_geometry.content_roll_degrees.to_radians();
                renderer.render(context, source_view, c_az, c_el, c_roll);
            }
        }

        self.sync_camera_detect_capture();
        self.part("windows", t_part);

        // 4. Collect UI data snapshot (engine → UI, with UI-owned layout state)
        let t_part = std::time::Instant::now();
        let varda_ref = self.varda.as_ref()?;
        // The selection on what it was on as the last frame ended, wherever
        // that is now: the API may have moved channels since. The channels
        // as they are now are what this frame is drawn from.
        self.frame_order = super::state::ChannelOrder::of(varda_ref.mixer_ref().channels());
        self.layout
            .reanchor(&self.selection_anchor, &self.frame_order);
        let mut ui_data = crate::usecases::ui::build_ui_data(
            varda_ref,
            &self.layout,
            &self.deck_preview_textures,
            &self.channel_preview_textures,
            &self.output_preview_textures,
            self.main_output_texture,
        );
        ui_data.saved_layouts = self.saved_layouts.layouts();
        ui_data.window_choices = self.window_choices.choices();
        ui_data.recent_shows = std::sync::Arc::clone(&self.recent_shows);
        ui_data.can_undo = varda_ref.history_can_undo();
        ui_data.can_redo = varda_ref.history_can_redo();
        ui_data.pending_deck_loads = self
            .pending_deck_loads
            .load(std::sync::atomic::Ordering::Relaxed);
        ui_data.dome_preview_open = self.layout.dome_preview_open;
        ui_data.dome_preview_texture = self.dome_preview_texture;
        ui_data.camera_detect_texture = self.camera_detect_texture;
        ui_data.camera_detect_mode = self.layout.camera_detect_mode.clone();

        // Poll background detection results (non-blocking)
        while let Ok(response) = self.detect_res_rx.try_recv() {
            self.detect_in_flight = false;
            if response.is_capture {
                // Capture complete — transition to Preview mode
                let n = response.contours.len();
                self.camera_detect_contours = response.contours.clone();
                self.layout.camera_detect_mode = ui::CameraDetectMode::Preview {
                    camera_id: response.camera_id,
                    contours: response.contours,
                    selected: vec![true; n],
                };
                // Re-snapshot UIData mode since we just changed it
                ui_data.camera_detect_mode = self.layout.camera_detect_mode.clone();
            } else {
                // Live overlay update
                self.camera_detect_contours = response.contours;
            }
        }

        // Submit new detection work if in Live mode and no work in flight
        if let ui::CameraDetectMode::Live {
            camera_id,
            ref params,
        } = self.layout.camera_detect_mode
        {
            if !self.detect_in_flight {
                if let Some(frame) = varda_ref.camera_manager().snapshot_frame(camera_id) {
                    let _ = self.detect_req_tx.send(DetectRequest {
                        rgba: frame.0,
                        w: frame.1,
                        h: frame.2,
                        params: params.clone(),
                        is_capture: false,
                        camera_id,
                    });
                    self.detect_in_flight = true;
                }
            }
        }

        ui_data
            .camera_detect_contours
            .clone_from(&self.camera_detect_contours);
        self.part("ui data", t_part);

        // How the control window has been doing, for the performance figures.
        self.ui_frames += 1;
        if self.ui_frames.is_multiple_of(10) {
            if let Some(varda) = self.varda.as_mut() {
                varda.set_control_on_screen(
                    self.ui_on_screen.stats(),
                    crate::engine::ControlQueueStats {
                        waiting: self.ui_waiting_most,
                    },
                );
                varda.set_control_parts(self.ui_parts.stats(), self.ui_parts.slow_frames());
                self.ui_waiting_most = 0;
            }
        }
        Some(ui_data)
    }

    /// After the UI is laid out, with the engine: what the operator did,
    /// the Show menu, what the engine keeps of the UI, and the previews.
    /// Returns what the control window shows under the UI.
    fn ui_after(&mut self, mut ui_actions: ui::UIActions) -> Option<Shown> {
        let t_part = std::time::Instant::now();

        // 6. Apply all UI actions
        // 6a. UI-consumer-owned selection/layout state
        self.layout.apply_selections(&ui_actions);
        // What the operator has selected, as drawn: settled where it is once
        // this frame's changes are in (see `settle_selection`).
        let selected = self.layout.anchor(&self.frame_order);
        self.apply_screen_layout_actions(&mut ui_actions);

        // 6a2. Dome camera actions — apply to renderer (not layout state)
        {
            let dome_resized = false;
            for action in &ui_actions.session.dome_actions {
                match action {
                    ui::DomeAction::RotateCamera { delta_x, delta_y } => {
                        if let Some(renderer) = &mut self.dome_preview_renderer {
                            renderer.camera.rotate(*delta_x, *delta_y);
                        }
                    }
                    ui::DomeAction::ZoomCamera { delta } => {
                        if let Some(renderer) = &mut self.dome_preview_renderer {
                            renderer.camera.zoom(*delta);
                        }
                    }
                    ui::DomeAction::ResetCamera => {
                        if let Some(renderer) = &mut self.dome_preview_renderer {
                            renderer.camera.reset();
                        }
                    }
                    _ => {} // Config actions handled by layout.apply_selections
                }
            }
            // Handle dome resize if needed
            if let (Some(renderer), Some(varda)) = (&mut self.dome_preview_renderer, &self.varda) {
                let context = varda.gpu_context();
                if let Some(egui_renderer) = &mut self.egui_renderer {
                    // Check if dome_preview_texture needs re-registration after resize
                    if dome_resized {
                        let _ = dome_resized; // suppress unused warning
                    }
                    let _ = (context, egui_renderer, renderer); // used below if resize
                }
            }
        }

        self.apply_camera_detect_actions(&mut ui_actions);

        // 6b. Engine actions (delegated to VardaApp)
        {
            let varda = self.varda.as_mut()?;
            let egui_renderer = self.egui_renderer.as_mut()?;

            // ── Undo/redo: snapshot before undoable mutations ──
            // Unified scene+stage timeline with general gesture coalescing
            // (ui-engine-boundary.md WS3). A snapshot is pushed when the frame
            // carries any undoable mutation AND it is not the *continuation* of a
            // held drag — so a continuous gesture of any kind (warp drag, param
            // slider) collapses into a single undo step (snapshot on the first
            // frame). Undoability is decided by the single, compiler-checked
            // `command_is_undoable` predicate (via `batch_has_undoable`) for
            // migrated domains, plus the residual `has_undoable_*` gates for
            // fields not yet migrated to `commands`.
            let dirty = ui_actions.has_undoable_action()
                || ui_actions.has_undoable_stage_action()
                || varda.batch_has_undoable(&ui_actions.commands);
            // A recording pass is one long gesture: it pushed its own entry
            // when the first parameter was touched, and every write until it
            // ends belongs to that entry.
            if wants_history_snapshot(
                &mut self.prev_gesture_active,
                dirty,
                ui_actions.session.gesture_active || varda.is_recording(),
            ) {
                let snapshot = varda.history_snapshot(&self.layout);
                varda.push_history(snapshot);
            }

            // Intercept shader_to_add: resolve and route to background loading
            if let Some((place, gen_idx)) = ui_actions.session.shader_to_add.take() {
                if let Some(shader) = varda.resolve_generator(gen_idx) {
                    let token = self
                        .deck_load_targets
                        .record_before(place.channel_uuid, place.before_deck_uuid);
                    let context = varda.gpu_context();
                    VardaApp::spawn_deck_loads(
                        &self.deck_load_tx,
                        context,
                        &self.pending_deck_loads,
                        varda.render_width(),
                        varda.render_height(),
                        Vec::new(),
                        Vec::new(),
                        vec![(token, shader)],
                    );
                }
            }

            // Where each channel moved this frame will end up, found before the
            // move: the selection is held by position and follows it after.
            let moves = channel_moves(varda, &ui_actions.commands);
            let engine_outcome = varda.apply_engine_actions(&mut ui_actions);
            for (uuid, from, to) in moves {
                if varda
                    .mixer_ref()
                    .channels()
                    .get(to)
                    .is_some_and(|c| c.uuid() == uuid)
                {
                    self.layout.fixup_channel_move(from, to);
                }
            }
            apply_deck_texture_outcomes(
                &engine_outcome.texture_outcomes,
                egui_renderer,
                varda.gpu_context(),
                varda.mixer_ref(),
                &mut self.deck_preview_textures,
            );

            // ── Drain MIDI-triggered global actions ──
            if std::mem::take(&mut varda.midi_pending_undo) {
                ui_actions.session.undo_requested = true;
            }
            if std::mem::take(&mut varda.midi_pending_redo) {
                ui_actions.session.redo_requested = true;
            }
            if std::mem::take(&mut varda.midi_pending_save) {
                ui_actions.session.save_requested = true;
            }

            // ── Undo/redo: restore via the engine's shared timeline ──
            // The restore (scene + stage diff-apply) lives on `VardaApp` so the
            // windowed and headless/API consumers behave identically; the
            // runner only layers on UI-specific refresh (dome layout flags +
            // GPU preview texture re-registration).
            if ui_actions.session.undo_requested || ui_actions.session.redo_requested {
                let undo = ui_actions.session.undo_requested;
                let outcome = varda.history_gui(&self.layout, undo);
                if let crate::engine::CommandOutcome::HistoryRestored {
                    structural_changed: _,
                    dome_layout,
                } = outcome
                {
                    // Dome layout flags live in UI layout, not engine state.
                    self.layout.dome_mode_active = dome_layout.dome_mode_active;
                    self.layout.dome_preset = dome_layout.dome_preset;
                    self.layout.dome_geometry = dome_layout.dome_geometry;

                    let label = if undo { "↩ Undo" } else { "↪ Redo" };
                    varda.notify_info(label);

                    // Previews need nothing here, even when channels or
                    // decks came and went: each frame's preview sync
                    // registers new ones, retires gone ones, and keeps the
                    // rest pointed at their gamma-encoded copies. Pointing
                    // them at the engine's own linear textures (as this
                    // once did) showed them dark and over-contrasty until a
                    // restart.
                }
            }

            varda.apply_ui_actions(&ui_actions);
            varda.update_controller_leds();

            // A resolution change recreates the engine's textures, but the
            // previews show gamma-encoded copies that are re-encoded from
            // whatever the engine holds each frame, so they need no repair.
            // Re-pointing them at the new linear textures (as this once did)
            // showed every preview dark and over-contrasty until a restart.

            // Fix up selection state after channel removal
            if let Some(ch_idx) = engine_outcome.removed_channel {
                self.layout.fixup_channel_removal(ch_idx);
            }

            // Written on a thread of its own, the outputs carrying on; "saved"
            // is said once the files have landed.
            if ui_actions.session.save_requested {
                varda.save_in_background(&self.layout);
            }

            // Where recordings go: a folder picker, off the frame.
            if std::mem::take(&mut ui_actions.session.choose_record_folder)
                && self.record_folder_pick.is_none()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                let start = varda.record_settings().options.folder.clone();
                std::thread::spawn(move || {
                    let _ = tx.send(
                        rfd::FileDialog::new()
                            .set_title("Where recordings go")
                            .set_directory(&start)
                            .pick_folder(),
                    );
                });
                self.record_folder_pick = Some(rx);
            }
            if let Some(rx) = &self.record_folder_pick {
                if let Ok(picked) = rx.try_recv() {
                    self.record_folder_pick = None;
                    if let Some(folder) = picked {
                        let options = crate::record::RecordOptions {
                            folder,
                            ..varda.record_settings().options.clone()
                        };
                        let _ = varda.cmd_set_record_options(options);
                    }
                }
            }
            if let Some(file) = ui_actions.session.reveal_file.take() {
                reveal_in_file_manager(&file);
            }

            // Spawn file dialogs on background threads (non-blocking)
            if let Some(uuid) = ui_actions.session.open_image_dialog_for_channel.take() {
                VardaApp::open_file_dialog(&self.file_dialog_tx, FileDialogKind::Image, Some(uuid));
            }
            if let Some(uuid) = ui_actions.session.open_video_dialog_for_channel.take() {
                VardaApp::open_file_dialog(&self.file_dialog_tx, FileDialogKind::Video, Some(uuid));
            }
            if std::mem::take(&mut ui_actions.session.open_image_dialog_for_new_inputs) {
                VardaApp::open_file_dialog(&self.file_dialog_tx, FileDialogKind::Image, None);
            }
            if std::mem::take(&mut ui_actions.session.open_video_dialog_for_new_inputs) {
                VardaApp::open_file_dialog(&self.file_dialog_tx, FileDialogKind::Video, None);
            }

            // Files dropped in from Finder: decks in drop order, on one loader
            // thread so they land in that order.
            for (place, paths) in std::mem::take(&mut ui_actions.session.files_to_add) {
                deck_load::add_dropped_files(
                    varda,
                    &mut self.deck_load_targets,
                    &self.deck_load_tx,
                    &self.pending_deck_loads,
                    &place,
                    paths,
                );
            }
            // Dropped where no channel is: an input each.
            let as_inputs = std::mem::take(&mut ui_actions.session.files_as_inputs);
            if !as_inputs.is_empty() {
                deck_load::add_dropped_files_as_inputs(
                    varda,
                    &mut self.deck_load_targets,
                    &mut self.inputs_awaiting_files,
                    &self.deck_load_tx,
                    &self.pending_deck_loads,
                    as_inputs,
                );
            }

            // Poll completed file dialog results → spawn background deck loads.
            // The dialog carries a channel UUID rather than an index: the user
            // may have spent minutes browsing while the UI stayed live.
            while let Ok(result) = self.file_dialog_rx.try_recv() {
                let targets: Vec<String> = if let Some(uuid) = &result.channel_uuid {
                    if varda.mixer_ref().find_channel_by_uuid(uuid).is_none() {
                        log::warn!(
                            "Dropping {} file dialog result(s): channel {} no longer exists",
                            result.paths.len(),
                            uuid
                        );
                        continue;
                    }
                    vec![uuid.clone(); result.paths.len()]
                } else {
                    // A new input for each file, named after it; one undo
                    // takes them all away.
                    let snapshot = varda.history_snapshot(&self.layout);
                    varda.push_history(snapshot);
                    result
                        .paths
                        .iter()
                        .filter_map(|path| {
                            let uuid = deck_load::new_input_for(varda, path)?;
                            self.inputs_awaiting_files.insert(uuid.clone());
                            Some(uuid)
                        })
                        .collect()
                };
                // One loader thread, in the order picked: with a thread per
                // file, several slides picked together landed shuffled.
                // Stacked in one input, last first: the first picked is in
                // front, at the top of the lists of layers.
                let stacked = result.channel_uuid.is_some();
                let mut files: Vec<_> = result
                    .paths
                    .into_iter()
                    .zip(targets)
                    .map(|(path, channel_uuid)| {
                        let token = self.deck_load_targets.record(channel_uuid);
                        let file = match result.kind {
                            FileDialogKind::Image => crate::app::DeckFile::Image(path),
                            FileDialogKind::Video => crate::app::DeckFile::Video(path),
                        };
                        (token, file)
                    })
                    .collect();
                if stacked {
                    files.reverse();
                }
                VardaApp::spawn_ordered_deck_loads(
                    &self.deck_load_tx,
                    varda.gpu_context(),
                    &self.pending_deck_loads,
                    (varda.render_width(), varda.render_height()),
                    files,
                );
            }

            // Poll completed background deck loads (non-blocking)
            while let Ok(result) = self.deck_load_rx.try_recv() {
                let target = self.deck_load_targets.claim(result.token);
                match result.deck {
                    Ok(deck) => {
                        // Resolve the target here, not at spawn: the channel list
                        // can change while a decode or shader compile runs, and an
                        // index captured back then would now name a different
                        // channel. See `/spec/api-addressing.md`.
                        let Some((channel_uuid, before_deck)) = target else {
                            log::warn!(
                                "Dropping background load '{}': no target channel was recorded",
                                result.name
                            );
                            continue;
                        };
                        let Some(ch_idx) = varda.mixer_ref().find_channel_by_uuid(&channel_uuid)
                        else {
                            log::warn!(
                                "Dropping background load '{}': channel {} no longer exists",
                                result.name,
                                channel_uuid
                            );
                            continue;
                        };
                        // Same post-construction wiring the synchronous
                        // `add_deck` command performs — analyzer startup and
                        // required-device acquisition. Without it a shader
                        // dropped from the Library renders against blank
                        // preprocessor textures with no error shown.
                        let mut deck = deck;
                        // A deck that cannot be finalized is not added (the
                        // notice is logged too); an input made for its file
                        // goes with it.
                        if let Err(e) = varda.finalize_new_deck(&mut deck) {
                            varda.notify_error(format!("Failed to add deck: {e}"));
                            if self.inputs_awaiting_files.remove(&channel_uuid) {
                                deck_load::remove_if_empty(varda, &channel_uuid);
                            }
                            continue;
                        }
                        let deck_uuid = deck.uuid().to_string();
                        self.inputs_awaiting_files.remove(&channel_uuid);
                        if let Some(ch) = varda.mixer_mut().channel_mut(ch_idx) {
                            // Before the deck it was dropped in front of, if
                            // that deck is still in this channel; else last.
                            let idx = ch.insert_deck_before(deck, before_deck.as_deref());
                            log::info!(
                                "Background load complete: deck {} to channel {}: {}",
                                idx,
                                channel_uuid,
                                result.name
                            );
                        }
                        // Re-borrow for texture registration (separate from mixer borrow)
                        register_deck_preview_texture(
                            egui_renderer,
                            varda.gpu_context(),
                            varda.mixer_ref(),
                            &deck_uuid,
                            &mut self.deck_preview_textures,
                        );
                    }
                    Err(e) => {
                        log::error!("Background deck load failed for '{}': {}", result.name, e);
                        // An input made for this file goes with it.
                        if let Some((channel_uuid, _)) = target {
                            if self.inputs_awaiting_files.remove(&channel_uuid) {
                                deck_load::remove_if_empty(varda, &channel_uuid);
                            }
                        }
                    }
                }
            }
        }


        // The Show menu: its dialogs, and moving between shows.
        if let Some(action) = ui_actions.session.show_action.take() {
            self.start_show_action(action);
        }
        self.poll_show_pick();
        self.show_moved();
        self.part("actions", t_part);

        // What the engine keeps of the UI: the cued channels (their off-air
        // previews render live, issue #72), the deck being edited, and the
        // dome's content rotation (real-time, MIDI-mappable).
        {
            let varda = self.varda.as_mut()?;
            settle_selection(&mut self.layout, &selected, varda);
            varda.set_preview_channels(self.layout.preview_channels());
            varda.set_ui_selected_deck(self.layout.selected_deck);
            self.selection_anchor = self.layout.anchor(&super::state::ChannelOrder::of(
                varda.mixer_ref().channels(),
            ));
            let dome = &self.layout.dome_geometry;
            varda.set_domemaster_content_rotation(
                dome.content_azimuth_degrees.to_radians(),
                dome.content_elevation_degrees.to_radians(),
                dome.content_roll_degrees.to_radians(),
            );
            // Interactive HTML windows are winit windows: drawn here, on the
            // main thread, where winit may be asked for things.
            #[cfg(feature = "html")]
            varda.render_interactive();
        }

        // Previews, gamma-encoded from the engine's latest frame for the
        // thumbnails the UI draws.
        let t_part = std::time::Instant::now();
        self.encode_previews();
        self.part("previews", t_part);

        let varda = self.varda.as_ref()?;
        crate::renderer::present_trace::TRACE.note(
            CONTROL,
            u64::from(self.ui_frames),
            // The frame drawn last: its count has moved on past it.
            crate::renderer::present_trace::Step::Sampled {
                engine_frame: varda.frame_stats.frame_count.saturating_sub(1),
            },
        );
        Some(Shown {
            picture: Some(varda.mixer_ref().composite_view().clone()),
            target_fps: varda.target_fps(),
        })
    }

    /// Apply egui's texture changes, draw and present the UI, then release the
    /// textures egui freed. Both halves run on every frame — including one the
    /// surface cannot be drawn to (e.g. Occluded at startup) — so the renderer
    /// never misses an upload (the font atlas is sent only once) or keeps a
    /// freed texture.
    fn submit_frame(
        &mut self,
        paint_jobs: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
        mut textures_delta: egui::TexturesDelta,
        shown: &Shown,
    ) {
        let t_part = std::time::Instant::now();
        if let (Some(context), Some(egui_renderer)) = (&self.gpu, &mut self.egui_renderer) {
            for (id, deltas) in &textures_delta.set {
                for delta in deltas {
                    egui_renderer.update_texture(&context.device, &context.queue, *id, delta);
                }
            }
        }
        self.part("ui textures", t_part);
        let parts = self.present_ui(paint_jobs, pixels_per_point, shown.picture.as_ref());
        if let Some(ws) = &self.window_surface {
            let times = crate::renderer::context::take_presented_times(&ws.surface);
            self.ui_queue.reported(times.len());
            self.ui_waiting_most = self.ui_waiting_most.max(self.ui_queue.waiting());
            if !times.is_empty() {
                let frame = 1.0 / f64::from(shown.target_fps.max(1));
                self.ui_on_screen.record(&times, frame);
                crate::renderer::present_trace::TRACE.note(
                    CONTROL,
                    0,
                    crate::renderer::present_trace::Step::Shown { times },
                );
            }
        }
        for (part, took) in parts {
            self.ui_parts.add(part, took);
        }
        if let Some(egui_renderer) = &mut self.egui_renderer {
            for id in &textures_delta.free {
                egui_renderer.free_texture(id);
            }
        }
        textures_delta.clear();
    }

    /// Blit mixer output to screen, overlay egui, and present. Returns the
    /// time each part took, for the frame's parts.
    fn present_ui(
        &mut self,
        paint_jobs: &[egui::ClippedPrimitive],
        pixels_per_point: f32,
        picture: Option<&wgpu::TextureView>,
    ) -> Vec<(&'static str, std::time::Duration)> {
        use crate::renderer::present_trace::{Step, TRACE};
        let mut parts = Vec::with_capacity(4);
        let Some(context) = &self.gpu else {
            return parts;
        };
        let Some(win_surface) = &self.window_surface else {
            return parts;
        };

        let Some(egui_renderer) = &mut self.egui_renderer else {
            return parts;
        };

        let t_part = std::time::Instant::now();
        let _ = context.device.poll(wgpu::PollType::Poll);
        let acquired = win_surface.surface.get_current_texture();
        parts.push(("ui acquire", t_part.elapsed()));
        let had = matches!(
            acquired,
            wgpu::CurrentSurfaceTexture::Success(_) | wgpu::CurrentSurfaceTexture::Suboptimal(_)
        );
        let step = if had { Step::Acquired } else { Step::NoFrame };
        TRACE.note(CONTROL, u64::from(self.ui_frames), step);
        let t_draw = std::time::Instant::now();
        let output = match acquired {
            wgpu::CurrentSurfaceTexture::Success(o) => o,
            wgpu::CurrentSurfaceTexture::Suboptimal(o) => {
                log::warn!("UI surface suboptimal, will reconfigure");
                o
            }
            wgpu::CurrentSurfaceTexture::Outdated => {
                log::warn!("UI surface outdated, reconfiguring");
                context.configure_surface(&win_surface.surface, &win_surface.surface_config);
                match win_surface.surface.get_current_texture() {
                    wgpu::CurrentSurfaceTexture::Success(o)
                    | wgpu::CurrentSurfaceTexture::Suboptimal(o) => o,
                    other => {
                        log::error!("Failed to get surface texture after reconfigure: {other:?}");
                        return parts;
                    }
                }
            }
            other => {
                log::debug!("UI surface unavailable: {other:?}");
                return parts;
            }
        };
        let view = output
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let mut encoder = context
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Screen Encoder"),
            });

        let bind_group = match (&self.blit_pipeline, picture) {
            (Some(blit), Some(picture)) => Some(blit.create_bind_group(&context.device, picture)),
            _ => None,
        };

        // The scale egui laid this frame out and read the pointer at — zoom
        // (Cmd +/−) included. The bare window scale factor drops the zoom, so
        // at any zoom but 100% the drawn UI and the pointer drift apart.
        let screen_desc = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [win_surface.size.width, win_surface.size.height],
            pixels_per_point,
        };

        egui_renderer.update_buffers(
            &context.device,
            &context.queue,
            &mut encoder,
            paint_jobs,
            &screen_desc,
        );

        {
            let mut rp = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Control Window Pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            if let (Some(bg), Some(blit)) = (&bind_group, &self.blit_pipeline) {
                blit.render(&mut rp, bg);
            }
            let mut rp_static = rp.forget_lifetime();
            egui_renderer.render(&mut rp_static, paint_jobs, &screen_desc);
        }

        context.submit(std::iter::once(encoder.finish()));
        TRACE.note(CONTROL, u64::from(self.ui_frames), Step::Submitted);
        parts.push(("ui draw", t_draw.elapsed()));
        let t_part = std::time::Instant::now();
        context.queue.present(output);
        TRACE.note(CONTROL, u64::from(self.ui_frames), Step::Presented);
        self.ui_queue.presented();
        parts.push(("ui present", t_part.elapsed()));
        parts
    }
}

/// A test hook: the control window freezes `freeze` once a second, as a busy
/// moment of UI would — `NIDA_TEST_UI_STALL=25` for 25 ms with the engine
/// left to its thread, `=25:hold` holding the engine meanwhile, as when both
/// ran on the main thread. The outputs' on-screen hitches
/// (`/api/state/performance`) show the difference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UiStall {
    freeze: std::time::Duration,
    holding_engine: bool,
}

impl UiStall {
    fn from_env() -> Option<Self> {
        let stall = Self::parse(&std::env::var("NIDA_TEST_UI_STALL").ok()?)?;
        log::warn!("Test hook: the control window freezes {stall:?} once a second");
        Some(stall)
    }

    fn parse(text: &str) -> Option<Self> {
        let (ms, hold) = match text.split_once(':') {
            Some((ms, "hold")) => (ms, true),
            Some(_) => return None,
            None => (text, false),
        };
        let ms: u64 = ms.trim().parse().ok().filter(|ms| *ms > 0)?;
        Some(Self {
            freeze: std::time::Duration::from_millis(ms),
            holding_engine: hold,
        })
    }
}

/// How long the control window waits for the engine thread to finish a
/// frame before giving up on its own frame. A frame takes a few
/// milliseconds; a command that loads a show, longer.
const ENGINE_PATIENCE: std::time::Duration = std::time::Duration::from_millis(100);

/// The control window's name in a present trace.
const CONTROL: &str = "control window";

/// A screen's `CGDirectDisplayID`, where there is one (macOS).
// `None` off macOS, where there are no display ids.
#[allow(clippy::unnecessary_wraps)]
fn display_id(monitor: &winit::monitor::MonitorHandle) -> Option<u32> {
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::MonitorHandleExtMacOS as _;
        Some(monitor.native_id())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = monitor;
        None
    }
}

/// What the control window shows under its UI, taken with the engine.
struct Shown {
    /// The show's composited picture.
    picture: Option<wgpu::TextureView>,
    target_fps: u32,
}

/// Whether a show folder was named on the command line (`--workspace`),
/// rather than by the app bundle's environment.
fn workspace_on_command_line() -> bool {
    std::env::args().any(|a| a == "--workspace" || a.starts_with("--workspace="))
}

/// What the window says when the HTTP API could not start on `port`.
fn api_off(port: u16) -> String {
    format!(
        "No HTTP API: port {port} is busy, most likely with another program. Nothing can \
         control Nida over HTTP until Nida is opened again with the port free."
    )
}

/// Show `file` selected in Finder — or open its folder in the file manager
/// elsewhere.
fn reveal_in_file_manager(file: &std::path::Path) {
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open")
        .arg("-R")
        .arg(file)
        .spawn();
    #[cfg(not(target_os = "macos"))]
    let spawned = std::process::Command::new("xdg-open")
        .arg(file.parent().unwrap_or(file))
        .spawn();
    if let Err(e) = spawned {
        log::warn!("Could not show {} in the file manager: {e}", file.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    // ── Frame pacing ────────────────────────────────────────────────
    //
    // `UIRunner::new` needs no window, GPU, or event loop — every such field is
    // `None` — so the pacing logic is directly exercisable.

    fn runner() -> UIRunner {
        UIRunner::new(AppConfig::parse_from(["varda", "--headless"]))
    }

    fn headless_app_with(channels: usize) -> Option<VardaApp> {
        let gpu = GpuContext::new_headless().ok()?;
        let mut app = VardaApp::new(gpu, &crate::testing::headless_config()).ok()?;
        while app.mixer_ref().channels().len() < channels {
            app.execute_command(crate::engine::EngineCommand::AddChannel);
        }
        Some(app)
    }

    fn channel_order(app: &VardaApp) -> Vec<String> {
        let channels = app.mixer_ref().channels();
        channels.iter().map(|c| c.uuid().to_string()).collect()
    }

    /// Undo puts a moved input back; the selection stays on that input, so
    /// the next take sends it — held by position, it stayed where the input
    /// had been, on another one.
    #[test]
    fn undoing_a_move_keeps_the_selection_on_its_input() {
        use crate::engine::{CommandOutcome, EngineCommand};
        let Some(mut app) = headless_app_with(3) else {
            return;
        };
        let before = channel_order(&app);
        let b = before[1].clone();
        let mut layout = super::super::UILayoutState {
            selected_channel: Some(1),
            ..Default::default()
        };
        // As a frame does: the snapshot, the move, the selection along.
        let snapshot = app.history_snapshot(&layout);
        app.push_history(snapshot);
        app.execute_command(EngineCommand::MoveChannel {
            channel_uuid: b.clone(),
            before_channel_uuid: None,
        });
        layout.fixup_channel_move(1, 2);
        assert_eq!(channel_order(&app)[2], b);

        // The next frames, drawn from the channels as they are: Undo, Redo.
        for (undo, at) in [(true, 1), (false, 2)] {
            let drawn = super::super::state::ChannelOrder::of(app.mixer_ref().channels());
            let selected = layout.anchor(&drawn);
            let restored = app.history_gui(&layout, undo);
            assert!(matches!(restored, CommandOutcome::HistoryRestored { .. }));
            settle_selection(&mut layout, &selected, &app);
            assert_eq!(channel_order(&app)[at], b, "undo {undo}");
            assert_eq!(
                layout.selected_channel,
                Some(at),
                "still on it, undo {undo}"
            );
        }
    }

    /// A layer selected and dragged into another input while the API moves
    /// that input, both in one frame: the selection stays on the layer,
    /// so the next take sends the input it is in now.
    #[test]
    fn the_selection_follows_its_layer_into_another_input_mid_frame() {
        use crate::engine::{CommandResult, EngineCommand};
        let Some(mut app) = headless_app_with(3) else {
            return;
        };
        let order = channel_order(&app);
        let layer =
            |app: &mut VardaApp| match app.execute_command(EngineCommand::AddSolidColorDeck {
                channel_uuid: order[0].clone(),
                color: [1.0, 0.0, 0.0, 1.0],
            }) {
                CommandResult::OkWithId { uuid } => uuid,
                other => panic!("{other:?}"),
            };
        let (d, _e) = (layer(&mut app), layer(&mut app));
        let mut layout = super::super::UILayoutState {
            selected_deck: Some((0, 0)),
            ..Default::default()
        };
        let drawn = super::super::state::ChannelOrder::of(app.mixer_ref().channels());
        let selected = layout.anchor(&drawn);
        app.execute_command(EngineCommand::MoveDeck {
            deck_uuid: d.clone(),
            dst_channel_uuid: order[2].clone(),
            before_deck_uuid: None,
        });
        app.execute_command(EngineCommand::MoveChannel {
            channel_uuid: order[2].clone(),
            before_channel_uuid: Some(order[1].clone()),
        });
        settle_selection(&mut layout, &selected, &app);
        let (ch, dk) = layout.selected_deck.expect("still selected");
        let channel = &app.mixer_ref().channels()[ch];
        assert_eq!(channel.decks[dk].deck.uuid(), d, "the layer it was on");
        assert_eq!(channel.uuid(), order[2], "in the input it went to");
    }

    #[test]
    fn cadence_anchor_is_cleared_when_fps_is_uncapped() {
        let mut runner = runner();
        runner.advance_cadence_anchor(60);
        assert!(runner.cadence_anchor.is_some());
        runner.advance_cadence_anchor(0);
        assert!(
            runner.cadence_anchor.is_none(),
            "target_fps 0 means uncapped — no pacing deadline"
        );
    }

    #[test]
    fn cadence_anchor_starts_one_budget_ahead() {
        let mut runner = runner();
        let before = std::time::Instant::now();
        runner.advance_cadence_anchor(60);
        let anchor = runner.cadence_anchor.expect("anchor set");
        let budget = std::time::Duration::from_secs_f64(1.0 / 60.0);
        assert!(anchor >= before + budget, "at least one budget out");
        assert!(
            anchor <= std::time::Instant::now() + budget,
            "not more than one budget out"
        );
    }

    /// A frame that finishes inside its budget keeps the existing cadence: the
    /// anchor advances by exactly one budget rather than resetting to `now`.
    /// This is what stops frame times from drifting.
    #[test]
    fn cadence_anchor_advances_by_one_budget_when_on_time() {
        let mut runner = runner();
        let budget = std::time::Duration::from_secs_f64(1.0 / 60.0);
        // Far enough ahead that `now` is comfortably before the deadline.
        let anchor = std::time::Instant::now() + budget * 10;
        runner.cadence_anchor = Some(anchor);
        runner.advance_cadence_anchor(60);
        assert_eq!(
            runner.cadence_anchor,
            Some(anchor + budget),
            "on-time frame advances the anchor by exactly one budget"
        );
    }

    /// A frame that overshot its deadline restarts the cadence from `now` rather
    /// than chasing the stale anchor, which would emit a burst of zero-budget
    /// catch-up frames.
    #[test]
    fn cadence_anchor_snaps_forward_after_an_overshoot() {
        let mut runner = runner();
        let budget = std::time::Duration::from_secs_f64(1.0 / 60.0);
        let stale = std::time::Instant::now()
            .checked_sub(budget * 10)
            .expect("clock is far enough past boot to subtract 10 frames");
        runner.cadence_anchor = Some(stale);
        let before = std::time::Instant::now();
        runner.advance_cadence_anchor(60);
        let anchor = runner.cadence_anchor.expect("anchor set");
        assert!(
            anchor >= before && anchor <= std::time::Instant::now(),
            "an overshot frame is followed at once — a whole budget's wait on \
             top of the overrun drops a frame every time one runs long"
        );
        assert_ne!(anchor, stale + budget, "must not chase the stale deadline");

        // From there the cadence holds: the next on-time frame is one budget on,
        // not a burst of catch-up frames.
        runner.advance_cadence_anchor(60);
        assert_eq!(runner.cadence_anchor, Some(anchor + budget));
    }

    #[test]
    fn cadence_budget_scales_with_target_fps() {
        let mut fast = runner();
        fast.advance_cadence_anchor(120);
        let fast_anchor = fast.cadence_anchor.expect("anchor");
        let mut slow = runner();
        slow.advance_cadence_anchor(30);
        let slow_anchor = slow.cadence_anchor.expect("anchor");
        assert!(
            fast_anchor < slow_anchor,
            "a higher target fps yields a nearer deadline"
        );
    }

    // ── Detection worker thread ─────────────────────────────────────

    /// The worker echoes `is_capture` and `camera_id` back untouched, which is how
    /// the runner tells a freeze-frame capture from a live overlay refresh.
    #[test]
    fn detect_worker_round_trips_request_metadata() {
        let (req_tx, req_rx) = std::sync::mpsc::channel();
        let res_rx = spawn_detect_thread(req_rx);

        req_tx
            .send(DetectRequest {
                rgba: vec![0; 4 * 8 * 8],
                w: 8,
                h: 8,
                params: crate::surface::detect::DetectionParams::default(),
                is_capture: true,
                camera_id: 7,
            })
            .expect("send request");

        let res = res_rx
            .recv_timeout(std::time::Duration::from_secs(10))
            .expect("worker replied");
        assert!(res.is_capture, "capture flag echoed back");
        assert_eq!(res.camera_id, 7);
    }

    /// A blank frame yields no contours rather than letting the error escape the
    /// thread — the worker must survive undetectable input and stay available.
    #[test]
    fn detect_worker_survives_undetectable_input_and_keeps_serving() {
        let (req_tx, req_rx) = std::sync::mpsc::channel();
        let res_rx = spawn_detect_thread(req_rx);

        for _ in 0..2 {
            req_tx
                .send(DetectRequest {
                    rgba: vec![0; 4 * 8 * 8],
                    w: 8,
                    h: 8,
                    params: crate::surface::detect::DetectionParams::default(),
                    is_capture: false,
                    camera_id: 1,
                })
                .expect("send request");
            let res = res_rx
                .recv_timeout(std::time::Duration::from_secs(10))
                .expect("worker replied");
            assert!(res.contours.is_empty(), "blank frame has no contours");
        }
    }

    // ── render_headless ─────────────────────────────────────────────
    //
    // A real `ActiveEventLoop` cannot be constructed in a test, so `WindowHost`
    // stands in for it. Everything else is real: a headless `GpuContext` and a
    // genuine `VardaApp`, driven frame by frame.

    #[derive(Default)]
    struct FakeHost {
        exits: std::cell::Cell<u32>,
        outputs_created: std::cell::Cell<u32>,
        monitors_refreshed: std::cell::Cell<u32>,
    }

    impl WindowHost for FakeHost {
        fn exit(&self) {
            self.exits.set(self.exits.get() + 1);
        }

        fn create_pending_outputs(&self, _varda: &mut VardaApp) {
            self.outputs_created.set(self.outputs_created.get() + 1);
        }

        fn refresh_monitors(&self, _varda: &mut VardaApp) {
            self.monitors_refreshed
                .set(self.monitors_refreshed.get() + 1);
        }

        #[cfg(feature = "html")]
        fn create_pending_interactive(&self, _varda: &mut VardaApp) {}
    }

    /// A runner with a real headless engine attached, or `None` when the machine
    /// has no GPU adapter — matching the skip-without-adapter pattern used by the
    /// other GPU tests.
    fn headless_runner() -> Option<UIRunner> {
        let gpu = GpuContext::new_headless().ok()?;
        // One config for both halves, so the engine and the runner agree on
        // which scratch workspace a shutdown save writes to. The scratch
        // workspace is not optional: `render_headless` saves on shutdown.
        let config = crate::testing::headless_config();
        let varda = VardaApp::new(gpu, &config).expect("VardaApp::new");
        let mut runner = UIRunner::new(config);
        runner.varda = Some(Box::new(varda));
        Some(runner)
    }

    #[test]
    fn render_headless_drives_a_frame_with_no_window() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let host = FakeHost::default();
        runner.render_headless(&host);

        assert_eq!(host.exits.get(), 0, "a normal frame must not exit");
        assert_eq!(
            host.outputs_created.get(),
            1,
            "each frame reconciles pending output windows"
        );
        assert_eq!(
            host.monitors_refreshed.get(),
            1,
            "each frame refreshes the monitor list"
        );
        assert_eq!(runner.publish_counter, 1);
    }

    /// On the timer, a frame moves what moves by whole frames (a playlist's
    /// transition) on to its slot on the even cadence, not to the moment the
    /// loop happened to wake — which may be late.
    #[test]
    fn render_headless_moves_frames_on_to_their_cadence_slot() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let host = FakeHost::default();
        let slot = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(9))
            .unwrap();
        runner.cadence_anchor = Some(slot);
        runner.render_headless(&host);
        let varda = runner.varda.as_ref().unwrap();
        assert_eq!(varda.mixer_ref().frame_timing.refresh_at, Some(slot));
    }

    /// State publishing is gated to every tenth frame to keep snapshot cost off
    /// the per-frame path. Observed through the shared reader the HTTP API uses.
    #[test]
    fn render_headless_publishes_state_every_tenth_frame() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let reader = runner
            .varda
            .as_ref()
            .expect("engine attached")
            .state_reader();
        // Start from a known-empty slot regardless of construction-time publishing.
        reader.write().expect("state lock").take();

        let host = FakeHost::default();
        for frame in 1..10 {
            runner.render_headless(&host);
            assert!(
                reader.read().expect("state lock").is_none(),
                "frame {frame} must not publish"
            );
        }

        runner.render_headless(&host);
        assert_eq!(runner.publish_counter, 10);
        assert!(
            reader.read().expect("state lock").is_some(),
            "the tenth frame publishes engine state"
        );
    }

    /// A flagged shutdown exits and skips the frame's work entirely, so nothing
    /// is rendered or reconciled after the signal.
    /// The API's Shutdown raises the flag the window loop reads: answered
    /// "ok", the windowed app went on running.
    #[test]
    fn the_apis_shutdown_raises_the_flag_the_loop_quits_on() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let mut varda = runner.varda.take().unwrap();
        runner.share_shutdown(&mut varda);
        assert!(!runner
            .shutdown_flag
            .load(std::sync::atomic::Ordering::SeqCst));
        assert!(matches!(
            varda.execute_command(crate::engine::EngineCommand::Shutdown),
            crate::engine::CommandResult::Ok
        ));
        assert!(runner
            .shutdown_flag
            .load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn render_headless_exits_and_skips_work_when_shutdown_flagged() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let workspace = runner
            .config
            .workspace_root
            .clone()
            .expect("headless_config supplies a scratch workspace");
        runner
            .shutdown_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);

        let host = FakeHost::default();
        runner.render_headless(&host);

        // This test writes a default scene to disk. Pin where, so the isolation
        // is enforced rather than merely conventional.
        assert!(
            workspace.join(".varda").join("scene.json").is_file(),
            "the shutdown save must land in the scratch workspace"
        );
        assert_eq!(host.exits.get(), 1, "shutdown exits the event loop");
        assert_eq!(
            host.outputs_created.get(),
            0,
            "shutdown returns before reconciling outputs"
        );
        assert_eq!(
            runner.publish_counter, 0,
            "shutdown returns before the publish gate"
        );
    }

    /// A timeline drag pushes a mutation every frame it moves. Exactly one of
    /// them may become an undo entry, or a two-second drag would bury the rest
    /// of the session's history.
    #[test]
    fn a_held_drag_is_one_undo_entry() {
        let mut prev = false;
        let frames: Vec<bool> = (0..40)
            .map(|_| wants_history_snapshot(&mut prev, true, true))
            .collect();
        assert_eq!(frames.iter().filter(|pushed| **pushed).count(), 1);
        assert!(frames[0], "the entry belongs to the frame that started it");
    }

    /// Releasing and grabbing again is a second edit, and undo has to be able to
    /// step between them.
    #[test]
    fn a_second_drag_gets_its_own_entry() {
        let mut prev = false;
        let mut pushes = 0;
        for gesture in [true, true, false, true, true] {
            if wants_history_snapshot(&mut prev, true, gesture) {
                pushes += 1;
            }
        }
        assert_eq!(pushes, 3, "two drags and the release between them");
    }

    /// Discrete edits are unaffected: every click is still its own step.
    #[test]
    fn clicks_are_not_coalesced() {
        let mut prev = false;
        let pushes = (0..5)
            .filter(|_| wants_history_snapshot(&mut prev, true, false))
            .count();
        assert_eq!(pushes, 5);
    }

    #[test]
    fn the_ui_stall_test_hook_reads_a_time_and_whether_it_holds_the_engine() {
        let ms = std::time::Duration::from_millis;
        assert_eq!(
            UiStall::parse("25"),
            Some(UiStall {
                freeze: ms(25),
                holding_engine: false
            })
        );
        assert_eq!(
            UiStall::parse("40:hold"),
            Some(UiStall {
                freeze: ms(40),
                holding_engine: true
            })
        );
        assert_eq!(UiStall::parse("0"), None);
        assert_eq!(UiStall::parse("25:grab"), None);
        assert_eq!(UiStall::parse("soon"), None);
    }

    /// A frame that changed nothing never takes a snapshot, gesture or not.
    #[test]
    fn a_quiet_frame_records_nothing() {
        let mut prev = false;
        assert!(!wants_history_snapshot(&mut prev, false, true));
        assert!(!wants_history_snapshot(&mut prev, false, false));
    }

    /// The windowed app saves the show on its way out, and once: closing its
    /// window saves it and ends the loop, and the loop's end (`exiting`, where
    /// Quit lands too) must not save it again.
    #[test]
    fn the_show_is_saved_once_on_the_way_out() {
        let Ok(gpu) = GpuContext::new_headless() else {
            return;
        };
        let app = VardaApp::new(gpu, &crate::testing::headless_config()).expect("VardaApp::new");
        let scene = app.session.workspace.scene_path();
        assert!(!scene.exists(), "a new workspace");
        let mut runner = runner();
        runner.varda = Some(Box::new(app));

        runner.shut_down();
        assert!(scene.exists(), "saved on the way out");
        std::fs::remove_file(&scene).expect("remove the saved scene");
        runner.shut_down();
        assert!(!scene.exists(), "saved once");
    }

    /// What the operator is told, as the toasts show it.
    fn said(varda: &VardaApp) -> Vec<String> {
        varda
            .session
            .notifications
            .visible()
            .iter()
            .map(|n| n.message.clone())
            .collect()
    }

    fn recent_in(file: &std::path::Path) -> Vec<std::path::PathBuf> {
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap()
    }

    /// A Save As through the API is answered by the runner's own frames once
    /// the show has moved: each frame takes in what the writer has landed.
    #[test]
    fn the_headless_runner_answers_a_save_as_once_the_show_has_moved() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let host = FakeHost::default();
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("Day 2");
        let varda = runner.varda.as_mut().unwrap();
        varda.slow_saves_for_test(std::time::Duration::from_millis(300));
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        varda
            .command_sender()
            .send((
                crate::engine::EngineCommand::SaveShowAs {
                    folder: target.clone(),
                },
                Some(reply),
            ))
            .unwrap();
        let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let result = loop {
            runner.render_headless(&host);
            if let Ok(result) = answer.try_recv() {
                break result;
            }
            assert!(std::time::Instant::now() < until, "never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        assert!(
            matches!(result, crate::engine::CommandResult::Ok),
            "{result:?}"
        );
        assert_eq!(runner.varda.as_ref().unwrap().show_folder(), target);
    }

    /// A show saved on a layout the operator had saved as "Broadcast",
    /// before Broadcast was a built-in, opens named after that layout — kept
    /// now as "Broadcast (saved)" — not after the built-in.
    #[test]
    fn a_show_opens_on_the_saved_layout_a_builtin_took_the_name_of() {
        use crate::usecases::ui::screen_layout::{
            remove_pane, store_saved, Builtin, Pane, SavedLayout, SavedLayoutStore, ScreenLayout,
        };
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("layouts.json");
        let mut mine = Builtin::Classic.dock();
        remove_pane(&mut mine, Pane::Setup);
        let saved = SavedLayout {
            name: "Broadcast".into(),
            dock: mine.clone(),
            was: None,
        };
        store_saved(&path, &[saved]).unwrap();
        runner.saved_layouts = SavedLayoutStore::open(path);
        // As Nida 0.5 saved it, when "Broadcast" was the operator's own.
        runner.layout.screen = ScreenLayout {
            name: "Broadcast".into(),
            dock: mine,
            locked: false,
            naming: 0,
        };
        let varda = runner.varda.as_mut().unwrap();
        varda.save_workspace(&runner.layout).unwrap();
        varda.flush_saves();
        let folder = varda.show_folder().to_path_buf();
        runner.open_show(&folder);
        assert_eq!(runner.layout.screen.name, "Broadcast (saved)");
        assert!(!runner.layout.screen.has(Pane::Setup), "its panels");
    }

    /// "Layout ▸ Save as" keeps the layout at once and hands its file to the
    /// show's writer: the outputs do not wait for the disk, and it is said
    /// to be saved once it is there.
    #[test]
    fn a_layout_saved_is_written_in_the_background_and_said_once_there() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("layouts.json");
        runner.saved_layouts =
            crate::usecases::ui::screen_layout::SavedLayoutStore::open(path.clone());
        runner
            .varda
            .as_mut()
            .unwrap()
            .slow_saves_for_test(std::time::Duration::from_millis(300));
        let mut actions = ui::UIActions::new();
        actions.session.save_layout_as = Some("Show day".into());
        let t = std::time::Instant::now();
        runner.apply_screen_layout_actions(&mut actions);
        assert!(t.elapsed() < std::time::Duration::from_millis(150));
        assert_eq!(runner.saved_layouts.layouts().len(), 1, "kept at once");
        assert_eq!(runner.layout.screen.name, "Show day");
        assert!(!path.exists(), "not written yet");
        let varda = runner.varda.as_mut().unwrap();
        assert!(!said(varda).iter().any(|m| m.contains("Saved layout")));
        varda.flush_saves();
        assert!(path.is_file());
        assert!(said(varda).contains(&"Saved layout \"Show day\"".to_string()));
    }

    /// A saved layout whose file does not land is not kept: once it is
    /// known, the menu is read again from the file, as a restart would.
    #[test]
    fn a_layout_whose_file_does_not_land_is_not_kept() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        // A file where the layouts' folder should be.
        std::fs::write(tmp.path().join("settings"), "").unwrap();
        let path = tmp.path().join("settings").join("layouts.json");
        runner.saved_layouts =
            crate::usecases::ui::screen_layout::SavedLayoutStore::open(path.clone());
        let mut actions = ui::UIActions::new();
        actions.session.save_layout_as = Some("Show day".into());
        runner.apply_screen_layout_actions(&mut actions);
        assert_eq!(runner.saved_layouts.layouts().len(), 1, "kept at once");
        let varda = runner.varda.as_mut().unwrap();
        varda.flush_saves();
        assert!(said(varda)
            .iter()
            .any(|m| m.starts_with("Could not save layouts to")));
        runner.take_lost_layouts();
        assert!(
            runner.saved_layouts.layouts().is_empty(),
            "as the file has it"
        );
    }

    /// A layout write that breaks is a failure like any other: once it has
    /// landed the menu is as the file has it, and later ones still recover.
    #[test]
    fn a_layout_write_that_breaks_is_recovered_from() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("layouts.json");
        runner.saved_layouts =
            crate::usecases::ui::screen_layout::SavedLayoutStore::open(path.clone());
        let save = |runner: &mut UIRunner, name: &str| {
            let mut actions = ui::UIActions::new();
            actions.session.save_layout_as = Some(name.into());
            runner.apply_screen_layout_actions(&mut actions);
        };
        for _ in 0..2 {
            runner.varda.as_mut().unwrap().break_files_for_test(1);
            save(&mut runner, "Broken");
            runner.varda.as_mut().unwrap().flush_saves();
            runner.take_lost_layouts();
            assert!(
                runner.saved_layouts.layouts().is_empty(),
                "as the file has it"
            );
        }
    }

    /// A layout write that failed is not undone while a later one is on its
    /// way: that one wrote the whole list, the failed change with it, and
    /// once it lands the menu and the file agree.
    #[test]
    fn a_failed_layout_write_waits_for_the_one_after_it() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("layouts.json");
        runner.saved_layouts =
            crate::usecases::ui::screen_layout::SavedLayoutStore::open(path.clone());
        let save = |runner: &mut UIRunner, name: &str| {
            let mut actions = ui::UIActions::new();
            actions.session.save_layout_as = Some(name.into());
            runner.apply_screen_layout_actions(&mut actions);
        };
        runner.varda.as_mut().unwrap().refuse_files_for_test(1);
        save(&mut runner, "Refused");
        runner.varda.as_mut().unwrap().flush_saves();
        runner
            .varda
            .as_mut()
            .unwrap()
            .slow_saves_for_test(std::time::Duration::from_millis(300));
        save(&mut runner, "Later");
        runner.take_lost_layouts();
        assert_eq!(
            runner.saved_layouts.layouts().len(),
            2,
            "not while one is on its way"
        );
        runner.varda.as_mut().unwrap().flush_saves();
        runner.take_lost_layouts();
        assert_eq!(runner.saved_layouts.layouts().len(), 2);
        let on_disk = crate::usecases::ui::screen_layout::SavedLayoutStore::open(path);
        assert_eq!(on_disk.layouts().len(), 2, "the later write carried both");
    }

    /// A Save As that has landed puts the show it moved to at the head of
    /// the Recent list, written in the background; a show that has not
    /// moved writes nothing.
    #[test]
    fn a_show_that_moved_heads_the_recent_list() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        let tmp = tempfile::tempdir().unwrap();
        let recent = tmp.path().join("recent-shows.json");
        runner.recent_file = Some(recent.clone());
        runner.named_after = runner.varda.as_ref().unwrap().show_folder().to_path_buf();
        runner.show_moved();
        runner.varda.as_mut().unwrap().flush_saves();
        assert!(!recent.exists(), "nothing moved, nothing written");
        let target = tmp.path().join("Day 2");
        let varda = runner.varda.as_mut().unwrap();
        varda
            .save_show_as(&target, &crate::usecases::ui::UILayoutState::default())
            .unwrap();
        varda.flush_saves();
        runner.show_moved();
        assert_eq!(runner.named_after, target);
        runner.varda.as_mut().unwrap().flush_saves();
        assert_eq!(recent_in(&recent).first(), Some(&target));
    }

    /// Quitting while a Save As copies the show: the show it moves to is
    /// the one the Recent list keeps, to open next time.
    #[test]
    fn quitting_mid_save_as_keeps_the_new_show_for_next_time() {
        let Some(mut runner) = headless_runner() else {
            return;
        };
        // As the window's runner quits.
        runner.config.headless = false;
        let tmp = tempfile::tempdir().unwrap();
        let recent = tmp.path().join("recent-shows.json");
        runner.recent_file = Some(recent.clone());
        runner.named_after = runner.varda.as_ref().unwrap().show_folder().to_path_buf();
        let target = tmp.path().join("Day 2");
        let varda = runner.varda.as_mut().unwrap();
        varda.slow_saves_for_test(std::time::Duration::from_millis(300));
        varda
            .save_show_as(&target, &crate::usecases::ui::UILayoutState::default())
            .unwrap();
        runner.shut_down();
        assert_eq!(recent_in(&recent).first(), Some(&target));
        assert!(target.join(".varda/scene.json").is_file());
    }

    /// A test's runner keeps no Recent list of this computer's: its first
    /// frame names the window after its scratch show, which once put that
    /// show at the head of the operator's list, for Nida to reopen.
    #[test]
    fn a_test_runner_keeps_no_recent_list_of_this_computers() {
        assert_eq!(runner().recent_file, None);
        let Some(mut runner) = headless_runner() else {
            return;
        };
        runner.show_moved();
        assert_eq!(runner.recent_file, None, "nowhere to write it");
    }
}
