//! The stream's video encoded in Nida on the media engine, its bit rate
//! changed while live (see `streaming::vt_encoder`). Frames go to the engine
//! on a timeline at the stream's own frame rate — the conversion ffmpeg's
//! `-r` made of the raw frames before — and come back out to ffmpeg as FLV,
//! each with its time (see `streaming::flv`): a frame the engine drops is one
//! held a slot longer, the rest where they belong against the sound.

use std::io::Write;
use std::process::ChildStdin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crossbeam_channel::Receiver;

use super::{FfmpegSubprocess, FrameCounters, NativeShared, NativeVideo, Placed, Timeline};
use crate::streaming::flv::{slot_ms, without_filler, FlvWriter};
use crate::streaming::vt_encoder::{Encoded, VtEncoder};
use crate::streaming::{RatePlan, StreamEncoder, StreamOptions, VideoCodec};

/// The engine ready to encode, and where what becomes of each frame comes.
pub(super) type Opened = (VtEncoder, Receiver<Encoded>);

/// The encoder in Nida for a stream of `options` at `width` × `height` and
/// `fps`, where it can be had: on the media engine, holding its rate as
/// `plan` says — constant, or its own average — with NV12 frames from the
/// GPU. Its rate changes while live — following the upload where
/// `options.adaptive` asks, and as the operator sets it either way. Else
/// `None`, and ffmpeg encodes, at the rate set.
pub(super) fn open(
    options: &StreamOptions,
    (width, height): (u32, u32),
    fps: u32,
    nv12: bool,
    plan: &RatePlan,
) -> Option<Opened> {
    let why_not = if !nv12 {
        Some("the picture size is not one the GPU converts")
    } else if plan.encoder != StreamEncoder::Hardware {
        Some("it needs the hardware encoder")
    } else {
        None
    };
    if let Some(why) = why_not {
        log::warn!("Stream: ffmpeg encodes, and the bit rate cannot change while live: {why}");
        return None;
    }
    let (tx, rx) = crossbeam_channel::unbounded();
    match VtEncoder::new(
        options.codec,
        (width, height),
        fps,
        (options.video_kbps, plan.constant),
        options.keyframe_secs,
        tx,
    ) {
        Ok(engine) if engine.constant() == plan.constant => Some((engine, rx)),
        Ok(_) => {
            log::warn!("Stream: the media engine took no constant rate; ffmpeg encodes instead");
            None
        }
        Err(e) => {
            log::warn!("Stream: the encoder in Nida could not start ({e}); ffmpeg encodes instead");
            None
        }
    }
}

/// Frames the engine and the pipe to ffmpeg may hold at once: a fifth of a
/// second's, as the frames waiting for the writer, and at least four.
fn room(fps: u32) -> u64 {
    u64::from(fps / 5).max(4)
}

/// Start encoding: the writer taking frames from `rx` to the engine, and the
/// thread writing what comes out to ffmpeg's `stdin`.
#[allow(clippy::too_many_arguments)] // Each is one shared piece of the encoder's state.
pub(super) fn start(
    (engine, events): Opened,
    stdin: ChildStdin,
    rx: mpsc::Receiver<(Instant, Vec<u8>)>,
    (fps, codec, audio_kbps): (u32, VideoCodec, u32),
    counters: FrameCounters,
    (write_failed, shutting_down): (Arc<AtomicBool>, Arc<AtomicBool>),
    label: &str,
    start: Option<Instant>,
) -> (std::thread::JoinHandle<()>, NativeVideo) {
    let shared = NativeShared::new(engine.kbps(), audio_kbps, fps);
    let output = {
        let (events, shared, shutting_down) = (events.clone(), shared.clone(), shutting_down.clone());
        let label = label.to_string();
        std::thread::Builder::new()
            .name(format!("stream-video-{label}"))
            .spawn(move || {
                write_out(stdin, &events, (codec, fps), &shared, (&write_failed, &shutting_down), &label);
            })
            .expect("failed to spawn the stream's video output thread")
    };
    let writer = {
        let shared = shared.clone();
        std::thread::Builder::new()
            .name(format!("ffmpeg-writer-{label}"))
            .spawn(move || encode_in(engine, &events, &rx, fps, &counters, &shared, &shutting_down, start))
            .expect("failed to spawn the stream's writer thread")
    };
    (
        writer,
        NativeVideo {
            shared,
            output: Some(output),
        },
    )
}

/// The writer: each frame where the moment it was drawn falls on the
/// stream's timeline (see `Timeline`), a slot no frame came for filled with
/// the frame before, each encoded at the rate asked for then. Waits while
/// the engine and the pipe hold all they may — as the raw writer waited on a
/// full pipe — so frames back up to the render thread, which counts what it
/// cannot hand on.
#[allow(clippy::too_many_arguments)]
fn encode_in(
    mut engine: VtEncoder,
    pending: &Receiver<Encoded>,
    rx: &mpsc::Receiver<(Instant, Vec<u8>)>,
    fps: u32,
    counters: &FrameCounters,
    shared: &NativeShared,
    shutting_down: &AtomicBool,
    start: Option<Instant>,
) {
    let mut timeline = Timeline::new(start, f64::from(fps.max(1)));
    let mut last: Option<Vec<u8>> = None;
    let mut slot = 0u64;
    let room = room(fps);
    'frames: for (made, frame) in rx {
        let Placed::After(repeats) = timeline.place(made) else {
            counters.left_out.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        for n in 0..=repeats {
            let picture = if n < repeats {
                last.as_deref().unwrap_or(&frame)
            } else {
                &frame
            };
            while engine.in_flight() + pending.len() as u64 >= room {
                if shutting_down.load(Ordering::SeqCst) || shared.failed() {
                    break 'frames;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            let wanted = shared.wanted_kbps();
            if wanted != engine.kbps() {
                match engine.set_kbps(wanted) {
                    Ok(()) => shared.applied(wanted),
                    Err(e) => log::warn!("Stream: {e}; staying at {} kbps", engine.kbps()),
                }
            }
            shared.going_in();
            if let Err(e) = engine.encode(picture, slot) {
                log::error!("Stream encoder: {e}");
                shared.fail();
                break 'frames;
            }
            // Its callback told of a failure: stop at once.
            if engine.failed() {
                shared.fail();
                break 'frames;
            }
            slot += 1;
        }
        if repeats > 0 {
            counters.padded.fetch_add(repeats, Ordering::Relaxed);
        }
        counters.written.fetch_add(1, Ordering::Relaxed);
        last = Some(frame);
    }
    // Every frame still in the engine out, then the session ended: its
    // events end with it, and the output thread after them.
    engine.finish();
}

/// The output: each encoded frame written to ffmpeg as FLV at its slot's
/// time, until the engine has ended and its events with it.
fn write_out(
    mut stdin: ChildStdin,
    events: &Receiver<Encoded>,
    (codec, fps): (VideoCodec, u32),
    shared: &NativeShared,
    (write_failed, shutting_down): (&Arc<AtomicBool>, &Arc<AtomicBool>),
    label: &str,
) {
    let mut flv = FlvWriter::new(codec);
    for event in events {
        match event {
            Encoded::Frame {
                slot,
                key,
                units,
                config,
            } => {
                // Variable: the engine's padding stays behind.
                let units = if shared.strip_filler.load(Ordering::Relaxed) {
                    without_filler(&units, codec)
                } else {
                    std::borrow::Cow::Borrowed(units.as_slice())
                };
                let bytes = flv.frame(slot_ms(slot, fps), key, &units, config.as_deref());
                if let Err(e) = stdin.write_all(&bytes) {
                    FfmpegSubprocess::report_write_error(&e, shutting_down, write_failed, label);
                    return;
                }
                shared.came_out();
            }
            Encoded::Dropped { slot } => {
                if shared.dropped() == 0 {
                    log::warn!("Stream: the media engine dropped a frame (slot {slot}); the one before is held a slot longer");
                }
                shared.note_dropped();
                shared.came_out();
            }
            Encoded::Failed(_) => {
                shared.fail();
                shared.came_out();
            }
        }
    }
    let _ = stdin.flush();
}
