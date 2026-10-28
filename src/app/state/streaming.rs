//! The stream in the engine: its settings, saved with the stage, and while it
//! runs a headless output of its own — kept apart from the outputs list —
//! that renders the source at the stream's size and feeds the encoder. See
//! [`crate::streaming`] for the encoder and destinations.

use std::time::{Duration, Instant};

use crate::engine::{CommandResult, ErrorCode};
use crate::renderer::context::{HeadlessOutput, OutputTarget, UnifiedOutput};
use crate::streaming::adaptive::{floor_kbps, RateControl, Seen};
use crate::streaming::{
    AdaptiveStatus, DestinationState, DestinationStatus, Remedy, StreamDestination, StreamOptions,
    StreamSettings, StreamStatus, DESTINATIONS,
};

use super::{encoder_fps, resolve_output_audio, OutputAudio};

/// Wait before starting the encoder again after it stopped by itself.
const ENCODER_RETRY: Duration = Duration::from_secs(2);
/// The stretch over which dropped encoder frames are counted.
const ENCODER_BEHIND_WINDOW: Duration = Duration::from_secs(10);
/// Time for a new encoder to spin up before its dropped frames count.
const ENCODER_SETTLE: Duration = Duration::from_secs(3);

/// The stream's state in the engine.
#[derive(Default)]
pub(crate) struct Streaming {
    pub(crate) settings: StreamSettings,
    /// The stream's own output while it is on. Held as a one-element list so
    /// the headless render pass can take it like any output.
    pub(crate) output: Vec<UnifiedOutput>,
    started_at: Option<Instant>,
    /// When to try the encoder again after it stopped by itself.
    retry_at: Option<Instant>,
    encoder_problem: Option<String>,
    /// Encoder frames dropped as of the last check, and each recent rise in
    /// the time they covered — enough of it means the encoder cannot keep up.
    dropped_seen: u64,
    drops: std::collections::VecDeque<(Instant, f64)>,
    /// The rate the encoder was started at: frames reach it at that rate
    /// whatever the target is changed to since.
    fps: u32,
    /// The time this encoder's dropped frames would have covered, as of the
    /// last check.
    dropped_secs_seen: f64,
    /// Frames dropped, and their time, by encoders this stream had before
    /// this one, and each destination's stream lost to a slow upload then
    /// (bytes, pieces): the stream's tally goes on when its encoder starts
    /// again, with a fresh fan-out.
    lost_before: u64,
    lost_secs_before: f64,
    upload_before: Vec<Lost>,
    /// The video rate following the upload, while the stream's encoder is in
    /// Nida; kept over a restart of the encoder, which starts at the rate the
    /// upload carried rather than at the one that was too much for it.
    rate: Option<RateControl>,
}

/// What a destination lost to a slow upload under the stream's earlier
/// encoders.
#[derive(Debug, Clone, Copy, Default)]
struct Lost {
    bytes: u64,
    pieces: u64,
    secs: f32,
}

impl Lost {
    fn add(&mut self, status: &DestinationStatus) {
        self.bytes += status.dropped_bytes;
        self.pieces += status.dropped;
        self.secs += status.dropped_secs;
    }
}

impl Streaming {
    /// Keep the tally of an encoder that is going — read from it now, its
    /// last frames included — for the stream's, which goes on with the next.
    fn bank(&mut self, sub: &crate::renderer::FfmpegSubprocess) {
        self.lost_before += sub.frames_dropped();
        self.lost_secs_before += sub.dropped_secs();
        self.dropped_seen = 0;
        self.dropped_secs_seen = 0.0;
        if let Some(fanout) = sub.fanout() {
            for (before, status) in self.upload_before.iter_mut().zip(fanout.status()) {
                before.add(&status);
            }
        }
    }
}

/// A destination's settings as sent by the UI or the API: the secrets may be
/// left out (`None`) to keep the ones saved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DestinationEdit {
    pub enabled: bool,
    pub name: String,
    pub url: String,
    pub key: Option<String>,
    pub username: String,
    pub password: Option<String>,
}

impl super::super::VardaApp {
    /// The stream's settings, secrets included.
    pub fn stream_settings(&self) -> &StreamSettings {
        &self.streaming.settings
    }

    /// Put saved settings in place, as a workspace loads. A stream that is
    /// running is stopped first: it belongs to the workspace being left.
    pub(crate) fn load_stream_settings(&mut self, settings: Option<StreamSettings>) {
        if self.stream_is_on() {
            self.cmd_stop_stream();
        }
        self.streaming.settings = settings.unwrap_or_default().normalized();
    }

    fn stream_is_on(&self) -> bool {
        !self.streaming.output.is_empty()
    }

    /// How the stream and each destination are doing.
    pub fn stream_status(&self) -> StreamStatus {
        let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first() else {
            return StreamStatus::stopped();
        };
        let running = h.subprocess.as_ref().and_then(|s| s.fanout());
        let live_status = running.map(crate::streaming::Fanout::status);
        let destinations = self
            .streaming
            .settings
            .destinations
            .iter()
            .enumerate()
            .map(|(i, dest)| {
                if !dest.enabled {
                    return DestinationStatus::off();
                }
                if let Some(problem) = dest.problem() {
                    return DestinationStatus::invalid(problem);
                }
                let mut status = live_status
                    .as_ref()
                    .and_then(|s| s.get(i).cloned())
                    .unwrap_or_else(|| DestinationStatus {
                        // Waiting for the encoder to come back.
                        state: crate::streaming::DestinationState::Reconnecting,
                        message: self.streaming.encoder_problem.clone(),
                        ..DestinationStatus::off()
                    });
                // What it lost while the stream's earlier encoders ran.
                if let Some(before) = self.streaming.upload_before.get(i) {
                    status.dropped_bytes += before.bytes;
                    status.dropped += before.pieces;
                    status.dropped_secs += before.secs;
                }
                status
            })
            .collect();
        let fps = if self.streaming.fps > 0 {
            self.streaming.fps
        } else {
            encoder_fps(self.target_fps)
        };
        let behind = falling_behind(&self.streaming.drops, Instant::now());
        StreamStatus {
            audio_bus: h.audio_bus.clone(),
            live: true,
            live_secs: self
                .streaming
                .started_at
                .map_or(0.0, |at| at.elapsed().as_secs_f32()),
            encoder_problem: self.streaming.encoder_problem.clone().or_else(|| {
                behind.then(|| {
                    "The encoder cannot keep up — lower the size or bit rate, or use hardware"
                        .to_string()
                })
            }),
            encoder_dropped: self.streaming.lost_before + self.streaming.dropped_seen,
            #[allow(clippy::cast_possible_truncation)]
            encoder_dropped_secs: (self.streaming.lost_secs_before
                + self.streaming.dropped_secs_seen) as f32,
            encoder_fps: fps,
            stream_fps: self.streaming.settings.options.fps.unwrap_or(fps).max(1),
            rate_live: h.subprocess.as_ref().is_some_and(|s| s.changes_rate_live()),
            rate_mode: h.subprocess.as_ref().and_then(|s| s.rate_plan()).map(|p| p.mode),
            rate_note: h
                .subprocess
                .as_ref()
                .and_then(|s| s.rate_plan())
                .and_then(|p| p.note.clone()),
            stream_kbps: running.and_then(crate::streaming::Fanout::input_kbps),
            adaptive: self.streaming.rate.as_ref().map(|rate| AdaptiveStatus {
                asked_kbps: rate.asked(),
                kbps: h
                    .subprocess
                    .as_ref()
                    .and_then(|s| s.video_kbps())
                    .unwrap_or_else(|| rate.target()),
                floor_kbps: rate.floor(),
            }),
            destinations,
        }
    }

    /// Change what is streamed and how. While the stream is on the source
    /// can change, and — for a stream encoded in Nida — the video bit rate
    /// and whether it follows the upload, from the next frame on; the rest
    /// needs a new encoder, so it waits for a stop.
    pub fn cmd_set_stream_options(&mut self, options: StreamOptions) -> CommandResult {
        let options = options.clamped();
        if self.stream_is_on() {
            let current = self.streaming.settings.options.clone();
            let live_rate = self.stream_rate_changes_live();
            let rest_alike = StreamOptions {
                source: current.source.clone(),
                video_kbps: current.video_kbps,
                adaptive: current.adaptive,
                rate_mode: current.rate_mode,
                ..options.clone()
            } == current;
            let rate_changed =
                options.video_kbps != current.video_kbps || options.adaptive != current.adaptive;
            let mode_changed = options.rate_mode != current.rate_mode;
            // Constant and Variable are one encoding, padded or not: that
            // changes while live in Nida. Average is another encoding.
            let held = |m: crate::streaming::RateMode| {
                matches!(m, crate::streaming::RateMode::Constant | crate::streaming::RateMode::Variable)
            };
            let refused = if !rest_alike {
                Some(if live_rate {
                    "Stop the stream to change that: only the picture and the video bit rate change while live"
                } else {
                    "Stop the stream to change that: only the picture changes while live"
                })
            } else if (rate_changed || mode_changed) && !live_rate {
                Some(if current.encoder == crate::streaming::StreamEncoder::Software {
                    "The video bit rate changes while live only with the Hardware encoder: stop the stream to change it"
                } else {
                    "This stream's encoder cannot change its bit rate while live (the log says why): stop the stream to change it"
                })
            } else if mode_changed && !(held(options.rate_mode) && held(current.rate_mode)) {
                Some("Average is encoded apart from Constant and Variable: stop the stream to change to it or from it")
            } else if mode_changed && !self.stream_holds_rate_constant() {
                // Set Constant, but sent as Average here: nothing to switch.
                Some("This Mac's media engine holds no constant rate: the stream goes out as Average whatever is set, and Variable needs the software encoder (stop the stream to change it)")
            } else {
                None
            };
            if let Some(message) = refused {
                return CommandResult::Err {
                    code: ErrorCode::InvalidInput,
                    message: message.into(),
                };
            }
            if let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first_mut() {
                h.source = options.source.clone();
            }
            let mode = options.rate_mode;
            self.streaming.settings.options = options;
            if rate_changed {
                self.follow_rate_set();
            }
            if mode_changed {
                if let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first_mut() {
                    if let Some(sub) = h.subprocess.as_mut() {
                        if sub.set_rate_mode(mode) {
                            log::info!("Stream rate held as {} while live", mode.label());
                        }
                    }
                }
            }
            return CommandResult::Ok;
        }
        self.streaming.settings.options = options;
        CommandResult::Ok
    }

    /// Whether the running stream's encoder holds its rate constant —
    /// Constant or Variable as it runs, not as set (see `RatePlan`).
    fn stream_holds_rate_constant(&self) -> bool {
        matches!(
            self.streaming.output.first(),
            Some(UnifiedOutput::Headless(h))
                if h.subprocess.as_ref().and_then(|s| s.rate_plan()).is_some_and(|p| p.constant)
        )
    }

    /// Whether the running stream's video rate changes while live: it is
    /// encoded in Nida.
    fn stream_rate_changes_live(&self) -> bool {
        matches!(
            self.streaming.output.first(),
            Some(UnifiedOutput::Headless(h))
                if h.subprocess.as_ref().is_some_and(|s| s.changes_rate_live())
        )
    }

    /// The video rate set, or whether it follows the upload, changed while
    /// live: the encoder in Nida takes it from the next frame on — at once
    /// where the upload does not hold the rate down (see
    /// [`RateControl::set_asked`]) — and each destination may now hold its
    /// network buffer at the new rate.
    fn follow_rate_set(&mut self) {
        let options = self.streaming.settings.options.clone();
        let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first() else {
            return;
        };
        let Some(sub) = h.subprocess.as_ref() else {
            return;
        };
        let size = (h.width, h.height);
        if options.adaptive {
            let fps = self.streaming.fps;
            match self.streaming.rate.as_mut() {
                Some(rate) => rate.set_asked(options.video_kbps, buffer_bytes(&options)),
                None => self.streaming.rate = Some(rate_control(&options, size, fps)),
            }
        } else {
            self.streaming.rate = None;
        }
        let kbps = self
            .streaming
            .rate
            .as_ref()
            .map_or(options.video_kbps, RateControl::target);
        sub.set_video_kbps(kbps);
        if let Some(fanout) = sub.fanout() {
            fanout.set_buffer(
                u64::from(options.video_kbps + options.audio_kbps) * 1000 / 8,
                options.buffer_secs,
            );
            if !options.adaptive {
                fanout.set_remedy(Remedy::LowerLive);
            }
        }
        log::info!(
            "Stream video bit rate set to {} kbps while live{}",
            options.video_kbps,
            if options.adaptive {
                ", lowered while the upload cannot carry it"
            } else {
                ", held there"
            }
        );
    }

    /// Change one destination. While the stream is on it takes effect at
    /// once: switched on, it connects; switched off, it hangs up; pointed
    /// elsewhere, it reconnects there. The others are not touched.
    pub fn cmd_set_stream_destination(
        &mut self,
        index: usize,
        edit: DestinationEdit,
    ) -> CommandResult {
        if index >= DESTINATIONS {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: format!(
                    "There are {DESTINATIONS} destinations, numbered 0 to {}",
                    DESTINATIONS - 1
                ),
            };
        }
        let dest = &mut self.streaming.settings.destinations[index];
        *dest = StreamDestination {
            enabled: edit.enabled,
            name: edit.name,
            url: edit.url.trim().to_string(),
            key: edit
                .key
                .map_or_else(|| dest.key.clone(), |k| k.trim().to_string()),
            username: edit.username.trim().to_string(),
            password: edit.password.unwrap_or_else(|| dest.password.clone()),
        };
        let dest = dest.clone();
        if let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first() {
            if let Some(fanout) = h.subprocess.as_ref().and_then(|s| s.fanout()) {
                fanout.set_destination(index, &dest);
            }
        }
        CommandResult::Ok
    }

    /// Start the stream to every destination switched on.
    pub fn cmd_start_stream(&mut self) -> CommandResult {
        if self.stream_is_on() {
            return CommandResult::Ok;
        }
        let settings = &self.streaming.settings;
        let on: Vec<&StreamDestination> =
            settings.destinations.iter().filter(|d| d.enabled).collect();
        if on.is_empty() {
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: "Switch on at least one destination".into(),
            };
        }
        if on.iter().all(|d| d.problem().is_some()) {
            let problem = on.iter().find_map(|d| d.problem()).unwrap_or_default();
            return CommandResult::Err {
                code: ErrorCode::InvalidInput,
                message: format!("No destination can be sent to: {problem}"),
            };
        }
        let options = &settings.options;
        let mut output = HeadlessOutput::new(
            &self.context.device,
            "Stream".into(),
            options.source.clone(),
            OutputTarget::Stream,
            options.width,
            options.height,
        );
        output.audio_bus = options
            .audio_bus
            .clone()
            .filter(|bus| self.audio_mix.bus_name(bus).is_some())
            .or_else(|| self.audio_mix.default_bus().map(str::to_string));
        self.streaming.output = vec![UnifiedOutput::Headless(output)];
        self.streaming.encoder_problem = None;
        self.streaming.retry_at = None;
        self.streaming.dropped_seen = 0;
        self.streaming.dropped_secs_seen = 0.0;
        self.streaming.lost_before = 0;
        self.streaming.lost_secs_before = 0.0;
        self.streaming.upload_before = vec![Lost::default(); crate::streaming::DESTINATIONS];
        self.streaming.drops.clear();
        // A new stream starts at the rate set.
        self.streaming.rate = None;
        if let Err(message) = self.spawn_stream_encoder() {
            self.release_stream_output();
            return CommandResult::Err {
                code: ErrorCode::InternalError,
                message,
            };
        }
        self.streaming.started_at = Some(Instant::now());
        log::info!("Stream started");
        CommandResult::Ok
    }

    /// Stop the stream. Each destination is told the stream has ended.
    pub fn cmd_stop_stream(&mut self) -> CommandResult {
        if self.stream_is_on() {
            self.release_stream_output();
            log::info!("Stream stopped");
        }
        self.streaming.started_at = None;
        self.streaming.encoder_problem = None;
        self.streaming.retry_at = None;
        self.streaming.rate = None;
        CommandResult::Ok
    }

    /// Stop the encoder and give back its audio.
    fn release_stream_output(&mut self) {
        for output in self.streaming.output.drain(..) {
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

    /// (Re)start the encoder of the stream's output, with a fresh audio tap.
    /// Whether the stream's frames reach its encoder as NV12 made on the GPU
    /// (for tests: nothing on screen says).
    #[doc(hidden)]
    pub fn stream_encoder_takes_nv12(&self) -> bool {
        matches!(
            self.streaming.output.first(),
            Some(UnifiedOutput::Headless(h))
                if h.nv12.is_some()
                    && h.subprocess.as_ref().is_some_and(|s| s.takes_nv12())
        )
    }

    /// The rate the running stream reckons a lost piece at, sound and
    /// picture, in kbit/s (for tests: nothing on screen says).
    #[doc(hidden)]
    pub fn stream_loss_reckoned_kbps(&self) -> Option<u32> {
        match self.streaming.output.first() {
            Some(UnifiedOutput::Headless(h)) => h
                .subprocess
                .as_ref()
                .and_then(|s| s.fanout())
                .map(crate::streaming::Fanout::reckoned_kbps),
            _ => None,
        }
    }

    fn spawn_stream_encoder(&mut self) -> Result<(), String> {
        let Self {
            streaming,
            audio_manager,
            audio_mix,
            session,
            context,
            target_fps,
            ..
        } = self;
        let Some(UnifiedOutput::Headless(h)) = streaming.output.first_mut() else {
            return Err("The stream is not on".into());
        };
        if let Some(mut old) = h.subprocess.take() {
            // Its tally, for the stream's (as `Streaming::bank`; `h` holds
            // the output, so field by field).
            streaming.lost_before += old.frames_dropped();
            streaming.lost_secs_before += old.dropped_secs();
            streaming.dropped_seen = 0;
            streaming.dropped_secs_seen = 0.0;
            if let Some(fanout) = old.fanout() {
                for (before, status) in streaming.upload_before.iter_mut().zip(fanout.status()) {
                    before.add(&status);
                }
            }
            old.stop();
        }
        if let Some(stale) = h.audio_pcm.take() {
            stale.release(audio_manager);
        }
        let (audio_input, passthrough) = resolve_output_audio(
            audio_manager,
            audio_mix,
            &mut session.notifications,
            OutputAudio {
                bus: h.audio_bus.as_deref(),
                device: None,
            },
            "Stream",
        );
        // Frames go to the encoder as NV12 made on the GPU where they can.
        let fps = encoder_fps(*target_fps);
        match crate::renderer::FfmpegSubprocess::spawn_stream_with(
            &streaming.settings,
            h.presentation_request,
            (h.width, h.height),
            fps,
            audio_input,
            crate::renderer::nv12::nv12_fits(h.width, h.height),
        ) {
            Ok(sub) => {
                let options = &streaming.settings.options;
                if sub.changes_rate_live() && options.adaptive {
                    let rate = streaming
                        .rate
                        .get_or_insert_with(|| rate_control(options, (h.width, h.height), fps));
                    // Started again: at the rate the upload carried.
                    sub.set_video_kbps(rate.target());
                } else {
                    streaming.rate = None;
                    if sub.changes_rate_live() {
                        if let Some(fanout) = sub.fanout() {
                            fanout.set_remedy(Remedy::LowerLive);
                        }
                    }
                }
                if let Some(resolved) = sub.presentation().cloned() {
                    h.set_resolved_presentation(&context.device, resolved);
                }
                h.keep_nv12(&context.device, sub.takes_nv12());
                h.subprocess = Some(Box::new(sub));
                h.audio_pcm = passthrough.map(Box::new);
                h.active = true;
                h.started_at = Some(Instant::now());
                streaming.fps = fps;
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

    /// Once a frame: notice an encoder that stopped by itself and start it
    /// again after a pause, and watch whether it keeps up.
    pub(crate) fn poll_stream(&mut self) {
        let Some(UnifiedOutput::Headless(h)) = self.streaming.output.first_mut() else {
            return;
        };
        let running = h.active && h.subprocess.as_mut().is_some_and(|s| s.is_running());
        if running {
            let dropped = h.subprocess.as_ref().map_or(0, |s| s.frames_dropped());
            let secs = h.subprocess.as_ref().map_or(0.0, |s| s.dropped_secs());
            // A frame or two is dropped while the encoder spins up; that is
            // not it falling behind.
            let settled = h.started_at.is_some_and(|at| at.elapsed() > ENCODER_SETTLE);
            let now = Instant::now();
            if secs > self.streaming.dropped_secs_seen && settled {
                self.streaming
                    .drops
                    .push_back((now, secs - self.streaming.dropped_secs_seen));
            }
            while self
                .streaming
                .drops
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > ENCODER_BEHIND_WINDOW)
            {
                self.streaming.drops.pop_front();
            }
            self.streaming.dropped_seen = dropped;
            self.streaming.dropped_secs_seen = secs;
            // The video rate following the upload.
            if let (Some(rate), Some(sub)) = (self.streaming.rate.as_mut(), h.subprocess.as_ref()) {
                follow_upload(rate, sub, now);
            }
            return;
        }
        let now = Instant::now();
        match self.streaming.retry_at {
            None => {
                let why = h
                    .subprocess
                    .as_ref()
                    .and_then(|s| s.last_error())
                    .unwrap_or_else(|| "it stopped".into());
                log::warn!("Stream encoder stopped: {why}");
                self.streaming.encoder_problem =
                    Some(format!("The encoder stopped ({why}) — restarting"));
                let gone = h.subprocess.take();
                h.active = false;
                if let Some(mut sub) = gone {
                    // Its last frames' tally too, read before it goes.
                    self.streaming.bank(&sub);
                    sub.stop();
                }
                self.streaming.retry_at = Some(now + ENCODER_RETRY);
            }
            Some(at) if now >= at => match self.spawn_stream_encoder() {
                Ok(()) => {
                    log::info!("Stream encoder restarted");
                    self.streaming.encoder_problem = None;
                    self.streaming.retry_at = None;
                    // The old encoder's tally was banked as it went.
                    self.streaming.drops.clear();
                }
                Err(error) => {
                    self.streaming.encoder_problem =
                        Some(format!("The encoder cannot start: {error}"));
                    self.streaming.retry_at = Some(now + ENCODER_RETRY * 2);
                }
            },
            Some(_) => {}
        }
    }
}

/// Look at the stream's destinations and ask its encoder for the rate their
/// uploads carry; say what is being done about one that struggles.
/// Bytes each destination of a stream of `options` may hold: its network
/// buffer at the rate set, sound and picture, as the fan-out has it.
fn buffer_bytes(options: &StreamOptions) -> usize {
    let bytes_per_sec = u64::from(options.video_kbps + options.audio_kbps) * 1000 / 8;
    usize::try_from((bytes_per_sec * u64::from(options.buffer_secs)).max(1024 * 1024))
        .unwrap_or(usize::MAX)
}

/// The video rate of a stream of `options`, `size` at `fps` frames a second
/// to the encoder, following its upload: from the rate set.
fn rate_control(options: &StreamOptions, (width, height): (u32, u32), fps: u32) -> RateControl {
    let out_fps = options.fps.unwrap_or(fps).max(1);
    RateControl::new(
        options.video_kbps,
        options.audio_kbps,
        floor_kbps(width, height, out_fps),
        buffer_bytes(options),
    )
}

fn follow_upload(rate: &mut RateControl, sub: &crate::renderer::FfmpegSubprocess, now: Instant) {
    let Some(fanout) = sub.fanout() else { return };
    let seen: Vec<Seen> = fanout
        .status()
        .iter()
        .map(|d| Seen {
            queued: usize::try_from(d.backlog_bytes).unwrap_or(usize::MAX),
            dropped: d.dropped,
            kbps: d.kbps,
            sending: matches!(d.state, DestinationState::Live | DestinationState::Struggling),
            live_secs: d.live_secs,
            generation: d.generation,
            network: d.network,
        })
        .collect();
    // The rate the engine took last: a lost piece is reckoned at it, and
    // whether it is as low as it goes is judged by it — a cut the engine
    // refused leaves it higher than asked.
    let applied = sub.video_kbps().unwrap_or_else(|| rate.target());
    let before = rate.target();
    let target = rate.look(now, &seen);
    if target != before {
        log::info!(
            "Stream video bit rate {before} → {target} kbps ({})",
            if target < before {
                "the upload is falling behind"
            } else {
                "the upload keeps up"
            }
        );
        sub.set_video_kbps(target);
    }
    fanout.set_remedy(if applied <= rate.floor() {
        Remedy::AtFloor
    } else {
        Remedy::Adapting
    });
}

/// Whether the encoder is falling behind: frames dropped covering more than
/// a hundredth of the last ten seconds — as measured when they were dropped,
/// so at whatever frame rate the show runs. A frame dropped now and then is
/// covered by repeating the one before and nobody sees it; that is not worth
/// a blinking button in the middle of a show.
fn falling_behind(drops: &std::collections::VecDeque<(Instant, f64)>, now: Instant) -> bool {
    let recent: f64 = drops
        .iter()
        .filter(|(at, _)| now.duration_since(*at) <= ENCODER_BEHIND_WINDOW)
        .map(|(_, secs)| secs)
        .sum();
    recent > ENCODER_BEHIND_WINDOW.as_secs_f64() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drops `ago` milliseconds back, each `frames` frames at `fps`.
    fn drops(
        fps: f64,
        ago_and_frames: &[(u64, u32)],
    ) -> (std::collections::VecDeque<(Instant, f64)>, Instant) {
        let now = Instant::now() + Duration::from_secs(60);
        let d = ago_and_frames
            .iter()
            .map(|&(ago, n)| {
                (
                    now.checked_sub(Duration::from_millis(ago)).unwrap(),
                    f64::from(n) / fps,
                )
            })
            .collect();
        (d, now)
    }

    #[test]
    fn an_odd_dropped_frame_is_not_falling_behind() {
        let (d, now) = drops(60.0, &[(9000, 1), (5000, 1), (1000, 1)]);
        assert!(!falling_behind(&d, now), "three frames in ten seconds");
    }

    #[test]
    fn a_steady_trickle_of_drops_is_falling_behind() {
        let (d, now) = drops(60.0, &[(8000, 3), (4000, 3), (500, 2)]);
        assert!(falling_behind(&d, now), "eight of 600 frames");
    }

    /// Four frames dropped at 30 fps over ten seconds are more than a
    /// hundredth of that time, whatever rate the encoder was started at.
    #[test]
    fn falling_behind_is_judged_by_the_time_lost() {
        let (d, now) = drops(30.0, &[(8000, 1), (6000, 1), (3000, 1), (500, 1)]);
        assert!(falling_behind(&d, now), "four of 300 frames");
    }

    #[test]
    fn old_drops_are_forgotten() {
        let (d, now) = drops(60.0, &[(30_000, 50), (500, 1)]);
        assert!(!falling_behind(&d, now));
    }
}
