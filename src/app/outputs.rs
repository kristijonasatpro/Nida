//! Output action processing for `VardaApp` (unified windowed + headless).

use super::VardaApp;
use crate::renderer::context::{
    AwaitedDisplay, HeadlessOutput, OutputSource, OutputTarget, OutputWindow, Reopen,
    SurfaceAssignment, UnifiedOutput,
};
use crate::renderer::edge_blend::SurfaceOverlapZones;

/// Logical box a new output window is fitted into when the stage has no saved
/// size for it.
const DEFAULT_OUTPUT_WINDOW_BOX: (f64, f64) = (1280.0, 720.0);

/// Frames an Auto resolution change must keep being wanted before it happens:
/// half a second at 60 fps. Moving the master reallocates every texture, so a
/// screen renegotiating its mode, or a cable being reseated, must not do it
/// twice on air.
pub(crate) const AUTO_RESOLUTION_SETTLE_FRAMES: u32 = 30;

/// Opening size for a new output window, as the master's aspect ratio fitted
/// inside [`DEFAULT_OUTPUT_WINDOW_BOX`].
///
/// The master is letterboxed into whatever size the window ends up, so this only
/// picks the starting shape — but starting at the master's aspect means a
/// vertical or square stage doesn't open in a 16:9 window with bars down both
/// sides that the operator has to drag out by hand.
fn default_output_window_size(render_width: u32, render_height: u32) -> (f64, f64) {
    let (box_w, box_h) = DEFAULT_OUTPUT_WINDOW_BOX;
    if render_width == 0 || render_height == 0 {
        return (box_w, box_h);
    }
    let aspect = f64::from(render_width) / f64::from(render_height);
    if aspect >= box_w / box_h {
        (box_w, box_w / aspect)
    } else {
        (box_h * aspect, box_h)
    }
}

impl VardaApp {
    /// The screen of the first output window — the one the show goes to,
    /// whose refresh frames are best started on.
    pub fn clock_monitor(&self) -> Option<winit::monitor::MonitorHandle> {
        self.output.outputs.iter().find_map(|output| match output {
            UnifiedOutput::Window(window) => window.screen().cloned(),
            UnifiedOutput::Headless(_) => None,
        })
    }

    /// Close every output a show lists that `listed` (a show's outputs, by
    /// uuid) does not, and let go of such outputs waiting to open: a show
    /// opened puts up its own outputs only — "yes, should close", the user
    /// (2026-10-04), the previous show's staying up having been the question.
    /// The stream's and the recording's own outputs, which no show lists,
    /// stay.
    pub(crate) fn close_outputs_not_in(&mut self, listed: &[String]) {
        use crate::engine::OutputCommands as _;
        let unlisted: Vec<String> = self
            .output
            .outputs
            .iter()
            .filter(|output| {
                !matches!(output, UnifiedOutput::Headless(h)
                    if matches!(h.target, OutputTarget::Stream | OutputTarget::Record))
            })
            .map(|output| output.uuid().to_string())
            .filter(|uuid| !listed.contains(uuid))
            .collect();
        for uuid in unlisted {
            if let Err(e) = self.close_output(&uuid) {
                log::warn!("Output {uuid} not closed: {e}");
            }
        }
        self.output
            .pending_output_creates
            .retain(|config| listed.contains(&config.uuid));
    }

    /// Make what a recording and the stream draw with once, on a thread of
    /// its own, so the first REC or STREAM finds the GPU's programs for them
    /// made: made then, they stopped the show's frames for a third of a
    /// second (a laptop, 2026-10-04); made a second time, they take a few
    /// milliseconds.
    pub fn make_recording_ahead(&self) {
        let device = self.context.device.clone();
        let spawned = std::thread::Builder::new()
            .name("recording-ahead".into())
            .spawn(move || {
                for target in [
                    crate::renderer::context::OutputTarget::Record,
                    crate::renderer::context::OutputTarget::Stream,
                ] {
                    let made = crate::renderer::context::HeadlessOutput::new(
                        &device,
                        "made ahead".into(),
                        crate::engine::OutputSource::Master,
                        target,
                        64,
                        64,
                    );
                    drop(made);
                }
                drop(crate::renderer::nv12::Nv12Converter::new(&device, 64, 64));
                log::info!("Recording and stream made ready ahead");
            });
        if let Err(e) = spawned {
            log::warn!("Recording not made ready ahead: {e}");
        }
    }

    /// The screens output windows are on, asked of each window now — on the
    /// main thread; `None` elsewhere, or while a window's screen is not
    /// known.
    /// Whether an output window is open: on a screen, which Night Shift
    /// warms (see `crate::night_shift`). A Blackmagic device's output is not
    /// a window, and is not warmed.
    pub fn outputs_on_screens(&self) -> bool {
        self.output
            .outputs
            .iter()
            .any(|output| matches!(output, UnifiedOutput::Window(_)))
    }

    pub fn output_screens_now(&self) -> Option<Vec<winit::monitor::MonitorHandle>> {
        self.output
            .outputs
            .iter()
            .filter_map(|output| match output {
                UnifiedOutput::Window(window) => Some(window.window.get().and_then(winit::window::Window::current_monitor)),
                UnifiedOutput::Headless(_) => None,
            })
            .collect()
    }

    /// What starts frames — a screen's refresh (`display`), or the timer —
    /// as the frame loop reports it.
    pub fn set_frame_clock(&mut self, description: String, display: Option<u32>) {
        log::info!("Frame clock: {description}");
        self.frame_stats.clock = description;
        self.frame_stats.clock_display = display;
    }

    /// Make again the output windows whose screen has stopped showing their
    /// frames, or asked to be (`OutputWindow::reopen`): closed, and made from
    /// their own saved settings in their place — what the operator did by
    /// hand, sending the output to its screen again. Seen with Stage Manager
    /// on, after an app switch. One whose frames go unshown again within half
    /// a minute waits longer each time before it is made again (1, 2, 4 …
    /// 60 s). Main thread.
    pub fn reopen_unshown_outputs(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        let now = std::time::Instant::now();
        // Not a range: a window that could not be made again leaves the list
        // one shorter.
        let mut idx = 0;
        while idx < self.output.outputs.len() {
            let at = idx;
            idx += 1;
            let UnifiedOutput::Window(window) = &mut self.output.outputs[at] else {
                continue;
            };
            let Some(why) = window.reopen else {
                continue;
            };
            #[cfg(target_os = "macos")]
            if why == Reopen::Unshown && window.unshown_because_hidden() {
                log::info!("Output '{}': hidden, so not made again", window.name);
                continue;
            }
            let uuid = window.uuid.clone();
            let name = window.name.clone();
            let calibration_mode = window.calibration_mode;
            let before = self.output.reopened.get(&uuid).copied();
            #[cfg(target_os = "macos")]
            let shown_here = window.shown_here();
            #[cfg(not(target_os = "macos"))]
            let shown_here = true;
            if why == Reopen::Unshown {
                match unshown_action(before, shown_here, now) {
                    UnshownAction::MakeAgain => {}
                    UnshownAction::Wait => continue,
                    UnshownAction::GiveUp => {
                        #[cfg(target_os = "macos")]
                        window.stop_watching_unshown();
                        log::warn!(
                            "Output '{name}': its screen says none of its frames were shown, made \
                             again or not — a screen that never says (DisplayLink, AirPlay, \
                             Sidecar): not made again for that"
                        );
                        self.session.notifications.notify_once(
                            format!("output_unreported:{uuid}"),
                            crate::notifications::NotificationLevel::Info,
                            format!(
                                "Output '{name}' is on a screen that does not say what it shows \
                                 (a DisplayLink or AirPlay screen): Nida no longer makes its \
                                 window again"
                            ),
                        );
                        continue;
                    }
                }
            }
            let config = crate::persistence::output_config(&self.output.outputs[at]);
            if let UnifiedOutput::Window(old) = self.output.outputs.remove(at) {
                old.destroy();
            }
            self.output.pending_output_creates.push(config);
            self.create_pending_outputs(event_loop);
            // Made at the end of the list: back to its place, which decides
            // which screen's refresh starts frames (the first window's).
            match self.output.outputs.iter().position(|o| o.uuid() == uuid) {
                Some(made) => {
                    let output = self.output.outputs.remove(made);
                    let place = at.min(self.output.outputs.len());
                    self.output.outputs.insert(place, output);
                    if let UnifiedOutput::Window(window) = &mut self.output.outputs[place] {
                        window.calibration_mode = calibration_mode;
                    }
                }
                // Not made (the error is logged and shown): the next output
                // has moved up into its place.
                None => idx = at,
            }
            if why == Reopen::Asked {
                log::info!("Output '{name}': made its window again, as asked");
                continue;
            }
            self.output
                .reopened
                .insert(uuid.clone(), Reopened::after(before, now));
            log::warn!("Output '{name}': made its window again");
            self.session.notifications.notify_once(
                format!("output_reopened:{uuid}"),
                crate::notifications::NotificationLevel::Info,
                format!(
                    "Output '{name}' stopped showing on its screen (macOS took its frames and \
                     showed none) — Nida made its window again"
                ),
            );
        }
    }

    /// Have an output's window made again (see `reopen_unshown_outputs`) at
    /// the next frame of the control window, whenever it was last made again.
    ///
    /// # Errors
    ///
    /// No output has `output_uuid`, or it has no window (a recording, say).
    pub fn request_reopen_output(&mut self, output_uuid: &str) -> anyhow::Result<()> {
        let idx = self.resolve_output(output_uuid)?;
        let UnifiedOutput::Window(window) = &mut self.output.outputs[idx] else {
            anyhow::bail!("Output '{output_uuid}' has no window to make again");
        };
        window.reopen = Some(Reopen::Asked);
        self.output.reopened.remove(output_uuid);
        Ok(())
    }

    /// Create pending outputs (deferred from UI actions).
    /// Windowed/Display outputs need the event loop; headless outputs are created directly.
    pub fn create_pending_outputs(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        use winit::window::Window;

        let pending: Vec<crate::scene::OutputConfig> =
            std::mem::take(&mut self.output.pending_output_creates);
        if !pending.is_empty() {
            // A restored display target is matched by monitor name, so the list
            // must be current here — on the first frame it has not been filled yet.
            self.refresh_monitors(event_loop);
        }
        for config in pending {
            // One output, one window: a second for an output already open —
            // asked twice, by a show opened and a window made again in the
            // same frame — would leave two of it, both saved with the show.
            if self.output.outputs.iter().any(|o| o.uuid() == config.uuid) {
                log::warn!("Output '{}' is open already: not made twice", config.name);
                continue;
            }
            // One-time migration: pre-8i.5 `.varda` files stored warp on the
            // assignment. Move it onto the surface (first assignment wins; an
            // existing surface warp — e.g. dome mesh — takes precedence).
            for a in &config.surface_assignments {
                if let Some(warp) = &a.legacy_warp_mode {
                    if let Some((_, surface)) = self
                        .output
                        .surface_manager
                        .find_by_uuid_mut(&a.surface_uuid)
                    {
                        if surface.warp.is_none() {
                            surface.warp = Some(warp.clone());
                        }
                    }
                }
            }
            let idx = self.output.outputs.len() + 1;
            let name = if config.name.is_empty() {
                format!("Output {idx}")
            } else {
                config.name.clone()
            };
            let target = crate::persistence::config_to_target_pub(&config.target);

            if target.is_windowed() {
                // Windowed/Display: needs an OS window
                // Not made key as it opens: made — or made again — an output
                // must not take the keyboard from the control window, where
                // the operator may be typing. (Clicked, it can still be key:
                // Escape there gives it back.)
                let mut window_attrs = super::app_id::tagged(
                    Window::default_attributes()
                        .with_title(crate::renderer::context::output_title(&name))
                        .with_active(false),
                    super::app_id::OUTPUT,
                );

                // Restore saved window size, or open at the master's shape.
                if let Some([w, h]) = config.window_size {
                    window_attrs =
                        window_attrs.with_inner_size(winit::dpi::PhysicalSize::new(w, h));
                } else {
                    let (w, h) = default_output_window_size(self.render_width, self.render_height);
                    window_attrs = window_attrs.with_inner_size(winit::dpi::LogicalSize::new(w, h));
                }

                // Set position hint in attributes (works on some platforms)
                if let Some([x, y]) = config.window_position {
                    window_attrs =
                        window_attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
                }

                match event_loop.create_window(window_attrs) {
                    Ok(window) => {
                        let window_static: &'static Window = Box::leak(Box::new(window));
                        match OutputWindow::new(&self.context, window_static, name.clone()) {
                            Ok(mut output) => {
                                output.uuid.clone_from(&config.uuid);
                                // Force position after full initialization — macOS
                                // ignores with_position() in attrs and surface.configure()
                                // can reset position, so we set it last.
                                if let Some([x, y]) = config.window_position {
                                    let position = winit::dpi::PhysicalPosition::new(x, y);
                                    if let Some(window) = output.window.get() {
                                        window.set_outer_position(position);
                                    }
                                    output.position = Some(position);
                                    log::info!(
                                        "Restored output '{}' position to ({}, {})",
                                        output.name,
                                        x,
                                        y
                                    );
                                }
                                // Restore surface assignments from config
                                output.surface_assignments = config
                                    .surface_assignments
                                    .iter()
                                    .map(|a| SurfaceAssignment {
                                        surface_uuid: a.surface_uuid.clone(),
                                        enabled: a.enabled,
                                        overlap_zones: SurfaceOverlapZones::default(),
                                    })
                                    .collect();
                                output.edge_blend_mode = config.edge_blend_mode;
                                output.edge_blend = config.edge_blend;
                                output.rotation = config.rotation;
                                output.led.clone_from(&config.led);
                                // A display target goes fullscreen on its monitor — directly,
                                // or once it can (see `adopt_awaited_displays`) — and waits
                                // windowed if that monitor is not connected.
                                if let OutputTarget::Display { ref name, .. } = target {
                                    match super::monitors::find(&self.output.cached_monitors, name)
                                    {
                                        #[cfg(not(target_os = "linux"))]
                                        Some((_, handle)) => {
                                            output.set_target(target.clone(), Some(handle.clone()));
                                        }
                                        // Wayland compositors do not reliably honour a fullscreen
                                        // request made while the window is still being mapped —
                                        // Hyprland keeps it only for whichever window gets focus.
                                        // Open it, then move it once it has presented a frame.
                                        #[cfg(target_os = "linux")]
                                        Some(_) => {
                                            output.set_target(OutputTarget::Windowed, None);
                                            output.awaiting_display = Some(AwaitedDisplay {
                                                monitor: name.clone(),
                                                screen: None,
                                                was_missing: false,
                                            });
                                        }
                                        None => {
                                            let output_name = &output.name;
                                            log::warn!(
                                                "Monitor '{name}' not available for output \
                                                 '{output_name}' — windowed until it connects"
                                            );
                                            self.session.notifications.warn(format!(
                                                "Monitor '{name}' not connected — output \
                                                 '{output_name}' opened as a window and moves \
                                                 there when it connects"
                                            ));
                                            output.set_target(OutputTarget::Windowed, None);
                                            output.awaiting_display = Some(AwaitedDisplay {
                                                monitor: name.clone(),
                                                screen: None,
                                                was_missing: true,
                                            });
                                        }
                                    }
                                }
                                if let Err(error) = output
                                    .set_presentation_request(&self.context, config.presentation)
                                {
                                    log::warn!(
                                        "Output '{}' presentation request fell back during restore: {error}",
                                        output.name
                                    );
                                }
                                log::info!("Created output window '{}'", output.name);
                                self.output.outputs.push(UnifiedOutput::Window(output));
                                self.refresh_presentation_notification(
                                    self.output.outputs.len() - 1,
                                );
                            }
                            Err(e) => {
                                log::error!("Failed to create output window: {e}");
                                self.session
                                    .notifications
                                    .error(format!("Failed to create output: {e}"));
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to create output window: {e}");
                        self.session
                            .notifications
                            .error(format!("Failed to create window: {e}"));
                    }
                }
            } else {
                self.restore_headless_output(&config, &name, target);
            }
        }
    }

    /// Make the headless output `config` saves — a recording, stream, NDI,
    /// Syphon or Blackmagic output — named `name`, sending to `target`.
    pub(crate) fn restore_headless_output(
        &mut self,
        config: &crate::scene::OutputConfig,
        name: &str,
        target: OutputTarget,
    ) {
        // Only the master or a bus can be saved as a route: both are
        // named without indices, so neither shifts when channels do.
        let source = match &config.source {
            OutputSource::Master | OutputSource::BusProgram(_) => config.source.clone(),
            other => {
                log::warn!(
                    "Output '{name}': source {other} cannot be restored, sending Master"
                );
                OutputSource::Master
            }
        };
        let mut headless = HeadlessOutput::new(
            &self.context.device,
            name.to_string(),
            source,
            target,
            self.render_width,
            self.render_height,
        );
        headless.uuid.clone_from(&config.uuid);
        headless.audio_bus.clone_from(&config.audio_bus);
        // Restore surface assignments from config
        headless.surface_assignments = config
            .surface_assignments
            .iter()
            .map(|a| SurfaceAssignment {
                surface_uuid: a.surface_uuid.clone(),
                enabled: a.enabled,
                overlap_zones: SurfaceOverlapZones::default(),
            })
            .collect();
        headless.edge_blend_mode = config.edge_blend_mode;
        headless.edge_blend = config.edge_blend;
        headless.rotation = config.rotation;
        headless.led.clone_from(&config.led);
        headless.set_presentation_request(&self.context.device, config.presentation);
        if matches!(&headless.target, OutputTarget::NdiSend { .. }) {
            let resolved = self
                .external_io
                .ndi_manager
                .resolve_presentation(config.presentation);
            headless.set_resolved_presentation(&self.context.device, resolved);
        }
        log::info!("Created headless output '{name}'");
        self.output.outputs.push(UnifiedOutput::Headless(headless));
        self.refresh_presentation_notification(self.output.outputs.len() - 1);
        // A Blackmagic output running when the stage was saved comes back on
        // with it; its device not there, it starts once the device appears
        // (`poll_blackmagic_outputs`), and is saved running meanwhile.
        if config.running
            && matches!(config.target, crate::scene::OutputTargetConfig::DeckLink { .. })
        {
            if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.last_mut() {
                h.keep_running = true;
            }
            // Discovery tells of devices a moment after the stage opens: one
            // not there yet is waited for, and said missing only if it
            // stays so (`tell_outputs_still_waiting`).
            if let crate::engine::CommandResult::Err { message, .. } =
                self.cmd_start_output(&config.uuid)
            {
                log::info!("{message} — waiting for it");
                if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.last_mut() {
                    h.waiting_since = Some(std::time::Instant::now());
                }
            }
        }
    }

    /// Recompute per-surface edge blend for all Auto-mode outputs based on surface topology.
    pub fn recompute_auto_edge_blend(&mut self) {
        use crate::renderer::edge_blend::{
            compute_auto_edge_blend, EdgeBlendMode, MappedRegion, OutputSurfaceInfo,
            SurfaceOverlapZones,
        };

        // Check if any output is in Auto mode — early exit if none.
        let auto_count = self
            .output
            .outputs
            .iter()
            .filter(|o| o.edge_blend_mode() == EdgeBlendMode::Auto)
            .count();
        if auto_count == 0 {
            return;
        }
        log::debug!("[edge-blend] recompute_auto: {auto_count} outputs in Auto mode");

        // Build OutputSurfaceInfo for each output (include surface_uuid in MappedRegion).
        let infos: Vec<OutputSurfaceInfo> = self
            .output
            .outputs
            .iter()
            .enumerate()
            .map(|(idx, output)| {
                let mut regions = Vec::new();
                for assignment in output.surface_assignments() {
                    if let Some((_, surface)) = self
                        .output
                        .surface_manager
                        .find_by_uuid(&assignment.surface_uuid)
                    {
                        let bb = surface.bounding_box();
                        regions.push(MappedRegion {
                            source_key: format!("{:?}", surface.source),
                            bbox: [bb.x, bb.y, bb.width, bb.height],
                            surface_uuid: assignment.surface_uuid.clone(),
                            vertices: surface.vertices.clone(),
                            extra_contours: surface.extra_contours.clone(),
                            holes: surface.hole_contours.clone(),
                        });
                    }
                }
                let default_gamma = output.edge_blend().left.gamma;
                OutputSurfaceInfo {
                    output_idx: idx,
                    edge_blend_mode: output.edge_blend_mode(),
                    default_gamma,
                    regions,
                }
            })
            .collect();

        // Clear overlap zones on all Auto-mode assignments before applying new results.
        for output in &mut self.output.outputs {
            if output.edge_blend_mode() == EdgeBlendMode::Auto {
                for assignment in output.surface_assignments_mut() {
                    assignment.overlap_zones = SurfaceOverlapZones::default();
                }
            }
        }

        // Compute per-surface overlap zones and apply to assignments.
        let results = compute_auto_edge_blend(&infos);
        log::debug!("[edge-blend] computed {} results", results.len());
        for result in &results {
            log::debug!(
                "[edge-blend]   output={} surface={} zones={}",
                result.output_idx,
                result.surface_uuid,
                result.overlap_zones.zones.len(),
            );
        }
        for result in results {
            let output = &mut self.output.outputs[result.output_idx];
            for assignment in output.surface_assignments_mut() {
                if assignment.surface_uuid == result.surface_uuid {
                    assignment.overlap_zones = result.overlap_zones;
                    break;
                }
            }
        }
    }

    /// Move every output that is waiting for a monitor onto it, once the
    /// monitor is connected and the window has been mapped. Runs after each
    /// monitor refresh, so a wall powered on after the laptop — or re-plugged
    /// after a restart — takes its output without the operator re-assigning it.
    /// While the resolution is on Auto, move the master to the largest screen
    /// an output is on: a 4K TV gets a 4K picture, Full HD screens a Full HD
    /// one. What the screen runs at is set by the OS (macOS Displays settings,
    /// the Hyprland monitor line); Varda follows it.
    /// Say once, for each output screen, when the show's frame rate does not
    /// fit its refresh (see [`super::monitors::uneven_rate_warning`]). Looked
    /// at every two seconds: a screen's mode or the show's rate may change.
    pub(crate) fn warn_on_uneven_rates(&mut self) {
        let now = std::time::Instant::now();
        if self.output.rate_check_at.is_some_and(|at| now < at) {
            return;
        }
        self.output.rate_check_at = Some(now + std::time::Duration::from_secs(2));
        let fps = self.target_fps;
        let mut warnings = Vec::new();
        for output in &self.output.outputs {
            let UnifiedOutput::Window(w) = output else {
                continue;
            };
            let OutputTarget::Display { name, .. } = &w.target else {
                continue;
            };
            let Some((_, handle)) = super::monitors::find(&self.output.cached_monitors, name)
            else {
                continue;
            };
            let Some(millihertz) = handle.refresh_rate_millihertz() else {
                continue;
            };
            let key = (w.name.clone(), millihertz, fps);
            if self.output.rate_warned.contains(&key) {
                continue;
            }
            let hz = f64::from(millihertz) / 1000.0;
            if let Some(text) = super::monitors::uneven_rate_warning(&w.name, hz, fps) {
                warnings.push((key, text));
            }
        }
        for (key, text) in warnings {
            log::warn!("{text}");
            self.session.notifications.warn(text);
            self.output.rate_warned.insert(key);
        }
    }

    pub(crate) fn follow_output_screens(&mut self) {
        let wanted = if self.render_auto {
            self.largest_output_screen()
        } else {
            None
        };
        self.settle_auto_resolution(wanted);
    }

    /// Physical size of the largest screen a window output is on. Outputs
    /// still waiting for their screen do not count.
    fn largest_output_screen(&self) -> Option<(u32, u32)> {
        self.output
            .outputs
            .iter()
            .filter_map(|output| {
                let UnifiedOutput::Window(w) = output else {
                    return None;
                };
                let OutputTarget::Display { name, .. } = &w.target else {
                    return None;
                };
                let (index, _) = super::monitors::find(&self.output.cached_monitors, name)?;
                let size = self.output.monitor_sizes.get(index)?;
                Some((size.width, size.height))
            })
            .max_by_key(|&(w, h)| u64::from(w) * u64::from(h))
    }

    /// One frame of Auto: move the master to `wanted` once it has been wanted
    /// for [`AUTO_RESOLUTION_SETTLE_FRAMES`] frames running.
    pub(crate) fn settle_auto_resolution(&mut self, wanted: Option<(u32, u32)>) {
        let current = (self.render_width, self.render_height);
        let Some(wanted) = wanted.filter(|&(w, h)| w > 0 && h > 0 && (w, h) != current) else {
            self.auto_resolution_pending = None;
            return;
        };
        let frames = match self.auto_resolution_pending {
            Some((size, frames)) if size == wanted => frames + 1,
            _ => 1,
        };
        if frames < AUTO_RESOLUTION_SETTLE_FRAMES {
            self.auto_resolution_pending = Some((wanted, frames));
            return;
        }
        self.auto_resolution_pending = None;
        log::info!(
            "Auto resolution: {}×{} to match the output screens",
            wanted.0,
            wanted.1
        );
        self.set_render_resolution(wanted.0, wanted.1);
    }

    /// An output covering a screen that has gone — unplugged, its cable
    /// pulled — is let go of it: a plain window again, waiting for that
    /// screen, the operator told; it moves back once the screen returns (see
    /// [`Self::adopt_awaited_displays`]), as at a show opened without it.
    /// Left covering, macOS moved it onto the control window's screen, over
    /// the control window, until the operator found Esc (a laptop,
    /// 2026-10-06). Only once the screen has been missing a moment: the
    /// screens are listed afresh through any change of them.
    pub(crate) fn release_lost_displays(&mut self) {
        let now = std::time::Instant::now();
        for output in &mut self.output.outputs {
            let UnifiedOutput::Window(w) = output else {
                continue;
            };
            let OutputTarget::Display { name, .. } = &w.target else {
                w.display_missing_since = None;
                continue;
            };
            // The screen itself, not its name: names change as screens come
            // and go, and a connected screen renamed is not a lost one.
            let Some(covered) = &w.covered_monitor else {
                w.display_missing_since = None;
                continue;
            };
            if w.awaiting_display.is_some() {
                continue;
            }
            let present = super::monitors::still_connected(covered, &self.output.cached_monitors);
            match screen_gone(present, w.display_missing_since, now) {
                ScreenGone::Here => w.display_missing_since = None,
                ScreenGone::Missing(since) => w.display_missing_since = Some(since),
                ScreenGone::Gone => {
                    let name = name.clone();
                    let screen = Some(covered.clone());
                    log::warn!(
                        "Output '{}': monitor '{name}' is gone — windowed until it connects",
                        w.name
                    );
                    self.session.notifications.warn(format!(
                        "Monitor '{name}' disconnected — output '{}' is a window until it is \
                         back, then moves onto it again",
                        w.name
                    ));
                    w.set_target(OutputTarget::Windowed, None);
                    w.awaiting_display = Some(AwaitedDisplay {
                        monitor: name,
                        screen,
                        was_missing: true,
                    });
                }
            }
        }
    }

    pub(crate) fn adopt_awaited_displays(&mut self) {
        for output in &mut self.output.outputs {
            let UnifiedOutput::Window(w) = output else {
                continue;
            };
            // Hyprland drops a fullscreen request for a window it has not
            // mapped yet, so there the move waits for a first frame. Nowhere
            // else does it need to: on macOS a window opened while the displays
            // slept never presented, and waiting stranded it windowed forever
            // after they woke.
            if cfg!(target_os = "linux") && !w.presented {
                continue;
            }
            let Some(AwaitedDisplay {
                monitor: name,
                screen,
                was_missing,
            }) = w.awaiting_display.clone()
            else {
                continue;
            };
            let monitors = &self.output.cached_monitors;
            let Some(monitor_index) = awaited_index(monitors, screen.as_ref(), || {
                super::monitors::find(monitors, &name).map(|(i, _)| i)
            }) else {
                continue;
            };
            let handle = &monitors[monitor_index].1;
            w.set_target(
                OutputTarget::Display {
                    name: name.clone(),
                    monitor_index,
                },
                Some(handle.clone()),
            );
            if let Err(error) = w.set_presentation_request(&self.context, w.presentation_request) {
                log::warn!(
                    "Output '{}' presentation request fell back on '{name}': {error}",
                    w.name
                );
            }
            log::info!("Output '{}' is now fullscreen on '{name}'", w.name);
            if was_missing {
                self.session.notifications.info(format!(
                    "Monitor '{name}' connected — output '{}' moved onto it",
                    w.name
                ));
            }
        }
    }
}

/// Of `monitors`, the one an output waiting for a screen goes to: the very
/// screen it lost (`screen`), when it lost one — never another — else the
/// one `by_name` finds.
fn awaited_index<H: PartialEq>(
    monitors: &[(String, H)],
    screen: Option<&H>,
    by_name: impl FnOnce() -> Option<usize>,
) -> Option<usize> {
    match screen {
        Some(lost) => monitors.iter().position(|(_, h)| h == lost),
        None => by_name(),
    }
}

/// How long a screen an output covers may be missing from the screens listed
/// before the output is let go of it (see `VardaApp::release_lost_displays`).
const SCREEN_GONE_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// Whether the screen an output covers is there, missing (since when), or
/// gone — missing [`SCREEN_GONE_AFTER`] by `now`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenGone {
    Here,
    Missing(std::time::Instant),
    Gone,
}

fn screen_gone(
    present: bool,
    missing_since: Option<std::time::Instant>,
    now: std::time::Instant,
) -> ScreenGone {
    if present {
        return ScreenGone::Here;
    }
    let since = missing_since.unwrap_or(now);
    if now.saturating_duration_since(since) >= SCREEN_GONE_AFTER {
        ScreenGone::Gone
    } else {
        ScreenGone::Missing(since)
    }
}

/// When an output's window was last made again because its frames went
/// unshown, and how many times running that has happened within half a
/// minute of the time before (see `VardaApp::reopen_unshown_outputs`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Reopened {
    at: std::time::Instant,
    again: u32,
    /// When the remakes counted in `lately` began, and how many: those
    /// within [`REMAKES_LATELY`] of each other.
    since: std::time::Instant,
    lately: u32,
}

impl Reopened {
    /// Made again at `now`, having last been at `before`.
    pub(crate) fn after(before: Option<Self>, now: std::time::Instant) -> Self {
        let again = match before {
            Some(b) if now.saturating_duration_since(b.at) < REOPEN_SOON => b.again + 1,
            _ => 0,
        };
        let (since, lately) = match before {
            Some(b) if now.saturating_duration_since(b.since) < REMAKES_LATELY => {
                (b.since, b.lately + 1)
            }
            _ => (now, 1),
        };
        Self {
            at: now,
            again,
            since,
            lately,
        }
    }

    /// Made again [`MOST_REMAKES`] times within [`REMAKES_LATELY`]: making
    /// it again does not keep it showing.
    fn too_often(&self, now: std::time::Instant) -> bool {
        self.lately >= MOST_REMAKES && now.saturating_duration_since(self.since) < REMAKES_LATELY
    }

    /// The soonest the window may be made again: a second later, doubled for
    /// each time running, a minute at most.
    pub(crate) fn next_allowed(&self) -> std::time::Instant {
        let wait = std::time::Duration::from_secs(1u64 << self.again.min(6)).min(REOPEN_WAIT_MOST);
        self.at + wait
    }
}

/// What becomes of an output whose frames went unshown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnshownAction {
    /// Its window is made again.
    MakeAgain,
    /// Not yet: it was made again a moment ago.
    Wait,
    /// Never again for that: made again a moment ago, the window made then
    /// has shown too few frames said shown — its screen never says what it
    /// shows (a `DisplayLink`, `AirPlay` or Sidecar screen) — or made again
    /// too often lately: making it again only blinks it.
    GiveUp,
}

/// [`UnshownAction`] for an output last made again for this at `before`, a
/// second's frames of it said shown since it was made or put on its screen
/// or not (`shown_here`), at `now`. Only a remake of a moment ago — the one
/// that made this window — tells about its screen: one long ago, or before
/// the output moved, does not.
pub(crate) fn unshown_action(
    before: Option<Reopened>,
    shown_here: bool,
    now: std::time::Instant,
) -> UnshownAction {
    match before {
        Some(b) if !shown_here && now.saturating_duration_since(b.at) < REOPEN_SOON => {
            UnshownAction::GiveUp
        }
        Some(b) if b.too_often(now) => UnshownAction::GiveUp,
        Some(b) if now < b.next_allowed() => UnshownAction::Wait,
        _ => UnshownAction::MakeAgain,
    }
}

/// Made again within this of the time before, an output waits longer.
const REOPEN_SOON: std::time::Duration = std::time::Duration::from_secs(30);
/// An output made again this many times within [`REMAKES_LATELY`] is not
/// made again for its frames going unshown: a screen that says what it
/// shows only now and then would blink it for ever.
const MOST_REMAKES: u32 = 3;
/// See [`MOST_REMAKES`].
const REMAKES_LATELY: std::time::Duration = std::time::Duration::from_secs(300);
/// The longest an output waits before being made again.
const REOPEN_WAIT_MOST: std::time::Duration = std::time::Duration::from_secs(60);

#[cfg(test)]
mod tests {
    use super::default_output_window_size;

    /// An output whose frames go unshown is made again — the Stage Manager
    /// case: its screen took them and showed none, till the window was made
    /// again — then waits before the next time. Made again once and still
    /// showing not one frame, its screen never says what it shows (a
    /// `DisplayLink` screen): it is never made again for that, as each time
    /// only blinks it.
    #[test]
    fn an_output_whose_screen_never_says_what_it_shows_is_made_again_once() {
        use super::{unshown_action, Reopened, UnshownAction};
        let secs = std::time::Duration::from_secs;
        let now = std::time::Instant::now();
        assert_eq!(unshown_action(None, false, now), UnshownAction::MakeAgain, "the first time");
        assert_eq!(unshown_action(None, true, now), UnshownAction::MakeAgain);
        let once = Reopened::after(None, now);
        let soon = now + std::time::Duration::from_millis(500);
        assert_eq!(unshown_action(Some(once), true, soon), UnshownAction::Wait, "it showed: wait");
        let later = now + secs(5);
        assert_eq!(unshown_action(Some(once), true, later), UnshownAction::MakeAgain);
        assert_eq!(unshown_action(Some(once), false, soon), UnshownAction::GiveUp, "none shown");
        assert_eq!(unshown_action(Some(once), false, later), UnshownAction::GiveUp);
        // A remake long ago, or before the output moved, tells nothing of
        // this window: made again (once more).
        let long_ago = now + secs(3600);
        assert_eq!(unshown_action(Some(once), false, long_ago), UnshownAction::MakeAgain);
    }

    /// A screen that says what it shows only now and then — a second's
    /// frames said shown, then none, each time — is made again three times
    /// in five minutes at most, then left alone: not blinked for ever.
    #[test]
    fn an_output_made_again_too_often_lately_is_left_alone() {
        use super::{unshown_action, Reopened, UnshownAction};
        let secs = std::time::Duration::from_secs;
        let start = std::time::Instant::now();
        let mut record = None;
        let mut made = 0;
        for minute in 0..30_u64 {
            let now = start + secs(minute * 20);
            match unshown_action(record, true, now) {
                UnshownAction::MakeAgain => {
                    record = Some(Reopened::after(record, now));
                    made += 1;
                }
                UnshownAction::Wait => {}
                UnshownAction::GiveUp => break,
            }
        }
        assert_eq!(made, 3, "three times, then left alone");
        // Spread over more than five minutes, each one cured it.
        let mut record = None;
        for hour in 0..5_u64 {
            let now = start + secs(hour * 3600);
            assert_eq!(unshown_action(record, true, now), UnshownAction::MakeAgain);
            record = Some(Reopened::after(record, now));
        }
    }

    /// An output that lost its screen goes back to that screen only, not to
    /// another that took its name; one waiting since the show opened goes by
    /// name.
    #[test]
    fn an_output_goes_back_to_the_screen_it_lost() {
        use super::awaited_index;
        let monitors = [("Monitor #0".to_string(), 7), ("Monitor #1".to_string(), 9)];
        assert_eq!(awaited_index(&monitors, Some(&9), || Some(0)), Some(1), "its own");
        assert_eq!(awaited_index(&monitors, Some(&5), || Some(0)), None, "not another");
        assert_eq!(awaited_index(&monitors, None, || Some(0)), Some(0), "by name");
    }

    /// A screen an output covers is gone once it has been missing a second:
    /// missing a moment — the screens listed afresh as they change — it is
    /// not; back, it is there again.
    #[test]
    fn a_screen_is_gone_once_missing_a_second() {
        use super::{screen_gone, ScreenGone, SCREEN_GONE_AFTER};
        let start = std::time::Instant::now();
        assert_eq!(screen_gone(true, None, start), ScreenGone::Here);
        assert_eq!(screen_gone(false, None, start), ScreenGone::Missing(start));
        let soon = start + SCREEN_GONE_AFTER / 2;
        assert_eq!(screen_gone(false, Some(start), soon), ScreenGone::Missing(start));
        assert_eq!(screen_gone(true, Some(start), soon), ScreenGone::Here, "back");
        assert_eq!(
            screen_gone(false, Some(start), start + SCREEN_GONE_AFTER),
            ScreenGone::Gone
        );
    }

    /// An output made again soon after the time before waits twice as long
    /// each time, a minute at most; one that stayed good half a minute starts
    /// over.
    #[test]
    fn an_output_made_again_and_again_waits_longer_each_time() {
        use super::Reopened;
        let start = std::time::Instant::now();
        let at = |secs: u64| start + std::time::Duration::from_secs(secs);
        let first = Reopened::after(None, at(0));
        assert_eq!(first.next_allowed(), at(1));
        let second = Reopened::after(Some(first), at(5));
        assert_eq!(second.next_allowed(), at(7));
        let third = Reopened::after(Some(second), at(10));
        assert_eq!(third.next_allowed(), at(14));
        let (mut last, mut secs) = (third, 10);
        for _ in 0..10 {
            secs += 20;
            last = Reopened::after(Some(last), at(secs));
        }
        assert_eq!(last.next_allowed(), at(secs + 60), "a minute at most");
        let later = Reopened::after(Some(last), at(secs + 31));
        assert_eq!(later.next_allowed(), at(secs + 32), "starts over");
    }

    /// Aspect the window opens at, to compare against the master's.
    fn aspect((w, h): (f64, f64)) -> f64 {
        w / h
    }

    #[test]
    fn sixteen_by_nine_master_fills_the_default_box() {
        assert_eq!(default_output_window_size(1920, 1080), (1280.0, 720.0));
        assert_eq!(default_output_window_size(3840, 2160), (1280.0, 720.0));
    }

    #[test]
    fn vertical_master_opens_vertical() {
        let size = default_output_window_size(1080, 1920);
        assert!(
            (aspect(size) - 1080.0 / 1920.0).abs() < 1e-9,
            "expected the master's aspect, got {size:?}"
        );
        // Height is the binding dimension for anything taller than the box.
        assert!((size.1 - 720.0).abs() < 1e-9, "got {size:?}");
        assert!(
            size.0 < size.1,
            "a vertical stage must not open wider than tall"
        );
    }

    #[test]
    fn square_master_opens_square() {
        assert_eq!(default_output_window_size(1080, 1080), (720.0, 720.0));
    }

    #[test]
    fn ultrawide_master_is_bounded_by_width() {
        let size = default_output_window_size(5120, 1440);
        assert!((size.0 - 1280.0).abs() < 1e-9, "got {size:?}");
        assert!(
            (aspect(size) - 5120.0 / 1440.0).abs() < 1e-9,
            "got {size:?}"
        );
    }

    #[test]
    fn zero_resolution_falls_back_to_the_box() {
        // set_render_resolution rejects zero, so this only guards against a
        // divide by zero if that ever changes.
        assert_eq!(default_output_window_size(0, 0), (1280.0, 720.0));
        assert_eq!(default_output_window_size(1920, 0), (1280.0, 720.0));
    }
}
