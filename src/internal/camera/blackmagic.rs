//! A Blackmagic input as a camera — an UltraStudio Recorder, a DeckLink card
//! — taken through Blackmagic's own DeckLink API (`crate::decklink::input`)
//! rather than as macOS lists it: the format coming in is followed as the
//! device tells it, its sound comes with its pictures (into the mixer as a
//! live source's, see `VardaApp::live_sounds`), a device with nothing
//! plugged in shows black, not its last picture, and one with no signal on
//! the connection it is set to (SDI, say, its source on HDMI) looks on its
//! others till one comes.

use super::{
    choose, stalled, CallBack, CameraColour, CameraDeviceInfo, CameraMode, Found, Frame, Rate,
    SignalSeen, Sink, RETRY_EVERY,
};
use crate::decklink::input::{
    self, Capture, Colorspace, Connection, InputDevice, Picture, Pixels, Taker,
};
use anyhow::{anyhow, Context, Result};
use nokhwa::utils::{CameraIndex, FrameFormat};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};
use yuvutils_rs::{uyvy422_to_rgba, YuvPackedImage, YuvRange, YuvStandardMatrix};

/// How a Blackmagic input is named among the cameras: "UltraStudio
/// Recorder 3G (Blackmagic)" — told from the same box as macOS lists it.
pub(super) fn camera_name(device: &str) -> String {
    crate::decklink::screen_label(device)
}

/// The Blackmagic inputs, as cameras.
pub(super) fn found() -> Vec<Found> {
    input::input_devices()
        .into_iter()
        .map(|d| Found {
            index: CameraIndex::String(d.unique_id.clone()),
            name: camera_name(&d.name),
            rates: rates_of(&d),
            follows_signal: d.detects,
            unique_id: d.unique_id,
            formats: Vec::new(),
        })
        .collect()
}

/// The formats a device is offered at: whole frames only — opened to
/// follow what comes in, it takes woven fields too.
fn rates_of(device: &InputDevice) -> Vec<Rate> {
    device
        .modes
        .iter()
        .filter(|m| m.progressive && m.fps > 0.0 && m.width > 0 && m.height > 0)
        .map(|m| Rate {
            width: m.width,
            height: m.height,
            format: FrameFormat::YUYV,
            fps: m.fps,
            code: m.code,
        })
        .collect()
}

/// Every how many pictures the signal is looked at (see `Sink::note_signal`).
const SIGNAL_EVERY: u32 = 15;

/// What a picture is, as the log tells it: size, layout, rate, and whether
/// anything is plugged in.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Seen {
    width: u32,
    height: u32,
    pixels: Option<Pixels>,
    /// Hundredths of a frame a second.
    rate: Option<u32>,
    no_signal: bool,
}

/// Pictures with a signal behind them as they come: whether the last had
/// one, and how many have so far.
#[derive(Default)]
struct Signal {
    now: AtomicBool,
    frames: AtomicU64,
}

/// Pictures with a signal that make a connection one with a signal: not a
/// picture or two left from the connection before.
const SETTLE: u64 = 3;

impl Signal {
    fn saw(&self, signal: bool) {
        self.now.store(signal, Ordering::Relaxed);
        if signal {
            self.frames.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The count to tell a signal on the connection it is on from now by.
    fn mark(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    /// A signal on the connection it has been on since `mark`: the last
    /// picture had one, and enough have since.
    fn since(&self, mark: u64) -> bool {
        self.now.load(Ordering::Relaxed) && self.frames.load(Ordering::Relaxed) >= mark + SETTLE
    }
}

/// Takes a device's pictures into its camera's sink, and its sound.
struct Into {
    sink: Arc<Sink>,
    name: String,
    looks: u32,
    seen: Option<Seen>,
    /// The signal behind its pictures.
    signal: Arc<Signal>,
}

impl Into {
    /// Tell the log when what comes in changes — and, its rate changed, the
    /// sink, which times its pictures by it.
    fn note(&mut self, picture: &Picture<'_>) {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let seen = Seen {
            width: picture.width,
            height: picture.height,
            pixels: picture.pixels,
            rate: picture.fps.map(|fps| (fps * 100.0).round() as u32),
            no_signal: picture.no_signal,
        };
        if self.seen == Some(seen) {
            return;
        }
        let before = self.seen.replace(seen);
        if seen.no_signal {
            log::warn!("Camera '{}': no signal plugged in", self.name);
            return;
        }
        log::info!(
            "Camera '{}' frames: {}×{} {} at {}",
            self.name,
            seen.width,
            seen.height,
            match seen.pixels {
                Some(Pixels::Uyvy) => "2vuy",
                Some(Pixels::Bgra) => "BGRA",
                None => "(a layout Nida does not read)",
            },
            picture.fps.map_or_else(
                || "an unsaid rate".to_string(),
                |fps| format!("{fps:.2} fps")
            )
        );
        if let (Some(before), Some(fps)) = (before, picture.fps) {
            if before.rate != seen.rate {
                self.sink.opening(fps);
            }
        }
    }
}

impl Taker for Into {
    fn picture(&mut self, picture: &Picture<'_>) {
        let called = crate::host_clock::host_seconds();
        self.signal.saw(!picture.no_signal);
        self.note(picture);
        let sink = &self.sink;
        let colour = sink.colour();
        let mut data = sink.buffer();
        let fresh = data.capacity() == 0;
        let size = if picture.no_signal {
            black(&mut data, picture.width, picture.height)
        } else {
            read(picture, &mut data, colour)
        };
        let look = self.looks.is_multiple_of(SIGNAL_EVERY);
        self.looks = self.looks.wrapping_add(1);
        if look && !picture.no_signal {
            if let Some((seen, sampled)) = seen_signal(picture, colour) {
                sink.note_signal(seen, sampled);
            }
        }
        match size {
            Some((width, height)) => sink.put_called(
                Frame {
                    data,
                    width,
                    height,
                },
                called,
                Some(CallBack {
                    called,
                    stamped: false,
                    fresh,
                }),
            ),
            None => sink.recycle(data),
        }
    }

    fn sound(&mut self, samples: &[i32], channels: usize) {
        self.sink.put_sound(samples, channels);
    }
}

/// The matrix a picture is read with: as the device says, else by size
/// (SD is Rec. 601) — unless set by hand.
fn matrix_of(picture: &Picture<'_>, colour: CameraColour) -> YuvStandardMatrix {
    let said = match picture.colorspace {
        Colorspace::Rec601 => YuvStandardMatrix::Bt601,
        Colorspace::Rec2020 => YuvStandardMatrix::Bt2020,
        Colorspace::Unsaid if picture.height < 720 => YuvStandardMatrix::Bt601,
        Colorspace::Rec709 | Colorspace::Unsaid => YuvStandardMatrix::Bt709,
    };
    super::picture_matrix(colour, said)
}

/// A picture into RGBA in `out`: its size, or none for one Nida cannot
/// read. A device's YUV is video range unless set by hand.
fn read(picture: &Picture<'_>, out: &mut Vec<u8>, colour: CameraColour) -> Option<(u32, u32)> {
    let (w, h) = (picture.width, picture.height);
    let (width, height) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    if width == 0 || height == 0 || picture.data.len() < picture.row_bytes * height {
        return None;
    }
    out.resize(width * height * 4, 0);
    match picture.pixels? {
        Pixels::Uyvy => {
            let row = width.div_ceil(2) * 4;
            let pixels = super::avf::tight(picture.data, picture.row_bytes, row, height)?;
            let packed = YuvPackedImage {
                yuy: &pixels,
                yuy_stride: u32::try_from(row).ok()?,
                width: w,
                height: h,
            };
            let range = super::picture_range(colour, YuvRange::Limited);
            uyvy422_to_rgba(&packed, out, w * 4, range, matrix_of(picture, colour)).ok()?;
        }
        Pixels::Bgra => {
            let stride = picture.row_bytes;
            if stride < width * 4 {
                return None;
            }
            for (row, dst) in out.chunks_exact_mut(width * 4).enumerate() {
                let src = &picture.data[row * stride..row * stride + width * 4];
                for (d, s) in dst
                    .as_chunks_mut::<4>()
                    .0
                    .iter_mut()
                    .zip(src.as_chunks::<4>().0)
                {
                    *d = [s[2], s[1], s[0], 255];
                }
            }
        }
    }
    if !picture.progressive {
        blend_fields(out, width * 4);
    }
    Some((w, h))
}

/// Two fields woven line by line into one picture, each line pair blended:
/// no combs where things move, at the cost of a little sharpness.
fn blend_fields(rgba: &mut [u8], row: usize) {
    for pair in rgba.chunks_exact_mut(row * 2) {
        let (top, bottom) = pair.split_at_mut(row);
        for (t, b) in top.iter_mut().zip(bottom.iter_mut()) {
            let mean = u8::try_from((u16::from(*t) + u16::from(*b)).div_ceil(2)).unwrap_or(u8::MAX);
            *t = mean;
            *b = mean;
        }
    }
}

/// How long a device goes without a signal before the next of its
/// connections is tried.
const LOOK_EVERY: Duration = Duration::from_secs(2);

/// Whether a capture's pictures have stalled for good: not while a
/// connection switched to within [`LOOK_EVERY`] may still be locking on.
fn stalled_for_good(stalled: bool, switched_at: Option<Instant>, now: Instant) -> bool {
    stalled && switched_at.is_none_or(|at| now.saturating_duration_since(at) >= LOOK_EVERY)
}

/// Whether the device still sends frames — marked as without a signal — so
/// that another connection may be looked on; one that sends none has
/// stalled, and is opened afresh instead.
fn frames_coming(since_frame: Option<Duration>) -> bool {
    since_frame.is_some_and(|quiet| quiet < LOOK_EVERY)
}

/// The connection a device opened afresh is to be on: `remembered` — the
/// one it had a signal on just before it stopped — else, no signal since it
/// last moved (perhaps a box that sends no frames without one, its source
/// elsewhere), the next after the one `last_tried`: so it comes round to
/// every one.
fn connection_on_reopen(
    remembered: Option<Connection>,
    offered: &[Connection],
    last_tried: Option<Connection>,
) -> Option<Connection> {
    remembered
        .filter(|on| offered.contains(on))
        .or_else(|| next_connection(offered, last_tried))
        .or(last_tried)
}

/// The connection a device opened afresh is to be on: `keeping` — the one
/// it was on, opened again only for the format that came in there — else
/// as [`connection_on_reopen`] says.
fn connection_after_reopen(
    keeping: Option<Connection>,
    remembered: Option<Connection>,
    offered: &[Connection],
    last_tried: Option<Connection>,
) -> Option<Connection> {
    keeping
        .filter(|on| offered.contains(on))
        .or_else(|| connection_on_reopen(remembered, offered, last_tried))
}

/// The connection to look on next after `now`, round the device's own; none
/// for a device with only one.
fn next_connection(offered: &[Connection], now: Option<Connection>) -> Option<Connection> {
    if offered.len() < 2 {
        return None;
    }
    let at = now.and_then(|c| offered.iter().position(|o| *o == c));
    Some(offered[at.map_or(0, |i| (i + 1) % offered.len())])
}

/// A black picture of the size the device is at, in `out`.
fn black(out: &mut Vec<u8>, w: u32, h: u32) -> Option<(u32, u32)> {
    let pixels = usize::try_from(w).ok()? * usize::try_from(h).ok()?;
    if pixels == 0 {
        return None;
    }
    out.clear();
    out.extend(std::iter::repeat_n([0, 0, 0, 255], pixels).flatten());
    Some((w, h))
}

/// What a picture says of the signal (see `Sink::note_signal`).
fn seen_signal(picture: &Picture<'_>, colour: CameraColour) -> Option<(SignalSeen, (u32, u32))> {
    let (width, height) = (
        usize::try_from(picture.width).ok()?,
        usize::try_from(picture.height).ok()?,
    );
    let yuv = picture.pixels? == Pixels::Uyvy;
    let sampled = if yuv && picture.data.len() >= picture.row_bytes * height {
        super::below_black(picture.data, picture.row_bytes, (width, height), (1, 2))
    } else {
        (0, 0)
    };
    let seen = SignalSeen {
        width: picture.width,
        height: picture.height,
        format: if yuv { "2vuy" } else { "BGRA" }.into(),
        matrix: super::avf::matrix_label(matrix_of(picture, colour)),
        full_range: yuv.then(|| super::picture_range(colour, YuvRange::Limited) == YuvRange::Full),
        ..SignalSeen::default()
    };
    Some((seen, sampled))
}

/// The device of `unique_id` as discovery has it now.
fn plugged_in(unique_id: &str) -> Option<InputDevice> {
    input::input_devices()
        .into_iter()
        .find(|d| d.unique_id == unique_id)
}

/// Open the device at `mode` — its best when none, or not offered: the
/// capture, its format, and the connections it takes video in on.
/// The format to open a device at once the one that came in differs from the
/// one it was opened at (`opened`): that one, when it is on offer — whole
/// frames; woven fields are taken as they come.
fn detected_rate(detected: Option<u32>, opened: u32, rates: &[Rate]) -> Option<Rate> {
    let code = detected.filter(|code| *code != opened)?;
    rates.iter().find(|r| r.code == code).copied()
}

/// What opening a device gave: its capture, the format, the connections it
/// takes video in on, and the formats on offer.
type Opened = (Capture, Rate, Vec<Connection>, Vec<Rate>);

fn open(
    device: &CameraDeviceInfo,
    mode: Option<CameraMode>,
    prefer: Option<u32>,
    sink: &Arc<Sink>,
    signal: &Arc<Signal>,
) -> Result<Opened> {
    let name = &device.name;
    let found =
        plugged_in(&device.unique_id).with_context(|| format!("'{name}' is not connected"))?;
    let rates = rates_of(&found);
    // The format that came in last, when it is on offer; else as asked.
    let rate = prefer
        .and_then(|code| rates.iter().find(|r| r.code == code).copied())
        .or_else(|| choose(&rates, mode))
        .with_context(|| format!("'{name}' offers no picture Nida can read"))?;
    sink.opening(rate.fps);
    signal.now.store(false, Ordering::Relaxed);
    let taker = Into {
        sink: Arc::clone(sink),
        name: name.clone(),
        looks: 0,
        seen: None,
        signal: Arc::clone(signal),
    };
    let capture = Capture::open(found.arrival, rate.code, Box::new(taker))
        .map_err(|e| anyhow!("'{name}' did not open: {e}"))?;
    Ok((capture, rate, found.connections, rates))
}

/// A Blackmagic input's thread: opens it, says how, then keeps it open —
/// opened again when its device comes back, its pictures stall, or another
/// format is asked for — until told to stop.
pub(super) fn run(
    device: &CameraDeviceInfo,
    mode: Option<CameraMode>,
    sink: &Arc<Sink>,
    stop: &AtomicBool,
    opened: &mpsc::Sender<Result<CameraMode>>,
) {
    let name = &device.name;
    let mut mode = mode;
    let signal = Arc::new(Signal::default());
    let (mut capture, mut connections, mut rates, mut opened_code) =
        match open(device, mode, None, sink, &signal) {
            Ok((capture, rate, connections, rates)) => {
                log::info!(
                    "Camera '{name}' opened through Blackmagic's API at {}, on {}",
                    rate.label(),
                    capture
                        .connection()
                        .map_or("its connection", Connection::label)
                );
                let _ = opened.send(Ok(rate.mode()));
                (Some(capture), connections, rates, rate.code)
            }
            Err(e) => {
                let _ = opened.send(Err(e));
                return;
            }
        };
    let mut started = Instant::now();
    let mut retry_at = started;
    // Since when it has had no signal on the connection it is on.
    let mut quiet_since: Option<Instant> = None;
    let mut looked = false;
    // When it last switched connection; the one it last tried, and whether a
    // signal has come since; the one it found a signal on — set again when
    // it is opened afresh, should the box have gone back.
    let mut switched_at: Option<Instant> = None;
    let mut last_tried = capture.as_ref().and_then(Capture::connection);
    let mut had_signal = false;
    let mut found_on: Option<Connection> = None;
    // The signal's count when it last opened or moved.
    let mut mark = signal.mark();
    // The format that came in last, opened at from then on.
    let mut prefer: Option<u32> = None;
    // Opened again for the format that came in: on the connection it was
    // on, which had it.
    let mut keep_on: Option<Connection> = None;
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(Duration::from_millis(50));
        let now = Instant::now();
        if let Some(c) = capture.as_ref() {
            if signal.since(mark) {
                if std::mem::take(&mut looked) {
                    let on = c.connection().map_or("a connection", Connection::label);
                    log::info!("Camera '{name}': a signal on {on}");
                }
                found_on = c.connection();
                had_signal = true;
                quiet_since = None;
            } else {
                let since = *quiet_since.get_or_insert(now);
                if now - since >= LOOK_EVERY && frames_coming(sink.since_last_frame()) {
                    let on = c.connection();
                    if let Some(next) = next_connection(&connections, on) {
                        if c.set_connection(next) {
                            log::info!(
                                "Camera '{name}': no signal on {} — looking on {}",
                                on.map_or("its connection", Connection::label),
                                next.label()
                            );
                            looked = true;
                            switched_at = Some(now);
                            last_tried = Some(next);
                            had_signal = false;
                            mark = signal.mark();
                        }
                    }
                    quiet_since = Some(now);
                }
            }
        }
        // The format that came in differs from the one opened at: opened
        // again at it, as though it had been chosen — the box left in the
        // format it was opened at, its picture switched within, stuttered on
        // air where a box opened at the signal's own format did not (an M5
        // Max, 2026-10-29).
        if let Some(rate) = capture
            .as_ref()
            .and_then(|c| detected_rate(c.detected_mode(), opened_code, &rates))
        {
            log::info!(
                "Camera '{name}': the signal is {} — opening it again at that",
                rate.label()
            );
            prefer = Some(rate.code);
            keep_on = capture.as_ref().and_then(Capture::connection);
            capture = None;
            retry_at = now;
        }
        if sink.take_restart() {
            retry_at = now;
            if let Some(asked) = sink.take_mode() {
                mode = asked;
                prefer = None;
            }
            if capture.take().is_some() {
                log::info!("Camera '{name}' reset: opening it again");
            }
        }
        let lost = capture.as_ref().is_none_or(|c| {
            !c.alive()
                || stalled_for_good(
                    stalled(now - started, sink.since_last_frame(), sink.stall_limit()),
                    switched_at,
                    now,
                )
        });
        if !lost || now < retry_at {
            continue;
        }
        if capture.take().is_some() {
            log::warn!("Camera '{name}' stopped: trying it again");
            sink.note_stop();
        }
        retry_at = now + RETRY_EVERY;
        match open(device, mode, prefer, sink, &signal) {
            Ok((c, rate, offered, on_offer)) => {
                opened_code = rate.code;
                rates = on_offer;
                log::info!("Camera '{name}' is back, at {}", rate.label());
                started = Instant::now();
                quiet_since = None;
                connections = offered;
                let now_on = c.connection();
                // No signal since it last moved: what it found before is no
                // longer where the source is.
                if !had_signal {
                    found_on = None;
                }
                let remembered = found_on;
                let keeping = keep_on.take();
                let to = connection_after_reopen(
                    keeping,
                    remembered,
                    &connections,
                    last_tried.or(now_on),
                );
                had_signal = false;
                last_tried = now_on;
                mark = signal.mark();
                if let Some(on) = to.filter(|on| Some(*on) != now_on) {
                    if c.set_connection(on) {
                        if keeping.is_none() && remembered.is_none() {
                            log::info!(
                                "Camera '{name}': no signal on {} — looking on {}",
                                now_on.map_or("its connection", Connection::label),
                                on.label()
                            );
                        }
                        switched_at = Some(started);
                        last_tried = Some(on);
                        mark = signal.mark();
                    }
                }
                capture = Some(c);
            }
            Err(e) => log::debug!("Camera '{name}' is not back yet: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(
        data: &[u8],
        row_bytes: usize,
        pixels: Pixels,
        colorspace: Colorspace,
    ) -> Picture<'_> {
        Picture {
            data,
            row_bytes,
            width: 2,
            height: 2,
            pixels: Some(pixels),
            colorspace,
            no_signal: false,
            progressive: true,
            fps: Some(50.0),
        }
    }

    /// A device's YUV is read at video range: its black (16) is black, its
    /// white (235) white, rows padded or not.
    #[test]
    fn a_devices_yuv_is_read_at_video_range() {
        // Two rows of U Y V Y U... with 4 bytes of padding each.
        let row = [128, 16, 128, 235, 0, 0, 0, 0];
        let data: Vec<u8> = row.iter().chain(row.iter()).copied().collect();
        let mut out = Vec::new();
        let p = picture(&data, 8, Pixels::Uyvy, Colorspace::Rec709);
        assert_eq!(read(&p, &mut out, CameraColour::default()), Some((2, 2)));
        assert!(
            out[0] < 3 && out[1] < 3 && out[2] < 3,
            "black: {:?}",
            &out[..4]
        );
        assert!(
            out[4] > 252 && out[5] > 252 && out[6] > 252,
            "white: {:?}",
            &out[4..8]
        );
        assert_eq!(out[3], 255);
    }

    /// RGB from the device comes as BGRA, and is turned the right way.
    #[test]
    fn rgb_from_the_device_is_turned_into_rgba() {
        let data = [1, 2, 3, 0, 4, 5, 6, 0, 7, 8, 9, 0, 10, 11, 12, 0];
        let mut out = Vec::new();
        let p = picture(&data, 8, Pixels::Bgra, Colorspace::Unsaid);
        assert_eq!(read(&p, &mut out, CameraColour::default()), Some((2, 2)));
        assert_eq!(&out[..8], &[3, 2, 1, 255, 6, 5, 4, 255]);
    }

    /// Two fields woven are blended line pair by line pair: no combs.
    #[test]
    fn woven_fields_are_blended() {
        // Two rows of two BGRA pixels: white over black.
        let data = [255, 255, 255, 0, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        let mut out = Vec::new();
        let mut p = picture(&data, 8, Pixels::Bgra, Colorspace::Unsaid);
        p.progressive = false;
        assert_eq!(read(&p, &mut out, CameraColour::default()), Some((2, 2)));
        assert_eq!(&out[..4], &[128, 128, 128, 255]);
        assert_eq!(&out[8..12], &[128, 128, 128, 255]);
    }

    /// With no signal, the next connection is looked on, round the device's
    /// own; a device with one is left as it is.
    #[test]
    fn the_next_connection_is_looked_on_round_the_devices_own() {
        let both = [Connection::Sdi, Connection::Hdmi];
        assert_eq!(
            next_connection(&both, Some(Connection::Sdi)),
            Some(Connection::Hdmi)
        );
        assert_eq!(
            next_connection(&both, Some(Connection::Hdmi)),
            Some(Connection::Sdi)
        );
        assert_eq!(next_connection(&both, None), Some(Connection::Sdi));
        assert_eq!(
            next_connection(&[Connection::Hdmi], Some(Connection::Hdmi)),
            None
        );
    }

    /// Pictures stalled count as lost, but not within a connection switch's
    /// time to lock on.
    #[test]
    fn a_stall_just_after_a_connection_switch_is_waited_out() {
        let now = Instant::now();
        assert!(stalled_for_good(true, None, now));
        assert!(!stalled_for_good(
            true,
            Some(now),
            now + Duration::from_millis(500)
        ));
        assert!(stalled_for_good(true, Some(now), now + LOOK_EVERY));
        assert!(!stalled_for_good(false, None, now));
    }

    /// Another connection is looked on only while the device still sends
    /// frames: one gone silent is opened afresh instead.
    #[test]
    fn connections_are_looked_on_only_while_frames_come() {
        assert!(frames_coming(Some(Duration::from_millis(20))));
        assert!(!frames_coming(Some(LOOK_EVERY)));
        assert!(!frames_coming(None));
    }

    /// Opened afresh, a device goes back to the connection it had a signal
    /// on just before; else it moves on from the one it last tried, so it
    /// comes round to every one.
    #[test]
    fn a_device_opened_afresh_goes_back_or_moves_on() {
        let three = [Connection::Sdi, Connection::Hdmi, Connection::OpticalSdi];
        let (sdi, hdmi, optical) = (
            Some(Connection::Sdi),
            Some(Connection::Hdmi),
            Some(Connection::OpticalSdi),
        );
        assert_eq!(connection_on_reopen(hdmi, &three, optical), hdmi, "back");
        assert_eq!(
            connection_on_reopen(None, &three, sdi),
            hdmi,
            "on to the next"
        );
        assert_eq!(
            connection_on_reopen(None, &three, hdmi),
            optical,
            "and the next"
        );
        assert_eq!(connection_on_reopen(None, &three, optical), sdi, "round");
        let one = [Connection::Hdmi];
        assert_eq!(connection_on_reopen(None, &one, hdmi), hdmi, "its only one");
    }

    /// A connection has a signal only once a few pictures with one have come
    /// since it was moved to — not one left from the connection before.
    #[test]
    fn a_signal_counts_once_it_has_settled() {
        let signal = Signal::default();
        signal.saw(true);
        let mark = signal.mark();
        assert!(!signal.since(mark), "none yet on this one");
        signal.saw(true);
        signal.saw(true);
        assert!(!signal.since(mark), "two");
        signal.saw(true);
        assert!(signal.since(mark), "three");
        signal.saw(false);
        assert!(!signal.since(mark), "gone");
    }

    /// Opened again only for the format that came in, a device stays on the
    /// connection it was on — not moved on for want of a settled signal.
    #[test]
    fn a_format_reopen_keeps_its_connection() {
        let both = [Connection::Sdi, Connection::Hdmi];
        let hdmi = Some(Connection::Hdmi);
        assert_eq!(connection_after_reopen(hdmi, None, &both, hdmi), hdmi);
        assert_eq!(
            connection_after_reopen(None, None, &both, hdmi),
            Some(Connection::Sdi),
            "else moved on, as before"
        );
    }

    /// The format that came in, differing from the one opened and on offer,
    /// is the one to open at; the same, unknown, or not on offer (woven
    /// fields), none.
    #[test]
    fn the_format_that_came_in_is_opened_at() {
        let rate = |code, fps| Rate {
            width: 1920,
            height: 1080,
            format: FrameFormat::YUYV,
            fps,
            code,
        };
        let rates = [rate(1, 60.0), rate(2, 50.0)];
        assert_eq!(detected_rate(Some(2), 1, &rates).map(|r| r.code), Some(2));
        assert_eq!(detected_rate(Some(1), 1, &rates), None, "as opened");
        assert_eq!(detected_rate(None, 1, &rates), None, "none told");
        assert_eq!(detected_rate(Some(9), 1, &rates), None, "not on offer");
    }

    /// Nothing plugged in shows black, not whatever the device holds.
    #[test]
    fn no_signal_shows_black() {
        let mut out = vec![9; 3];
        assert_eq!(black(&mut out, 2, 1), Some((2, 1)));
        assert_eq!(out, [0, 0, 0, 255, 0, 0, 0, 255]);
    }

    /// SD said nothing of is read as Rec. 601, HD as Rec. 709; what the
    /// device says wins.
    #[test]
    fn the_matrix_is_as_said_else_by_size() {
        let data = [0u8; 16];
        let mut p = picture(&data, 8, Pixels::Uyvy, Colorspace::Unsaid);
        p.height = 576;
        assert_eq!(
            matrix_of(&p, CameraColour::default()),
            YuvStandardMatrix::Bt601
        );
        p.height = 1080;
        assert_eq!(
            matrix_of(&p, CameraColour::default()),
            YuvStandardMatrix::Bt709
        );
        p.colorspace = Colorspace::Rec2020;
        assert_eq!(
            matrix_of(&p, CameraColour::default()),
            YuvStandardMatrix::Bt2020
        );
    }

    /// Only whole-frame formats are offered; woven fields come in only by
    /// following what the device is sent.
    #[test]
    fn only_whole_frame_formats_are_offered() {
        let mode = |name: &str, progressive, fps| input::InputMode {
            name: name.into(),
            width: 1920,
            height: 1080,
            fps,
            progressive,
            code: 1,
        };
        let device = InputDevice {
            name: "UltraStudio Recorder 3G".into(),
            unique_id: "decklink:1".into(),
            arrival: 1,
            detects: true,
            connections: vec![Connection::Sdi, Connection::Hdmi],
            modes: vec![mode("1080i50", false, 25.0), mode("1080p50", true, 50.0)],
        };
        let rates = rates_of(&device);
        assert_eq!(rates.len(), 1);
        assert!((rates[0].fps - 50.0).abs() < 1e-9);
    }
}
