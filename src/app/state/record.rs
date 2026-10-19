//! Recording in the engine: its settings, saved with the stage, and while it
//! runs a headless output of its own — kept apart from the outputs list —
//! that renders the source at the recording's size and feeds the encoder.
//! See [`crate::record`].

use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::engine::{CommandResult, ErrorCode};
use crate::record::{RecordOptions, RecordSettings, RecordStatus};
use crate::renderer::context::{HeadlessOutput, OutputTarget, UnifiedOutput};

use super::{encoder_fps, resolve_output_audio, OutputAudio};

/// Wait before continuing in a new file after the encoder stopped by itself.
const ENCODER_RETRY: Duration = Duration::from_secs(1);
/// The stretch over which dropped encoder frames are counted.
const BEHIND_WINDOW: Duration = Duration::from_secs(10);
/// Time for a new encoder to spin up before its dropped frames count.
const SETTLE: Duration = Duration::from_secs(3);

/// Recording's state in the engine.
#[derive(Default)]
pub(crate) struct Recording {
    pub(crate) settings: RecordSettings,
    /// The recording's own output while it runs, as a one-element list the
    /// headless render pass takes like any output.
    pub(crate) output: Vec<UnifiedOutput>,
    started_at: Option<Instant>,
    /// The take's time stamp, shared by every file of it.
    stamp: String,
    /// Frozen for the take, including encoder-recovery files.
    take_name: String,
    /// Files of this take, in order.
    files: Vec<PathBuf>,
    last_file: Option<PathBuf>,
    retry_at: Option<Instant>,
    problem: Option<String>,
    dropped_seen: u64,
    /// The time this file's dropped frames covered as of the last check, and
    /// each recent rise in it.
    dropped_secs_seen: f64,
    drops: std::collections::VecDeque<(Instant, f64)>,
    /// Frames dropped by earlier files of this take.
    dropped_before: u64,
    /// The rate the current file's encoder was started at: frames reach it
    /// at that rate whatever the target is changed to since.
    fps: u32,
    /// The current file's size and the free space, looked up away from the
    /// engine while the take runs.
    disk: Option<crate::record::DiskWatch>,
}

impl super::super::VardaApp {
    pub fn record_settings(&self) -> &RecordSettings {
        &self.recording.settings
    }

    /// Put saved settings in place, as a workspace loads. A recording that
    /// is running is stopped first.
    pub(crate) fn load_record_settings(&mut self, settings: Option<RecordSettings>) {
        if self.record_running() {
            self.cmd_stop_recording();
        }
        let mut settings = settings.unwrap_or_default();
        settings.options = settings.options.normalized();
        self.recording.settings = settings;
    }

    pub fn record_running(&self) -> bool {
        !self.recording.output.is_empty()
    }

    /// How the recording is doing.
    /// Whether the recording's frames reach its encoder as NV12 made on the
    /// GPU (for tests: nothing on screen says).
    #[doc(hidden)]
    pub fn record_encoder_takes_nv12(&self) -> bool {
        matches!(
            self.recording.output.first(),
            Some(UnifiedOutput::Headless(h))
                if h.nv12.is_some()
                    && h.subprocess.as_ref().is_some_and(|s| s.takes_nv12())
        )
    }

    pub fn record_status(&self) -> RecordStatus {
        let last_file = self
            .recording
            .last_file
            .as_ref()
            .map(|p| p.display().to_string());
        if !self.record_running() {
            return RecordStatus::idle(last_file);
        }
        let current = self.recording.files.last();
        let (bytes, free_bytes) = self
            .recording
            .disk
            .as_ref()
            .map_or((0, None), |disk| (disk.bytes(), disk.free()));
        let fps = if self.recording.fps > 0 {
            self.recording.fps
        } else {
            encoder_fps(self.target_fps)
        };
        // Frames dropped covering more than a hundredth of the last stretch,
        // as measured when they were dropped, at whatever rate the show runs.
        let recent: f64 = self
            .recording
            .drops
            .iter()
            .filter(|(at, _)| at.elapsed() <= BEHIND_WINDOW)
            .map(|(_, secs)| secs)
            .sum();
        let behind = recent > BEHIND_WINDOW.as_secs_f64() / 100.0;
        let problem = self.recording.problem.clone().or_else(|| {
            if free_bytes.is_some_and(|b| b < crate::record::LOW_SPACE_BYTES) {
                Some("The disk is nearly full".into())
            } else if behind {
                Some("The encoder cannot keep up — frames are being repeated".into())
            } else {
                None
            }
        });
        RecordStatus {
            recording: true,
            secs: self
                .recording
                .started_at
                .map_or(0.0, |at| at.elapsed().as_secs_f32()),
            file: current.map(|p| p.display().to_string()),
            files: self
                .recording
                .files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            bytes,
            free_bytes,
            problem,
            dropped: self.recording.dropped_before + self.recording.dropped_seen,
            fps,
            last_file,
        }
    }

    /// Source changes apply live; a name change applies to the next take.
    pub fn cmd_set_record_options(&mut self, options: RecordOptions) -> CommandResult {
        let options = options.normalized();
        if self.record_running() {
            let current = &self.recording.settings.options;
            let only_source = RecordOptions {
                source: current.source.clone(),
                name: current.name.clone(),
                ..options.clone()
            } == *current;
            if !only_source {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: "Stop recording to change its settings".into(),
                };
            }
            if let Some(UnifiedOutput::Headless(h)) = self.recording.output.first_mut() {
                h.source = options.source.clone();
            }
        }
        self.recording.settings.options = options;
        CommandResult::Ok
    }

    /// Start recording into a new file named by the date and time.
    pub fn cmd_start_recording(&mut self) -> CommandResult {
        if self.record_running() {
            return CommandResult::Ok;
        }
        let options = self.recording.settings.options.clone();
        let size = options
            .size
            .unwrap_or((self.render_width, self.render_height));
        let mut output = HeadlessOutput::new(
            &self.context.device,
            "Record".into(),
            options.source.clone(),
            OutputTarget::Record,
            size.0,
            size.1,
        );
        output.audio_bus = options
            .audio_bus
            .clone()
            .filter(|bus| self.audio_mix.bus_name(bus).is_some())
            .or_else(|| self.audio_mix.default_bus().map(str::to_string));
        self.recording.output = vec![UnifiedOutput::Headless(output)];
        self.recording.stamp = crate::record::timestamp_now();
        self.recording.take_name.clone_from(&options.name);
        self.recording.files.clear();
        self.recording.problem = None;
        self.recording.retry_at = None;
        self.recording.dropped_before = 0;
        self.recording.disk = Some(crate::record::DiskWatch::start(options.folder.clone()));
        if let Err(message) = self.spawn_record_encoder() {
            self.release_record_output();
            return CommandResult::Err {
                code: ErrorCode::InternalError,
                message,
            };
        }
        self.recording.started_at = Some(Instant::now());
        CommandResult::Ok
    }

    /// Stop recording. The file is finished on a background thread.
    pub fn cmd_stop_recording(&mut self) -> CommandResult {
        if self.record_running() {
            self.release_record_output();
            self.recording.last_file = self.recording.files.last().cloned();
            log::info!("Recording stopped");
        }
        self.recording.started_at = None;
        self.recording.problem = None;
        self.recording.retry_at = None;
        CommandResult::Ok
    }

    fn release_record_output(&mut self) {
        self.recording.disk = None;
        for output in self.recording.output.drain(..) {
            if let UnifiedOutput::Headless(mut h) = output {
                if let Some(mut sub) = h.subprocess.take() {
                    sub.stop();
                }
                if let Some(pass) = h.audio_pcm.take() {
                    pass.release(&mut self.audio_manager);
                }
            }
        }
    }

    /// Start the encoder on the take's next file, with a fresh audio tap.
    fn spawn_record_encoder(&mut self) -> Result<(), String> {
        let Self {
            recording,
            audio_manager,
            audio_mix,
            session,
            context,
            target_fps,
            ..
        } = self;
        let Some(UnifiedOutput::Headless(h)) = recording.output.first_mut() else {
            return Err("Not recording".into());
        };
        if let Some(mut old) = h.subprocess.take() {
            recording.dropped_before += old.frames_dropped();
            old.stop();
        }
        if let Some(stale) = h.audio_pcm.take() {
            stale.release(audio_manager);
        }
        let options = &recording.settings.options;
        let path = crate::record::file_path(
            &options.folder,
            &recording.take_name,
            &recording.stamp,
            recording.files.len() + 1,
            options.format.extension(),
        );
        let (audio_input, passthrough) = resolve_output_audio(
            audio_manager,
            audio_mix,
            &mut session.notifications,
            OutputAudio {
                bus: h.audio_bus.as_deref(),
                device: None,
            },
            "Record",
        );
        // Frames go to the encoder as NV12 made on the GPU where they can.
        let fps = encoder_fps(*target_fps);
        match crate::renderer::FfmpegSubprocess::spawn_record_with(
            options,
            (h.width, h.height),
            fps,
            audio_input,
            &path,
            crate::renderer::nv12::nv12_fits(h.width, h.height),
        ) {
            Ok(sub) => {
                h.keep_nv12(&context.device, sub.takes_nv12());
                h.subprocess = Some(Box::new(sub));
                h.audio_pcm = passthrough.map(Box::new);
                h.active = true;
                h.started_at = Some(Instant::now());
                if let Some(disk) = &recording.disk {
                    disk.file(path.clone());
                }
                recording.files.push(path);
                recording.dropped_seen = 0;
                recording.dropped_secs_seen = 0.0;
                recording.drops.clear();
                recording.fps = fps;
                Ok(())
            }
            Err(error) => {
                if let Some(pass) = passthrough {
                    pass.release(audio_manager);
                }
                Err(error.to_string())
            }
        }
    }

    /// Kill the recording's encoder as a crash would — for tests.
    #[doc(hidden)]
    pub fn kill_record_encoder_for_test(&mut self) {
        if let Some(UnifiedOutput::Headless(h)) = self.recording.output.first_mut() {
            if let Some(sub) = h.subprocess.as_mut() {
                sub.kill_process();
            }
        }
    }

    /// Once a frame: continue in a new file when the encoder stopped by
    /// itself, and watch whether it keeps up.
    pub(crate) fn poll_recording(&mut self) {
        let Some(UnifiedOutput::Headless(h)) = self.recording.output.first_mut() else {
            return;
        };
        let running = h.active && h.subprocess.as_mut().is_some_and(|s| s.is_running());
        if running {
            let dropped = h.subprocess.as_ref().map_or(0, |s| s.frames_dropped());
            let secs = h.subprocess.as_ref().map_or(0.0, |s| s.dropped_secs());
            let settled = h.started_at.is_some_and(|at| at.elapsed() > SETTLE);
            let now = Instant::now();
            if secs > self.recording.dropped_secs_seen && settled {
                self.recording
                    .drops
                    .push_back((now, secs - self.recording.dropped_secs_seen));
            }
            while self
                .recording
                .drops
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > BEHIND_WINDOW)
            {
                self.recording.drops.pop_front();
            }
            self.recording.dropped_seen = dropped;
            self.recording.dropped_secs_seen = secs;
            return;
        }
        let now = Instant::now();
        match self.recording.retry_at {
            None => {
                let why = h
                    .subprocess
                    .as_ref()
                    .and_then(|s| s.last_error())
                    .unwrap_or_else(|| "it stopped".into());
                log::warn!("Recording encoder stopped: {why}");
                self.recording.problem = Some(format!(
                    "The recording stopped ({why}) — continuing in a new file"
                ));
                h.active = false;
                self.recording.retry_at = Some(now + ENCODER_RETRY);
            }
            Some(at) if now >= at => match self.spawn_record_encoder() {
                Ok(()) => {
                    log::info!("Recording continues in a new file");
                    self.recording.retry_at = None;
                    // The problem stays shown: the take is now in two files.
                }
                Err(error) => {
                    self.recording.problem = Some(format!("Cannot record: {error}"));
                    self.recording.retry_at = Some(now + ENCODER_RETRY * 3);
                }
            },
            Some(_) => {}
        }
    }
}
