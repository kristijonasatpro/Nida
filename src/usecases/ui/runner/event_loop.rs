//! winit event-loop wiring: the `ApplicationHandler` callbacks and the
//! [`WindowHost`] abstraction over the parts of `ActiveEventLoop` the render
//! paths need.
//!
//! This is the only place that reacts to raw winit events. Everything it decides
//! is delegated straight back to `UIRunner`, so the per-frame logic stays
//! testable without an event loop — see `/spec/app-presentation-boundary.md`.

use super::UIRunner;
use crate::app::VardaApp;
use crate::renderer::context::GpuContext;
use winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::ActiveEventLoop,
    window::{Window, WindowId},
};

/// The slice of the winit event loop that the per-frame render paths actually use.
///
/// `render_headless` and `render` are long functions, but they touch
/// `ActiveEventLoop` in only a handful of places: to exit, and to let `VardaApp`
/// reconcile output windows and monitors (which must happen on the event-loop
/// thread). Naming that dependency as a trait lets the render paths be driven
/// without a real event loop, which is otherwise impossible to construct in a
/// test. See `/spec/app-presentation-boundary.md` for why winit access is
/// confined to this boundary.
pub(crate) trait WindowHost {
    /// Ask the event loop to terminate.
    fn exit(&self);
    /// Create any output windows the engine has queued.
    fn create_pending_outputs(&self, varda: &mut VardaApp);
    /// Refresh the engine's cached monitor list.
    fn refresh_monitors(&self, varda: &mut VardaApp);
    /// Create any queued interactive (HTML) surfaces.
    #[cfg(feature = "html")]
    fn create_pending_interactive(&self, varda: &mut VardaApp);
}

impl WindowHost for ActiveEventLoop {
    fn exit(&self) {
        ActiveEventLoop::exit(self);
    }

    fn create_pending_outputs(&self, varda: &mut VardaApp) {
        varda.create_pending_outputs(self);
    }

    fn refresh_monitors(&self, varda: &mut VardaApp) {
        varda.refresh_monitors(self);
    }

    #[cfg(feature = "html")]
    fn create_pending_interactive(&self, varda: &mut VardaApp) {
        varda.create_pending_interactive(self);
    }
}

impl ApplicationHandler for UIRunner {
    /// The engine thread drew a frame (see `engine_thread`): the control
    /// window shows it at its next turn.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, (): ()) {
        self.ui_due = true;
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        // Guard re-entry (resumed can be called multiple times on some platforms)
        if self.varda.is_some() {
            return;
        }

        let startup_t0 = std::time::Instant::now();
        log::info!("[STARTUP] resumed() entered — beginning initialization");

        if self.config.headless {
            // Headless: no main window, no egui — GPU without window surface
            log::info!("[STARTUP] Headless mode: skipping main window creation");
            let gpu = match GpuContext::new_headless() {
                Ok(gpu) => gpu,
                Err(e) => {
                    log::error!("Failed to create headless GPU context: {e}");
                    event_loop.exit();
                    return;
                }
            };
            self.finish_init(gpu, None, startup_t0, event_loop);
            // Headless has no window, so no events ever arrive to wake the
            // loop. winit's default `ControlFlow` is `Wait`, which suspends
            // the thread until one does — so without this the loop runs only
            // while incidental startup events trickle in, then blocks forever
            // and stops draining commands. Pacing is handled by the sleep in
            // `about_to_wait`, so polling here costs no spin.
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
        } else {
            // Windowed: create main UI window, then spawn GPU init on a background
            // thread so we return from resumed() immediately.  On macOS (especially
            // under Rosetta / Intel), blocking the main thread during Metal device
            // creation causes a GCD dispatch-queue deadlock because Metal needs to
            // dispatch work back to the main queue.  By returning to the event loop
            // we keep that queue alive; about_to_wait() polls the thread handle and
            // finishes initialization once the GPU is ready.
            let window_icon = {
                static ICON_BYTES: &[u8] = include_bytes!("../../../../assets/icon.png");
                image::load_from_memory(ICON_BYTES).ok().and_then(|img| {
                    let rgba = img.into_rgba8();
                    let (w, h) = (rgba.width(), rgba.height());
                    winit::window::Icon::from_rgba(rgba.into_raw(), w, h).ok()
                })
            };
            let mut window_attrs = crate::app::app_id::tagged(
                Window::default_attributes()
                    .with_title("Nida")
                    .with_inner_size(winit::dpi::LogicalSize::new(1920, 1080)),
                crate::app::app_id::MAIN,
            );
            if let Some(icon) = window_icon {
                window_attrs = window_attrs.with_window_icon(Some(icon));
            }

            let window_static: &'static Window = match event_loop.create_window(window_attrs) {
                Ok(w) => {
                    log::info!("[STARTUP] Window created ({:.0?})", startup_t0.elapsed());
                    Box::leak(Box::new(w))
                }
                Err(e) => {
                    log::error!("Failed to create window: {e}");
                    event_loop.exit();
                    return;
                }
            };
            self.main_window_id = Some(window_static.id());
            self.window = Some(window_static);

            // Kick a redraw so macOS marks the window as "live" — required for
            // Metal/CALayer to function correctly (see wgpu#5722).
            window_static.request_redraw();

            // Create wgpu instance + surface on the main thread (macOS requires
            // NSView/CAMetalLayer access from the main thread), then hand off
            // adapter/device creation to a background thread.
            log::info!("[STARTUP] Creating surface on main thread...");
            let (instance, surface, size) =
                match GpuContext::create_surface_for_window(window_static) {
                    Ok(triple) => triple,
                    Err(e) => {
                        log::error!("Failed to create surface: {e}");
                        event_loop.exit();
                        return;
                    }
                };

            log::info!("[STARTUP] Spawning GPU adapter/device init on background thread...");
            self.startup_t0 = Some(startup_t0);
            self.gpu_init_handle = Some(std::thread::spawn(move || {
                pollster::block_on(GpuContext::new_with_surface(instance, surface, size))
            }));

            // Return immediately — about_to_wait() will complete initialization.
            // Keep polling so the event loop stays responsive.
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.varda.is_none() && self.engine.is_none() {
            return;
        }
        if self.main_window_id == Some(window_id) {
            if let (Some(window), Some(egui_state)) = (self.window, &mut self.egui_state) {
                if egui_state.on_window_event(window, &event).consumed {
                    return;
                }
            }
            match event {
                WindowEvent::CloseRequested => {
                    log::info!("Close requested, saving workspace and exiting...");
                    self.quit(event_loop);
                }
                WindowEvent::Resized(new_size) => {
                    self.cached_screen_size = new_size;
                    if let (Some(ws), Some(gpu)) = (&mut self.window_surface, &self.gpu) {
                        ws.resize(gpu, new_size);
                    }
                }
                WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                    self.cached_scale_factor = scale_factor as f32;
                }
                WindowEvent::RedrawRequested => {
                    self.render(event_loop);
                    // Frame pacing: don't request_redraw() here.
                    // about_to_wait() asks for the next once the engine
                    // thread has drawn one.
                }
                _ => {}
            }
        } else {
            // An output (or interactive HTML) window: the engine's, so it is
            // borrowed for events that change it — not for every pointer move.
            let wanted = matches!(
                event,
                WindowEvent::CloseRequested
                    | WindowEvent::Resized(_)
                    | WindowEvent::Moved(_)
                    | WindowEvent::Occluded(_)
                    | WindowEvent::KeyboardInput { .. }
            ) || cfg!(feature = "html");
            if wanted {
                let handled = self.with_engine(|this| {
                    if let Some(varda) = this.varda.as_mut() {
                        output_window_event(varda, window_id, event);
                    }
                });
                if handled.is_none() {
                    log::warn!("An output window's event went unhandled: the engine was busy");
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // The API's Shutdown or Ctrl-C: the windowed app saves and quits as
        // on ⌘Q (headless does so in `render_headless`) — Ctrl-C while the
        // GPU is still being set up included. Answered "ok", it went on
        // running.
        if !self.config.headless
            && self
                .shutdown_flag
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            log::info!("Shutdown requested: saving workspace and exiting...");
            self.quit(event_loop);
            return;
        }

        // ── Phase 2 of deferred GPU init: poll the background thread ────
        if let Some(handle) = self.gpu_init_handle.as_ref() {
            if handle.is_finished() {
                let handle = self.gpu_init_handle.take().unwrap();
                let startup_t0 = self.startup_t0.take().unwrap();
                match handle.join().expect("GPU init thread panicked") {
                    Ok((gpu, win_surface)) => {
                        log::info!("[STARTUP] GPU context ready ({:.0?})", startup_t0.elapsed());
                        self.finish_init(gpu, Some(win_surface), startup_t0, event_loop);
                    }
                    Err(e) => {
                        log::error!("Failed to create render context: {e}");
                        event_loop.exit();
                        return;
                    }
                }
            } else {
                // GPU init still in progress — keep the event loop alive.
                event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
                return;
            }
        }

        if self.config.headless {
            // Read target_fps from engine state (runtime-mutable via UI/API).
            let target_fps = self
                .varda
                .as_ref()
                .map_or(self.config.target_fps, |varda| varda.target_fps());
            // Reasserted every iteration so the loop cannot be left in `Wait`
            // by any other code path; see the note in `resumed`.
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
            // Headless: adaptive sleep-based pacing
            if target_fps > 0 {
                if let Some(deadline) = self.cadence_anchor {
                    let now = std::time::Instant::now();
                    if deadline > now {
                        std::thread::sleep(deadline - now);
                    }
                }
            }
            self.render_headless(event_loop);
            self.advance_cadence_anchor(target_fps);
        } else {
            // Windowed: the engine thread paces the show (see
            // `engine_thread`); the control window draws once it has drawn a
            // frame — or every tenth of a second without one, so the UI
            // stays live should the engine stop drawing.
            if self.engine.as_ref().is_some_and(|e| e.baton().is_gone()) {
                log::error!("The engine thread stopped: exiting");
                self.quit(event_loop);
                return;
            }
            let quiet = self
                .last_ui_frame
                .is_none_or(|at| at.elapsed() >= UI_FALLBACK);
            if std::mem::take(&mut self.ui_due) || quiet {
                if let Some(w) = self.window {
                    w.request_redraw();
                }
            }
            event_loop.set_control_flow(winit::event_loop::ControlFlow::WaitUntil(
                std::time::Instant::now() + UI_FALLBACK,
            ));
        }
    }

    /// The loop is ending. On macOS that includes Quit — ⌘Q, the Dock's
    /// Quit, a script's `quit` — which never sends the window
    /// `CloseRequested`: macOS closes the windows, winit calls this, and the
    /// process ends as soon as it returns. Without saving here a show quit
    /// that way lost everything since it was last saved (the show laptop,
    /// 2026-09-30: a channel and an output set up over two hours). Headless
    /// saves on its own way out (`render_headless`).
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if !self.config.headless && !self.shut_down {
            log::info!("Quit: saving workspace and exiting...");
            self.shut_down();
        }
        // Night Shift, if Nida holds it off, given back however it ends:
        // headless too, whose Quit (⌘Q, the Dock) comes here without its
        // shutdown flag (see `night_shift`).
        if let Some(kept) = self.night_shift.give_back_now() {
            log::info!("Quit: {kept:?}");
        }
    }
}

/// How long the control window goes without the engine thread drawing a
/// frame before it draws one anyway.
const UI_FALLBACK: std::time::Duration = std::time::Duration::from_millis(100);

/// An event on an output window (main thread, engine borrowed).
fn output_window_event(varda: &mut VardaApp, window_id: WindowId, event: WindowEvent) {
    // Interactive HTML window: forward input to the WebView and consume.
    #[cfg(feature = "html")]
    if varda.handle_interactive_event(window_id, &event) {
        return;
    }
    match event {
        WindowEvent::CloseRequested => {
            if let Some(name) = varda.close_output_window_by_id(window_id) {
                log::info!("Output window '{name}' closed");
            }
        }
        WindowEvent::Resized(new_size) => {
            varda.resize_output_window_by_id(window_id, new_size);
        }
        WindowEvent::Moved(position) => {
            varda.move_output_window_by_id(window_id, position);
        }
        WindowEvent::Occluded(hidden) => {
            varda.occlude_output_window_by_id(window_id, hidden);
        }
        // Esc on an output takes it off its screen, like leaving a
        // slideshow: the way out when one lands on the wrong screen.
        WindowEvent::KeyboardInput { event, .. }
            if event.state.is_pressed()
                && event.logical_key
                    == winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape) =>
        {
            if let Some(name) = varda.take_output_off_screen_by_id(window_id) {
                log::info!("Output '{name}' taken off its screen (Esc)");
            }
        }
        _ => {}
    }
}

impl UIRunner {
    /// Save the show and end the app.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        self.shut_down();
        event_loop.exit();
    }

    /// Save the show and stop the API — once, however the windowed app ends:
    /// its window closed, or Quit (see `exiting`). The engine thread is
    /// stopped first, so nothing draws into windows that are going.
    pub(super) fn shut_down(&mut self) {
        if std::mem::replace(&mut self.shut_down, true) {
            return;
        }
        if let Some(engine) = self.engine.take() {
            if let Some(app) = engine.stop() {
                self.varda = Some(app);
            }
        }
        if let Some(varda) = self.varda.as_mut() {
            if let Err(e) = varda.save_workspace(&self.layout) {
                log::error!("{e}");
            }
            varda.refuse_commands();
        }
        // A Save As that landed as Nida quit moved the show: it is the one
        // to open next time, the Recent list written before Nida goes.
        if !self.config.headless {
            self.show_moved();
            if let Some(varda) = self.varda.as_mut() {
                varda.flush_saves();
            }
        }
        if let Some(api) = self.api_handle.take() {
            api.shutdown();
        }
        // Night Shift, if Nida holds it off, given back (see `night_shift`).
        if let Some(kept) = self.night_shift.give_back_now() {
            log::info!("Quit: {kept:?}");
        }
    }
}
