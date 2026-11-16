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

use super::{
    take_live, FfmpegSubprocess, FrameCounters, NativeShared, NativeVideo, Placed, RefusedCredit,
    Timeline,
};
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

/// The encoder in Nida for a recording in `codec` at `width` × `height` and
/// `fps`, `kbps` its average, a keyframe a second (what an MP4's pieces close
/// on): on the media engine, with NV12 frames from the GPU. Each frame goes
/// with its own time, so one that falls behind skips ahead (see
/// `take_live`) instead of encoding repeats — repeats took the engine from a
/// 4K60 stream beside it, and the recording got two fresh frames a second
/// (M5 Max, 2026-10-29). `None` where it will not open: ffmpeg encodes.
pub(super) fn open_for_record(
    codec: VideoCodec,
    (width, height): (u32, u32),
    fps: u32,
    kbps: u32,
) -> Option<Opened> {
    let (tx, rx) = crossbeam_channel::unbounded();
    match VtEncoder::new(codec, (width, height), fps, (kbps, false), 1, tx) {
        Ok(engine) => Some((engine, rx)),
        Err(e) => {
            log::warn!(
                "Recording: the encoder in Nida could not start ({e}); ffmpeg encodes instead"
            );
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
    (fps, codec, audio_kbps, fill): (u32, VideoCodec, u32, u64),
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
            .spawn(move || {
                encode_in(
                    engine,
                    &events,
                    &rx,
                    (fps, fill),
                    &counters,
                    &shared,
                    &shutting_down,
                    start,
                );
            })
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
    (fps, fill): (u32, u64),
    counters: &FrameCounters,
    shared: &NativeShared,
    shutting_down: &AtomicBool,
    start: Option<Instant>,
) {
    let mut timeline = Timeline::of_show(start, f64::from(fps.max(1)));
    let mut last: Option<Vec<u8>> = None;
    let mut slot = 0u64;
    let room = room(fps);
    // One slot to the engine, once it has room, at the rate asked for then.
    // False when the stream ends or fails.
    let put = |engine: &mut VtEncoder, picture: &[u8], slot: u64| -> bool {
        while engine.in_flight() + pending.len() as u64 >= room {
            if shutting_down.load(Ordering::SeqCst) || shared.failed() {
                return false;
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
            return false;
        }
        // Its callback told of a failure: stop at once.
        if engine.failed() {
            shared.fail();
            return false;
        }
        true
    };
    // Frames the render thread could not hand on: the slots a skip covers
    // count only past those, already counted as lost (see `RefusedCredit`).
    let mut refused = counters.dropped.load(Ordering::Relaxed);
    let mut credit = RefusedCredit::default();
    'frames: while let Ok(head) = rx.recv() {
        let refused_now = counters.dropped.load(Ordering::Relaxed);
        let refused_since = refused_now.saturating_sub(refused);
        refused = refused_now;
        credit.refused(refused_since);
        let mut waiting = vec![head];
        waiting.extend(std::iter::from_fn(|| rx.try_recv().ok()));
        // Fallen behind real time: the newest frame only (see `take_live`).
        let (frames, let_go) = take_live(waiting);
        counters.left_out.fetch_add(let_go, Ordering::Relaxed);
        for (made, frame) in frames {
            // Far past the next slot: on from this frame's own time, the
            // time between skipped — the stream stays live.
            // Frames backed up: nothing filled, on from the newest's time.
            let (skipped, placed) = timeline.place_live(made, if let_go > 0 { 0 } else { fill });
            if skipped > 0 && (let_go > 0 || refused_since > 0) {
                // Frames backed up: the encoder behind, its trouble — but
                // for the slots of frames already counted, refused.
                let lost_slots = credit.skipped(skipped);
                let before = counters.skipped.fetch_add(lost_slots, Ordering::Relaxed);
                if before == 0 {
                    log::warn!(
                        "Stream: fell behind real time; {skipped} frames skipped to stay live \
                         (the media engine busy?)"
                    );
                }
            } else if skipped > 0 {
                // The frames came one by one: the show was held up.
                counters.held.fetch_add(skipped, Ordering::Relaxed);
            }
            slot += skipped;
            let Some(placed) = placed else {
                // A step of a long skip: the frame before, once, there.
                counters.left_out.fetch_add(1, Ordering::Relaxed);
                if let Some(picture) = last.as_deref() {
                    if !put(&mut engine, picture, slot) {
                        break 'frames;
                    }
                    slot += 1;
                    timeline.wrote_one();
                    counters.padded.fetch_add(1, Ordering::Relaxed);
                    credit.filled(1);
                }
                continue;
            };
            let Placed::After(repeats) = placed else {
                counters.left_out.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            for n in 0..=repeats {
                let picture = if n < repeats {
                    last.as_deref().unwrap_or(&frame)
                } else {
                    &frame
                };
                if !put(&mut engine, picture, slot) {
                    break 'frames;
                }
                slot += 1;
            }
            if repeats > 0 {
                counters.padded.fetch_add(repeats, Ordering::Relaxed);
                credit.filled(repeats);
            }
            counters.written.fetch_add(1, Ordering::Relaxed);
            last = Some(frame);
        }
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
