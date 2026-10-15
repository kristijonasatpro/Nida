//! A video deck's soundtrack, decoded on a thread of its own and kept in step
//! with the picture.
//!
//! The picture's decode thread owns the playhead: it loops, seeks, pauses and
//! gets suspended by residency. The soundtrack follows that playhead rather
//! than keeping a clock of its own. Each tick it compares where the picture is
//! with where the sound is:
//!
//! - a small difference is closed by reading the sound up to half a percent
//!   fast or slow, which nobody hears;
//! - a large one — a loop, a seek, a clip starting — re-seeks the soundtrack to
//!   the picture, a gap of a few milliseconds;
//! - a paused, suspended, reversed or off-speed clip is silent. Pitch-shifted
//!   or backwards sound is not what a conference wants from a clip.
//!
//! Decoding stops while nobody can hear the clip (muted, sent nowhere, or off
//! air on the bus it follows), so a show with thirty clips decodes one.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use ffmpeg_next as ffmpeg;

use super::core::SAMPLE_RATE;
use super::reader::{Frame, RateReader};
use crate::video::PlaybackSnapshot;

/// How far ahead of the mix the soundtrack is decoded.
const DECODE_AHEAD_SECS: f64 = 0.2;
/// Past this difference from the picture the soundtrack re-seeks instead of
/// drifting back into step.
const RESYNC_SECS: f64 = 0.1;
/// Seconds of drift turned into read-rate correction per second.
const CORRECTION_GAIN: f64 = 0.25;
/// Largest read-rate correction: half a percent, inaudible as pitch.
const MAX_CORRECTION: f64 = 0.005;
/// How often the decode thread checks the picture.
const TICK: std::time::Duration = std::time::Duration::from_millis(5);
/// A soundtrack that starts later than the picture is padded with silence, up
/// to this much.
const MAX_LEAD_IN_SECS: f64 = 5.0;

/// The part of a soundtrack the mixer reads, shared with its decode thread.
#[derive(Default)]
pub struct ClipAudioShared {
    queue: Mutex<ClipQueue>,
    /// Whether anyone can hear this clip. Set by the mixer's owner every
    /// frame; the decode thread idles while it is false.
    wanted: AtomicBool,
}

#[derive(Default)]
struct ClipQueue {
    /// Decoded sound at the mix rate, stereo.
    frames: VecDeque<Frame>,
    /// Clip time, in seconds, of the front of `frames`.
    front_pts: f64,
    /// The mixer plays the queue only while this is set.
    playing: bool,
    /// Read-rate trim that closes a small drift from the picture.
    correction: f64,
    reader: RateReader,
}

impl ClipAudioShared {
    /// Fill `out` (already silent) with the next block of the soundtrack.
    pub fn pull(&self, out: &mut [Frame]) {
        let Ok(mut guard) = self.queue.lock() else {
            return;
        };
        let q = &mut *guard;
        if !q.playing {
            return;
        }
        let before = q.frames.len();
        q.reader.read(&mut q.frames, 1.0 + q.correction, out);
        let consumed = before - q.frames.len();
        q.front_pts += consumed as f64 / f64::from(SAMPLE_RATE);
    }

    /// Say whether anyone can hear this clip.
    pub fn set_wanted(&self, wanted: bool) {
        self.wanted.store(wanted, Ordering::Relaxed);
    }

    fn silence(&self) {
        if let Ok(mut q) = self.queue.lock() {
            q.playing = false;
            q.frames.clear();
            q.reader.reset();
            q.correction = 0.0;
        }
    }
}

/// A clip's soundtrack: the decode thread and what it shares with the mixer.
/// Dropping it stops the thread and silences the clip.
pub struct ClipAudio {
    shared: Arc<ClipAudioShared>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ClipAudio {
    /// Open the soundtrack of the video at `path`, following the playhead
    /// published in `video`. `Ok(None)` for a file with no sound.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened or its audio decoder
    /// cannot be created.
    pub fn open(path: &Path, video: Arc<Mutex<PlaybackSnapshot>>) -> Result<Option<Self>> {
        let Some(decoder) = ClipDecoder::open(path)? else {
            return Ok(None);
        };
        let shared = Arc::new(ClipAudioShared::default());
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let shared = shared.clone();
            let stop = stop.clone();
            std::thread::Builder::new()
                .name("clip-audio".into())
                .spawn(move || follow_picture(decoder, &shared, &video, &stop))
                .context("spawning the clip audio thread")?
        };
        Ok(Some(Self {
            shared,
            stop,
            thread: Some(thread),
        }))
    }

    /// What the mixer reads.
    pub fn shared(&self) -> Arc<ClipAudioShared> {
        self.shared.clone()
    }
}

impl Drop for ClipAudio {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.shared.silence();
    }
}

/// The decode thread: keep the soundtrack where the picture is.
fn follow_picture(
    mut dec: ClipDecoder,
    shared: &ClipAudioShared,
    video: &Mutex<PlaybackSnapshot>,
    stop: &AtomicBool,
) {
    let snapshot = || video.lock().ok().map(|s| s.clone());
    let mut synced = false;
    let mut chunk = Vec::new();
    while !stop.load(Ordering::Acquire) {
        let Some(snap) = snapshot() else {
            return;
        };
        let audible = snap.playing
            && !snap.suspended
            && !snap.reverse
            && (snap.effective_speed - 1.0).abs() < 0.01
            && shared.wanted.load(Ordering::Relaxed);
        if !audible {
            if synced {
                shared.silence();
                synced = false;
            }
            std::thread::sleep(TICK * 2);
            continue;
        }
        let video_now = snap.position_now();

        if !synced {
            if let Err(e) = prime(&mut dec, shared, video_now, &snapshot) {
                log::warn!("Clip audio: {e}");
            }
            synced = true;
            continue;
        }

        let (audio_now, depth) = match shared.queue.lock() {
            Ok(q) => (q.front_pts, q.frames.len()),
            Err(_) => return,
        };
        let drift = video_now - audio_now;
        // Past the end of the sound there is nothing to seek to; wait for the
        // picture to loop or be moved back.
        let past_end = dec.eof && depth == 0 && video_now >= audio_now - RESYNC_SECS;
        if drift.abs() > RESYNC_SECS && !past_end {
            synced = false;
            continue;
        }
        if let Ok(mut q) = shared.queue.lock() {
            q.correction = (drift * CORRECTION_GAIN).clamp(-MAX_CORRECTION, MAX_CORRECTION);
        }

        let mut ahead = depth as f64 / f64::from(SAMPLE_RATE);
        while ahead < DECODE_AHEAD_SECS && !dec.eof && !stop.load(Ordering::Relaxed) {
            chunk.clear();
            if let Err(e) = dec.decode_more(&mut chunk) {
                log::warn!("Clip audio decode: {e}");
                break;
            }
            ahead += chunk.len() as f64 / f64::from(SAMPLE_RATE);
            if let Ok(mut q) = shared.queue.lock() {
                q.frames.extend(chunk.iter().copied());
            }
        }
        std::thread::sleep(TICK);
    }
}

/// Seek the soundtrack to the picture and hand the mixer a fresh queue that
/// starts exactly where the picture is by the time it is ready.
fn prime(
    dec: &mut ClipDecoder,
    shared: &ClipAudioShared,
    target: f64,
    snapshot: &dyn Fn() -> Option<PlaybackSnapshot>,
) -> Result<()> {
    dec.seek(target)?;
    let mut frames: VecDeque<Frame> = VecDeque::new();
    let mut start: Option<f64> = None;
    let mut chunk = Vec::new();
    loop {
        chunk.clear();
        let pts = dec.decode_more(&mut chunk)?;
        if chunk.is_empty() {
            if dec.eof {
                break;
            }
            continue;
        }
        let first = *start.get_or_insert(pts.unwrap_or(target));
        frames.extend(chunk.iter().copied());
        if first + frames.len() as f64 / f64::from(SAMPLE_RATE) >= target + DECODE_AHEAD_SECS {
            break;
        }
    }
    // Decoding took a moment and the picture moved on; start where it is now.
    let now = snapshot().map_or(target, |s| s.position_now());
    let start = start.unwrap_or(now);
    let rate = f64::from(SAMPLE_RATE);
    if now >= start {
        let skip = (((now - start) * rate).round() as usize).min(frames.len());
        frames.drain(..skip);
    } else {
        let lead = ((start - now).min(MAX_LEAD_IN_SECS) * rate).round() as usize;
        for _ in 0..lead {
            frames.push_front([0.0; 2]);
        }
    }
    if let Ok(mut q) = shared.queue.lock() {
        q.frames = frames;
        q.front_pts = now;
        q.reader.reset();
        q.correction = 0.0;
        q.playing = true;
    }
    Ok(())
}

/// The audio half of a video file: demuxer, decoder and resampler to the mix
/// format. Opened separately from the picture's, so the two never contend.
struct ClipDecoder {
    ictx: ffmpeg::format::context::Input,
    decoder: ffmpeg::decoder::Audio,
    stream_index: usize,
    time_base: f64,
    /// When the file starts: clip time 0, as the picture counts it (see
    /// [`crate::video::clip_start`]).
    start: f64,
    resampler: Option<ffmpeg::software::resampling::Context>,
    decoded: ffmpeg::frame::Audio,
    /// The decoder has been told the file ended.
    eof_sent: bool,
    /// Every sample in the file has been produced.
    eof: bool,
}

// SAFETY: like `VideoPlayer`, the decoder owns its FFmpeg allocations
// exclusively and is only ever used by the one thread it is moved into.
unsafe impl Send for ClipDecoder {}

impl ClipDecoder {
    fn open(path: &Path) -> Result<Option<Self>> {
        ffmpeg::init().context("initialising FFmpeg")?;
        let ictx = ffmpeg::format::input(&path).context("opening the video for its sound")?;
        let Some(stream) = ictx.streams().best(ffmpeg::media::Type::Audio) else {
            return Ok(None);
        };
        let stream_index = stream.index();
        let tb = stream.time_base();
        let time_base = f64::from(tb.numerator()) / f64::from(tb.denominator().max(1));
        let start = crate::video::clip_start(&ictx);
        let decoder = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?
            .decoder()
            .audio()
            .context("creating the audio decoder")?;
        log::info!(
            "Clip audio: {} Hz, {} channel(s)",
            decoder.rate(),
            decoder.channels()
        );
        Ok(Some(Self {
            ictx,
            decoder,
            stream_index,
            time_base,
            start,
            resampler: None,
            decoded: ffmpeg::frame::Audio::empty(),
            eof_sent: false,
            eof: false,
        }))
    }

    fn seek(&mut self, secs: f64) -> Result<()> {
        let ts = ((secs.max(0.0) + self.start) * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        self.ictx.seek(ts, ..ts).context("seeking the soundtrack")?;
        self.decoder.flush();
        self.resampler = None;
        self.eof_sent = false;
        self.eof = false;
        Ok(())
    }

    /// Decode until some sound comes out or the file ends, appending it to
    /// `out` at the mix rate. Returns the clip time of the first frame
    /// appended, when the file says.
    fn decode_more(&mut self, out: &mut Vec<Frame>) -> Result<Option<f64>> {
        loop {
            match self.decoder.receive_frame(&mut self.decoded) {
                Ok(()) => {
                    let pts = self
                        .decoded
                        .pts()
                        .map(|p| p as f64 * self.time_base - self.start);
                    self.resample_into(out)?;
                    return Ok(pts);
                }
                Err(ffmpeg::Error::Eof) => {
                    self.eof = true;
                    return Ok(None);
                }
                // Anything else is "send more input".
                Err(_) => {}
            }
            if self.eof_sent {
                self.eof = true;
                return Ok(None);
            }
            if let Some((stream, packet)) = self.ictx.packets().next() {
                if stream.index() == self.stream_index {
                    // A corrupt packet costs a few milliseconds of sound, not
                    // the soundtrack.
                    let _ = self.decoder.send_packet(&packet);
                }
            } else {
                let _ = self.decoder.send_eof();
                self.eof_sent = true;
            }
        }
    }

    fn resample_into(&mut self, out: &mut Vec<Frame>) -> Result<()> {
        use ffmpeg::format::sample::Type as SampleType;
        use ffmpeg::format::Sample;

        let rate = self.decoded.rate().max(1);
        let mut layout = self.decoded.channel_layout();
        if layout.is_empty() {
            // A WAV or other plain PCM often says only how many channels it
            // has. The resampler checks each frame against the layout it was
            // made for, so the frame must say the same, or every frame is
            // refused.
            layout = ffmpeg::ChannelLayout::default(i32::from(self.decoded.channels().max(1)));
            self.decoded.set_channel_layout(layout);
        }
        let format = self.decoded.format();
        let stale = self.resampler.as_ref().is_some_and(|r| {
            let input = r.input();
            input.format != format || input.rate != rate || input.channel_layout != layout
        });
        if stale || self.resampler.is_none() {
            self.resampler = Some(
                ffmpeg::software::resampling::Context::get(
                    format,
                    layout,
                    rate,
                    Sample::F32(SampleType::Packed),
                    ffmpeg::ChannelLayout::STEREO,
                    SAMPLE_RATE,
                )
                .context("creating the audio resampler")?,
            );
        }
        let Some(resampler) = self.resampler.as_mut() else {
            return Ok(());
        };
        let capacity = self.decoded.samples() * SAMPLE_RATE as usize / rate as usize + 256;
        let mut converted = ffmpeg::frame::Audio::new(
            Sample::F32(SampleType::Packed),
            capacity,
            ffmpeg::ChannelLayout::STEREO,
        );
        resampler
            .run(&self.decoded, &mut converted)
            .context("resampling the soundtrack")?;
        let n = converted.samples();
        let bytes = &converted.data(0)[..n * 8];
        for frame in bytes.as_chunks::<8>().0 {
            let l = f32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
            let r = f32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]);
            out.push([l, r]);
        }
        Ok(())
    }
}
