//! The sound devices the buses play on, opened, kept and dropped on a thread
//! of their own.
//!
//! Finding a device asks the OS about every sound device on the machine, and
//! with virtual devices installed (Teams, Zoom, NDI, OBS) that took a third of
//! a second. A bus whose device was missing — a show opened on another Mac,
//! an HDMI output switched off — was looked for every 2 s on the render
//! thread, and the whole picture froze each time. Now the render thread only
//! says which devices it wants and reads which are playing.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::core::OutputRing;
use super::RETRY;

/// A device playing a bus.
pub(super) trait Playing {
    /// Whether it has reported an error (usually: unplugged).
    fn failed(&self) -> bool;
    /// A live bus's: the times its mics ran short so far.
    fn underruns(&self) -> Option<u64> {
        None
    }
    /// How it plays when not as set: on another pair of outputs.
    fn note(&self) -> Option<&str> {
        None
    }
    /// A live bus's: what its callback did with its mics' queue since asked.
    fn events(&self) -> Vec<super::live::LiveEvent> {
        Vec::new()
    }
    /// Fade out and play silence until closed or let back (see
    /// `DeviceOutput::hush`).
    fn hush(&self) {}
    /// Back from [`Self::hush`].
    fn unhush(&self) {}
    /// Frames played wholly silent since hushed, and its callbacks' size;
    /// `None` when it cannot say (then the time counts).
    fn silent_frames(&self) -> Option<(u64, u64)> {
        None
    }
}

impl Playing for super::device::DeviceOutput {
    fn failed(&self) -> bool {
        Self::failed(self)
    }
    fn underruns(&self) -> Option<u64> {
        Self::underruns(self)
    }
    fn note(&self) -> Option<&str> {
        Self::note(self)
    }
    fn events(&self) -> Vec<super::live::LiveEvent> {
        Self::events(self)
    }
    fn hush(&self) {
        Self::hush(self);
    }
    fn unhush(&self) {
        Self::unhush(self);
    }
    fn silent_frames(&self) -> Option<(u64, u64)> {
        Some(Self::silent_frames(self))
    }
}

/// A live bus's device (see `device::open_live`): its mics' feed (every mic
/// in each frame), each mic by input UUID with its gain in the feed's
/// order, the bus's gain, and the buffer asked for.
#[derive(Clone)]
pub(super) struct LiveWant {
    pub frames: u32,
    pub feed: crossbeam_channel::Receiver<crate::audio::LiveFrame>,
    /// What its capture has promised and what is gone (see
    /// [`crate::audio::LiveCounts`]).
    pub counts: Arc<crate::audio::LiveCounts>,
    pub mics: Vec<(String, Arc<std::sync::atomic::AtomicU32>)>,
    pub bus_gain: Arc<std::sync::atomic::AtomicU32>,
    /// Its mics' and its own processing.
    pub control: Arc<super::live::LiveControl>,
    /// The rest of the bus, from the mix.
    pub rest: crossbeam_channel::Receiver<super::reader::Frame>,
}

impl PartialEq for LiveWant {
    fn eq(&self, other: &Self) -> bool {
        self.frames == other.frames
            && Arc::ptr_eq(&self.bus_gain, &other.bus_gain)
            && Arc::ptr_eq(&self.control, &other.control)
            && self.rest.same_channel(&other.rest)
            && self.feed.same_channel(&other.feed)
            && self.mics.len() == other.mics.len()
            && self
                .mics
                .iter()
                .zip(&other.mics)
                .all(|(a, b)| a.0 == b.0 && Arc::ptr_eq(&a.1, &b.1))
    }
}

/// What opens a device the usual way, from its ring: `device::open`, or a
/// stand-in for tests.
pub(super) type Opener =
    Box<dyn FnMut(&str, u16, Arc<OutputRing>) -> anyhow::Result<Box<dyn Playing>> + Send>;

/// What opens a device live (see [`LiveWant`]): `device::open_live`, or a
/// stand-in for tests.
pub(super) type LiveOpener =
    Box<dyn FnMut(&str, u16, &LiveWant) -> anyhow::Result<Box<dyn Playing>> + Send>;

/// What opens the buses' devices, the usual way and live.
pub(super) struct Openers {
    pub usual: Opener,
    pub live: LiveOpener,
}

impl Openers {
    /// The system's devices.
    pub fn system() -> Self {
        Self {
            usual: Box::new(|name, first_channel, ring| {
                super::device::open(name, first_channel, ring).map(|d| Box::new(d) as Box<dyn Playing>)
            }),
            live: Box::new(|name, first_channel, live| {
                super::device::open_live(
                    name,
                    first_channel,
                    live.frames,
                    super::live::LiveMics {
                        frames: live.feed.clone(),
                        counts: Arc::clone(&live.counts),
                        gains: live.mics.iter().map(|(_, gain)| Arc::clone(gain)).collect(),
                        control: Arc::clone(&live.control),
                        rest: live.rest.clone(),
                    },
                    Arc::clone(&live.bus_gain),
                )
                .map(|d| Box::new(d) as Box<dyn Playing>)
            }),
        }
    }

    /// `open` the usual way and no device live, refused as the system
    /// refuses a device it has not got — nothing asked of it, which with a
    /// dozen devices and other tests asking takes seconds. For tests.
    #[cfg(test)]
    pub fn standing_in(open: Opener) -> Self {
        Self {
            usual: open,
            live: Box::new(|name, _, _| anyhow::bail!("no sound device called '{name}'")),
        }
    }
}

/// A device a bus (or the monitor, by its key) wants to play on.
#[derive(Clone)]
pub(super) struct Want {
    pub key: String,
    pub device: String,
    pub first_channel: u16,
    pub ring: Arc<OutputRing>,
    /// Played live — its mics mixed in the device's callback — not from
    /// `ring` (see `device::open_live`).
    pub live: Option<LiveWant>,
}

impl PartialEq for Want {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.device == other.device
            && self.first_channel == other.first_channel
            && Arc::ptr_eq(&self.ring, &other.ring)
            && self.live == other.live
    }
}

/// What the render thread and the device thread share.
#[derive(Default)]
struct Shared {
    wanted: Mutex<Vec<Want>>,
    /// The keys whose devices are playing.
    playing: Mutex<Vec<String>>,
    /// Those of them played live (see `device::open_live`).
    playing_live: Mutex<Vec<String>>,
    /// Why a wanted device does not play as set, by key (see [`notes`]).
    notes: Mutex<Vec<(String, String)>>,
    /// Counts changes to `playing`, `playing_live` and `notes`.
    generation: AtomicU64,
    /// Look for missing devices now, not at their next retry.
    retry_now: AtomicBool,
    stop: AtomicBool,
}

/// The device thread, as the render thread sees it.
pub(super) struct Outputs {
    shared: Arc<Shared>,
    wake: mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Last wanted, as posted.
    posted: Vec<Want>,
    /// The generation of `playing` last read.
    seen: u64,
    playing: Vec<String>,
    playing_live: Vec<String>,
    notes: Vec<(String, String)>,
}

impl Outputs {
    /// A device thread opening devices with `open`.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to start the thread.
    pub fn spawn(open: Openers) -> Self {
        let shared = Arc::new(Shared::default());
        let (wake, woken) = mpsc::channel();
        let thread = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("audio-outputs".into())
                .spawn(move || run(&shared, &woken, open))
                .expect("failed to start the sound output thread")
        };
        Self {
            shared,
            wake,
            thread: Some(thread),
            posted: Vec::new(),
            seen: 0,
            playing: Vec::new(),
            playing_live: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Say which devices are wanted. Never waits on a device.
    pub fn want(&mut self, wanted: Vec<Want>) {
        if wanted == self.posted {
            return;
        }
        if let Ok(mut shared) = self.shared.wanted.lock() {
            shared.clone_from(&wanted);
        }
        self.posted = wanted;
        let _ = self.wake.send(());
    }

    /// Look for missing devices now (a device just plugged in).
    pub fn retry_now(&self) {
        self.shared.retry_now.store(true, Ordering::Relaxed);
        let _ = self.wake.send(());
    }

    /// Whether the playing devices changed since last asked. Never waits on a
    /// device.
    pub fn changed(&mut self) -> bool {
        let generation = self.shared.generation.load(Ordering::Acquire);
        if generation == self.seen {
            return false;
        }
        self.seen = generation;
        if let Ok(playing) = self.shared.playing.lock() {
            self.playing.clone_from(&playing);
        }
        if let Ok(live) = self.shared.playing_live.lock() {
            self.playing_live.clone_from(&live);
        }
        if let Ok(notes) = self.shared.notes.lock() {
            self.notes.clone_from(&notes);
        }
        true
    }

    /// Why the device of `key` does not play as set — another pair of
    /// outputs, its live mixing refused, or why it would not open — as last
    /// read by [`Self::changed`].
    pub fn note(&self, key: &str) -> Option<&str> {
        self.notes
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, note)| note.as_str())
    }

    /// Whether the device of `key` is played live — its ring not read — as
    /// last read by [`Self::changed`].
    pub fn is_playing_live(&self, key: &str) -> bool {
        self.playing_live.iter().any(|k| k == key)
    }

    /// Take the device of `key` as playing live, for tests.
    #[cfg(test)]
    pub fn pretend_live(&mut self, key: &str) {
        self.playing.push(key.to_string());
        self.playing_live.push(key.to_string());
    }

    /// Whether the device of `key` is playing, as last read by [`Self::changed`].
    pub fn is_playing(&self, key: &str) -> bool {
        self.playing.iter().any(|k| k == key)
    }
}

impl Drop for Outputs {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        let _ = self.wake.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether `why` device `key` would not open is new — said once, not each
/// time it is looked for again (every [`RETRY`], an unplugged device filled
/// the log); noted as said.
fn first_said(said: &mut HashMap<String, String>, key: &str, why: &str) -> bool {
    if said.get(key).is_some_and(|before| before == why) {
        return false;
    }
    said.insert(key.to_string(), why.to_string());
    true
}

/// Forget why a bus's device would not open, and when it was tried, once
/// the bus wants another device, pair or live mixing (`failed`: what each
/// was when it failed). Kept, the old device's reason showed on the bus as
/// the new one's, and the new one waited out the old one's retry.
fn forget_changed(
    wanted: &[Want],
    failed: &mut HashMap<String, Want>,
    said: &mut HashMap<String, String>,
    tried: &mut HashMap<String, Instant>,
) {
    failed.retain(|key, before| {
        let same = wanted.iter().any(|w| {
            w.key == *key
                && w.device == before.device
                && w.first_channel == before.first_channel
                && w.live.is_some() == before.live.is_some()
        });
        if !same {
            said.remove(key);
            tried.remove(key);
        }
        same
    });
}

/// Why each wanted device does not play as set, by key, sorted: a playing
/// one's own note (another pair) after why its live mixing was refused; a
/// missing one's reason it would not open.
fn notes(
    wanted: &[Want],
    devices: &HashMap<String, (Want, Box<dyn Playing>, bool)>,
    refused: &HashMap<String, String>,
    said: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut notes: Vec<(String, String)> = wanted
        .iter()
        .filter_map(|want| {
            let note = match devices.get(&want.key) {
                Some((_, device, _)) => {
                    let parts: Vec<&str> = refused
                        .get(&want.key)
                        .map(String::as_str)
                        .into_iter()
                        .chain(device.note())
                        .collect();
                    (!parts.is_empty()).then(|| parts.join("; "))
                }
                None => said.get(&want.key).cloned(),
            };
            note.map(|n| (want.key.clone(), n))
        })
        .collect();
    notes.sort();
    notes
}

/// Why a device stopped.
const FAILED: &str = "the device stopped (an error)";

/// Why a playing device closes, or `None` to keep it: no longer wanted,
/// wanted otherwise (`now`), or `failed`.
/// How long a device plays silence before a live stream with a smaller
/// buffer opens on it: longer than the 250 ms it replays as its buffer
/// changes — counted in silent frames played, two callbacks more for what
/// is still on its way out.
const HUSH: Duration = Duration::from_millis(300);
const HUSH_FRAMES: u64 = 14_400;
/// The longest a device stays hushed, whatever its callbacks say: one that
/// stopped calling back must not hold a change for ever.
const HUSH_MOST: Duration = Duration::from_secs(2);

/// A stream playing silence: since when, on what device, and whether it is
/// another's change it is silent for — let back once that is done.
struct Hushed {
    since: Instant,
    device: String,
    resume: bool,
}

/// Whether `device`, hushed for `so_far`, has played silence long enough.
fn silent_enough(device: &dyn Playing, so_far: Duration) -> bool {
    so_far >= HUSH_MOST
        || match device.silent_frames() {
            Some((silent, call)) => silent >= HUSH_FRAMES + 2 * call,
            None => so_far >= HUSH,
        }
}

/// Whether a stream playing as `was` (live or not) is hushed before it is
/// closed for `now`: a live stream on the same device with a smaller buffer
/// than it had — the device's buffer shrinking (the usual way's is the
/// device's own, never smaller than a live one's).
fn hush_first(was: &Want, was_live: bool, now: &Want) -> bool {
    let Some(new) = &now.live else {
        return false;
    };
    now.device == was.device
        && (!was_live || was.live.as_ref().is_some_and(|old| old.frames > new.frames))
}

fn close_why(now: Option<&Want>, was: &Want, failed: bool) -> Option<&'static str> {
    Some(match now {
        None => "no longer wanted",
        Some(w) if w.device != was.device => "its device changed",
        Some(w) if w.first_channel != was.first_channel => "its channels changed",
        Some(w) if w.live != was.live => "it went live, or its live mics or buffer changed",
        Some(w) if w != was => "its mix started again",
        Some(_) if failed => FAILED,
        Some(_) => return None,
    })
}

/// The device thread: keep open what is wanted, looking for a missing device
/// every [`RETRY`]; drop what is not, or has failed.
fn run(shared: &Shared, woken: &mpsc::Receiver<()>, open: Openers) {
    let Openers {
        usual: mut open,
        live: mut open_live,
    } = open;
    let mut devices: HashMap<String, (Want, Box<dyn Playing>, bool)> = HashMap::new();
    let mut tried: HashMap<String, Instant> = HashMap::new();
    // Why each device would not open, as last said (see `first_said`).
    let mut said: HashMap<String, String> = HashMap::new();
    // What each device not opened was wanted as when it failed.
    let mut failed: HashMap<String, Want> = HashMap::new();
    let mut published: Vec<String> = Vec::new();
    let mut published_live: Vec<String> = Vec::new();
    let mut published_notes: Vec<(String, String)> = Vec::new();
    // Why a playing device's live mixing was refused, by key: kept while
    // it plays the usual way.
    let mut refused: HashMap<String, String> = HashMap::new();
    // Why each device was last closed: said when it opens again, so a
    // device opened over and over (a click each time) says what did it.
    let mut closed: HashMap<String, &'static str> = HashMap::new();
    // A live bus's underruns as last said, and when.
    let mut underruns_told: HashMap<String, (u64, Option<Instant>)> = HashMap::new();
    // Streams playing silence before a smaller-buffer live stream opens on
    // their device (see `HUSH`), by key.
    let mut hushing: HashMap<String, Hushed> = HashMap::new();
    // Live streams waiting to open, by key, on a device whose other streams
    // play silence first (an added, moved or retried live bus).
    let mut waiting_open: HashMap<String, String> = HashMap::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let wanted = shared.wanted.lock().map(|w| w.clone()).unwrap_or_default();
        // Each device with a change waiting: whether every stream on it has
        // played silence long enough.
        let mut ready: HashMap<String, bool> = HashMap::new();
        // Streams closed after their device's silence this pass: their
        // replacements open first.
        let mut replacing: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (key, h) in &hushing {
            if let Some((_, device, _)) = devices.get(key) {
                let done = silent_enough(device.as_ref(), h.since.elapsed());
                let all = ready.entry(h.device.clone()).or_insert(true);
                *all &= done;
            }
        }
        devices.retain(|key, (want, device, live)| {
            let now = wanted.iter().find(|w| w.key == *key);
            let why = close_why(now, want, device.failed());
            // Silent for another's change on its device: kept so till done.
            if hushing.get(key).is_some_and(|h| h.resume) {
                if why.is_none() {
                    return true;
                }
                hushing.remove(key);
            }
            let Some(why) = why else {
                if hushing.remove(key).is_some() {
                    // Its change undone while it was silent: opened again,
                    // as it is wanted, rather than left silent.
                    log::info!(
                        "Audio bus output '{}' closed: its change was undone while it was silent",
                        want.device
                    );
                    closed.insert(key.clone(), "its change was undone while it was silent");
                    refused.remove(key);
                    underruns_told.remove(key);
                    tried.remove(key);
                    return false;
                }
                return true;
            };
            if why != FAILED && now.is_some_and(|now| hush_first(want, *live, now)) {
                if hushing.get(key).is_some_and(|h| !h.resume)
                    && ready.get(&want.device).copied().unwrap_or(false)
                {
                    // Silent long enough: closed now, and its replacement
                    // opens before anything else on its device.
                    replacing.insert(key.clone());
                }
                if !hushing.contains_key(key) {
                    device.hush();
                    log::info!(
                        "Audio bus output '{}': silent {} ms before it opens with a smaller buffer",
                        want.device,
                        HUSH.as_millis()
                    );
                    hushing.insert(
                        key.clone(),
                        Hushed {
                            since: Instant::now(),
                            device: want.device.clone(),
                            resume: false,
                        },
                    );
                    return true;
                }
                if !ready.get(&want.device).copied().unwrap_or(false) {
                    return true;
                }
            }
            hushing.remove(key);
            log::info!("Audio bus output '{}' closed: {why}", want.device);
            closed.insert(key.clone(), why);
            refused.remove(key);
            // A new stream counts its own.
            underruns_told.remove(key);
            // Closed to open as now wanted: at once, not after the retry
            // of a missing device (silent for two seconds after a buffer
            // or Live switched soon after it opened). A failed one waits.
            if why != FAILED {
                tried.remove(key);
            }
            false
        });
        // Every other stream on a device with a change waiting goes silent
        // too: what it plays is in what the device replays.
        waiting_open.retain(|key, _| wanted.iter().any(|w| w.key == *key && w.live.is_some()));
        let mut changing: std::collections::HashSet<String> = hushing
            .values()
            .filter(|h| !h.resume)
            .map(|h| h.device.clone())
            .chain(waiting_open.values().cloned())
            .collect();
        for (key, (want, device, _)) in &devices {
            if changing.contains(&want.device) && !hushing.contains_key(key) {
                device.hush();
                hushing.insert(
                    key.clone(),
                    Hushed {
                        since: Instant::now(),
                        device: want.device.clone(),
                        resume: true,
                    },
                );
            }
        }
        // One no longer wanted, wanted again, is said again.
        said.retain(|key, _| wanted.iter().any(|w| w.key == *key));
        forget_changed(&wanted, &mut failed, &mut said, &mut tried);
        if shared.retry_now.swap(false, Ordering::Relaxed) {
            tried.clear();
        }
        // A replacement after its device's silence first, then the rest:
        // another stream opened before it would play into the device as its
        // buffer shrinks.
        let replacements_first = wanted
            .iter()
            .filter(|w| replacing.contains(&w.key))
            .chain(wanted.iter().filter(|w| !replacing.contains(&w.key)));
        for want in replacements_first {
            if devices.contains_key(&want.key)
                || tried.get(&want.key).is_some_and(|t| t.elapsed() < RETRY)
                // Nothing opens on a device going silent for a change: it
                // would change the device's buffer before its past is silent.
                || (changing.contains(&want.device) && !waiting_open.contains_key(&want.key))
            {
                continue;
            }
            // A live stream opening on a device others play on (added, moved,
            // retried): they play silence first, as for a replacement.
            if want.live.is_some() {
                let others: Vec<&String> = devices
                    .iter()
                    .filter(|(key, (w, _, _))| w.device == want.device && **key != want.key)
                    .map(|(key, _)| key)
                    .collect();
                let mut fresh = false;
                for key in &others {
                    if !hushing.contains_key(*key) {
                        devices[*key].1.hush();
                        hushing.insert(
                            (*key).clone(),
                            Hushed {
                                since: Instant::now(),
                                device: want.device.clone(),
                                resume: true,
                            },
                        );
                        fresh = true;
                    }
                }
                if fresh {
                    log::info!(
                        "Audio bus output '{}': others on it silent {} ms before it opens live",
                        want.device,
                        HUSH.as_millis()
                    );
                    waiting_open.insert(want.key.clone(), want.device.clone());
                    // Nothing else opens on it meanwhile, from this pass on.
                    changing.insert(want.device.clone());
                    continue;
                }
                let quiet = others
                    .iter()
                    .all(|key| hushing.get(*key).is_none_or(|h| silent_enough(devices[*key].1.as_ref(), h.since.elapsed())));
                if !quiet {
                    continue;
                }
                waiting_open.remove(&want.key);
            }
            tried.insert(want.key.clone(), Instant::now());
            let opened = match &want.live {
                Some(live) => open_live(&want.device, want.first_channel, live)
                    .map(|d| (d, true))
                    .or_else(|e| {
                        // Played the usual way, from its ring, rather than
                        // not at all; the mix told so (see `is_playing_live`),
                        // and the bus shows why (see `notes`).
                        log::warn!("Audio bus output '{}': {e}", want.device);
                        let opened = open(&want.device, want.first_channel, Arc::clone(&want.ring))
                            .map(|d| (d, false));
                        if opened.is_ok() {
                            refused.insert(
                                want.key.clone(),
                                format!("Live refused ({e}): played the usual way"),
                            );
                        }
                        opened
                    }),
                None => open(&want.device, want.first_channel, Arc::clone(&want.ring)).map(|d| (d, false)),
            };
            match opened {
                Ok(device) => {
                    failed.remove(&want.key);
                    if said.remove(&want.key).is_some() {
                        log::info!("Audio bus output: '{}' playing again", want.device);
                    }
                    match closed.remove(&want.key) {
                        Some(why) => log::info!(
                            "Audio bus output '{}' opened again (it closed: {why})",
                            want.device
                        ),
                        None => log::info!("Audio bus output '{}' opened", want.device),
                    }
                    let (device, live) = device;
                    devices.insert(want.key.clone(), (want.clone(), device, live));
                }
                Err(e) => {
                    failed.insert(want.key.clone(), want.clone());
                    let why = e.to_string();
                    if first_said(&mut said, &want.key, &why) {
                        log::warn!("Audio bus output: {why} (looked for again every few seconds)");
                    } else {
                        log::debug!("Audio bus output: {why}");
                    }
                }
            }
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
        }
        // Streams silent for another's change, let back once it is done.
        let still: std::collections::HashSet<String> = hushing
            .values()
            .filter(|h| !h.resume)
            .map(|h| h.device.clone())
            .chain(waiting_open.values().cloned())
            .collect();
        hushing.retain(|key, h| {
            if !h.resume || still.contains(&h.device) {
                return true;
            }
            if let Some((_, device, _)) = devices.get(key) {
                device.unhush();
            }
            false
        });
        // A live bus's callback: how it started, let go or ran short, to the
        // frame — what to look at when a live open sounds wrong.
        for (want, device, _) in devices.values() {
            for event in device.events() {
                log::info!("Audio bus output '{}' (live): {event}", want.device);
            }
        }
        // A live bus's mics running short: said as it happens, now and then.
        for (want, device, _) in devices.values() {
            if let Some(now) = device.underruns() {
                let told = underruns_told.entry(want.key.clone()).or_insert((0, None));
                if now > told.0 && told.1.is_none_or(|at: Instant| at.elapsed() >= Duration::from_secs(5)) {
                    log::warn!(
                        "Audio bus output '{}' (live): its mics ran short {} time(s) so far",
                        want.device,
                        now
                    );
                    *told = (now, Some(Instant::now()));
                }
            }
        }
        let mut playing: Vec<String> = devices.keys().cloned().collect();
        playing.sort();
        let mut live: Vec<String> = devices
            .iter()
            .filter(|(_, (_, _, live))| *live)
            .map(|(key, _)| key.clone())
            .collect();
        live.sort();
        let notes = notes(&wanted, &devices, &refused, &said);
        if playing != published || live != published_live || notes != published_notes {
            if let Ok(mut shared_playing) = shared.playing.lock() {
                shared_playing.clone_from(&playing);
            }
            if let Ok(mut shared_live) = shared.playing_live.lock() {
                shared_live.clone_from(&live);
            }
            if let Ok(mut shared_notes) = shared.notes.lock() {
                shared_notes.clone_from(&notes);
            }
            shared.generation.fetch_add(1, Ordering::Release);
            published = playing;
            published_live = live;
            published_notes = notes;
        }
        // Woken by a change of wants; otherwise, in time to notice a failed
        // device and to retry a missing one.
        // Looked at again soon while a stream plays silence before closing.
        let wait = if hushing.is_empty() && waiting_open.is_empty() { 250 } else { 20 };
        let _ = woken.recv_timeout(Duration::from_millis(wait));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    /// Why a device would not open is said once, not each time it is looked
    /// for again; said again when the reason changes.
    #[test]
    fn a_missing_device_is_said_once() {
        let mut said = HashMap::new();
        let gone = "no sound device called 'USB Capture'";
        assert!(super::first_said(&mut said, "main", gone));
        assert!(!super::first_said(&mut said, "main", gone), "said already");
        assert!(
            super::first_said(&mut said, "phones", gone),
            "another output"
        );
        assert!(
            super::first_said(&mut said, "main", "device busy"),
            "another reason"
        );
    }

    struct Fake(Option<&'static str>);

    impl super::Playing for Fake {
        fn failed(&self) -> bool {
            false
        }
        fn note(&self) -> Option<&str> {
            self.0
        }
    }

    fn want(key: &str) -> super::Want {
        super::Want {
            key: key.into(),
            device: "Solo".into(),
            first_channel: 2,
            ring: std::sync::Arc::default(),
            live: None,
        }
    }

    /// A fake stream that says when it is hushed and let back.
    struct Hushable(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::atomic::AtomicU32>,
    );
    impl super::Playing for Hushable {
        fn failed(&self) -> bool {
            false
        }
        fn hush(&self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        fn unhush(&self) {
            self.0.store(false, std::sync::atomic::Ordering::Relaxed);
            // Counted after the flag is cleared, and released: a test that
            // has read the count (`Acquire`) reads the flag as left here.
            self.1.fetch_add(1, std::sync::atomic::Ordering::Release);
        }
    }

    /// The passes of the device thread, as `Passes` counts them, in which
    /// the streams a test watches were opened, or closed.
    type InPasses = std::sync::Arc<std::sync::Mutex<Vec<u32>>>;

    /// A fake stream that counts the device thread's passes: asked its
    /// events once in each, after what that pass closes and opens. Kept
    /// open on a device of its own, it tells a test whether another stream
    /// was opened again in the pass that closed it — at once — or in a
    /// later one, however fast or slowly the thread runs.
    struct Passes(std::sync::Arc<std::sync::atomic::AtomicU32>);
    impl super::Playing for Passes {
        fn failed(&self) -> bool {
            false
        }
        fn events(&self) -> Vec<super::super::live::LiveEvent> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Vec::new()
        }
    }

    /// A fake stream that says when it is hushed and when it is let back —
    /// and, with a `gate`, holds the device thread in its hush till the gate
    /// opens, or the watchdog's time is up: what a test posts meanwhile (in
    /// less than that) is what the thread finds when it next looks — and,
    /// closed, in which pass (see `Passes`).
    struct Watched {
        hushed: std::sync::Arc<std::sync::atomic::AtomicBool>,
        gate: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        passes: std::sync::Arc<std::sync::atomic::AtomicU32>,
        closed_in: InPasses,
    }
    impl super::Playing for Watched {
        fn failed(&self) -> bool {
            false
        }
        fn hush(&self) {
            self.hushed.store(true, std::sync::atomic::Ordering::Release);
            if let Some(gate) = &self.gate {
                wait_for(WATCHDOG, || gate.load(std::sync::atomic::Ordering::Acquire));
            }
        }
        fn unhush(&self) {
            self.hushed.store(false, std::sync::atomic::Ordering::Release);
        }
    }
    impl Drop for Watched {
        fn drop(&mut self) {
            if let Ok(mut closed_in) = self.closed_in.lock() {
                closed_in.push(self.passes.load(std::sync::atomic::Ordering::Relaxed));
            }
        }
    }

    /// A fake stream that says when it is hushed, and of its silence always
    /// the same: so many frames played in callbacks of such a size, or that
    /// it cannot say (see `Playing::silent_frames`).
    struct Played(std::sync::Arc<std::sync::atomic::AtomicBool>, Option<(u64, u64)>);
    impl super::Playing for Played {
        fn failed(&self) -> bool {
            false
        }
        fn hush(&self) {
            self.0.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        fn silent_frames(&self) -> Option<(u64, u64)> {
            self.1
        }
    }

    fn live_want(key: &str, frames: u32) -> super::Want {
        let mut w = want(key);
        w.live = Some(super::LiveWant {
            frames,
            feed: crossbeam_channel::bounded(1).1,
            counts: std::sync::Arc::default(),
            mics: Vec::new(),
            bus_gain: std::sync::Arc::default(),
            control: std::sync::Arc::default(),
            rest: crossbeam_channel::bounded(1).1,
        });
        w
    }

    /// The longest a test waits for the device thread: a watchdog, far
    /// longer than anything here takes, so that a busy machine fails nothing.
    const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(10);

    /// Each device opened, told by the device thread as it opens it: when,
    /// and whether the stream a test watches was silent then. What a test
    /// asserts of an order or a silence it reads here, not by when it gets
    /// to look itself.
    type Opened = std::sync::Arc<std::sync::Mutex<Vec<(std::time::Instant, bool)>>>;

    /// Waits up to `most` for `done`.
    fn wait_for(most: std::time::Duration, done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() && start.elapsed() < most {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Going live undone while its stream plays silence: it opens again as
    /// wanted, not left silent — and at once, in the pass of the device
    /// thread that closed it, not after the two seconds a missing device
    /// waits.
    #[test]
    fn a_change_undone_while_silent_opens_again() {
        type Flags = std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>>>;
        let flags: Flags = std::sync::Arc::default();
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (opened_in, closed_in) = (InPasses::default(), InPasses::default());
        let (made, held) = (std::sync::Arc::clone(&flags), std::sync::Arc::clone(&gate));
        let (opens, closes) = (std::sync::Arc::clone(&opened_in), std::sync::Arc::clone(&closed_in));
        let passes = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |name, _, _| {
            let stream: Box<dyn super::Playing> = if name == "Other" {
                Box::new(Passes(std::sync::Arc::clone(&passes)))
            } else {
                let hushed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
                opens.lock().unwrap().push(passes.load(std::sync::atomic::Ordering::Relaxed));
                // Told last: it is what the test waits for.
                made.lock().unwrap().push(std::sync::Arc::clone(&hushed));
                // Hushed, it holds the device thread till the undoing below
                // is posted — the test given the watchdog's time to get
                // there, no more: the thread's next look finds the change
                // undone and the stream still silent.
                Box::new(Watched {
                    hushed,
                    gate: Some(std::sync::Arc::clone(&held)),
                    passes: std::sync::Arc::clone(&passes),
                    closed_in: std::sync::Arc::clone(&closes),
                })
            };
            Ok(stream)
        })));
        let count = || flags.lock().unwrap().len();
        let usual = want("a");
        // On a device of its own, never changed: it counts the passes.
        let mut other = want("b");
        other.device = "Other".into();
        outputs.want(vec![usual.clone(), other.clone()]);
        wait_for(WATCHDOG, || count() == 1);
        let mut live = live_want("a", 64);
        live.ring = std::sync::Arc::clone(&usual.ring);
        outputs.want(vec![live, other.clone()]);
        let silent = || flags.lock().unwrap()[0].load(std::sync::atomic::Ordering::Acquire);
        wait_for(WATCHDOG, silent);
        assert!(silent(), "silent first");
        // Wanted exactly as it was: nothing to close it for but the silence.
        outputs.want(vec![usual, other]);
        gate.store(true, std::sync::atomic::Ordering::Release);
        wait_for(WATCHDOG, || count() == 2);
        assert_eq!(count(), 2, "opened again, once");
        let last = std::sync::Arc::clone(flags.lock().unwrap().last().unwrap());
        assert!(!last.load(std::sync::atomic::Ordering::Acquire), "not left silent");
        let (opened_in, closed_in) = (opened_in.lock().unwrap().clone(), closed_in.lock().unwrap().clone());
        assert_eq!(
            opened_in.get(1),
            closed_in.first(),
            "opened again in the pass that closed it, not left for the retry"
        );
    }

    /// Another stream on the same device goes silent too while one waits to
    /// go live with a smaller buffer, and comes back once that has opened.
    #[test]
    fn other_streams_on_the_device_go_silent_and_come_back() {
        let b_hushed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let b_back = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (bh, bb) = (std::sync::Arc::clone(&b_hushed), std::sync::Arc::clone(&b_back));
        let opened: Opened = std::sync::Arc::default();
        let order: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
        let (told, seen) = (std::sync::Arc::clone(&opened), std::sync::Arc::clone(&order));
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |_, _, ring| {
            // Which stream: told by how many frames its ring was given below
            // (a none, b one, c two).
            let frames = ring.frames.lock().unwrap().len();
            seen.lock().unwrap().push(frames);
            // When, and whether b was silent then.
            told.lock().unwrap().push((std::time::Instant::now(), bh.load(std::sync::atomic::Ordering::Relaxed)));
            Ok(Box::new(if frames == 1 {
                Hushable(std::sync::Arc::clone(&bh), std::sync::Arc::clone(&bb))
            } else {
                Hushable(std::sync::Arc::default(), std::sync::Arc::default())
            }))
        })));
        let count = || opened.lock().unwrap().len();
        let b = want("b");
        b.ring.frames.lock().unwrap().push_back([0.0; 2]);
        outputs.want(vec![want("a"), b.clone()]);
        wait_for(WATCHDOG, || count() == 2);
        let asked = std::time::Instant::now();
        // And c, new on the same device meanwhile: it waits for the silence.
        let c = want("c");
        c.ring.frames.lock().unwrap().extend([[0.0; 2]; 2]);
        outputs.want(vec![live_want("a", 64), b, c]);
        wait_for(WATCHDOG, || count() == 4 && b_back.load(std::sync::atomic::Ordering::Acquire) == 1);
        // The device thread stopped: what it did is all there, and stays so
        // while it is read.
        drop(outputs);
        let opens = opened.lock().unwrap().clone();
        assert_eq!(opens.len(), 4, "a opened again and c, b never closed");
        assert_eq!(*order.lock().unwrap(), [0, 1, 0, 2], "a and b; then a again, and c only after it");
        for &(at, b_silent) in &opens[2..] {
            // The silence starts after it is asked for: counted from the
            // asking, it is no shorter however slowly the test gets here.
            let after = at.duration_since(asked);
            assert!(after >= super::HUSH, "a again and c only after the silence: {after:?}");
            assert!(b_silent, "b silent too, till both had opened");
        }
        assert_eq!(b_back.load(std::sync::atomic::Ordering::Acquire), 1, "b back");
        assert!(!b_hushed.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// After its device's silence, a stream's replacement opens before a
    /// stream new on that device, whatever their order in the list.
    #[test]
    fn a_replacement_opens_before_anything_new_on_its_device() {
        let order: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
        let seen = std::sync::Arc::clone(&order);
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |_, _, ring| {
            // Which stream: told by how many frames its ring was given below.
            seen.lock().unwrap().push(ring.frames.lock().unwrap().len());
            Ok(Box::new(Hushable(std::sync::Arc::default(), std::sync::Arc::default())))
        })));
        let a = want("a");
        a.ring.frames.lock().unwrap().push_back([0.0; 2]);
        outputs.want(vec![a.clone()]);
        wait_for(WATCHDOG, || order.lock().unwrap().len() == 1);
        let mut live = live_want("a", 64);
        live.ring = std::sync::Arc::clone(&a.ring);
        let c = want("c");
        c.ring.frames.lock().unwrap().extend([[0.0; 2]; 2]);
        outputs.want(vec![c, live]);
        wait_for(WATCHDOG, || order.lock().unwrap().len() == 3);
        assert_eq!(*order.lock().unwrap(), [1, 1, 2], "a, a again, then c");
    }

    /// A live bus added on a device another stream plays on: that stream goes
    /// silent first, the live one opens after the silence, and the other
    /// comes back.
    #[test]
    fn a_live_bus_added_beside_another_waits_for_its_silence() {
        let b_hushed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let b_back = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (bh, bb) = (std::sync::Arc::clone(&b_hushed), std::sync::Arc::clone(&b_back));
        let opened: Opened = std::sync::Arc::default();
        let order: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
        let (told, seen) = (std::sync::Arc::clone(&opened), std::sync::Arc::clone(&order));
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |_, _, ring| {
            // Which stream: c's ring is the one given a frame below.
            seen.lock().unwrap().push(ring.frames.lock().unwrap().len());
            // When, and whether b was silent then; the first to open is b.
            let first = {
                let mut list = told.lock().unwrap();
                list.push((std::time::Instant::now(), bh.load(std::sync::atomic::Ordering::Relaxed)));
                list.len() == 1
            };
            Ok(Box::new(if first {
                Hushable(std::sync::Arc::clone(&bh), std::sync::Arc::clone(&bb))
            } else {
                Hushable(std::sync::Arc::default(), std::sync::Arc::default())
            }))
        })));
        let count = || opened.lock().unwrap().len();
        let b = want("b");
        outputs.want(vec![b.clone()]);
        wait_for(WATCHDOG, || count() == 1);
        let asked = std::time::Instant::now();
        // And c, wanted after it on the same device: it waits too.
        let c = want("c");
        c.ring.frames.lock().unwrap().push_back([0.0; 2]);
        outputs.want(vec![b, live_want("a", 64), c]);
        wait_for(WATCHDOG, || count() == 3 && b_back.load(std::sync::atomic::Ordering::Acquire) == 1);
        // The device thread stopped: what it did is all there, and stays so
        // while it is read.
        drop(outputs);
        let opens = opened.lock().unwrap().clone();
        assert_eq!(opens.len(), 3, "a and c opened");
        assert_eq!(*order.lock().unwrap(), [0, 0, 1], "b; then a, and c only after it");
        assert!(opens[1].1, "b silent first: so it was when the next opened");
        for &(at, _) in &opens[1..] {
            let after = at.duration_since(asked);
            assert!(after >= super::HUSH, "a and c opened after the silence: {after:?}");
        }
        assert_eq!(b_back.load(std::sync::atomic::Ordering::Acquire), 1, "b let back");
        assert!(!b_hushed.load(std::sync::atomic::Ordering::Relaxed), "b back");
    }

    /// A live bus added beside another stream, then taken back while that
    /// stream goes silent for it — taken off the device, or made a usual
    /// bus: the stream is let back, and what is wanted next on the device
    /// opens — neither kept waiting for a bus that is no longer to open
    /// live.
    #[test]
    fn a_live_bus_taken_back_while_it_waits_lets_the_others_play() {
        // Taken off: b, then c — the bus never opened.
        taken_back_while_it_waits(false, &[0, 2]);
        // Made a usual bus: b, the bus the usual way, then c.
        taken_back_while_it_waits(true, &[0, 1, 2]);
    }

    /// `a_live_bus_taken_back_while_it_waits_lets_the_others_play`, one way
    /// of taking the bus back: off the device, or — `usual` — wanted on it
    /// the usual way instead. `opened` = the streams that must have opened,
    /// in order, each told by the frames its ring was given (b none, the
    /// bus one, c two).
    fn taken_back_while_it_waits(usual: bool, opened: &[usize]) {
        let b_hushed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let gate = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let order: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
        let (bh, held, seen) =
            (std::sync::Arc::clone(&b_hushed), std::sync::Arc::clone(&gate), std::sync::Arc::clone(&order));
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |_, _, ring| {
            // Which stream: told by how many frames its ring was given below
            // (b none, a one, c two).
            let frames = ring.frames.lock().unwrap().len();
            let stream: Box<dyn super::Playing> = if frames == 0 {
                // Hushed, b holds the device thread till the bus is taken
                // back below — the test given the watchdog's time to get
                // there, no more: the thread's next look finds the bus no
                // longer to open live, and b still silent for it.
                Box::new(Watched {
                    hushed: std::sync::Arc::clone(&bh),
                    gate: Some(std::sync::Arc::clone(&held)),
                    passes: std::sync::Arc::default(),
                    closed_in: InPasses::default(),
                })
            } else {
                Box::new(Fake(None))
            };
            // Told last: it is what the test waits for.
            seen.lock().unwrap().push(frames);
            Ok(stream)
        })));
        let count = || order.lock().unwrap().len();
        let b = want("b");
        outputs.want(vec![b.clone()]);
        wait_for(WATCHDOG, || count() == 1);
        let a = live_want("a", 64);
        a.ring.frames.lock().unwrap().push_back([0.0; 2]);
        outputs.want(vec![b.clone(), a.clone()]);
        let silent = || b_hushed.load(std::sync::atomic::Ordering::Acquire);
        wait_for(WATCHDOG, silent);
        assert!(silent(), "b silent first (usual: {usual})");
        // The bus taken back — off the device, or left on it as a usual
        // bus, its ring the same — and c new on the device.
        let c = want("c");
        c.ring.frames.lock().unwrap().extend([[0.0; 2]; 2]);
        let mut then = vec![b];
        if usual {
            then.push(super::Want { live: None, ..a });
        }
        then.push(c);
        outputs.want(then);
        gate.store(true, std::sync::atomic::Ordering::Release);
        wait_for(WATCHDOG, || count() == opened.len() && !silent());
        // The device thread stopped: what it did is all there, and stays so
        // while it is read.
        drop(outputs);
        assert_eq!(*order.lock().unwrap(), opened, "what opened, in order (usual: {usual})");
        assert!(!silent(), "b let back (usual: {usual})");
    }

    /// Hushed first only for a live stream with a smaller buffer on the same
    /// device: from the usual way, or a live one with a bigger buffer.
    #[test]
    fn only_a_shrinking_buffer_on_the_same_device_is_hushed_first() {
        let live = |frames| {
            let mut w = want("a");
            w.live = Some(super::LiveWant {
                frames,
                feed: crossbeam_channel::bounded(1).1,
                counts: std::sync::Arc::default(),
                mics: Vec::new(),
                bus_gain: std::sync::Arc::default(),
                control: std::sync::Arc::default(),
                rest: crossbeam_channel::bounded(1).1,
            });
            w
        };
        let usual = want("a");
        assert!(super::hush_first(&usual, false, &live(64)), "usual → live");
        assert!(super::hush_first(&live(128), true, &live(64)), "128 → 64");
        assert!(!super::hush_first(&live(64), true, &live(128)), "64 → 128");
        assert!(!super::hush_first(&live(64), true, &live(64)), "the same");
        assert!(!super::hush_first(&live(64), true, &usual), "live → usual");
        let mut elsewhere = live(64);
        elsewhere.device = "Octo".into();
        assert!(!super::hush_first(&usual, false, &elsewhere), "another device");
    }

    /// Silent long enough: once a stream says it has played the device's
    /// replay in silent frames and two callbacks more, however short the
    /// time; by the time alone when it cannot say; and, whatever it says,
    /// once the longest wait is over.
    #[test]
    fn silence_is_told_by_frames_played_and_by_the_clock_without_them() {
        let played = |frames| Played(std::sync::Arc::default(), Some((frames, 512)));
        let cannot_say = Played(std::sync::Arc::default(), None);
        let enough = super::HUSH_FRAMES + 2 * 512;
        let a_moment = std::time::Duration::from_millis(1);
        assert!(
            super::silent_enough(&played(enough), std::time::Duration::ZERO),
            "played out: at once"
        );
        assert!(
            !super::silent_enough(&played(enough - 1), super::HUSH),
            "a frame short of two callbacks more: not by the time"
        );
        assert!(
            !super::silent_enough(&cannot_say, super::HUSH.saturating_sub(a_moment)),
            "cannot say: not before the time"
        );
        assert!(super::silent_enough(&cannot_say, super::HUSH), "cannot say: by the time");
        assert!(
            !super::silent_enough(&played(0), super::HUSH_MOST.saturating_sub(a_moment)),
            "nothing played: not before the longest wait"
        );
        assert!(
            super::silent_enough(&played(0), super::HUSH_MOST),
            "nothing played: the longest wait is over"
        );
    }

    /// A stream on the usual way, wanted live on the same device (a smaller
    /// buffer): it plays silence first — hushed, and closed and opened again
    /// only after the device's replay has nothing but silence to give.
    #[test]
    fn a_stream_going_live_plays_silence_before_it_closes() {
        let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opened: Opened = std::sync::Arc::default();
        let told = std::sync::Arc::clone(&opened);
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |_, _, _| {
            // When, and whether the stream before this one had been hushed.
            told.lock().unwrap().push((std::time::Instant::now(), flag.load(std::sync::atomic::Ordering::Relaxed)));
            Ok(Box::new(Hushable(std::sync::Arc::clone(&flag), std::sync::Arc::default())))
        })));
        let count = || opened.lock().unwrap().len();
        outputs.want(vec![want("a")]);
        wait_for(WATCHDOG, || count() == 1);
        let asked = std::time::Instant::now();
        outputs.want(vec![live_want("a", 64)]);
        wait_for(WATCHDOG, || count() == 2);
        let opens = opened.lock().unwrap().clone();
        assert_eq!(opens.len(), 2, "opened again");
        let (at, was_hushed) = opens[1];
        assert!(was_hushed, "hushed before it closed");
        // The silence starts after it is asked for: counted from the asking,
        // it is no shorter however slowly the test gets here.
        let after = at.duration_since(asked);
        assert!(after >= super::HUSH, "opened again only after the silence: {after:?}");
    }

    /// Waits (no longer than `WATCHDOG`) for the devices of `keys` to play.
    fn wait_playing(outputs: &mut super::Outputs, keys: &[&str]) {
        let start = std::time::Instant::now();
        while !keys.iter().all(|key| outputs.is_playing(key)) && start.elapsed() < WATCHDOG {
            std::thread::sleep(std::time::Duration::from_millis(2));
            outputs.changed();
        }
    }

    /// A bus wanted live is opened by the live opener, as it is wanted, and
    /// told as playing live; one wanted the usual way by the usual opener.
    #[test]
    fn a_live_want_is_opened_live_and_a_usual_one_the_usual_way() {
        type Asked = std::sync::Arc<std::sync::Mutex<Vec<(String, u16, Option<u32>)>>>;
        let asked: Asked = std::sync::Arc::default();
        let (usual, live) = (std::sync::Arc::clone(&asked), std::sync::Arc::clone(&asked));
        let mut outputs = super::Outputs::spawn(super::Openers {
            usual: Box::new(move |name, first_channel, _| {
                usual.lock().unwrap().push((name.to_string(), first_channel, None));
                Ok(Box::new(Fake(None)))
            }),
            live: Box::new(move |name, first_channel, wanted| {
                live.lock().unwrap().push((name.to_string(), first_channel, Some(wanted.frames)));
                Ok(Box::new(Fake(None)))
            }),
        });
        // On a device each: one opening live waits for no other's silence.
        let mut other = want("b");
        other.device = "Other".into();
        outputs.want(vec![live_want("a", 64), other]);
        wait_playing(&mut outputs, &["a", "b"]);
        assert!(outputs.is_playing("a") && outputs.is_playing("b"), "both play");
        assert!(outputs.is_playing_live("a"), "the one wanted live plays live");
        assert!(!outputs.is_playing_live("b"), "the other does not");
        assert_eq!((outputs.note("a"), outputs.note("b")), (None, None));
        let mut asked = asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(
            asked,
            [("Other".to_string(), 2, None), ("Solo".to_string(), 2, Some(64))],
            "each opened once, its own way"
        );
    }

    /// The tests' stand-in opens no device live: a bus wanted live is refused
    /// in the system's words for a device it has not got, and played the
    /// usual way — the system's devices not asked.
    #[test]
    fn the_stand_in_refuses_live_and_plays_the_usual_way() {
        let mut outputs =
            super::Outputs::spawn(super::Openers::standing_in(Box::new(|_, _, _| Ok(Box::new(Fake(None))))));
        outputs.want(vec![live_want("a", 64)]);
        wait_playing(&mut outputs, &["a"]);
        assert!(outputs.is_playing("a"), "plays");
        assert!(!outputs.is_playing_live("a"), "not live");
        assert_eq!(
            outputs.note("a"),
            Some("Live refused (no sound device called 'Solo'): played the usual way")
        );
    }

    /// A device closed to open as now wanted (another pair, Live or its
    /// buffer switched) opens again at once — in the pass of the device
    /// thread that closed it — not after the two seconds a missing device
    /// waits, even just after it opened.
    #[test]
    fn a_device_wanted_otherwise_opens_again_at_once() {
        let (opened_in, closed_in) = (InPasses::default(), InPasses::default());
        let (opens, closes) = (std::sync::Arc::clone(&opened_in), std::sync::Arc::clone(&closed_in));
        let passes = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let mut outputs = super::Outputs::spawn(super::Openers::standing_in(Box::new(move |name, _, _| {
            let stream: Box<dyn super::Playing> = if name == "Other" {
                Box::new(Passes(std::sync::Arc::clone(&passes)))
            } else {
                opens.lock().unwrap().push(passes.load(std::sync::atomic::Ordering::Relaxed));
                Box::new(Watched {
                    hushed: std::sync::Arc::default(),
                    gate: None,
                    passes: std::sync::Arc::clone(&passes),
                    closed_in: std::sync::Arc::clone(&closes),
                })
            };
            Ok(stream)
        })));
        let count = || opened_in.lock().unwrap().len();
        let first = want("a");
        // On a device of its own, never changed: it counts the passes.
        let mut other = want("b");
        other.device = "Other".into();
        outputs.want(vec![first.clone(), other.clone()]);
        wait_for(WATCHDOG, || count() == 1);
        assert_eq!(count(), 1, "opened");
        let mut moved = first;
        moved.first_channel = 0;
        outputs.want(vec![moved, other]);
        wait_for(WATCHDOG, || count() == 2);
        let (opened_in, closed_in) = (opened_in.lock().unwrap().clone(), closed_in.lock().unwrap().clone());
        assert_eq!(opened_in.len(), 2, "opened again");
        assert_eq!(
            opened_in.get(1),
            closed_in.first(),
            "opened again in the pass that closed it, not after the retry's two seconds"
        );
    }

    /// A bus moved off a device that would not open to another device or
    /// pair forgets the old reason and is tried at once; one still set the
    /// same keeps both.
    #[test]
    fn a_bus_moved_off_a_missing_device_forgets_why() {
        let mut failed = HashMap::from([("a".to_string(), want("a")), ("b".to_string(), want("b"))]);
        let mut said = HashMap::from([
            ("a".to_string(), "no sound device called 'Solo'".to_string()),
            ("b".to_string(), "no sound device called 'Solo'".to_string()),
        ]);
        let now = std::time::Instant::now();
        let mut tried = HashMap::from([("a".to_string(), now), ("b".to_string(), now)]);
        let mut moved = want("a");
        moved.device = "Scarlett".into();
        super::forget_changed(&[moved, want("b")], &mut failed, &mut said, &mut tried);
        assert!(!said.contains_key("a") && !tried.contains_key("a") && !failed.contains_key("a"));
        assert!(said.contains_key("b") && tried.contains_key("b"), "b is set as it was");
        let mut other_pair = want("b");
        other_pair.first_channel = 0;
        super::forget_changed(&[other_pair], &mut failed, &mut said, &mut tried);
        assert!(said.is_empty() && tried.is_empty() && failed.is_empty());
    }

    /// A bus shows why it does not play as set: its live mixing refused,
    /// then its device's own note (another pair); a missing device, why it
    /// would not open; one playing as set, nothing.
    #[test]
    fn a_bus_not_playing_as_set_says_why() {
        let wanted = [want("a"), want("b"), want("c"), want("d")];
        let mut devices: HashMap<String, (super::Want, Box<dyn super::Playing>, bool)> =
            HashMap::new();
        devices.insert("a".into(), (want("a"), Box::new(Fake(Some("on 1-2"))), false));
        devices.insert("b".into(), (want("b"), Box::new(Fake(None)), true));
        let refused = HashMap::from([("a".to_string(), "Live refused (x)".to_string())]);
        let said = HashMap::from([
            ("c".to_string(), "no sound device called 'Solo'".to_string()),
            ("b".to_string(), "said before it opened".to_string()),
        ]);
        assert_eq!(
            super::notes(&wanted, &devices, &refused, &said),
            vec![
                ("a".to_string(), "Live refused (x); on 1-2".to_string()),
                ("c".to_string(), "no sound device called 'Solo'".to_string()),
            ]
        );
    }
}
