//! Output management state mutations.

use super::super::VardaApp;
use crate::engine::{CommandResult, ErrorCode};
use crate::renderer::context::{
    AudioPassthrough, AwaitedDisplay, CalibrationMode, HeadlessOutput, OutputSource, OutputTarget,
    UnifiedOutput,
};
use crate::renderer::edge_blend::EdgeBlendMode;

/// How often Blackmagic outputs waiting for their device look for it.
const LOOK_FOR_DEVICE: std::time::Duration = std::time::Duration::from_secs(1);

/// How long a Blackmagic output saved running waits for its device, as the
/// stage opens, before it is said missing: discovery tells of a connected
/// device within a second or so.
const DEVICE_MISSING_AFTER: std::time::Duration = std::time::Duration::from_secs(5);

impl VardaApp {
    /// Configure the next take. Renaming never stops an active encoder.
    pub fn cmd_configure_recording_output(
        &mut self,
        output_uuid: &str,
        name: Option<&str>,
        folder: Option<&str>,
    ) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(error) => return error.into(),
        };
        let Some(UnifiedOutput::Headless(h)) = self.output.outputs.get_mut(idx) else {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "This output is not a recording".into(),
            };
        };
        let OutputTarget::Recording { path, codec, .. } = &mut h.target else {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "This output is not a recording".into(),
            };
        };
        if h.active && folder.is_some() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "Stop recording to change its folder".into(),
            };
        }
        if folder.is_some_and(|folder| folder.trim().is_empty()) {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "Choose a recording folder".into(),
            };
        }
        if let Some(name) = name {
            h.name = crate::record::recording_name(name, "Recording");
        }
        let folder = folder.map_or_else(
            || crate::record::output_recording_folder(path),
            std::path::Path::new,
        );
        *path = crate::record::output_recording_base(folder, &h.name, codec)
            .display()
            .to_string();
        CommandResult::Ok
    }

    /// Set the output target for a windowed or headless output.
    pub fn cmd_set_output_target(&mut self, idx: usize, target: OutputTarget) -> CommandResult {
        let ndi_resolution = matches!(&target, OutputTarget::NdiSend { .. }).then(|| {
            let request = self
                .output
                .outputs
                .get(idx)
                .map(UnifiedOutput::presentation_request)
                .unwrap_or_default();
            self.external_io.ndi_manager.resolve_presentation(request)
        });
        {
            let Some(output) = self.output.outputs.get_mut(idx) else {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: "Output not found".into(),
                };
            };
            match output {
                UnifiedOutput::Window(w) => {
                    if target.is_windowed() {
                        match &target {
                            // Addressed by name, as restore and adoption are: the
                            // index a client sends is stale once a monitor is
                            // plugged or unplugged, and would put the output on
                            // whichever screen now has that position.
                            OutputTarget::Display { name, .. } => {
                                if let Some((monitor_index, handle)) =
                                    crate::app::monitors::find(&self.output.cached_monitors, name)
                                {
                                    w.set_target(
                                        OutputTarget::Display {
                                            name: name.clone(),
                                            monitor_index,
                                        },
                                        Some(handle.clone()),
                                    );
                                } else {
                                    w.set_target(OutputTarget::Windowed, None);
                                    w.awaiting_display = Some(AwaitedDisplay {
                                        monitor: name.clone(),
                                        screen: None,
                                        was_missing: true,
                                    });
                                    self.session.notifications.warn(format!(
                                        "Monitor '{name}' not connected — output '{}' \
                                         moves there when it connects",
                                        w.name
                                    ));
                                }
                            }
                            _ => w.set_target(target, None),
                        }
                        if let Err(error) =
                            w.set_presentation_request(&self.context, w.presentation_request)
                        {
                            return CommandResult::Err {
                                code: ErrorCode::InternalError,
                                message: format!(
                                    "Failed to reconfigure output presentation: {error}"
                                ),
                            };
                        }
                    }
                }
                UnifiedOutput::Headless(h) => {
                    // A running Blackmagic output's levels: taken up live,
                    // the output running on.
                    let levels_only = match (&h.target, &target) {
                        (
                            OutputTarget::DeckLink { device, mode, .. },
                            OutputTarget::DeckLink {
                                device: to_device,
                                mode: to_mode,
                                ..
                            },
                        ) => device == to_device && mode == to_mode,
                        _ => false,
                    };
                    if levels_only && h.active {
                        if let (Some(sender), OutputTarget::DeckLink { full_range, .. }) =
                            (&h.decklink, &target)
                        {
                            sender.set_full_range(*full_range);
                        }
                        log::info!("Output '{}' now {target}", h.name);
                        h.target = target;
                    } else if target.is_headless() {
                        if h.active {
                            log::info!("Output '{}' stopped: its target changed to {target}", h.name);
                            if let Some(mut sub) = h.subprocess.take() {
                                sub.stop();
                            }
                            // A Blackmagic device is let go as its sender drops;
                            // stopped by the change, as by hand.
                            h.decklink = None;
                            h.keep_running = false;
                            let passthrough = h.audio_pcm.take();
                            h.active = false;
                            h.started_at = None;
                            if let Some(pass) = passthrough {
                                pass.release(&mut self.audio_manager);
                            }
                        }
                        h.target = target;
                        h.set_presentation_request(&self.context.device, h.presentation_request);
                        if let Some(resolved) = ndi_resolution {
                            h.set_resolved_presentation(&self.context.device, resolved);
                        }
                    }
                }
            }
        }
        self.refresh_presentation_notification(idx);
        CommandResult::Ok
    }

    /// Create a new headless output with the given target.
    pub fn cmd_create_headless_output(&mut self, target: OutputTarget) -> CommandResult {
        let idx = self.output.outputs.len() + 1;
        let name = if matches!(target, OutputTarget::Recording { .. }) {
            format!("Recording {idx}")
        } else {
            format!("Output {idx}")
        };
        let is_ndi = matches!(&target, OutputTarget::NdiSend { .. });
        let mut headless = HeadlessOutput::new(
            &self.context.device,
            name.clone(),
            OutputSource::Master,
            target,
            self.render_width,
            self.render_height,
        );
        // Like vMix, a new recording or stream carries the first audio bus.
        headless.audio_bus = self.audio_mix.default_bus().map(str::to_string);
        if is_ndi {
            let resolved = self
                .external_io
                .ndi_manager
                .resolve_presentation(headless.presentation_request);
            headless.set_resolved_presentation(&self.context.device, resolved);
        }
        log::info!("Created headless output '{name}'");
        self.output.outputs.push(UnifiedOutput::Headless(headless));
        CommandResult::Ok
    }

    /// Resize every headless output to the render resolution, returning the
    /// names of any that had to be stopped to do it.
    ///
    /// Recording, NDI, Syphon and the ffmpeg stream targets all size their
    /// buffers from the render resolution, but they did so only once, when they
    /// were created. Changing the project resolution afterwards left them at the
    /// old size and the composite was stretch-blitted into it, so a recording
    /// made after switching a project to portrait still wrote a landscape file
    /// with squashed content.
    ///
    /// Anything backed by an ffmpeg subprocess has to stop: the encoder was
    /// spawned with a fixed `-s WxH` and feeding it raw frames of another size
    /// desyncs the stream rather than failing cleanly. Restarting it here would
    /// be worse than stopping — a recording would reopen the same path and
    /// truncate the take already on disk. NDI and Syphon carry their dimensions
    /// per frame and keep running across the change.
    pub(in crate::app) fn resize_headless_outputs(
        &mut self,
        width: u32,
        height: u32,
    ) -> Vec<String> {
        let device = &self.context.device;
        let mut stopped = Vec::new();
        let mut released = Vec::new();
        for output in &mut self.output.outputs {
            let UnifiedOutput::Headless(h) = output else {
                continue;
            };
            if h.width == width && h.height == height {
                continue;
            }
            // A Blackmagic output keeps its format's size, the show scaled
            // to it.
            if h.decklink.is_some() {
                continue;
            }
            if let Some(mut sub) = h.subprocess.take() {
                sub.stop();
                h.active = false;
                h.started_at = None;
                released.extend(h.audio_pcm.take());
                stopped.push(h.name.clone());
            }
            h.resize(device, width, height);
        }
        for pass in released {
            self.release_passthrough(Some(*pass));
        }
        stopped
    }

    /// Start a headless output (spawn ffmpeg subprocess or activate NDI/Syphon).
    pub fn cmd_start_output(&mut self, output_uuid: &str) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        // Snapshot what we need so no borrow of `self.output` is held across the
        // audio-subscription and spawn work (which borrow other `self` fields).
        // Also take any stale subscription left by a prior delivery failure.
        //
        // The encoder's frame size is fixed for the life of the subprocess, so
        // pin the output to the render resolution first. `set_render_resolution`
        // already keeps outputs in step; this is belt and braces for the paths
        // that assign `render_width`/`render_height` directly, such as the
        // workspace loader applying a scene's resolution.
        let (render_width, render_height) = (self.render_width, self.render_height);
        let device = &self.context.device;
        let (
            target,
            name,
            width,
            height,
            presentation_request,
            mut readback_format,
            stale,
            audio_bus,
        ) = match self.output.outputs.get_mut(idx) {
            Some(UnifiedOutput::Headless(h)) => {
                if h.active {
                    return CommandResult::Ok; // already active
                }
                h.resize(device, render_width, render_height);
                (
                    h.target.clone(),
                    h.name.clone(),
                    h.width,
                    h.height,
                    h.presentation_request,
                    h.readback.format(),
                    h.audio_pcm.take(),
                    h.audio_bus.clone(),
                )
            }
            _ => {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: "Output not found or not headless".into(),
                }
            }
        };
        if let OutputTarget::Recording { codec, .. } = &target {
            let resolved = match crate::renderer::FfmpegSubprocess::probe_recording_presentation(
                codec,
                presentation_request,
                device.features().contains(
                    wgpu::Features::TEXTURE_FORMAT_16BIT_NORM
                        | wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES,
                ),
            ) {
                Ok(resolved) => resolved,
                Err(error) => {
                    self.release_passthrough(stale.map(|subscription| *subscription));
                    return CommandResult::Err {
                        code: ErrorCode::Unavailable,
                        message: error.to_string(),
                    };
                }
            };
            if let Some(UnifiedOutput::Headless(output)) = self.output.outputs.get_mut(idx) {
                output.set_resolved_presentation(device, resolved);
                let nv12 = matches!(
                    codec,
                    crate::renderer::context::RecordingCodec::H264
                        | crate::renderer::context::RecordingCodec::H265
                ) && output.resolved_presentation.resolved
                    == crate::engine::value::render::PresentationDepth::Sdr8;
                // A new take must not receive readbacks left by the last take.
                output.keep_nv12(device, false);
                output.keep_nv12(device, nv12);
                readback_format = if output.nv12.is_some() {
                    crate::renderer::ReadbackFormat::Nv12
                } else {
                    output.readback.format()
                };
            }
        }
        self.release_passthrough(stale.map(|b| *b));

        // Resolve optional audio passthrough (emits a notification + falls back
        // to video-only if the device is missing — Decision 6).
        let (audio_input, passthrough) = resolve_output_audio(
            &mut self.audio_manager,
            &self.audio_mix,
            &mut self.session.notifications,
            OutputAudio {
                bus: audio_bus.as_deref(),
                device: target.audio_device(),
            },
            &name,
        );

        let fps = encoder_fps(self.target_fps);

        let spawn_result = match &target {
            OutputTarget::SrtStream { url, codec, .. } => {
                crate::renderer::FfmpegSubprocess::spawn_srt(
                    url,
                    codec,
                    presentation_request,
                    width,
                    height,
                    fps,
                    audio_input,
                )
            }
            OutputTarget::Recording { path, codec, .. } => {
                let path = crate::record::output_recording_base(
                    crate::record::output_recording_folder(path),
                    &name,
                    codec,
                );
                crate::renderer::FfmpegSubprocess::spawn_recording_with_presentation(
                    &path.display().to_string(),
                    codec,
                    width,
                    height,
                    fps,
                    audio_input,
                    presentation_request,
                    readback_format,
                )
            }
            OutputTarget::HlsStream {
                name: target_name,
                codec,
                low_latency,
                ..
            } => crate::renderer::FfmpegSubprocess::spawn_hls(
                target_name,
                codec,
                presentation_request,
                width,
                height,
                fps,
                *low_latency,
                audio_input,
            ),
            OutputTarget::DashStream {
                name: target_name,
                codec,
                ..
            } => crate::renderer::FfmpegSubprocess::spawn_dash(
                target_name,
                codec,
                presentation_request,
                width,
                height,
                fps,
                audio_input,
            ),
            OutputTarget::RtmpStream {
                url,
                codec,
                codec_contract,
                ..
            } => crate::renderer::FfmpegSubprocess::spawn_rtmp(
                url,
                codec,
                *codec_contract,
                presentation_request,
                width,
                height,
                fps,
                audio_input,
            ),
            OutputTarget::NdiSend { .. } => {
                // No ffmpeg subprocess. The sender carries its audio bus
                // (see `NdiManager::keep_audio`), not device passthrough.
                self.release_passthrough(passthrough);
                if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.get_mut(idx) {
                    h.active = true;
                    h.started_at = Some(std::time::Instant::now());
                }
                return CommandResult::Ok;
            }
            OutputTarget::SyphonServer { .. } => {
                // No ffmpeg subprocess; Syphon doesn't carry passthrough audio.
                self.release_passthrough(passthrough);
                #[cfg(target_os = "macos")]
                {
                    if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.get_mut(idx) {
                        h.active = true;
                        h.started_at = Some(std::time::Instant::now());
                    }
                    return CommandResult::Ok;
                }
                // Parity with the Syphon receive path (cmd_add_syphon_deck): reject
                // explicitly on non-macOS so an API client gets clear feedback rather
                // than a silently inert output.
                #[cfg(not(target_os = "macos"))]
                {
                    return CommandResult::Err {
                        code: ErrorCode::Unavailable,
                        message: "Syphon is only available on macOS".into(),
                    };
                }
            }
            OutputTarget::DeckLink {
                device,
                mode,
                full_range,
            } => {
                // No ffmpeg subprocess and, for now, no sound: the device's
                // own thread puts each picture up (see `crate::decklink`), at
                // its format's size, the show scaled to it.
                self.release_passthrough(passthrough);
                return match crate::decklink::DeckLinkSender::start(&name, device, mode, *full_range) {
                    Ok(sender) => {
                        let device = &self.context.device;
                        if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.get_mut(idx) {
                            h.resize(device, sender.width, sender.height);
                            h.decklink = Some(Box::new(sender));
                            h.keep_running = true;
                            h.waiting_since = None;
                            h.active = true;
                            h.started_at = Some(std::time::Instant::now());
                        }
                        log::info!("Blackmagic output '{name}' on {target}");
                        CommandResult::Ok
                    }
                    Err(e) => CommandResult::Err {
                        code: ErrorCode::Unavailable,
                        message: format!("Blackmagic output '{name}': {e}"),
                    },
                };
            }
            _ => {
                self.release_passthrough(passthrough);
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: "Cannot start windowed target".into(),
                };
            }
        };

        match spawn_result {
            Ok(sub) => {
                let presentation = sub.presentation().cloned();
                if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.get_mut(idx) {
                    if let Some(resolved) = presentation {
                        h.set_resolved_presentation(&self.context.device, resolved);
                    }
                    h.keep_nv12(&self.context.device, sub.takes_nv12());
                    if matches!(target, OutputTarget::Recording { .. }) {
                        h.recording_file = Some(sub.label().into());
                    }
                    h.subprocess = Some(Box::new(sub));
                    h.audio_pcm = passthrough.map(Box::new);
                    h.active = true;
                }
                self.refresh_presentation_notification(idx);
                CommandResult::Ok
            }
            Err(e) => {
                // Spawn failed — release the PCM subscription we reserved.
                self.release_passthrough(passthrough);
                CommandResult::Err {
                    code: ErrorCode::InternalError,
                    message: e.to_string(),
                }
            }
        }
    }

    /// Once a frame, at most every [`LOOK_FOR_DEVICE`]: look at the Blackmagic
    /// devices connected, and start each Blackmagic output kept running but
    /// stopped — its device unplugged, or missing as the stage opened — once
    /// its device appears again, as an output's window goes to its screen
    /// when the screen is plugged in; and let each running one tell the log
    /// how it fares, when due.
    pub(crate) fn poll_blackmagic_outputs(&mut self) {
        let any = self.output.outputs.iter().any(|o| {
            matches!(o, UnifiedOutput::Headless(h)
                if matches!(h.target, OutputTarget::DeckLink { .. }))
        });
        if !any {
            return;
        }
        let now = std::time::Instant::now();
        if self
            .output
            .blackmagic_looked
            .is_some_and(|at| now.duration_since(at) < LOOK_FOR_DEVICE)
        {
            return;
        }
        self.output.blackmagic_looked = Some(now);
        self.start_outputs_whose_device_is_back(&crate::decklink::known_devices());
        self.tell_outputs_still_waiting(now);
        for output in &self.output.outputs {
            if let UnifiedOutput::Headless(h) = output {
                if let Some(sender) = &h.decklink {
                    sender.tell_if_due();
                }
            }
        }
    }

    /// Say, once, which Blackmagic outputs saved running still have no
    /// device [`DEVICE_MISSING_AFTER`] after the stage opened, as of `now`.
    pub(crate) fn tell_outputs_still_waiting(&mut self, now: std::time::Instant) {
        let mut told = Vec::new();
        for output in &mut self.output.outputs {
            let UnifiedOutput::Headless(h) = output else {
                continue;
            };
            let Some(since) = h.waiting_since else {
                continue;
            };
            if h.active {
                h.waiting_since = None;
            } else if now.saturating_duration_since(since) >= DEVICE_MISSING_AFTER {
                h.waiting_since = None;
                if let OutputTarget::DeckLink { device, .. } = &h.target {
                    told.push(format!(
                        "Blackmagic output '{}': '{device}' is not connected — it starts once it is",
                        h.name
                    ));
                }
            }
        }
        for message in told {
            log::warn!("{message}");
            self.session.notifications.warn(message);
        }
    }

    /// The Blackmagic devices connected now being `devices`: start each
    /// output kept running but stopped whose device was not connected at
    /// the last look. Only then — a device there all along that will not
    /// take pictures is not tried again and again; unplug and plug it in.
    pub(crate) fn start_outputs_whose_device_is_back(
        &mut self,
        devices: &[crate::decklink::DeckLinkDevice],
    ) {
        let names: Vec<String> = devices.iter().map(|d| d.name.clone()).collect();
        let before = self.output.blackmagic_seen.replace(names.clone());
        let back = |device: &str| {
            names.iter().any(|n| n == device)
                && !before.as_ref().is_some_and(|b| b.iter().any(|n| n == device))
        };
        let starting: Vec<(String, String)> = self
            .output
            .outputs
            .iter()
            .filter_map(|o| match o {
                UnifiedOutput::Headless(h) if h.keep_running && !h.active => match &h.target {
                    OutputTarget::DeckLink { device, .. } if back(device) => {
                        Some((h.uuid.clone(), h.name.clone()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect();
        for (uuid, name) in starting {
            match self.cmd_start_output(&uuid) {
                CommandResult::Ok
                | CommandResult::OkWithId { .. }
                | CommandResult::OkWithData { .. } => {
                    self.session.notifications.info(format!(
                        "Blackmagic output '{name}': its device is connected — started"
                    ));
                }
                CommandResult::Err { message, .. } => {
                    log::warn!("{message}");
                    self.session.notifications.warn(message);
                }
            }
        }
    }

    /// Give the screen outputs address as `monitor` a name of the operator's
    /// own, kept on this computer — or, with none or a blank one, take its
    /// name away.
    pub fn cmd_rename_screen(&mut self, monitor: &str, name: Option<&str>) -> CommandResult {
        match self.screen_name_file(monitor, name) {
            Ok(file) => {
                if let Some(file) = file {
                    self.write_for_command(file, None);
                }
                CommandResult::Ok
            }
            Err(refused) => refused,
        }
    }

    /// The screen named at once, and the file its name is kept in — written
    /// in the background (see `persistence::writer`) — or why not.
    pub(crate) fn screen_name_file(
        &mut self,
        monitor: &str,
        name: Option<&str>,
    ) -> Result<Option<crate::persistence::writer::FileSave>, CommandResult> {
        let Some(at) = self.output.cached_monitors.iter().position(|(n, _)| n == monitor) else {
            return Err(CommandResult::Err {
                code: ErrorCode::NotFound,
                message: format!("no screen named '{monitor}' is connected"),
            });
        };
        let Some(identity) = self.output.screen_facts.get(at).map(|f| f.identity.clone()) else {
            return Err(CommandResult::Err {
                code: ErrorCode::Unavailable,
                message: format!("'{monitor}' is not known well enough to name yet"),
            });
        };
        log::info!("Screen '{monitor}' named {name:?}");
        Ok(self
            .output
            .screen_names
            .name(&identity, name)
            .map(|(path, content)| crate::persistence::writer::FileSave {
                label: "the screens' names".into(),
                path,
                content,
                said: None,
                failed: Some("The screen's name could not be kept".into()),
                presets: false,
                then: None,
            }))
    }

    /// Put each screen's name big on it for a few seconds.
    pub fn cmd_identify_screens(&mut self) -> CommandResult {
        let keys: Vec<String> = self.output.cached_monitors.iter().map(|(n, _)| n.clone()).collect();
        let sizes: Vec<(u32, u32)> = (0..keys.len())
            .map(|i| {
                let size = self.output.monitor_sizes.get(i).copied().unwrap_or_default();
                (size.width, size.height)
            })
            .collect();
        let labels = crate::app::screens::labels(
            &keys,
            &self.output.screen_facts,
            &self.output.screen_names,
            &sizes,
        );
        let shown: Vec<(u32, String)> = self
            .output
            .screen_facts
            .iter()
            .zip(labels)
            .filter_map(|(fact, label)| fact.display_id.map(|id| (id, label)))
            .collect();
        crate::app::screens::identify(shown);
        CommandResult::Ok
    }

    /// Release a reserved PCM subscription (used when an output fails to start or
    /// is a non-ffmpeg target that can't carry passthrough audio).
    fn release_passthrough(&mut self, passthrough: Option<AudioPassthrough>) {
        if let Some(pass) = passthrough {
            pass.release(&mut self.audio_manager);
        }
    }

    /// Stop a headless output (kill subprocess and deactivate), as asked —
    /// ⏹ Stop, or the API.
    pub fn cmd_stop_output(&mut self, output_uuid: &str) -> CommandResult {
        self.stop_output(output_uuid, "asked (⏹ Stop, or the API)")
    }

    /// Stop a headless output, saying in the log why.
    fn stop_output(&mut self, output_uuid: &str, why: &str) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        if let Some(UnifiedOutput::Headless(h)) = self.output.outputs.get_mut(idx) {
            // Stopped by hand: not to be started again by itself.
            h.keep_running = false;
            if h.active {
                log::info!("Output '{}' stopped: {why}", h.name);
                if let Some(mut sub) = h.subprocess.take() {
                    sub.stop();
                }
                // A Blackmagic device is let go as its sender drops.
                h.decklink = None;
                let passthrough = h.audio_pcm.take();
                h.active = false;
                h.started_at = None;
                // Disjoint field borrow (audio_manager vs. output): release the
                // PCM subscription so the cpal callback stops fanning to it.
                if let Some(pass) = passthrough {
                    pass.release(&mut self.audio_manager);
                }
            }
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found or not headless".into(),
            }
        }
    }

    /// Set the calibration display mode on a windowed output.
    pub fn cmd_set_calibration_mode(
        &mut self,
        output_uuid: &str,
        mode: CalibrationMode,
    ) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        match self.output.outputs.get_mut(idx) {
            Some(UnifiedOutput::Window(w)) => {
                w.calibration_mode = mode;
                CommandResult::Ok
            }
            // A recording, stream or Blackmagic output: the whole test card,
            // or its picture — it has no surfaces of its own to card.
            Some(UnifiedOutput::Headless(h)) if mode != CalibrationMode::Surfaces => {
                h.calibration_mode = mode;
                CommandResult::Ok
            }
            Some(UnifiedOutput::Headless(_)) => CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "an output without a window shows the whole test card \
                          (Projector), or its picture"
                    .into(),
            },
            None => CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            },
        }
    }

    /// Move one corner-pin corner of a surface's warp (per-surface).
    pub fn cmd_set_warp_corner(
        &mut self,
        surface_uuid: &str,
        corner_idx: usize,
        position: [f32; 2],
    ) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_warp_corner(corner_idx, position);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Clear a surface's warp (back to no-warp / native position).
    pub fn cmd_reset_warp(&mut self, surface_uuid: &str) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.reset_warp();
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Set the warp grid resolution for a surface, converting its warp into a
    /// `cols` × `rows` mesh while preserving the current deformation. Dimensions
    /// are clamped to `[2, MAX_WARP_SUBDIVISIONS]` in the domain method.
    pub fn cmd_set_warp_subdivisions(
        &mut self,
        surface_uuid: &str,
        cols: u32,
        rows: u32,
    ) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_warp_subdivisions(cols, rows);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Move a single mesh grid point (row-major) of a surface's mesh warp.
    /// No-op on the geometry if the surface's warp is not a mesh; still returns
    /// `Ok` so callers can treat it uniformly.
    pub fn cmd_set_warp_mesh_point(
        &mut self,
        surface_uuid: &str,
        row: usize,
        col: usize,
        position: [f32; 2],
    ) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_warp_mesh_point(row, col, position);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Bind/unbind a surface's warp from its shape (auto-warp). Unbinding
    /// materialises the conforming warp for manual editing.
    pub fn cmd_set_warp_bound(&mut self, surface_uuid: &str, bound: bool) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_warp_bound(bound);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Convert a surface's warp into a smooth bezier patch grid (8i.6).
    pub fn cmd_convert_warp_to_bezier(&mut self, surface_uuid: &str) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.convert_warp_to_bezier();
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Move a bezier-warp control anchor. No-op on the geometry if the warp is
    /// not bezier; still returns `Ok` so callers can treat it uniformly.
    pub fn cmd_move_warp_anchor(
        &mut self,
        surface_uuid: &str,
        row: usize,
        col: usize,
        position: [f32; 2],
    ) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_warp_bezier_anchor(row, col, position);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Move a bezier-warp tangent handle. No-op on the geometry if the warp is
    /// not bezier; still returns `Ok`.
    pub fn cmd_move_warp_handle(
        &mut self,
        surface_uuid: &str,
        horizontal: bool,
        row: usize,
        col: usize,
        which: usize,
        position: [f32; 2],
    ) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_warp_bezier_handle(horizontal, row, col, which, position);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Set the bezier-warp control-cage resolution. No-op on the geometry if the
    /// warp is not bezier; still returns `Ok`.
    pub fn cmd_set_bezier_cage_subdivisions(
        &mut self,
        surface_uuid: &str,
        cols: u32,
        rows: u32,
    ) -> CommandResult {
        if let Some((_, surface)) = self.output.surface_manager.find_by_uuid_mut(surface_uuid) {
            surface.set_bezier_cage_subdivisions(cols, rows);
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Surface not found".into(),
            }
        }
    }

    /// Set edge blend configuration for an output.
    pub fn cmd_set_edge_blend(
        &mut self,
        output_uuid: &str,
        config: crate::renderer::edge_blend::EdgeBlendConfig,
    ) -> CommandResult {
        let output_idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        if let Some(output) = self.output.outputs.get_mut(output_idx) {
            match output {
                UnifiedOutput::Window(w) => {
                    w.edge_blend = config;
                }
                UnifiedOutput::Headless(h) => {
                    h.edge_blend = config;
                }
            }
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            }
        }
    }

    /// Set edge blend mode for an output; triggers auto-recompute if mode is Auto.
    pub fn cmd_set_edge_blend_mode(
        &mut self,
        output_uuid: &str,
        mode: EdgeBlendMode,
    ) -> CommandResult {
        let output_idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        if let Some(output) = self.output.outputs.get_mut(output_idx) {
            match output {
                UnifiedOutput::Window(w) => {
                    w.edge_blend_mode = mode;
                }
                UnifiedOutput::Headless(h) => {
                    h.edge_blend_mode = mode;
                }
            }
            if mode == EdgeBlendMode::Auto {
                self.recompute_auto_edge_blend();
            }
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            }
        }
    }

    /// Set output rotation and rebuild intermediate textures.
    pub fn cmd_set_output_rotation(
        &mut self,
        output_uuid: &str,
        rotation: crate::renderer::context::OutputRotation,
    ) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        if let Some(output) = self.output.outputs.get_mut(idx) {
            match output {
                UnifiedOutput::Window(w) => {
                    w.set_rotation(&self.context.device, rotation);
                }
                UnifiedOutput::Headless(h) => {
                    h.set_rotation(rotation);
                }
            }
            CommandResult::Ok
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            }
        }
    }

    /// Route a headless output from the master or from one bus's program.
    ///
    /// This is vMix's per-output source: the stream can follow a bus that
    /// carries the lower third while the recording follows one that does not.
    /// Only `Master` and `BusProgram` are accepted — both are named without
    /// indices, so a saved route cannot drift onto another channel — and the
    /// bus must exist now. A window is refused: its picture comes from its
    /// surfaces, and a bus reaches a screen through the switcher.
    pub fn cmd_set_output_source(
        &mut self,
        output_uuid: &str,
        source: OutputSource,
    ) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        match &source {
            OutputSource::Master => {}
            OutputSource::BusProgram(bus) if self.production_buses.contains(bus) => {}
            OutputSource::BusProgram(bus) => {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("no bus with uuid '{bus}'"),
                };
            }
            other => {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: format!(
                        "an output can send Master or a bus's program, not {other}; \
                         put that source on a bus and route the bus"
                    ),
                };
            }
        }
        match self.output.outputs.get_mut(idx) {
            Some(UnifiedOutput::Headless(h)) => {
                h.source = source;
                CommandResult::Ok
            }
            Some(UnifiedOutput::Window(_)) => CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "a window shows its surfaces; send a bus to a screen with the \
                          switcher's Screen picker"
                    .into(),
            },
            None => CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            },
        }
    }

    /// Choose the audio-mixer bus a recording, stream or NDI sender carries,
    /// or none. An NDI sender changes over at once; a recording or stream the
    /// next time it starts, since the encoder's audio input is fixed while it
    /// runs.
    pub fn cmd_set_output_audio_bus(
        &mut self,
        output_uuid: &str,
        bus: Option<String>,
    ) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        if let Some(bus) = &bus {
            if !self.audio_mix.has_bus(bus) {
                return CommandResult::Err {
                    code: ErrorCode::NotFound,
                    message: format!("no audio bus with uuid '{bus}'"),
                };
            }
        }
        match self.output.outputs.get_mut(idx) {
            Some(UnifiedOutput::Headless(h)) => {
                h.audio_bus = bus;
                CommandResult::Ok
            }
            Some(UnifiedOutput::Window(_)) => CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "a window carries no sound; play an audio bus on a sound device".into(),
            },
            None => CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            },
        }
    }

    /// Set requested SDR precision and dithering, preserving any fallback status.
    pub fn cmd_set_output_presentation(
        &mut self,
        output_uuid: &str,
        request: crate::engine::value::render::PresentationRequest,
    ) -> CommandResult {
        let idx = match self.resolve_output(output_uuid) {
            Ok(idx) => idx,
            Err(e) => return e.into(),
        };
        let restart = self.output.outputs.get(idx).is_some_and(
            |output| matches!(output, UnifiedOutput::Headless(headless) if headless.active),
        );
        if restart {
            let stopped = self.stop_output(output_uuid, "to start again with a new precision");
            if !matches!(stopped, CommandResult::Ok) {
                return stopped;
            }
        }
        let ndi_resolution = self
            .output
            .outputs
            .get(idx)
            .is_some_and(|output| matches!(output.target(), OutputTarget::NdiSend { .. }))
            .then(|| self.external_io.ndi_manager.resolve_presentation(request));
        if let Some(output) = self.output.outputs.get_mut(idx) {
            if let Err(error) = output.set_presentation_request(&self.context, request) {
                return CommandResult::Err {
                    code: ErrorCode::InternalError,
                    message: format!("Failed to configure output presentation: {error}"),
                };
            }
            if let (UnifiedOutput::Headless(headless), Some(resolved)) = (output, ndi_resolution) {
                headless.set_resolved_presentation(&self.context.device, resolved);
            }
            self.refresh_presentation_notification(idx);
            if restart {
                self.cmd_start_output(output_uuid)
            } else {
                CommandResult::Ok
            }
        } else {
            CommandResult::Err {
                code: ErrorCode::NotFound,
                message: "Output not found".into(),
            }
        }
    }

    /// Synchronize the one-shot fallback warning with one output's resolution.
    pub(crate) fn refresh_presentation_notification(&mut self, idx: usize) {
        let Some((uuid, name, resolved)) = self.output.outputs.get(idx).map(|output| {
            (
                output.uuid().to_string(),
                output.name().to_string(),
                output.resolved_presentation().clone(),
            )
        }) else {
            return;
        };
        let prefix = format!("presentation_fallback:{uuid}:");
        if let Some(reason) = resolved.fallback_reason {
            self.session.notifications.notify_once(
                format!("{prefix}{reason}"),
                crate::notifications::NotificationLevel::Warning,
                format!(
                    "Output '{name}' requested {} but is delivering {} ({}): {reason}",
                    resolved.requested.label(),
                    resolved.resolved.label(),
                    resolved.pixel_format,
                ),
            );
        } else if self.session.notifications.clear_once_key_prefix(&prefix) {
            self.session.notifications.info(format!(
                "Output '{name}' presentation recovered and is delivering {} ({})",
                resolved.resolved.label(),
                resolved.pixel_format,
            ));
        }
    }
}

/// The frame rate an ffmpeg output is opened at.
///
/// Must be the rate frames are actually produced at. A raw video input is timed
/// by position — frame N sits at N/fps — so if the encoder is told a rate the
/// renderer is not running at, the recording comes out at the wrong speed and
/// drifts against its own audio, which runs on the capture device's clock.
///
/// Every output used to be opened at a hardcoded 30 while the app defaults to
/// 60, so a stock recording was labelled at half the rate it was made: twice as
/// long as the session, in slow motion, with the audio running out halfway. It
/// also disabled the gap padding in `FfmpegSubprocess`, which measures the
/// renderer's shortfall against this same rate and saw a surplus instead.
///
/// Uncapped (`target_fps == 0`) has no rate to report, so it takes 60 — the
/// default cap, and the closest thing to an expected rate. A wildly different
/// actual rate will be corrected by frame padding rather than by mislabelling.
pub(crate) fn encoder_fps(target_fps: u32) -> u32 {
    const UNCAPPED_ASSUMED_FPS: u32 = 60;
    if target_fps == 0 {
        UNCAPPED_ASSUMED_FPS
    } else {
        target_fps
    }
}

/// Where a recording or stream's sound comes from: an audio-mixer bus, else
/// the target's own capture device, else nothing.
#[derive(Clone, Copy)]
pub(crate) struct OutputAudio<'a> {
    pub bus: Option<&'a str>,
    pub device: Option<&'a str>,
}

/// Resolve a persisted audio device name to a live PCM subscription for output
/// passthrough. Returns `(AudioInput for ffmpeg, AudioPassthrough to retain for
/// teardown)`. On a missing/unopenable device, emits a warning and returns
/// `(None, None)` → video-only (Decision 6). Shared by `cmd_start_output` and
/// the SRT auto-restart path in the render loop, which both need a fresh tap
/// off disjoint field borrows rather than `&mut self`.
pub(crate) fn resolve_output_audio(
    audio_manager: &mut crate::audio::AudioManager,
    audio_mix: &crate::audio_mix::AudioEngine,
    notifications: &mut crate::notifications::NotificationSystem,
    audio: OutputAudio<'_>,
    output_name: &str,
) -> (
    Option<crate::renderer::AudioInput>,
    Option<AudioPassthrough>,
) {
    if let Some(bus) = audio.bus {
        let label = format!("Bus: {}", audio_mix.bus_name(bus).unwrap_or("?"));
        let Some(tap) = audio_mix.tap(bus) else {
            notifications.warn(format!(
                "Audio bus for output '{output_name}' no longer exists; recording/streaming video-only"
            ));
            return (None, None);
        };
        return (
            Some(crate::renderer::AudioInput {
                rx: tap.receiver,
                sample_rate: crate::audio_mix::SAMPLE_RATE,
                channels: 2,
                lost_samples: tap.lost_samples,
                started: tap.opened,
            }),
            Some(AudioPassthrough {
                source_id: None,
                token: tap.token,
                dropped: tap.dropped,
                label,
            }),
        );
    }
    let Some(device_name) = audio.device else {
        return (None, None);
    };
    let source_id = audio_manager
        .devices()
        .iter()
        .find(|d| d.name == device_name)
        .map(|d| d.id);
    let Some(source_id) = source_id else {
        notifications.warn(format!(
            "Audio device '{device_name}' not found for output '{output_name}'; recording/streaming video-only"
        ));
        return (None, None);
    };
    if let Some(sub) = audio_manager.subscribe_pcm(source_id) {
        let input = crate::renderer::AudioInput {
            rx: sub.receiver,
            sample_rate: sub.format.sample_rate,
            channels: sub.format.channels,
            lost_samples: sub.lost_samples,
            started: std::time::Instant::now(),
        };
        let passthrough = AudioPassthrough {
            source_id: Some(source_id),
            token: sub.token,
            dropped: sub.dropped,
            label: device_name.to_string(),
        };
        (Some(input), Some(passthrough))
    } else {
        notifications.warn(format!(
            "Failed to open audio device '{device_name}' for output '{output_name}'; video-only"
        ));
        (None, None)
    }
}

#[cfg(test)]
mod tests {
    use super::encoder_fps;

    /// An ffmpeg output must be opened at the rate frames are actually made.
    ///
    /// Raw video carries no per-frame timing, so the rate declared at spawn is
    /// the only thing that says how long the recording is. Declaring a rate the
    /// renderer is not running at plays the result back at the wrong speed and
    /// slides it against its own audio, which is timed by the capture device's
    /// sample clock and cannot be talked into agreeing.
    ///
    /// Every spawn site used to pass a literal 30 while the app defaults to 60.
    /// See /spec/av-sync.md.
    #[test]
    fn outputs_are_opened_at_the_rate_frames_are_produced() {
        for target in [24, 25, 30, 50, 60, 120, 144] {
            assert_eq!(
                encoder_fps(target),
                target,
                "an output must be encoded at the rate the renderer runs at"
            );
        }
    }

    /// Uncapped has no rate to declare, so it takes the default cap. Frame
    /// padding absorbs the difference if the real rate turns out lower.
    #[test]
    fn an_uncapped_renderer_falls_back_to_a_declarable_rate() {
        assert_eq!(encoder_fps(0), 60);
    }

    fn headless_app() -> Option<super::VardaApp> {
        let gpu = crate::renderer::context::GpuContext::new_headless().ok()?;
        super::VardaApp::new(gpu, &crate::testing::headless_config()).ok()
    }

    #[test]
    fn recording_outputs_start_stop_and_restart_independently() {
        use crate::engine::CommandResult;
        use crate::renderer::context::{OutputTarget, RecordingCodec, UnifiedOutput};
        if !std::process::Command::new("ffmpeg")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
        {
            return;
        }
        let Some(mut app) = headless_app() else {
            return;
        };
        app.set_render_resolution(64, 64);
        let dir = tempfile::tempdir().unwrap();
        let configured = dir.path().join("Shared.mp4");
        let mut ids = Vec::new();
        for _ in 0..2 {
            assert!(matches!(
                app.cmd_create_headless_output(OutputTarget::Recording {
                    path: configured.display().to_string(),
                    codec: RecordingCodec::H264,
                    audio_device: None
                }),
                CommandResult::Ok
            ));
            ids.push(app.output.outputs.last().unwrap().uuid().to_string());
        }
        for id in &ids {
            assert!(matches!(app.cmd_start_output(id), CommandResult::Ok));
        }
        let files: Vec<_> = app
            .output
            .outputs
            .iter()
            .filter_map(|o| match o {
                UnifiedOutput::Headless(h) => {
                    assert!(h.active);
                    assert!(h.nv12.is_some());
                    assert!(h.subprocess.as_ref().unwrap().takes_nv12());
                    h.recording_file.clone()
                }
                UnifiedOutput::Window(_) => None,
            })
            .collect();
        assert_eq!(files.len(), 2);
        assert_ne!(files[0], files[1]);
        assert!(matches!(
            app.cmd_configure_recording_output(&ids[0], Some("Clean feed"), None),
            CommandResult::Ok
        ));
        let renamed = app
            .output
            .outputs
            .iter()
            .find(|o| o.uuid() == ids[0])
            .unwrap();
        assert!(
            matches!(renamed, UnifiedOutput::Headless(h) if h.active && h.name == "Clean feed" && h.recording_file.as_ref() == Some(&files[0]))
        );
        assert!(matches!(
            app.cmd_configure_recording_output(&ids[0], None, Some("new folder")),
            CommandResult::Err { .. }
        ));
        assert!(matches!(app.cmd_stop_output(&ids[0]), CommandResult::Ok));
        let other = app
            .output
            .outputs
            .iter()
            .find(|o| o.uuid() == ids[1])
            .unwrap();
        assert!(matches!(other, UnifiedOutput::Headless(h) if h.active));
        assert!(matches!(app.cmd_start_output(&ids[0]), CommandResult::Ok));
        let restarted = app
            .output
            .outputs
            .iter()
            .find(|o| o.uuid() == ids[0])
            .unwrap();
        assert!(
            matches!(restarted, UnifiedOutput::Headless(h) if h.recording_file.as_ref() != Some(&files[0]))
        );
        assert!(
            matches!(restarted, UnifiedOutput::Headless(h) if h.recording_file.as_ref().unwrap().file_name().unwrap().to_string_lossy().starts_with("Clean feed "))
        );
        for id in &ids {
            app.cmd_stop_output(id);
        }
        assert!(!configured.exists(), "configured base is never overwritten");
    }

    /// A screen for these tests: takes every picture, at once.
    struct Taker;

    impl crate::decklink::Screen for Taker {
        fn show(&mut self, _uyvy: &[u8], _row_bytes: usize) -> Result<(), crate::decklink::Refused> {
            Ok(())
        }
    }

    /// A running Blackmagic output, on a stand-in device.
    fn running_blackmagic(app: &mut super::VardaApp) -> String {
        use crate::renderer::context::{HeadlessOutput, OutputSource, OutputTarget, UnifiedOutput};
        let mut output = HeadlessOutput::new(
            &app.context.device,
            "Box".into(),
            OutputSource::Master,
            OutputTarget::DeckLink {
                device: "Test Box".into(),
                mode: "1080p60".into(),
                full_range: false,
            },
            64,
            36,
        );
        let sender = crate::decklink::DeckLinkSender::start_with("Box", 64, 36, false, None, || {
            Ok(Box::new(Taker) as Box<dyn crate::decklink::Screen>)
        })
        .expect("started");
        output.decklink = Some(Box::new(sender));
        output.active = true;
        output.keep_running = true;
        let uuid = output.uuid.clone();
        app.output.outputs.push(UnifiedOutput::Headless(output));
        uuid
    }

    /// A running Blackmagic output's Levels change at once, the output
    /// running on — its device kept, not let go and opened again; another
    /// format still stops it, as before.
    #[test]
    fn a_running_blackmagic_outputs_levels_change_without_stopping_it() {
        use crate::engine::{CommandResult, EngineCommand};
        use crate::renderer::context::{OutputTarget, UnifiedOutput};
        let Some(mut app) = headless_app() else {
            return;
        };
        let uuid = running_blackmagic(&mut app);
        let set = |app: &mut super::VardaApp, mode: &str, full_range: bool| {
            app.execute_command(EngineCommand::SetOutputTarget {
                output_uuid: uuid.clone(),
                target: OutputTarget::DeckLink {
                    device: "Test Box".into(),
                    mode: mode.into(),
                    full_range,
                },
            })
        };
        let state = |app: &super::VardaApp| match &app.output.outputs[0] {
            UnifiedOutput::Headless(h) => (
                h.active,
                h.decklink.is_some(),
                matches!(h.target, OutputTarget::DeckLink { full_range: true, .. }),
            ),
            UnifiedOutput::Window(_) => panic!("headless"),
        };
        assert!(matches!(set(&mut app, "1080p60", true), CommandResult::Ok));
        assert_eq!(state(&app), (true, true, true), "running on, at full levels");
        assert!(matches!(set(&mut app, "1080p50", true), CommandResult::Ok));
        assert_eq!(state(&app), (false, false, true), "another format stops it");
    }

    /// An output without a window — a Blackmagic output, say — shows the
    /// whole test card on asking, not surfaces' cards.
    #[test]
    fn an_output_without_a_window_shows_the_test_card() {
        use crate::engine::{CommandResult, EngineCommand};
        use crate::renderer::context::CalibrationMode;
        let Some(mut app) = headless_app() else {
            return;
        };
        let uuid = running_blackmagic(&mut app);
        let set = |app: &mut super::VardaApp, mode| {
            app.execute_command(EngineCommand::SetCalibrationMode {
                output_uuid: uuid.clone(),
                mode,
            })
        };
        assert!(matches!(set(&mut app, CalibrationMode::Projector), CommandResult::Ok));
        let shown = app.build_engine_state().outputs.windows[0].calibration_mode;
        assert_eq!(shown, CalibrationMode::Projector);
        assert!(matches!(set(&mut app, CalibrationMode::Surfaces), CommandResult::Err { .. }));
        assert!(matches!(set(&mut app, CalibrationMode::Off), CommandResult::Ok));
    }
}
