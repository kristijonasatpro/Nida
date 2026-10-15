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
}

impl Playing for super::device::DeviceOutput {
    fn failed(&self) -> bool {
        Self::failed(self)
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
}

impl PartialEq for Want {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
            && self.device == other.device
            && self.first_channel == other.first_channel
            && Arc::ptr_eq(&self.ring, &other.ring)
    }
}

/// What the render thread and the device thread share.
#[derive(Default)]
struct Shared {
    wanted: Mutex<Vec<Want>>,
    /// The keys whose devices are playing.
    playing: Mutex<Vec<String>>,
    /// Counts changes to `playing`.
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
        true
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

/// The device thread: keep open what is wanted, looking for a missing device
/// every [`RETRY`]; drop what is not, or has failed.
fn run(shared: &Shared, woken: &mpsc::Receiver<()>, mut open: Opener) {
    let mut devices: HashMap<String, (Want, Box<dyn Playing>)> = HashMap::new();
    let mut tried: HashMap<String, Instant> = HashMap::new();
    let mut published: Vec<String> = Vec::new();
    while !shared.stop.load(Ordering::Relaxed) {
        let wanted = shared.wanted.lock().map(|w| w.clone()).unwrap_or_default();
        devices.retain(|key, (want, device)| {
            wanted.iter().any(|w| w.key == *key && w == want) && !device.failed()
        });
        if shared.retry_now.swap(false, Ordering::Relaxed) {
            tried.clear();
        }
        for want in &wanted {
            if devices.contains_key(&want.key)
                || tried.get(&want.key).is_some_and(|t| t.elapsed() < RETRY)
            {
                continue;
            }
            tried.insert(want.key.clone(), Instant::now());
            match open(&want.device, want.first_channel, Arc::clone(&want.ring)) {
                Ok(device) => {
                    devices.insert(want.key.clone(), (want.clone(), device));
                }
                Err(e) => log::warn!("Audio bus output: {e}"),
            }
            if shared.stop.load(Ordering::Relaxed) {
                return;
            }
        }
        let mut playing: Vec<String> = devices.keys().cloned().collect();
        playing.sort();
        if playing != published {
            if let Ok(mut shared_playing) = shared.playing.lock() {
                shared_playing.clone_from(&playing);
            }
            shared.generation.fetch_add(1, Ordering::Release);
            published = playing;
        }
        // Woken by a change of wants; otherwise, in time to notice a failed
        // device and to retry a missing one.
        let _ = woken.recv_timeout(Duration::from_millis(250));
    }
}
