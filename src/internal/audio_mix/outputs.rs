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

/// What opens a device: `device::open`, or a stand-in for tests.
pub(super) type Opener =
    Box<dyn FnMut(&str, u16, Arc<OutputRing>) -> anyhow::Result<Box<dyn Playing>> + Send>;

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
    pub fn spawn(open: Opener) -> Self {
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

/// Whether `device`, hushed since `since`, has played silence long enough.
fn silent_enough(device: &dyn Playing, since: Instant) -> bool {
    since.elapsed() >= HUSH_MOST
        || match device.silent_frames() {
            Some((silent, call)) => silent >= HUSH_FRAMES + 2 * call,
            None => since.elapsed() >= HUSH,
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
fn run(shared: &Shared, woken: &mpsc::Receiver<()>, mut open: Opener) {
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
                let done = silent_enough(device.as_ref(), h.since);
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
                    .all(|key| hushing.get(*key).is_none_or(|h| silent_enough(devices[*key].1.as_ref(), h.since)));
                if !quiet {
                    continue;
                }
                waiting_open.remove(&want.key);
            }
            tried.insert(want.key.clone(), Instant::now());
            let opened = match &want.live {
                Some(live) => super::device::open_live(
                    &want.device,
                    want.first_channel,
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
                .map(|d| (Box::new(d) as Box<dyn Playing>, true))
                .or_else(|e| {
                    // Played the usual way, from its ring, rather than not
                    // at all; the mix told so (see `is_playing_live`), and
                    // the bus shows why (see `notes`).
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
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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

    /// Waits up to `most` for `done`.
    fn wait_for(most: std::time::Duration, done: impl Fn() -> bool) {
        let start = std::time::Instant::now();
        while !done() && start.elapsed() < most {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    /// Going live undone while its stream plays silence: it opens again as
    /// wanted, not left silent.
    #[test]
    fn a_change_undone_while_silent_opens_again() {
        type Flags = std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<std::sync::atomic::AtomicBool>>>>;
        let flags: Flags = std::sync::Arc::default();
        let made = std::sync::Arc::clone(&flags);
        let mut outputs = super::Outputs::spawn(Box::new(move |_, _, _| {
            let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            made.lock().unwrap().push(std::sync::Arc::clone(&flag));
            Ok(Box::new(Hushable(flag, std::sync::Arc::default())))
        }));
        let count = || flags.lock().unwrap().len();
        let usual = want("a");
        outputs.want(vec![usual.clone()]);
        wait_for(std::time::Duration::from_secs(1), || count() == 1);
        let mut live = live_want("a", 64);
        live.ring = std::sync::Arc::clone(&usual.ring);
        outputs.want(vec![live]);
        wait_for(std::time::Duration::from_secs(1), || {
            flags.lock().unwrap()[0].load(std::sync::atomic::Ordering::Relaxed)
        });
        // Wanted exactly as it was: nothing to close it for but the silence.
        outputs.want(vec![usual]);
        wait_for(std::time::Duration::from_secs(1), || count() == 2);
        assert_eq!(count(), 2, "opened again");
        let last = std::sync::Arc::clone(flags.lock().unwrap().last().unwrap());
        assert!(!last.load(std::sync::atomic::Ordering::Relaxed), "not left silent");
    }

    /// Another stream on the same device goes silent too while one waits to
    /// go live with a smaller buffer, and comes back once that has opened.
    #[test]
    fn other_streams_on_the_device_go_silent_and_come_back() {
        let b_hushed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let b_back = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (bh, bb) = (std::sync::Arc::clone(&b_hushed), std::sync::Arc::clone(&b_back));
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = std::sync::Arc::clone(&opens);
        let mut outputs = super::Outputs::spawn(Box::new(move |_, _, ring| {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // b's stream is the one with its own ring marker below.
            let is_b = std::sync::Arc::strong_count(&ring) > 0 && ring.frames.lock().unwrap().len() == 1;
            Ok(Box::new(if is_b {
                Hushable(std::sync::Arc::clone(&bh), std::sync::Arc::clone(&bb))
            } else {
                Hushable(std::sync::Arc::default(), std::sync::Arc::default())
            }))
        }));
        let b = want("b");
        b.ring.frames.lock().unwrap().push_back([0.0; 2]);
        outputs.want(vec![want("a"), b.clone()]);
        wait_for(std::time::Duration::from_secs(1), || opens.load(std::sync::atomic::Ordering::Relaxed) == 2);
        let asked = std::time::Instant::now();
        // And c, new on the same device meanwhile: it waits for the silence.
        outputs.want(vec![live_want("a", 64), b, want("c")]);
        wait_for(std::time::Duration::from_secs(1), || b_hushed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(b_hushed.load(std::sync::atomic::Ordering::Relaxed), "b silent too");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(opens.load(std::sync::atomic::Ordering::Relaxed), 2, "c not opened during the silence");
        wait_for(std::time::Duration::from_secs(2), || b_back.load(std::sync::atomic::Ordering::Relaxed) == 1);
        assert!(asked.elapsed() >= super::HUSH, "after the silence");
        assert_eq!(b_back.load(std::sync::atomic::Ordering::Relaxed), 1, "b back");
        assert!(!b_hushed.load(std::sync::atomic::Ordering::Relaxed));
        wait_for(std::time::Duration::from_secs(1), || opens.load(std::sync::atomic::Ordering::Relaxed) == 4);
        assert_eq!(opens.load(std::sync::atomic::Ordering::Relaxed), 4, "a opened again and c, b never closed");
    }

    /// After its device's silence, a stream's replacement opens before a
    /// stream new on that device, whatever their order in the list.
    #[test]
    fn a_replacement_opens_before_anything_new_on_its_device() {
        let order: std::sync::Arc<std::sync::Mutex<Vec<usize>>> = std::sync::Arc::default();
        let seen = std::sync::Arc::clone(&order);
        let mut outputs = super::Outputs::spawn(Box::new(move |_, _, ring| {
            // Which stream: told by how many frames its ring was given below.
            seen.lock().unwrap().push(ring.frames.lock().unwrap().len());
            Ok(Box::new(Hushable(std::sync::Arc::default(), std::sync::Arc::default())))
        }));
        let a = want("a");
        a.ring.frames.lock().unwrap().push_back([0.0; 2]);
        outputs.want(vec![a.clone()]);
        wait_for(std::time::Duration::from_secs(1), || order.lock().unwrap().len() == 1);
        let mut live = live_want("a", 64);
        live.ring = std::sync::Arc::clone(&a.ring);
        let c = want("c");
        c.ring.frames.lock().unwrap().extend([[0.0; 2]; 2]);
        outputs.want(vec![c, live]);
        wait_for(std::time::Duration::from_secs(2), || order.lock().unwrap().len() == 3);
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
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = std::sync::Arc::clone(&opens);
        let mut outputs = super::Outputs::spawn(Box::new(move |_, _, _| {
            let first = counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 0;
            Ok(Box::new(if first {
                Hushable(std::sync::Arc::clone(&bh), std::sync::Arc::clone(&bb))
            } else {
                Hushable(std::sync::Arc::default(), std::sync::Arc::default())
            }))
        }));
        let b = want("b");
        outputs.want(vec![b.clone()]);
        wait_for(std::time::Duration::from_secs(1), || opens.load(std::sync::atomic::Ordering::Relaxed) == 1);
        let asked = std::time::Instant::now();
        // And c, wanted after it on the same device: it waits too.
        outputs.want(vec![b, live_want("a", 64), want("c")]);
        wait_for(std::time::Duration::from_secs(1), || b_hushed.load(std::sync::atomic::Ordering::Relaxed));
        assert!(b_hushed.load(std::sync::atomic::Ordering::Relaxed), "b silent first");
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(opens.load(std::sync::atomic::Ordering::Relaxed), 1, "nothing opened during the silence");
        wait_for(std::time::Duration::from_secs(2), || opens.load(std::sync::atomic::Ordering::Relaxed) == 3);
        assert!(asked.elapsed() >= super::HUSH, "a and c opened after the silence: {:?}", asked.elapsed());
        wait_for(std::time::Duration::from_secs(1), || b_back.load(std::sync::atomic::Ordering::Relaxed) == 1);
        assert!(!b_hushed.load(std::sync::atomic::Ordering::Relaxed), "b back");
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

    /// A stream on the usual way, wanted live on the same device (a smaller
    /// buffer): it plays silence first — hushed at once, closed and opened
    /// live only after the device's replay has nothing but silence to give.
    #[test]
    fn a_stream_going_live_plays_silence_before_it_closes() {

        let hushed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let (flag, counted) = (std::sync::Arc::clone(&hushed), std::sync::Arc::clone(&opens));
        let mut outputs = super::Outputs::spawn(Box::new(move |_, _, _| {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Box::new(Hushable(std::sync::Arc::clone(&flag), std::sync::Arc::default())))
        }));
        let first = want("a");
        outputs.want(vec![first.clone()]);
        let start = std::time::Instant::now();
        while opens.load(std::sync::atomic::Ordering::Relaxed) < 1 && start.elapsed() < std::time::Duration::from_secs(1) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let _ = first;
        let live = live_want("a", 64);
        let asked = std::time::Instant::now();
        outputs.want(vec![live]);
        while !hushed.load(std::sync::atomic::Ordering::Relaxed) && asked.elapsed() < std::time::Duration::from_secs(1) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(asked.elapsed() < std::time::Duration::from_millis(200), "hushed at once");
        while opens.load(std::sync::atomic::Ordering::Relaxed) < 2 && asked.elapsed() < std::time::Duration::from_secs(2) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let reopened = asked.elapsed();
        assert!(
            reopened >= super::HUSH && reopened < std::time::Duration::from_millis(800),
            "opened again after the silence: {reopened:?}"
        );
    }

    /// A device closed to open as now wanted (another pair, Live or its
    /// buffer switched) opens again at once, not after the two seconds a
    /// missing device waits — even just after it opened.
    #[test]
    fn a_device_wanted_otherwise_opens_again_at_once() {
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = std::sync::Arc::clone(&opens);
        let mut outputs = super::Outputs::spawn(Box::new(move |_, _, _| {
            counted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Ok(Box::new(Fake(None)))
        }));
        let until = |n: u32| {
            let end = std::time::Instant::now() + std::time::Duration::from_millis(1_500);
            while opens.load(std::sync::atomic::Ordering::Relaxed) < n && std::time::Instant::now() < end {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            opens.load(std::sync::atomic::Ordering::Relaxed)
        };
        let first = want("a");
        outputs.want(vec![first.clone()]);
        assert_eq!(until(1), 1, "opened");
        let mut moved = first;
        moved.first_channel = 0;
        outputs.want(vec![moved]);
        assert_eq!(until(2), 2, "opened again within 1.5 s, not after the 2 s retry");
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
