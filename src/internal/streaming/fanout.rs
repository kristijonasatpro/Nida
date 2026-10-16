//! One encoder's output copied to every destination's sender.
//!
//! The encoder writes MPEG-TS to its stdout. A reader thread cuts it into
//! whole 188-byte packets and offers each piece to every destination without
//! waiting: a destination whose upload has fallen behind loses pieces (and
//! says so) instead of holding up the encoder or the others. MPEG-TS is what
//! makes a destination that reconnects mid-stream work: its sender picks the
//! picture up at the next keyframe, as a TV tuner does.
//!
//! Each destination has a supervisor thread that runs its sender — an ffmpeg
//! copying the stream into FLV and pushing it over RTMP — watches its
//! progress, and starts it again after a pause when it stops, until the
//! destination is switched off or the stream stopped.

use std::io::{BufRead, Read, Write};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use super::ts::TsScan;
use super::{explain, DestinationState, DestinationStatus, RelayTarget, StreamDestination};
use crate::renderer::subprocess::StderrTail;

/// An MPEG-TS packet: pieces are cut on these so a dropped piece never leaves
/// half a packet behind.
const TS_PACKET: usize = 188;
/// The least a destination may hold however low the bit rate.
const MIN_BUFFER_BYTES: u64 = 1024 * 1024;
/// Pauses between reconnect attempts; the last repeats.
const BACKOFF: [u64; 5] = [1, 2, 4, 8, 10];
/// Live this long and a later drop starts the pauses from the first again.
const STABLE: Duration = Duration::from_secs(30);
/// No progress for this long while connected: the connection is dead even if
/// the socket has not noticed, so the sender is restarted.
const STALLED: Duration = Duration::from_secs(20);
/// Progress older than this while connected is reported as struggling.
const LAGGING: Duration = Duration::from_secs(3);
/// Nothing gone out for this long since the network changed under a
/// connection: the way it went is gone, and it is made again at once (see
/// [`super::network`]).
const NETWORK_QUIET: Duration = Duration::from_secs(2);
/// Still going out this long after the network changed — at the least, and
/// more than [`SOCKET_MOST`] gone out since: the way it uses still works.
const NETWORK_TRUST: Duration = Duration::from_secs(6);
/// The most a connection's socket may take in, as the system grows it
/// (macOS and Linux go up to 4 MB by default): a connection whose way is
/// gone goes on seeming to send until that is full. The system's own
/// sizing is left alone — one set by hand would hold a healthy connection
/// far away to less than it carries.
const SOCKET_MOST: u64 = 8 * 1024 * 1024;
/// Stream time sent over the recent window must reach this share of the
/// wall time, or the destination is struggling.
const REAL_TIME: f64 = 0.85;
/// The window rate and bit rate are measured over.
const WINDOW: Duration = Duration::from_secs(4);
/// A dropped piece keeps the destination marked struggling this long.
const DROP_MEMORY: Duration = Duration::from_secs(5);

type Piece = Arc<[u8]>;

/// A piece waiting for a destination's sender, and the time it holds at the
/// rate it was made at, in µs: thrown away later — on a reconnect — it is
/// worth that, whatever the rate is by then.
struct Queued {
    piece: Piece,
    us: u64,
}

impl std::ops::Deref for Queued {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.piece
    }
}

/// Each connection's number, never used twice: a destination reconnected or
/// replaced is a new one to whoever watches its figures.
static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

/// What a struggling destination's message tells the operator to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Remedy {
    /// The bit rate is the operator's to lower. Its settings are locked while
    /// live, so it says to stop first; a short dip passes by itself (the
    /// network buffer), so only if it goes on. The bit rate, not the size: a
    /// smaller picture is still sent at the bit rate set.
    #[default]
    LowerByHand,
    /// The bit rate is the operator's to lower, and it changes while live:
    /// the stream is encoded in Nida.
    LowerLive,
    /// Nida is lowering it.
    Adapting,
    /// Nida has lowered it as far as it goes.
    AtFloor,
}

impl Remedy {
    fn text(self) -> &'static str {
        match self {
            Self::LowerByHand => "If it goes on, stop, lower the Video bit rate, and start again.",
            Self::LowerLive => {
                "If it goes on, lower the Video bit rate in the Stream window: it changes while live."
            }
            Self::Adapting => "Nida is lowering the bit rate to fit.",
            Self::AtFloor => {
                "The bit rate is as low as it goes, and the upload still cannot carry it."
            }
        }
    }
}

/// The encoder's output, sent on to every destination.
pub struct Fanout {
    shared: Arc<Shared>,
    reader: Option<std::thread::JoinHandle<()>>,
}

struct Shared {
    slots: Mutex<Vec<Option<Runner>>>,
    /// Bytes of stream a destination may hold before it drops pieces.
    limit: AtomicUsize,
    /// How long (µs) a sender reads the stream to learn what is in it.
    probe_us: u64,
    /// The stream's bit rate now, sound and picture: what a lost piece's
    /// bytes were worth in time.
    rate_bps: AtomicU64,
    /// Where the video rate is read from instead, as the encoder took it,
    /// and the sound's rate beside it (see [`Fanout::reckon_at`]).
    rate_from: Mutex<Option<(Arc<AtomicU32>, u32)>>,
    /// The encoder's output as it came: when, and the bytes so far — what
    /// the stream is at, sound and picture (see [`Fanout::input_kbps`]).
    came: Mutex<std::collections::VecDeque<(Instant, u64)>>,
    remedy: Mutex<Remedy>,
}

/// One destination's running sender.
struct Runner {
    target: RelayTarget,
    tx: crossbeam_channel::Sender<Queued>,
    /// Bytes waiting in `tx` for the sender.
    queued: Arc<AtomicUsize>,
    state: Arc<RunnerState>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

#[derive(Default)]
struct RunnerState {
    inner: Mutex<Progress>,
    /// Set while a sender is connected and should be keeping up; pieces
    /// dropped outside it are not the upload's fault.
    sending: AtomicBool,
    dropped: AtomicU64,
    /// The bytes of those pieces.
    dropped_bytes: AtomicU64,
    /// The time those bytes held, in µs, at the rate they were made at.
    dropped_us: AtomicU64,
    /// Where it is in the stream, and the counts it has had (see [`Flow`]).
    flow: Mutex<Flow>,
    /// This connection's number (see [`NEXT_GENERATION`]).
    generation: AtomicU64,
    /// The limit it keeps, past the stream's, while it drains what it held
    /// when the limit was lowered (see [`Fanout::set_buffer`]); 0 when none.
    carry: AtomicUsize,
    /// It has been live since it was switched on: what is thrown away from
    /// then on — while it reconnects too — is stream its viewers miss.
    been_live: AtomicBool,
    /// The count of network changes when its connection began (see
    /// [`super::network::epoch`]).
    network: AtomicU64,
    /// The stream's rate pieces were last offered at, in bit/s: what the
    /// stream it throws away on reconnecting was worth in time.
    rate_bps: AtomicU64,
}

/// A destination's place in the stream when it falls too far behind.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
enum Skip {
    /// Sending as it comes.
    #[default]
    No,
    /// Fallen too far behind: still sending what finishes the units under
    /// way on the PIDs `open` — past the limit by `over` bytes so far — then
    /// skipping.
    Finishing { open: Vec<u16>, over: usize },
    /// Waiting for a keyframe to resume at — and on connecting, so what goes
    /// out starts whole.
    ToKeyframe,
}

/// The most a destination finishing its units may go past its limit.
const FINISH_MAX: usize = 2 * 1024 * 1024;

/// A destination's place in the stream: whether it takes what comes, and
/// each PID's continuity count it got last — carried on through a skip, so
/// it never sees one jump.
#[derive(Debug, Default)]
struct Flow {
    skip: Skip,
    counts: std::collections::HashMap<u16, u8>,
}

#[derive(Default)]
struct Progress {
    phase: Phase,
    message: Option<String>,
    detail: Option<String>,
    reconnects: u32,
    /// When the sender first reported stream time going out.
    live_since: Option<Instant>,
    /// When stream time last moved on.
    last_advance: Option<Instant>,
    /// Recent (when, stream time µs, bytes sent) reports.
    samples: std::collections::VecDeque<(Instant, i64, u64)>,
    last_drop: Option<Instant>,
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Phase {
    #[default]
    Connecting,
    Running,
    Waiting,
}

impl Fanout {
    /// Start reading `encoder` and sending to every destination that is on
    /// and can be sent to. A destination whose upload slows may fall up to
    /// `buffer_secs` behind — at `bytes_per_sec`, the stream's rate — and
    /// catch up afterwards before anything is lost; `keyframe_secs` is how
    /// long a sender joining mid-stream may wait for a keyframe.
    ///
    /// # Panics
    ///
    /// Panics if the reader thread cannot be spawned.
    pub fn start(
        encoder: ChildStdout,
        destinations: &[StreamDestination],
        bytes_per_sec: u64,
        keyframe_secs: u32,
        buffer_secs: u32,
    ) -> Self {
        let shared = Arc::new(Shared {
            slots: Mutex::new((0..destinations.len()).map(|_| None).collect()),
            limit: AtomicUsize::new(buffer_limit(bytes_per_sec, buffer_secs)),
            // ffmpeg reads five seconds of MPEG-TS before it starts by
            // default, since streams can appear in it at any time; a keyframe
            // and a second is enough to learn the picture and the sound.
            probe_us: (u64::from(keyframe_secs) + 1) * 1_000_000,
            rate_bps: AtomicU64::new(bytes_per_sec * 8),
            rate_from: Mutex::new(None),
            came: Mutex::new(std::collections::VecDeque::new()),
            remedy: Mutex::new(Remedy::default()),
        });
        let fanout = Self {
            shared: shared.clone(),
            reader: Some(
                std::thread::Builder::new()
                    .name("stream-fanout".into())
                    .spawn(move || read_encoder(encoder, &shared))
                    .expect("spawn the stream fan-out thread"),
            ),
        };
        for (index, dest) in destinations.iter().enumerate() {
            fanout.set_destination(index, dest);
        }
        fanout
    }

    /// Apply a destination's settings while live: start it, stop it, or —
    /// when where it sends has changed — reconnect it. Unchanged, it is left
    /// alone.
    pub fn set_destination(&self, index: usize, dest: &StreamDestination) {
        let want = if dest.enabled {
            dest.relay_target().ok()
        } else {
            None
        };
        let mut slots = self
            .shared
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(slot) = slots.get_mut(index) else {
            return;
        };
        if slot.as_ref().map(|r| &r.target) == want.as_ref() {
            return;
        }
        if let Some(old) = slot.take() {
            old.stop();
        }
        let rate_bps = self.shared.reckoned_bps();
        *slot = want.map(|target| Runner::start(index, target, self.shared.probe_us, rate_bps));
    }

    /// How each destination is doing, in order. A destination without a
    /// sender reads as off; the caller knows which of those are invalid.
    pub fn status(&self) -> Vec<DestinationStatus> {
        let remedy = *self
            .shared
            .remedy
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let slots = self
            .shared
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        slots
            .iter()
            .map(|slot| {
                slot.as_ref().map_or_else(DestinationStatus::off, |r| {
                    let mut status = r.state.status(remedy);
                    status.backlog_bytes = r.queued.load(Ordering::Relaxed) as u64;
                    status
                })
            })
            .collect()
    }

    /// Reckon lost pieces in time at the video rate `video` (kbit/s) holds —
    /// the rate an encoder in Nida last took, whoever changed it — and the
    /// sound's `audio_kbps` beside it, from now on.
    pub fn reckon_at(&self, video: Arc<AtomicU32>, audio_kbps: u32) {
        *self
            .shared
            .rate_from
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some((video, audio_kbps));
    }

    /// What the stream is at, sound and picture, in kbit/s: the encoder's
    /// output over the last [`WINDOW`], as MPEG-TS — what goes out a little
    /// smaller. Held Variable or Average it is what the picture asks for,
    /// not the rate set. `None` until it has half a window's figures, or
    /// once the encoder has sent nothing for one.
    pub fn input_kbps(&self) -> Option<u32> {
        self.shared.measured_kbps()
    }

    /// The stream's rate a lost piece is reckoned at now, sound and
    /// picture, in kbit/s.
    pub fn reckoned_kbps(&self) -> u32 {
        u32::try_from(self.shared.reckoned_bps() / 1000).unwrap_or(u32::MAX)
    }

    /// The stream's rate set while live, `bytes_per_sec` sound and picture:
    /// each destination may hold `buffer_secs` of it from now on. Lowered, a
    /// destination holding more than that keeps the limit it had until what
    /// it holds drains under the new one: stream it held fairly is not lost
    /// for the operator's lowering of the rate.
    pub fn set_buffer(&self, bytes_per_sec: u64, buffer_secs: u32) {
        let limit = buffer_limit(bytes_per_sec, buffer_secs);
        // Under the lock pieces are offered under: none is offered against
        // the new limit before each destination has what it carries.
        let slots = self.shared.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let before = self.shared.limit.load(Ordering::Relaxed);
        for runner in slots.iter().flatten() {
            let carry = &runner.state.carry;
            carry.store(carried(before, limit, carry.load(Ordering::Relaxed)), Ordering::Relaxed);
        }
        self.shared.limit.store(limit, Ordering::Relaxed);
    }

    /// What a struggling destination tells the operator to do.
    pub fn set_remedy(&self, remedy: Remedy) {
        *self
            .shared
            .remedy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = remedy;
    }

    /// Stop every sender. Each is given a moment to send what it has and
    /// close the connection properly; nothing here waits for that.
    pub fn stop(&mut self) {
        let runners: Vec<Runner> = self
            .shared
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter_mut()
            .filter_map(Option::take)
            .collect();
        for runner in runners {
            runner.stop();
        }
        // The reader ends by itself when the encoder exits and its output
        // closes; it holds no sender open.
        self.reader.take();
    }
}

impl Drop for Fanout {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Copy the encoder's output to every destination until it ends.
impl Shared {
    /// The stream's rate a lost piece is reckoned at now, in bit/s: what the
    /// encoder's output is at, as measured — the bytes lost are its bytes,
    /// and held Variable or Average it is not the rate set — else the rate
    /// the encoder took, where it says (see [`Fanout::reckon_at`]), else the
    /// rate it was started at.
    fn reckoned_bps(&self) -> u64 {
        if let Some(kbps) = self.measured_kbps() {
            return u64::from(kbps.max(1)) * 1000;
        }
        self.rate_from
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(
                || self.rate_bps.load(Ordering::Relaxed),
                |(video, audio)| u64::from(video.load(Ordering::Relaxed) + audio) * 1000,
            )
    }

    /// `bytes` came from the encoder `now`. After a silence of a window or
    /// more the measure starts afresh: spanning the silence it read a
    /// megabit stream as twenty kilobits, and a piece lost then as seconds.
    fn came_in(&self, bytes: usize, now: Instant) {
        let mut came = self.came.lock().unwrap_or_else(PoisonError::into_inner);
        if came.back().is_some_and(|&(at, _)| now.duration_since(at) >= WINDOW) {
            came.clear();
        }
        let total = came.back().map_or(0, |&(_, b)| b) + bytes as u64;
        came.push_back((now, total));
        while came.len() > 1 && came.get(1).is_some_and(|&(at, _)| now.duration_since(at) >= WINDOW) {
            came.pop_front();
        }
    }

    /// What the encoder's output is at, in kbit/s, over the last [`WINDOW`]:
    /// `None` until it has half a window's figures, or once it has sent
    /// nothing for one.
    fn measured_kbps(&self) -> Option<u32> {
        let came = self.came.lock().unwrap_or_else(PoisonError::into_inner);
        let (&(first, from), &(last, to)) = (came.front()?, came.back()?);
        let span = last.duration_since(first);
        let fresh = last.elapsed() < WINDOW;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Small, positive.
        (fresh && span >= WINDOW / 2)
            .then(|| ((to - from) as f64 * 8.0 / span.as_secs_f64() / 1000.0) as u32)
    }
}

/// The limit a destination keeps past a new one `limit`, the stream's
/// having been `before` and its own past that `carry`: the larger it had,
/// until it drains (see `offer`) or a new limit covers it — a rate lowered,
/// raised part way back, or set again, takes nothing it holds.
fn carried(before: usize, limit: usize, carry: usize) -> usize {
    let kept = if limit < before { before.max(carry) } else { carry };
    if kept > limit {
        kept
    } else {
        0
    }
}

/// Bytes a destination may hold: `buffer_secs` of a stream of
/// `bytes_per_sec`, and never under [`MIN_BUFFER_BYTES`].
fn buffer_limit(bytes_per_sec: u64, buffer_secs: u32) -> usize {
    usize::try_from((bytes_per_sec * u64::from(buffer_secs)).max(MIN_BUFFER_BYTES))
        .unwrap_or(usize::MAX)
}

fn read_encoder(mut encoder: ChildStdout, shared: &Shared) {
    let mut buf = vec![0u8; 64 * 1024];
    let mut pending: Vec<u8> = Vec::with_capacity(128 * 1024);
    let mut scan = TsScan::default();
    loop {
        let n = match encoder.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        pending.extend_from_slice(&buf[..n]);
        shared.came_in(n, Instant::now());
        let whole = pending.len() / TS_PACKET * TS_PACKET;
        if whole == 0 {
            continue;
        }
        let piece: Piece = pending.drain(..whole).collect::<Vec<u8>>().into();
        let keyframe = scan.scan(&piece).map(|at| Resume {
            at,
            tables: scan.tables(),
        });
        let rate_bps = shared.reckoned_bps();
        let slots = shared.slots.lock().unwrap_or_else(PoisonError::into_inner);
        for runner in slots.iter().flatten() {
            offer(
                (&runner.tx, &runner.queued, &runner.state),
                &piece,
                (keyframe.as_ref(), scan.streams()),
                shared.limit.load(Ordering::Relaxed),
                rate_bps,
            );
        }
    }
}

/// Where in a piece a keyframe starts, and the stream's tables to send
/// ahead of it for a destination resuming there.
struct Resume {
    at: usize,
    tables: Option<Vec<u8>>,
}

/// Queue a piece for one destination. One that already holds `limit` bytes
/// — its upload that far behind — sends only what finishes the frame and the
/// sound under way, loses the rest and every piece after until a keyframe
/// starts, and resumes there whole: thrown away piece by piece, the picture
/// broke up until the next keyframe, and a frame cut off halfway came out
/// broken. A destination connecting starts at a keyframe the same way.
/// `streams` are the stream's PIDs, as its tables say.
fn offer(
    (tx, queued, state): (&crossbeam_channel::Sender<Queued>, &AtomicUsize, &RunnerState),
    piece: &Piece,
    (keyframe, streams): (Option<&Resume>, &[u16]),
    limit: usize,
    rate_bps: u64,
) {
    state.rate_bps.store(rate_bps, Ordering::Relaxed);
    let mut flow = state.flow.lock().unwrap_or_else(PoisonError::into_inner);
    let Flow { skip, counts } = &mut *flow;
    let mut send = |piece: Piece| {
        let piece: Piece = super::ts::renumber(&piece, counts).map_or(piece, Into::into);
        let len = piece.len();
        let us = len as u64 * 8 * 1_000_000 / rate_bps.max(1);
        // Counted before it is sent: the sender takes it off as it writes
        // it, and the other way round the count could wrap below zero.
        queued.fetch_add(len, Ordering::Relaxed);
        if tx.send(Queued { piece, us }).is_err() {
            queued.fetch_sub(len, Ordering::Relaxed);
        }
    };
    // Under a limit lowered while live, the one it had until what it holds
    // drains under the new one.
    let carry = state.carry.load(Ordering::Relaxed);
    let limit = if carry == 0 {
        limit
    } else if queued.load(Ordering::Relaxed) + piece.len() > limit {
        limit.max(carry)
    } else {
        state.carry.store(0, Ordering::Relaxed);
        limit
    };
    let mut piece = piece.clone();
    match skip {
        Skip::Finishing { open, over } => {
            let kept = super::ts::finish_units(&piece, open);
            if *over + kept.len() > FINISH_MAX {
                // More than may go past the limit: lost too.
                state.note_drop(piece.len(), rate_bps);
                *skip = Skip::ToKeyframe;
                return;
            }
            state.note_drop(piece.len() - kept.len(), rate_bps);
            *over += kept.len();
            if !kept.is_empty() {
                send(kept.into());
            }
            if open.is_empty() {
                *skip = Skip::ToKeyframe;
            }
            return;
        }
        Skip::ToKeyframe => {
            let Some((at, tables)) = keyframe.and_then(|r| Some((r.at, r.tables.as_deref()?)))
            else {
                state.note_drop(piece.len(), rate_bps);
                return;
            };
            state.note_drop(at, rate_bps);
            piece = [tables, &piece[at..]].concat().into();
        }
        Skip::No => {}
    }
    if queued.load(Ordering::Relaxed) + piece.len() > limit {
        if *skip == Skip::No && state.sending.load(Ordering::Relaxed) {
            log::warn!("Stream destination fell too far behind; skipping to the next keyframe");
        }
        if *skip == Skip::ToKeyframe {
            // The keyframe does not fit either: wait for the next.
            state.note_drop(piece.len(), rate_bps);
            return;
        }
        let mut open = streams.to_vec();
        let kept = super::ts::finish_units(&piece, &mut open);
        if kept.len() > FINISH_MAX {
            state.note_drop(piece.len(), rate_bps);
            *skip = Skip::ToKeyframe;
            return;
        }
        let over = kept.len();
        state.note_drop(piece.len() - over, rate_bps);
        if over > 0 {
            send(kept.into());
        }
        *skip = if open.is_empty() {
            Skip::ToKeyframe
        } else {
            Skip::Finishing { open, over }
        };
        return;
    }
    *skip = Skip::No;
    send(piece);
}

impl Runner {
    fn start(index: usize, target: RelayTarget, probe_us: u64, rate_bps: u64) -> Self {
        // Unbounded: the limit is in bytes, kept by `offer`.
        let (tx, rx) = crossbeam_channel::unbounded();
        let queued = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(RunnerState::default());
        // The stream's rate until pieces say it.
        state.rate_bps.store(rate_bps, Ordering::Relaxed);
        // What it sends starts at a keyframe.
        state.flow.lock().unwrap_or_else(PoisonError::into_inner).skip = Skip::ToKeyframe;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (target, state, stop) = (target.clone(), state.clone(), stop.clone());
            let queued = queued.clone();
            std::thread::Builder::new()
                .name(format!("stream-dest-{}", index + 1))
                .spawn(move || supervise(&target, probe_us, (&rx, &queued), &state, &stop))
                .ok()
        };
        if thread.is_none() {
            state.lock().message = Some("Could not start the sender".into());
        }
        Self {
            target,
            tx,
            queued,
            state,
            stop,
            thread,
        }
    }

    /// Tell the supervisor to finish; it closes its sender and reaps it on
    /// its own thread.
    fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        self.thread.take();
    }
}

impl RunnerState {
    fn lock(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A piece of `bytes` thrown away, at a stream of `rate_bps`: counted, and
    /// its time with it, once the destination has been live — what is thrown
    /// away while it first connects is the start's, not stream its viewers
    /// miss. Only thrown away while sending is the upload's doing, and
    /// marks it struggling.
    fn note_drop(&self, bytes: usize, rate_bps: u64) {
        let sending = self.sending.load(Ordering::Relaxed);
        if self.note_lost(1, bytes, rate_bps) && sending {
            self.lock().last_drop = Some(Instant::now());
        }
    }

    /// `pieces` of `bytes` in all thrown away at a stream of `rate_bps`,
    /// counted once the destination has been live: whether they were.
    fn note_lost(&self, pieces: u64, bytes: usize, rate_bps: u64) -> bool {
        self.note_lost_us(pieces, bytes, bytes as u64 * 8 * 1_000_000 / rate_bps.max(1))
    }

    /// `pieces` of `bytes` in all, holding `us` of stream, thrown away:
    /// counted once the destination has been live — whether they were.
    fn note_lost_us(&self, pieces: u64, bytes: usize, us: u64) -> bool {
        let counted = bytes > 0
            && (self.sending.load(Ordering::Relaxed) || self.been_live.load(Ordering::Relaxed));
        if counted {
            self.dropped.fetch_add(pieces, Ordering::Relaxed);
            self.dropped_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
            self.dropped_us.fetch_add(us, Ordering::Relaxed);
        }
        counted
    }

    fn status(&self, remedy: Remedy) -> DestinationStatus {
        let now = Instant::now();
        let p = self.lock();
        let window: Vec<&(Instant, i64, u64)> = p
            .samples
            .iter()
            .filter(|(at, ..)| now.duration_since(*at) <= WINDOW)
            .collect();
        let (rate, kbps) = match (window.first(), window.last()) {
            (Some(a), Some(b)) if b.0 > a.0 => {
                let wall = b.0.duration_since(a.0).as_secs_f64();
                #[allow(clippy::cast_precision_loss)]
                let sent = (b.1 - a.1) as f64 / 1e6;
                #[allow(clippy::cast_precision_loss)]
                let bits = b.2.saturating_sub(a.2) as f64 * 8.0;
                (Some(sent / wall), bits / wall / 1000.0)
            }
            _ => (None, 0.0),
        };
        let lagging = p
            .last_advance
            .is_some_and(|at| now.duration_since(at) > LAGGING);
        let slow = rate.is_some_and(|r| r < REAL_TIME)
            && window
                .first()
                .is_some_and(|a| now.duration_since(a.0) >= WINDOW / 2);
        let dropping = p
            .last_drop
            .is_some_and(|at| now.duration_since(at) < DROP_MEMORY);
        let (state, message) = match p.phase {
            Phase::Waiting => (DestinationState::Reconnecting, p.message.clone()),
            Phase::Connecting => (DestinationState::Connecting, p.message.clone()),
            Phase::Running if p.live_since.is_none() => (DestinationState::Connecting, None),
            Phase::Running if lagging => (
                DestinationState::Struggling,
                Some("Nothing is getting out — the connection has stalled".into()),
            ),
            // What to do too: the operator doubted the bare verdict over a
            // phone's 5G hotspot, and it was right.
            Phase::Running if dropping => (
                DestinationState::Struggling,
                Some(format!(
                    "The upload is too slow — parts of the stream are lost. {}",
                    remedy.text()
                )),
            ),
            Phase::Running if slow => (
                DestinationState::Struggling,
                Some(format!(
                    "Sending slower than real time — the upload is too slow. {}",
                    remedy.text()
                )),
            ),
            Phase::Running => (DestinationState::Live, None),
        };
        #[allow(clippy::cast_possible_truncation)]
        DestinationStatus {
            state,
            kbps: kbps as f32,
            detail: if message.is_some() {
                p.detail.clone()
            } else {
                None
            },
            message,
            reconnects: p.reconnects,
            dropped: self.dropped.load(Ordering::Relaxed),
            dropped_bytes: self.dropped_bytes.load(Ordering::Relaxed),
            #[allow(clippy::cast_precision_loss)] // Seconds, to the µs.
            dropped_secs: self.dropped_us.load(Ordering::Relaxed) as f32 / 1e6,
            live_secs: p
                .live_since
                .map_or(0.0, |at| now.duration_since(at).as_secs_f32()),
            backlog_bytes: 0,
            generation: self.generation.load(Ordering::Relaxed),
            network: self.network.load(Ordering::Relaxed),
        }
    }
}

/// Run one destination's sender until told to stop, starting it again after
/// a pause whenever it stops by itself.
fn supervise(
    target: &RelayTarget,
    probe_us: u64,
    (rx, queued): (&crossbeam_channel::Receiver<Queued>, &Arc<AtomicUsize>),
    state: &RunnerState,
    stop: &AtomicBool,
) {
    let mut attempt = 0usize;
    while !stop.load(Ordering::SeqCst) {
        // What piled up while it was down is seconds old; start from now —
        // at a keyframe, so what it sends starts whole. Done under the lock
        // the pieces are offered under, so none slips in between.
        {
            let mut flow = state.flow.lock().unwrap_or_else(PoisonError::into_inner);
            drain(rx, queued, state);
            // A new sender reads a new stream: counts start again too.
            *flow = Flow {
                skip: Skip::ToKeyframe,
                counts: std::collections::HashMap::new(),
            };
        }
        {
            let mut p = state.lock();
            p.phase = Phase::Connecting;
            p.live_since = None;
            p.last_advance = None;
            p.samples.clear();
            // A new connection: its figures start afresh. Its number and the
            // network it goes out on, under the lock its status is read
            // under, so the two are read together.
            state
                .network
                .store(super::network::epoch_for(&target.url), Ordering::Relaxed);
            state
                .generation
                .store(NEXT_GENERATION.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
        }
        let outcome = run_sender(target, probe_us, (rx, queued), state, stop);
        state.sending.store(false, Ordering::Relaxed);
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let stable = state
            .lock()
            .live_since
            .is_some_and(|at| at.elapsed() >= STABLE);
        if stable {
            attempt = 0;
        }
        // The network changed under it: a new connection goes out the new
        // way at once — there is nothing to wait for.
        let moved = matches!(outcome, Some(Stopped::NetworkChanged));
        let pause = if moved {
            Duration::ZERO
        } else {
            let pause = Duration::from_secs(BACKOFF[attempt.min(BACKOFF.len() - 1)]);
            attempt += 1;
            pause
        };
        let line = outcome.map_or_else(|| "The sender stopped".into(), Stopped::into_line);
        {
            let mut p = state.lock();
            p.phase = Phase::Waiting;
            p.live_since = None;
            p.reconnects += 1;
            p.message = Some(explain(&line));
            p.detail = Some(line.clone());
        }
        log::warn!(
            "Stream to {} stopped: {line}; trying again in {} s",
            target.display,
            pause.as_secs()
        );
        // A change of network while it waits: no reason to wait on.
        let until = Instant::now() + pause;
        let network = state.network.load(Ordering::Relaxed);
        while Instant::now() < until
            && !stop.load(Ordering::SeqCst)
            && super::network::epoch_for(&target.url) == network
        {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    state.lock().phase = Phase::Waiting;
}

/// Throw away what waits for a sender about to start again: it piled up
/// while the last one was down, and is seconds old. Once the destination
/// has been live it is stream its viewers miss, and counted so — each piece
/// at the time it held when it was made.
fn drain(rx: &crossbeam_channel::Receiver<Queued>, queued: &AtomicUsize, state: &RunnerState) {
    let (mut pieces, mut bytes, mut us) = (0, 0, 0);
    while let Ok(waiting) = rx.try_recv() {
        queued.fetch_sub(waiting.len(), Ordering::Relaxed);
        pieces += 1;
        bytes += waiting.len();
        us += waiting.us;
    }
    state.note_lost_us(pieces, bytes, us);
}

/// The sender's command: MPEG-TS in on stdin, FLV out to the server, and
/// progress reports on stdout.
fn sender_command(target: &RelayTarget, probe_us: u64) -> Command {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-nostats", "-loglevel", "warning"])
        .args(["-progress", "pipe:1", "-stats_period", "0.5"])
        .args(["-analyzeduration", &probe_us.to_string()])
        .args(["-f", "mpegts", "-i", "pipe:0"])
        .args(["-map", "0", "-c", "copy"])
        // MPEG-TS carries no AAC setup, so FLV's first sequence header would
        // go out empty and the real one a packet later; a server that takes
        // the first falls back to FLV's fixed "44 kHz" and plays 48 kHz sound
        // 8 % low. `aac_seq_header_detect` writes a full one from the start.
        .args([
            "-f",
            "flv",
            "-flvflags",
            "no_duration_filesize+aac_seq_header_detect",
        ])
        // A write stuck this long (µs) is a dead connection: fail, reconnect.
        .args(["-rw_timeout", "10000000"])
        .args(&target.args)
        .arg(&target.url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd
}

/// Run the sender once, returning why if it stopped by itself.
fn run_sender(
    target: &RelayTarget,
    probe_us: u64,
    (rx, queued): (&crossbeam_channel::Receiver<Queued>, &Arc<AtomicUsize>),
    state: &RunnerState,
    stop: &AtomicBool,
) -> Option<Stopped> {
    let mut child = match sender_command(target, probe_us).spawn() {
        Ok(child) => child,
        Err(e) => return Some(Stopped::Said(format!("Could not start ffmpeg: {e}"))),
    };
    log::info!("Stream sending to {}", target.display);
    let tail = StderrTail::attach(&mut child);
    let done = Arc::new(AtomicBool::new(false));
    let writer = child.stdin.take().map(|mut stdin| {
        let (rx, done, queued) = (rx.clone(), done.clone(), queued.clone());
        std::thread::spawn(move || loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(waiting) => {
                    queued.fetch_sub(waiting.len(), Ordering::Relaxed);
                    if stdin.write_all(&waiting).is_err() {
                        return;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                    if done.load(Ordering::SeqCst) {
                        return;
                    }
                }
                Err(crossbeam_channel::RecvTimeoutError::Disconnected) => return,
            }
        })
    });
    // `state` outlives this call, but the reader thread needs it owned; it
    // reports through a channel instead.
    let (report_tx, report_rx) = crossbeam_channel::unbounded::<(i64, u64)>();
    let reader = child
        .stdout
        .take()
        .map(|stdout| std::thread::spawn(move || read_progress(stdout, &report_tx)));

    let mut stopping_since: Option<Instant> = None;
    let mut watch = NetworkWatch::new(state.network.load(Ordering::Relaxed));
    let end = loop {
        for (out_us, bytes) in report_rx.try_iter() {
            record_progress(state, out_us, bytes);
        }
        if stop.load(Ordering::SeqCst) {
            // Close its input so it sends what it has and hangs up properly;
            // give it two seconds before cutting it off.
            done.store(true, Ordering::SeqCst);
            let since = *stopping_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_secs(2) {
                kill(&mut child);
            }
        }
        match child.try_wait() {
            Ok(Some(_)) | Err(_) if stop.load(Ordering::SeqCst) => break End::Stopped,
            Ok(Some(_)) | Err(_) => break End::Exited,
            Ok(None) => {}
        }
        let stalled = state
            .lock()
            .last_advance
            .is_some_and(|at| at.elapsed() > STALLED);
        if stalled && !stop.load(Ordering::SeqCst) {
            break End::Stalled;
        }
        let (last_advance, written) = {
            let p = state.lock();
            (p.last_advance, p.samples.back().map_or(0, |&(_, _, bytes)| bytes))
        };
        let epoch = super::network::epoch_for(&target.url);
        if watch.gone(epoch, last_advance, written, Instant::now()) && !stop.load(Ordering::SeqCst) {
            break End::NetworkChanged;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    done.store(true, Ordering::SeqCst);
    kill(&mut child);
    if let Some(w) = writer {
        let _ = w.join();
    }
    if let Some(r) = reader {
        let _ = r.join();
    }
    match end {
        End::Stopped => None,
        End::Exited => {
            // Let the stderr reader take in the final lines first.
            std::thread::sleep(Duration::from_millis(100));
            Some(Stopped::Said(
                tail.last_error()
                    .unwrap_or_else(|| "The sender stopped".into()),
            ))
        }
        End::Stalled => Some(Stopped::Said(format!(
            "Nothing went out for {} s — reconnecting",
            STALLED.as_secs()
        ))),
        End::NetworkChanged => Some(Stopped::NetworkChanged),
    }
}

/// Why a sender's run ended.
enum End {
    /// The destination was switched off or the stream stopped.
    Stopped,
    /// ffmpeg quit by itself: refused, disconnected, failed.
    Exited,
    /// Connected but nothing went out for [`STALLED`].
    Stalled,
    /// The network changed under the connection, and nothing went out after.
    NetworkChanged,
}

/// A sender's view of the network under its connection (see
/// [`super::network`]).
struct NetworkWatch {
    /// The count of changes it trusts: when it connected, or the latest it
    /// went on sending through.
    trusted: u64,
    /// The latest count seen, since when, and the bytes it had written then.
    latest: u64,
    since: Option<Instant>,
    written_then: u64,
}

impl NetworkWatch {
    fn new(network: u64) -> Self {
        Self {
            trusted: network,
            latest: network,
            since: None,
            written_then: 0,
        }
    }

    /// Whether its connection is gone, the count of network changes being
    /// `epoch`, its last progress `last_advance` and the bytes it has written
    /// `written`: changed since it was trusted, and nothing out for
    /// [`NETWORK_QUIET`] — or nothing ever. One that has written more since
    /// the latest change than its socket could take in without sending (see
    /// [`SOCKET_MOST`]), [`NETWORK_TRUST`] at the least, goes a way that
    /// works, and that count is trusted: what it wrote went somewhere,
    /// however slow its stream.
    fn gone(&mut self, epoch: u64, last_advance: Option<Instant>, written: u64, now: Instant) -> bool {
        if epoch == self.trusted {
            return false;
        }
        if epoch != self.latest || self.since.is_none() {
            // Each change starts its own wait, and its own count.
            self.latest = epoch;
            self.since = Some(now);
            self.written_then = written;
        }
        if last_advance.is_none_or(|at| now.duration_since(at) > NETWORK_QUIET) {
            return true;
        }
        let long_enough = self
            .since
            .is_some_and(|at| now.duration_since(at) > NETWORK_TRUST);
        if long_enough && written.saturating_sub(self.written_then) > SOCKET_MOST {
            self.trusted = epoch;
            self.since = None;
        }
        false
    }
}

/// Why a sender stopped by itself.
enum Stopped {
    /// ffmpeg's last error line, or what was seen.
    Said(String),
    /// The network changed under its connection (see [`End::NetworkChanged`]).
    NetworkChanged,
}

impl Stopped {
    /// What it says, for the log and the destination's detail.
    fn into_line(self) -> String {
        match self {
            Self::Said(line) => line,
            Self::NetworkChanged => {
                "The network changed under the connection — reconnecting at once".into()
            }
        }
    }
}

fn kill(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// Parse ffmpeg's `-progress` blocks: `key=value` lines, each block ending
/// in `progress=continue`. Reports stream time sent (µs) and bytes sent.
fn read_progress(stdout: impl Read, reports: &crossbeam_channel::Sender<(i64, u64)>) {
    let mut out_us: Option<i64> = None;
    let mut bytes: u64 = 0;
    for line in std::io::BufReader::new(stdout).lines() {
        let Ok(line) = line else { return };
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "out_time_us" => out_us = value.trim().parse().ok(),
            "total_size" => bytes = value.trim().parse().unwrap_or(bytes),
            "progress" => {
                if let Some(us) = out_us {
                    if reports.send((us, bytes)).is_err() {
                        return;
                    }
                }
            }
            _ => {}
        }
    }
}

fn record_progress(state: &RunnerState, out_us: i64, bytes: u64) {
    let now = Instant::now();
    let mut p = state.lock();
    let advanced = p.samples.back().is_none_or(|&(_, prev, _)| out_us > prev);
    if out_us > 0 && advanced {
        p.last_advance = Some(now);
        if p.live_since.is_none() {
            p.live_since = Some(now);
            p.message = None;
            p.detail = None;
            state.sending.store(true, Ordering::Relaxed);
            state.been_live.store(true, Ordering::Relaxed);
            log::info!("Stream destination is live");
        }
    }
    p.phase = Phase::Running;
    p.samples.push_back((now, out_us, bytes));
    while p
        .samples
        .front()
        .is_some_and(|(at, ..)| now.duration_since(*at) > WINDOW * 2)
    {
        p.samples.pop_front();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(state: &RunnerState, at_ms: u64, out_ms: i64, bytes: u64, base: Instant) {
        let now = base + Duration::from_millis(at_ms);
        let mut p = state.lock();
        if out_ms > 0 && p.live_since.is_none() {
            p.live_since = Some(now);
            state.sending.store(true, Ordering::Relaxed);
        }
        if p.samples
            .back()
            .is_none_or(|&(_, prev, _)| out_ms * 1000 > prev)
        {
            p.last_advance = Some(now);
        }
        p.phase = Phase::Running;
        p.samples.push_back((now, out_ms * 1000, bytes));
    }

    /// Reports that ended just now, spaced half a second apart.
    fn recent(state: &RunnerState, reports: &[(i64, u64)]) {
        let span = 500 * (reports.len() as u64 - 1);
        let base = Instant::now()
            .checked_sub(Duration::from_millis(span))
            .unwrap();
        for (i, &(out_ms, bytes)) in reports.iter().enumerate() {
            progress(state, 500 * i as u64, out_ms, bytes, base);
        }
    }

    #[test]
    fn keeping_up_in_real_time_is_live_with_its_bit_rate() {
        let state = RunnerState::default();
        // 6 Mbit/s: 375 000 bytes every half second.
        let reports: Vec<(i64, u64)> = (0..7)
            .map(|i| (1000 + i * 500, 375_000 * i as u64))
            .collect();
        recent(&state, &reports);
        let s = state.status(Remedy::LowerByHand);
        assert_eq!(s.state, DestinationState::Live);
        assert!((s.kbps - 6000.0).abs() < 50.0, "{}", s.kbps);
    }

    #[test]
    fn falling_behind_real_time_is_struggling() {
        let state = RunnerState::default();
        // Stream time moves 250 ms per half second of wall time.
        let reports: Vec<(i64, u64)> = (0..7).map(|i| (1000 + i * 250, 100 * i as u64)).collect();
        recent(&state, &reports);
        let s = state.status(Remedy::LowerByHand);
        assert_eq!(s.state, DestinationState::Struggling);
        assert!(s.message.unwrap().contains("slower"));
    }

    #[test]
    fn a_stall_is_struggling_and_a_drop_is_remembered() {
        let state = RunnerState::default();
        let base = Instant::now().checked_sub(Duration::from_secs(10)).unwrap();
        progress(&state, 0, 1000, 0, base);
        progress(&state, 500, 1500, 10, base);
        assert_eq!(
            state.status(Remedy::LowerByHand).state,
            DestinationState::Struggling,
            "no progress for 9 s"
        );

        let fresh = RunnerState::default();
        recent(
            &fresh,
            &[(1000, 0), (1500, 1), (2000, 2), (2500, 3), (3000, 4)],
        );
        assert_eq!(fresh.status(Remedy::LowerByHand).state, DestinationState::Live);
        fresh.note_drop(4096, 8_000_000);
        fresh.note_drop(1000, 8_000_000);
        let s = fresh.status(Remedy::LowerByHand);
        assert_eq!(s.state, DestinationState::Struggling);
        assert_eq!(s.dropped, 2);
        assert_eq!(s.dropped_bytes, 5096, "their bytes counted too");
        assert!(
            s.message.as_deref().is_some_and(|m| m.contains("lower the Video bit rate")),
            "and what to do said: {:?}",
            s.message
        );
    }

    #[test]
    fn nothing_sent_yet_is_connecting_and_drops_then_do_not_count() {
        let state = RunnerState::default();
        assert_eq!(state.status(Remedy::LowerByHand).state, DestinationState::Connecting);
        state.note_drop(4096, 8_000_000);
        assert_eq!(
            (state.status(Remedy::LowerByHand).dropped, state.status(Remedy::LowerByHand).dropped_bytes),
            (0, 0),
            "not the upload's fault before it is up"
        );
    }

    #[test]
    fn a_destination_holds_its_buffer_in_bytes_then_drops() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let queued = AtomicUsize::new(0);
        let state = RunnerState::default();
        state.sending.store(true, Ordering::Relaxed);
        let piece: Piece = vec![0u8; 400].into();
        for _ in 0..3 {
            offer((&tx, &queued, &state), &piece, (None, &[]), 1000, 8_000_000);
        }
        assert_eq!(
            rx.len(),
            2,
            "two pieces fit in 1000 bytes, the third is dropped"
        );
        assert_eq!(state.dropped.load(Ordering::Relaxed), 1);
        assert_eq!(queued.load(Ordering::Relaxed), 800, "the bytes waiting counted");
        // What comes after waits for a keyframe (see below).
    }

    /// The rate lowered while live lowers the limit: a destination holding
    /// more than the new one keeps the one it had until what it holds drains
    /// under the new one — the stream it held fairly is not lost — and the
    /// new one holds from then on.
    #[test]
    fn a_limit_lowered_while_live_loses_nothing_held() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let queued = AtomicUsize::new(0);
        let state = RunnerState::default();
        state.sending.store(true, Ordering::Relaxed);
        let piece: Piece = vec![0u8; 400].into();
        for _ in 0..2 {
            offer((&tx, &queued, &state), &piece, (None, &[]), 1000, 8_000_000);
        }
        // Lowered to 500 while 800 wait: it keeps its 1000 meanwhile.
        state.carry.store(1000, Ordering::Relaxed);
        offer((&tx, &queued, &state), &piece, (None, &[]), 500, 8_000_000);
        assert_eq!(rx.len(), 2, "over the old limit too: dropped");
        let dropped = state.dropped.load(Ordering::Relaxed);
        assert_eq!(dropped, 1);
        // The sender drains it; a piece fits under the old limit — kept.
        while let Ok(sent) = rx.try_recv() {
            queued.fetch_sub(sent.len(), Ordering::Relaxed);
            if queued.load(Ordering::Relaxed) <= 400 {
                break;
            }
        }
        assert_eq!(queued.load(Ordering::Relaxed), 400);
        *state.flow.lock().unwrap() = Flow::default();
        offer((&tx, &queued, &state), &piece, (None, &[]), 500, 8_000_000);
        assert_eq!(queued.load(Ordering::Relaxed), 800, "held under the old limit");
        assert_eq!(state.dropped.load(Ordering::Relaxed), dropped, "nothing more lost");
        // Drained under the new limit, the new one holds.
        while let Ok(sent) = rx.try_recv() {
            queued.fetch_sub(sent.len(), Ordering::Relaxed);
        }
        offer((&tx, &queued, &state), &piece, (None, &[]), 500, 8_000_000);
        assert_eq!(state.carry.load(Ordering::Relaxed), 0, "the new limit from now on");
        offer((&tx, &queued, &state), &piece, (None, &[]), 500, 8_000_000);
        assert_eq!(rx.len(), 1, "a second piece is over the new limit");
    }

    /// Lowered, a destination keeps the larger limit it had, until a new
    /// limit covers it: raised part way back or set again, it still keeps
    /// it; raised past it, the new one is the only one.
    #[test]
    fn a_lowered_limit_is_carried_until_a_new_one_covers_it() {
        assert_eq!(carried(1000, 500, 0), 1000);
        assert_eq!(carried(800, 500, 1000), 1000, "lowered twice: the first it had");
        assert_eq!(carried(500, 700, 1000), 1000, "raised part way back");
        assert_eq!(carried(500, 500, 1000), 1000, "set again");
        assert_eq!(carried(500, 2000, 1000), 0, "raised past it");
        assert_eq!(carried(500, 500, 0), 0, "nothing to carry");
    }

    /// What waits for a sender starting again is thrown away: stream the
    /// viewers miss once the destination has been live, counted with the
    /// time each piece held when it was made — before that, only the
    /// start's. Nothing marks it struggling: the upload did not lose it.
    #[test]
    fn what_waited_through_a_reconnect_is_counted_lost() {
        for been_live in [false, true] {
            let (tx, rx) = crossbeam_channel::unbounded();
            let queued = AtomicUsize::new(0);
            let state = RunnerState::default();
            state.been_live.store(been_live, Ordering::Relaxed);
            state.rate_bps.store(1_000_000, Ordering::Relaxed);
            // Each piece the time it held when it was made: the first two
            // before the rate was cut to half.
            for us in [10_000, 10_000, 20_000] {
                let piece: Piece = vec![0u8; 1250].into();
                queued.fetch_add(piece.len(), Ordering::Relaxed);
                tx.send(Queued { piece, us }).unwrap();
            }
            drain(&rx, &queued, &state);
            assert_eq!(queued.load(Ordering::Relaxed), 0);
            assert!(rx.is_empty());
            let counted = if been_live { (3, 3750, 40_000) } else { (0, 0, 0) };
            assert_eq!(
                (
                    state.dropped.load(Ordering::Relaxed),
                    state.dropped_bytes.load(Ordering::Relaxed),
                    state.dropped_us.load(Ordering::Relaxed),
                ),
                counted,
                "been live: {been_live}"
            );
            assert!(state.lock().last_drop.is_none(), "not the upload's doing");
        }
    }

    /// A connection quiet after a change of network is gone. One going on
    /// sending through a change is trusted once it has written more than a
    /// socket could take in — not merely after a while: a slow stream's few
    /// hundred kilobits a second never fill one — and a second change
    /// starts its own wait and its own count.
    #[test]
    fn a_connection_quiet_after_a_change_of_network_is_gone() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        // A megabyte a second going out.
        let fast = |ms: u64| ms * 1000;
        let mut watch = NetworkWatch::new(0);
        assert!(!watch.gone(0, Some(at(0)), 0, at(100)), "no change");
        assert!(!watch.gone(1, Some(at(900)), fast(900), at(1000)), "changed, still going out");
        assert!(watch.gone(1, Some(at(900)), fast(900), at(3000)), "quiet two seconds after: gone");
        assert!(NetworkWatch::new(0).gone(1, None, 0, at(0)), "never went out: gone");

        let mut watch = NetworkWatch::new(0);
        for ms in (1000..=9000).step_by(500) {
            assert!(!watch.gone(1, Some(at(ms)), fast(ms), at(ms)), "going on through it at {ms}");
        }
        assert!(watch.gone(1, Some(at(9000)), fast(9000), at(11_100)), "8 MB since: not yet trusted");
        let mut watch = NetworkWatch::new(0);
        for ms in (1000..=9500).step_by(500) {
            assert!(!watch.gone(1, Some(at(ms)), fast(ms), at(ms)));
        }
        assert!(!watch.gone(1, Some(at(9500)), fast(9500), at(12_000)), "8.5 MB since: trusted");

        // A black picture's 250 kbps: seeming to send for a minute earns
        // nothing — a socket could hold it all.
        let slow = |ms: u64| ms * 31;
        let mut watch = NetworkWatch::new(0);
        for ms in (0..=60_000).step_by(500) {
            assert!(!watch.gone(1, Some(at(ms)), slow(ms), at(ms)));
        }
        assert!(watch.gone(1, Some(at(60_000)), slow(60_000), at(62_500)), "quiet after a minute: gone");

        // A second change just as the first would have been trusted: its
        // own wait and count, so the way dying with it is still caught.
        let mut watch = NetworkWatch::new(0);
        for ms in (0..=8000).step_by(500) {
            assert!(!watch.gone(1, Some(at(ms)), fast(ms), at(ms)));
        }
        assert!(!watch.gone(2, Some(at(8100)), fast(8100), at(8100)), "a second change");
        assert!(!watch.gone(2, Some(at(8600)), fast(8600), at(8600)), "not yet trusted");
        assert!(watch.gone(2, Some(at(8600)), fast(8600), at(10_700)), "quiet after the second: gone");
    }

    /// The sender is given no socket size: the system's own.
    #[test]
    fn the_system_sizes_a_senders_socket() {
        let target = RelayTarget {
            url: "rtmp://127.0.0.1:1935/live/key".into(),
            args: Vec::new(),
            display: "rtmp://127.0.0.1:1935/live".into(),
        };
        let args: Vec<String> = sender_command(&target, 3_000_000)
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.iter().any(|a| a.contains("buffer_size")), "{args:?}");
    }

    /// A lost piece is reckoned at what the stream is at, as measured — a
    /// slide held Variable at a megabit, not the four the rate is set to —
    /// and at the rate the encoder took until there is a measure, or once
    /// the encoder has gone quiet.
    #[test]
    fn losses_are_reckoned_at_what_the_stream_is_at() {
        let shared = Shared {
            slots: Mutex::new(Vec::new()),
            limit: AtomicUsize::new(1 << 20),
            probe_us: 0,
            rate_bps: AtomicU64::new(4_160_000),
            rate_from: Mutex::new(Some((Arc::new(AtomicU32::new(4000)), 160))),
            came: Mutex::new(std::collections::VecDeque::new()),
            remedy: Mutex::new(Remedy::default()),
        };
        assert_eq!(shared.reckoned_bps(), 4_160_000, "no measure yet: the rate the encoder took");
        let now = Instant::now();
        {
            let mut came = shared.came.lock().unwrap();
            // A megabit a second for three seconds, up to now.
            for tenth in 0..=30u64 {
                let at = now.checked_sub(Duration::from_millis(3000 - tenth * 100)).unwrap();
                came.push_back((at, tenth * 12_500));
            }
        }
        let measured = shared.reckoned_bps();
        assert!((990_000..=1_010_000).contains(&measured), "measured: {measured}");
        {
            let mut came = shared.came.lock().unwrap();
            for sample in came.iter_mut() {
                sample.0 = sample.0.checked_sub(WINDOW).unwrap();
            }
        }
        assert_eq!(shared.reckoned_bps(), 4_160_000, "gone quiet: the rate the encoder took");
        // Back after five seconds' silence: measured afresh, from the
        // first bytes back — not across the silence.
        let back = Instant::now();
        shared.came_in(12_500, back);
        assert_eq!(shared.reckoned_bps(), 4_160_000, "one sample back: no measure yet");
        for tenth in 1..=25u64 {
            shared.came_in(12_500, back + Duration::from_millis(tenth * 100));
        }
        let kbps = shared.measured_kbps().expect("measured again");
        assert!((990..=1010).contains(&kbps), "a megabit, not twenty kilobits: {kbps}");
    }

    /// Packets with their continuity counts left out, to compare what they
    /// carry.
    fn uncounted(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        for packet in out.chunks_mut(TS_PACKET) {
            packet[3] &= 0xF0;
        }
        out
    }

    /// A piece of the stream as the encoder writes it: the tables, then a
    /// keyframe's packets or another frame's.
    fn ts_piece(tables: bool, key: bool) -> Piece {
        use super::super::ts::tests::{packet, pat, pmt};
        let mut piece = Vec::new();
        if tables {
            piece.extend(pat(0x1000));
            piece.extend(pmt(0x1000, 0x101, 0x100));
        }
        piece.extend(packet(0x101, true, true, &[0xA]));
        piece.extend(packet(0x100, true, key, &[0xB]));
        piece.extend(packet(0x100, false, false, &[0xC]));
        piece.into()
    }

    /// A destination fallen as far behind as it may loses the piece, and
    /// every one after until a keyframe starts — where it resumes, after the
    /// stream's tables, so it starts whole. Its lost time is reckoned at the
    /// rate the stream had.
    #[test]
    fn a_destination_too_far_behind_resumes_at_a_keyframe() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let queued = AtomicUsize::new(0);
        let state = RunnerState::default();
        state.sending.store(true, Ordering::Relaxed);
        let mut scan = TsScan::default();
        let mut send = |piece: &Piece, queued: &AtomicUsize| {
            let keyframe = scan.scan(piece).map(|at| Resume {
                at,
                tables: scan.tables(),
            });
            // 1 Mbit/s: a 188-byte packet is 1.504 ms.
            offer((&tx, queued, &state), piece, (keyframe.as_ref(), scan.streams()), 1000, 1_000_000);
        };
        let first = ts_piece(true, true);
        send(&first, &queued);
        assert_eq!(rx.len(), 1, "room for the first piece");
        // Full: this one is lost, and the next, a frame not a keyframe.
        send(&ts_piece(false, false), &queued);
        let taken = rx.recv().unwrap();
        queued.fetch_sub(taken.len(), Ordering::Relaxed);
        send(&ts_piece(false, false), &queued);
        assert_eq!(rx.len(), 0, "nothing until a keyframe, though there is room");
        // A keyframe: resumed from it, the tables first.
        send(&ts_piece(false, true), &queued);
        let resumed = rx.recv().expect("resumed at the keyframe");
        assert_eq!(
            uncounted(&resumed[..2 * TS_PACKET]),
            uncounted(&first[..2 * TS_PACKET]),
            "the tables first"
        );
        assert_eq!(resumed[3] & 0x0F, 1, "counted on from the tables sent before");
        let first_after = &resumed[2 * TS_PACKET..3 * TS_PACKET];
        assert_eq!(
            (first_after[1], first_after[2], first_after[3] & 0xF0),
            (0x41, 0x00, 0x30),
            "then the keyframe's own packet: the video's, flagged"
        );
        assert_eq!(resumed.len(), 4 * TS_PACKET, "the audio packet before it left out");
        assert_eq!(state.dropped.load(Ordering::Relaxed), 3);
        let lost = state.dropped_us.load(Ordering::Relaxed);
        assert_eq!(lost, 7 * 1504, "seven packets' time lost");
    }

    /// Falling too far behind halfway through a frame, a destination still
    /// gets the rest of it — and of the sound under way — past its limit,
    /// then nothing until a keyframe: no frame goes out cut off.
    #[test]
    fn a_frame_under_way_is_finished_before_the_skip() {
        use super::super::ts::tests::{packet, pat, pmt};
        let (tx, rx) = crossbeam_channel::unbounded();
        let queued = AtomicUsize::new(0);
        let state = RunnerState::default();
        state.sending.store(true, Ordering::Relaxed);
        let mut scan = TsScan::default();
        let mut send = |piece: Vec<u8>| {
            let piece: Piece = piece.into();
            let keyframe = scan.scan(&piece).map(|at| Resume {
                at,
                tables: scan.tables(),
            });
            offer((&tx, &queued, &state), &piece, (keyframe.as_ref(), scan.streams()), 1000, 1_000_000);
        };
        send([pat(0x1000), pmt(0x1000, 0x101, 0x100), packet(0x100, true, true, &[1]), packet(0x100, false, false, &[2])].concat());
        assert_eq!(rx.len(), 1);
        // Over the limit: the frame's rest, and the sound's start ends it.
        let rest = [packet(0x100, false, false, &[3]), packet(0x100, false, false, &[4])].concat();
        send([rest.clone(), packet(0x101, true, true, &[5])].concat());
        assert_eq!(rx.len(), 2, "the frame's rest went, past the limit");
        // The next frame's start closes it; nothing more until a keyframe.
        send([packet(0x100, true, false, &[6]), packet(0x100, false, false, &[7])].concat());
        assert_eq!(rx.len(), 2);
        // The sender catches up.
        let mut got: Vec<Piece> = rx.try_iter().map(|q| q.piece).collect();
        for piece in &got {
            queued.fetch_sub(piece.len(), Ordering::Relaxed);
        }
        send([packet(0x100, true, true, &[8]), packet(0x100, false, false, &[9])].concat());
        got.extend(rx.try_iter().map(|q| q.piece));
        assert_eq!(got.len(), 3, "resumed at the keyframe");
        assert_eq!(uncounted(&got[1]), uncounted(&rest));
        let counts: Vec<u8> = got[1].chunks(TS_PACKET).map(|p| p[3] & 0x0F).collect();
        assert_eq!(counts, [2, 3], "the video's count carried on");
        assert_eq!(got[2].len(), 4 * TS_PACKET, "the tables, then the keyframe");
    }

    /// A destination connecting starts at a keyframe, and what it misses
    /// before is not counted lost.
    #[test]
    fn a_destination_connecting_starts_at_a_keyframe() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let queued = AtomicUsize::new(0);
        let state = RunnerState::default();
        state.flow.lock().unwrap().skip = Skip::ToKeyframe;
        let mut scan = TsScan::default();
        for (tables, key) in [(true, false), (false, false), (false, true)] {
            let piece = ts_piece(tables, key);
            let keyframe = scan.scan(&piece).map(|at| Resume {
                at,
                tables: scan.tables(),
            });
            offer((&tx, &queued, &state), &piece, (keyframe.as_ref(), scan.streams()), 1 << 20, 1_000_000);
        }
        assert_eq!(rx.len(), 1, "only from the keyframe on");
        assert_eq!(state.dropped.load(Ordering::Relaxed), 0, "not lost: not yet sending");
    }

    /// What a struggling destination says to do follows who lowers the rate.
    #[test]
    fn the_remedy_follows_who_lowers_the_rate() {
        let state = RunnerState::default();
        recent(&state, &[(1000, 0), (1500, 1), (2000, 2), (2500, 3), (3000, 4)]);
        state.note_drop(4096, 8_000_000);
        let said = |remedy| state.status(remedy).message.unwrap_or_default();
        assert!(said(Remedy::LowerByHand).contains("stop, lower the Video bit rate"));
        assert!(said(Remedy::LowerLive).contains("changes while live"));
        assert!(!said(Remedy::LowerLive).contains("stop"), "no need to stop");
        assert!(said(Remedy::Adapting).contains("Nida is lowering the bit rate"));
        assert!(said(Remedy::AtFloor).contains("as low as it goes"));
    }

    /// The AAC sequence headers in an FLV file, in order: each one's
    /// `AudioSpecificConfig`.
    fn aac_headers(flv: &[u8]) -> Vec<Vec<u8>> {
        let mut at = u32::from_be_bytes(flv[5..9].try_into().unwrap()) as usize + 4;
        let mut headers = Vec::new();
        while at + 11 <= flv.len() {
            let size = u32::from_be_bytes([0, flv[at + 1], flv[at + 2], flv[at + 3]]) as usize;
            let body = &flv[at + 11..(at + 11 + size).min(flv.len())];
            if flv[at] & 0x1f == 8 && body.len() >= 2 && body[1] == 0 {
                headers.push(body[2..].to_vec());
            }
            at += 11 + size + 4;
        }
        headers
    }

    #[test]
    fn the_first_sound_header_a_server_gets_says_48_khz() {
        let dir = std::env::temp_dir().join(format!("varda-flv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (ts, flv) = (dir.join("in.ts"), dir.join("out.flv"));
        let made = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi"])
            .args(["-i", "sine=frequency=440:sample_rate=48000", "-t", "1"])
            .args(["-c:a", "aac", "-ar", "48000", "-ac", "2", "-f", "mpegts"])
            .arg(&ts)
            .status()
            .is_ok_and(|s| s.success());
        assert!(made, "ffmpeg made the test stream");
        let target = RelayTarget {
            url: flv.to_string_lossy().into_owned(),
            args: Vec::new(),
            display: String::new(),
        };
        let sent = sender_command(&target, 1_000_000)
            .stdin(std::fs::File::open(&ts).unwrap())
            .stdout(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        let bytes = std::fs::read(&flv).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(sent, "the sender ran");
        let headers = aac_headers(&bytes);
        let first = headers.first().expect("a sequence header");
        assert!(first.len() >= 2, "empty first header: {headers:?}");
        let rate_index = ((first[0] & 7) << 1) | (first[1] >> 7);
        assert_eq!(rate_index, 3, "48 kHz: {headers:?}");
    }

    #[test]
    fn progress_blocks_are_read_as_time_and_bytes() {
        let progress = b"frame=1\nout_time_us=500000\ntotal_size=1000\nprogress=continue\n\
                         out_time_us=N/A\nprogress=continue\nout_time_us=1000000\ntotal_size=2000\nprogress=end\n";
        let (tx, rx) = crossbeam_channel::unbounded();
        read_progress(&progress[..], &tx);
        let got: Vec<(i64, u64)> = rx.try_iter().collect();
        assert_eq!(
            got,
            [(500_000, 1000), (1_000_000, 2000)],
            "a block without a time is skipped"
        );
    }
}
