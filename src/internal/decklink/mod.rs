//! Blackmagic video outputs on macOS and Windows: a DeckLink card or box —
//! an UltraStudio Monitor 3G on
//! Thunderbolt — as one more screen for the show, beside the two a laptop
//! drives itself (an M1 Pro, two). Its picture goes out on HDMI and SDI at
//! once, in one of the device's own video formats (1080p60 …).
//!
//! Each frame goes to the device as soon as it is read back, and the device
//! puts it up at its next frame (`DisplayVideoFrameSync`): no frames queued
//! ahead, so no delay added — and, the device's clock being its own, a frame
//! now and then shown twice or not at all, as on any screen not clocked by
//! the show. The device is driven from a thread of the output's own, so the
//! show's frames never wait for it.
//!
//! The DeckLink API comes with Blackmagic's Desktop Video and is loaded when
//! asked for: without it there are no devices, and nothing else changes. Its
//! headers are Blackmagic's DeckLink SDK 12.0 (`vendor/decklink-sdk`, under
//! Blackmagic's licence, as OBS ships them).

use std::sync::{Arc, Condvar, Mutex, PoisonError};

#[cfg(any(not(target_os = "macos"), test))]
mod convert;

/// One of a device's video formats.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DeckLinkMode {
    /// As Blackmagic names it: "1080p60", "1080p59.94" …
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Frames a second.
    pub fps: f64,
    /// The API's code for it.
    #[serde(skip)]
    pub code: u32,
}

/// A device that can put out video, and its formats.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DeckLinkDevice {
    pub name: String,
    pub modes: Vec<DeckLinkMode>,
    /// This arrival of it, as discovery numbers them: the same device
    /// plugged in again is another — an output opens on one arrival.
    #[serde(skip)]
    pub arrival: u64,
}

impl DeckLinkDevice {
    /// The format for a show at `fps`: 1080p at that rate, else the 1080p
    /// nearest it, else the first there is.
    pub fn mode_for(&self, fps: u32) -> Option<&DeckLinkMode> {
        let progressive_hd =
            |m: &&DeckLinkMode| m.width == 1920 && m.height == 1080 && m.name.contains('p');
        self.modes
            .iter()
            .filter(progressive_hd)
            .min_by(|a, b| {
                (a.fps - f64::from(fps))
                    .abs()
                    .total_cmp(&(b.fps - f64::from(fps)).abs())
            })
            .or_else(|| self.modes.first())
    }
}

/// How a device is named among the screens a bus can go to: "UltraStudio
/// Monitor 3G (Blackmagic)".
pub fn screen_label(device: &str) -> String {
    format!("{device} (Blackmagic)")
}

/// The DeckLink devices that can put out video, with their formats: none
/// without Desktop Video. As the API's discovery has them — it says when a
/// device arrives and goes, and its name and formats are read once, then —
/// so asking is cheap and never reaches the driver (see the shim: looking
/// for devices again and again while an output ran crashed Nida).
pub fn devices() -> Vec<DeckLinkDevice> {
    sdk::devices()
}

/// How often the devices discovery has are copied for the show: a box
/// plugged in or out is seen within it.
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_secs(1);

/// The devices as last copied, at once: copied every second
/// on a thread of their own, started by the first ask.
pub fn known_devices() -> Vec<DeckLinkDevice> {
    static KNOWN: std::sync::OnceLock<Arc<Mutex<Vec<DeckLinkDevice>>>> = std::sync::OnceLock::new();
    let known = KNOWN.get_or_init(|| {
        let known = Arc::new(Mutex::new(Vec::new()));
        let theirs = Arc::clone(&known);
        let spawned = std::thread::Builder::new()
            .name("blackmagic-devices".into())
            .spawn(move || {
                let mut told = None;
                loop {
                    let found = devices();
                    tell(&found, &mut told);
                    *theirs.lock().unwrap_or_else(PoisonError::into_inner) = found;
                    std::thread::sleep(LOOK_AGAIN);
                }
            });
        if let Err(e) = spawned {
            log::warn!("Blackmagic devices not looked for: {e}");
        }
        known
    });
    known.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Log what Blackmagic devices there are — Desktop Video's version, every
/// device and those that can put out video — when it changes from `told`:
/// what a device missing from the outputs panel is looked into with.
fn tell(found: &[DeckLinkDevice], told: &mut Option<Vec<String>>) {
    let names: Vec<String> = found.iter().map(|d| d.name.clone()).collect();
    if told.as_ref() == Some(&names) {
        return;
    }
    let Some(version) = sdk::api_version() else {
        if told.is_none() {
            log::debug!("Blackmagic: no Desktop Video, so no devices");
        }
        *told = Some(names);
        return;
    };
    let shown: Vec<String> = found
        .iter()
        .map(|d| format!("{} ({} formats)", d.name, d.modes.len()))
        .collect();
    log::info!(
        "Blackmagic: Desktop Video {version}; {} device(s), {} with video out: {}",
        sdk::all_device_count(),
        found.len(),
        if shown.is_empty() {
            "none".to_string()
        } else {
            shown.join(", ")
        }
    );
    *told = Some(names);
}

/// Why a device did not take a picture: its own error code, or a picture
/// not the format's size. Said in words only when the log is told — the
/// device's thread runs in real time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Refused {
    Code(i32),
    TooSmall,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Code(code) => write!(f, "error {code}"),
            Self::TooSmall => f.write_str("a picture smaller than the format"),
        }
    }
}

/// A device that shows pictures — the real one, or one for tests.
pub(crate) trait Screen: Send {
    /// Put up one 8-bit YUV ('2vuy') picture, `row_bytes` a line.
    fn show(&mut self, uyvy: &[u8], row_bytes: usize) -> Result<(), Refused>;

    /// What the device itself says it is doing, for the output's log.
    fn report(&mut self) -> Option<String> {
        None
    }
}

/// Turns the RGBA pictures read back into the 8-bit YUV a device takes —
/// '2vuy', video range, BT.709 from 720 lines (BT.601 below): Apple's
/// Accelerate on a Mac, about a millisecond for 1080p; Nida's own loop
/// elsewhere.
pub(crate) struct Converter {
    inner: sdk::Converter,
    width: u32,
    height: u32,
    /// Full levels, not video (see `OutputTarget::DeckLink`).
    full_range: bool,
}

impl Converter {
    pub(crate) fn new(width: u32, height: u32, full_range: bool) -> Result<Self, String> {
        Ok(Self {
            inner: sdk::Converter::new(height, full_range)?,
            width,
            height,
            full_range,
        })
    }

    /// Convert `rgba`, `width`×`height` at 4 bytes a pixel, into `uyvy`, at 2.
    pub(crate) fn run(&mut self, rgba: &[u8], uyvy: &mut [u8]) -> Result<(), String> {
        if self.width == 0 || !self.width.is_multiple_of(2) || self.height == 0 {
            return Err("DeckLink UYVY needs a nonzero picture with an even width".into());
        }
        let bytes = (self.width as usize)
            .checked_mul(self.height as usize)
            .and_then(|p| p.checked_mul(4))
            .ok_or_else(|| "a picture too large".to_string())?;
        if rgba.len() < bytes || uyvy.len() < bytes / 2 {
            return Err(format!("a picture not {}×{}", self.width, self.height));
        }
        self.inner.run(
            &rgba[..bytes],
            &mut uyvy[..bytes / 2],
            self.width,
            self.height,
        )
    }
}

/// How long something took, picture by picture, since it was last told.
#[derive(Default, Clone, Copy)]
struct Took {
    count: u32,
    total: std::time::Duration,
    most: std::time::Duration,
}

impl Took {
    fn add(&mut self, took: std::time::Duration) {
        self.count += 1;
        self.total += took;
        self.most = self.most.max(took);
    }
}

impl std::fmt::Display for Took {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mean = if self.count == 0 {
            0.0
        } else {
            self.total.as_secs_f64() * 1000.0 / f64::from(self.count)
        };
        write!(
            f,
            "{mean:.1} ms (most {:.1})",
            self.most.as_secs_f64() * 1000.0
        )
    }
}

/// The pictures between the show and a Blackmagic output's threads, and how
/// they fare.
#[derive(Default)]
struct Slot {
    /// The newest picture, RGBA, not yet converted.
    picture: Option<Vec<u8>>,
    /// The memory of pictures converted, for the next — at most
    /// [`SPARE_PICTURES`]; any more are let go outside the lock.
    spares: Vec<Vec<u8>>,
    /// The newest picture converted, not yet put up.
    converted: Option<Vec<u8>>,
    /// Converted pictures' memory, for the next.
    spare_converted: Vec<Vec<u8>>,
    stop: bool,
    shown: u64,
    /// Pictures let go for a newer: before they were converted, or after but
    /// before the device took them.
    replaced: u64,
    /// The levels pictures are converted to: full, or video.
    full_range: bool,
    /// Pictures the device has not taken since it last took one.
    failing: u32,
    /// Shared, so the show reads it without copying it under the lock.
    failed: Option<Arc<str>>,
    /// When the device's thread handed it the picture it is taking now.
    showing_since: Option<std::time::Instant>,
    /// Conversions, and the device taking pictures, since the log was told:
    /// how long each took, and of that how long on a CPU — the rest waiting,
    /// to run or on the device.
    convert: Took,
    convert_cpu: Took,
    device: Took,
    device_cpu: Took,
    /// When the show last handed a picture over, and the time between
    /// pictures since the log was told: late ones the show's, not the
    /// output's.
    handed_at: Option<std::time::Instant>,
    between: Took,
    /// When the picture waiting, and the one converted, were handed over;
    /// and how long pictures waited, from then until the device took them.
    picture_at: Option<std::time::Instant>,
    converted_at: Option<std::time::Instant>,
    age: Took,
    /// What the device last said it is doing, for the log: shared, so the
    /// log takes it without copying it under the lock.
    doing: Option<Arc<str>>,
}

/// Pictures' memory kept for the next: one being filled while one waits
/// and one is converted needs no more.
const SPARE_PICTURES: usize = 2;

/// Keep `picture`'s memory for the next, if there is room: else it is given
/// back, to be let go once the lock is.
fn keep(spares: &mut Vec<Vec<u8>>, picture: Vec<u8>) -> Option<Vec<u8>> {
    if spares.len() < SPARE_PICTURES {
        spares.push(picture);
        None
    } else {
        Some(picture)
    }
}

/// Pictures in a row a device may not take — half a second's — before its
/// output gives up on it: unplugged, say, not a moment's hiccup.
const GIVE_UP_AFTER: u32 = 30;

/// How long a device may take one picture — some 12 ms, a slow one tens —
/// before it is taken for gone: unplugged mid-picture, it may never return.
const HUNG_AFTER: std::time::Duration = std::time::Duration::from_secs(1);

/// How often a running Blackmagic output tells the log how it fares.
const REPORT_EVERY: std::time::Duration = std::time::Duration::from_secs(10);

/// What of a CPU each of an output's threads may need a frame at 1080p, and
/// how soon after it is woken: the conversion; the device's thread — a copy
/// and the call, its wait for the device's frame needing none. Run in real
/// time (see `presenter::run_in_real_time`); more pixels, more.
const CONVERT_BUDGET: (std::time::Duration, std::time::Duration) = (
    std::time::Duration::from_millis(3),
    std::time::Duration::from_millis(6),
);
const DEVICE_BUDGET: (std::time::Duration, std::time::Duration) = (
    std::time::Duration::from_millis(2),
    std::time::Duration::from_millis(4),
);

/// `budget` for pictures `width`×`height`, a frame lasting `frame`: more for
/// more pixels than 1080p's, never past the frame.
fn budget_for(
    budget: (std::time::Duration, std::time::Duration),
    frame: std::time::Duration,
    width: u32,
    height: u32,
) -> (std::time::Duration, std::time::Duration) {
    let scale = (f64::from(width) * f64::from(height) / (1920.0 * 1080.0)).max(1.0);
    let constraint = budget.1.mul_f64(scale).min(frame);
    (budget.0.mul_f64(scale).min(constraint), constraint)
}

/// The calling thread's own run in real time, a frame lasting `frame`, if
/// given; else at interactive priority.
fn run_on_time(
    what: &str,
    frame: Option<std::time::Duration>,
    budget: (std::time::Duration, std::time::Duration),
    width: u32,
    height: u32,
) {
    match frame {
        Some(frame) => {
            let (computation, constraint) = budget_for(budget, frame, width, height);
            crate::renderer::presenter::run_in_real_time(what, frame, computation, constraint);
        }
        None => crate::renderer::presenter::raise_priority(what),
    }
}

/// The CPU time the calling thread has had: against how long a step took,
/// the work in it, not the waiting.
#[cfg(unix)]
fn thread_cpu() -> std::time::Duration {
    let mut at = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: fills the struct given.
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut at) } != 0 {
        return std::time::Duration::ZERO;
    }
    std::time::Duration::new(
        u64::try_from(at.tv_sec).unwrap_or(0),
        u32::try_from(at.tv_nsec).unwrap_or(0),
    )
}

#[cfg(not(unix))]
fn thread_cpu() -> std::time::Duration {
    std::time::Duration::ZERO
}

struct Shared {
    slot: Mutex<Slot>,
    /// A picture for the conversion thread.
    pictures: Condvar,
    /// A converted picture for the device's thread.
    converted: Condvar,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A Blackmagic output while it runs: a thread turning each picture handed
/// to it into the device's 8-bit YUV, and one putting each up on the device —
/// the next converted while the one before goes to the device, which takes
/// it in some 12 ms (an UltraStudio Monitor 3G at 1080p), so that together
/// they keep up with 60 a second. Dropped, it stops, and the device is let go.
pub struct DeckLinkSender {
    /// The output's name, for the log.
    name: String,
    /// Its claim on its device, once its thread has it (see `let_go`).
    claim: Option<ClaimSlot>,
    shared: Arc<Shared>,
    /// The thread putting pictures up, and the one converting them.
    threads: Vec<std::thread::JoinHandle<()>>,
    /// The size its pictures must be: the format's.
    pub width: u32,
    pub height: u32,
    /// The format's frame time, milliseconds: the device's refresh.
    pub refresh_ms: Option<f64>,
    /// Where the log was last told how the output fares.
    told: Mutex<Told>,
}

impl DeckLinkSender {
    /// Start putting the output `name`'s pictures up on the device named
    /// `device`, in its format named `mode`. The device is opened on the
    /// output's own thread, the show's frames not waiting for it: one that
    /// will not open says so in [`failed`](Self::failed).
    ///
    /// # Errors
    ///
    /// When there is no such device or format.
    pub fn start(name: &str, device: &str, mode: &str, full_range: bool) -> anyhow::Result<Self> {
        let find =
            |devices: Vec<DeckLinkDevice>| devices.into_iter().find(|d| d.name == device);
        // As last copied, at once; else as discovery has it this moment — a
        // box that has just arrived.
        let found = find(known_devices())
            .or_else(|| find(devices()))
            .ok_or_else(|| anyhow::anyhow!("no Blackmagic device named '{device}'"))?;
        let format = found
            .modes
            .iter()
            .find(|m| m.name == mode)
            .ok_or_else(|| anyhow::anyhow!("'{device}' has no format named '{mode}'"))?
            .clone();
        let arrival = found.arrival;
        let (code, width, height) = (format.code, format.width, format.height);
        let frame = (format.fps > 0.0).then(|| std::time::Duration::from_secs_f64(1.0 / format.fps));
        let device = device.to_string();
        let claim: ClaimSlot = Arc::default();
        let ours = Arc::clone(&claim);
        let mut sender = Self::start_with(name, width, height, full_range, frame, move || {
            // Waits while an output just stopped still lets the device go.
            let held = hold_device(&device, arrival, DEVICE_FREED_WITHIN)?;
            *ours.lock().unwrap_or_else(PoisonError::into_inner) =
                Some((held.device.clone(), held.token));
            let screen = sdk::open(arrival, code, width, height)?;
            Ok(Box::new(Held {
                screen,
                _held: held,
            }) as Box<dyn Screen>)
        })?;
        sender.refresh_ms = (format.fps > 0.0).then(|| 1000.0 / format.fps);
        sender.claim = Some(claim);
        Ok(sender)
    }

    /// Start the threads converting the output `name`'s pictures,
    /// `width`×`height`, and putting them up on the screen `open` gives —
    /// opened on the second, which keeps it. A frame lasting `frame`, both
    /// run in real time, the second once the screen is open. One that will
    /// not open says why in [`failed`](Self::failed).
    pub(crate) fn start_with(
        name: &str,
        width: u32,
        height: u32,
        full_range: bool,
        frame: Option<std::time::Duration>,
        open: impl FnOnce() -> Result<Box<dyn Screen>, String> + Send + 'static,
    ) -> anyhow::Result<Self> {
        let mut converter =
            Converter::new(width, height, full_range).map_err(anyhow::Error::msg)?;
        let row_bytes = width as usize * 2;
        let bytes = row_bytes * height as usize;
        // The converted pictures' memory, made now: none made in real time.
        // One converting, one waiting, one going up.
        let shared = Arc::new(Shared {
            slot: Mutex::new(Slot {
                full_range,
                spare_converted: (0..3).map(|_| vec![0; bytes]).collect(),
                spares: Vec::with_capacity(SPARE_PICTURES),
                ..Slot::default()
            }),
            pictures: Condvar::new(),
            converted: Condvar::new(),
        });
        let theirs = Arc::clone(&shared);
        let output = name.to_string();
        let device = std::thread::Builder::new()
            .name(format!("nida-blackmagic-{name}"))
            .spawn(move || {
                crate::renderer::presenter::raise_priority("A Blackmagic output");
                match open() {
                    Ok(mut screen) => {
                        theirs.lock().doing = screen.report().map(Arc::from);
                        run_on_time("A Blackmagic output", frame, DEVICE_BUDGET, width, height);
                        show_until_stopped(&theirs, screen.as_mut(), &output, row_bytes);
                    }
                    Err(why) => {
                        theirs.lock().failed = Some(Arc::from(format!(
                            "the Blackmagic device would not open: {why}"
                        )));
                    }
                }
            })?;
        let theirs = Arc::clone(&shared);
        let converting = std::thread::Builder::new()
            .name(format!("nida-blackmagic-{name}-yuv"))
            .spawn(move || {
                run_on_time(
                    "A Blackmagic output's conversion",
                    frame,
                    CONVERT_BUDGET,
                    width,
                    height,
                );
                convert_until_stopped(&theirs, &mut converter, bytes);
            });
        let converting = match converting {
            Ok(thread) => thread,
            Err(e) => {
                shared.lock().stop = true;
                shared.converted.notify_all();
                return Err(e.into());
            }
        };
        Ok(Self {
            name: name.to_string(),
            claim: None,
            shared,
            threads: vec![device, converting],
            width,
            height,
            refresh_ms: None,
            told: Mutex::new(Told::new()),
        })
    }

    /// The newest picture, RGBA, the format's size: put up at the device's
    /// next frame. One handed before and not yet converted is let go.
    pub fn send(&self, rgba: &[u8]) {
        // Copied in with the lock let go: the threads putting pictures up
        // on time never wait on a whole picture's copy.
        let mut picture = self.shared.lock().spares.pop().unwrap_or_default();
        picture.clear();
        picture.extend_from_slice(rgba);
        let now = std::time::Instant::now();
        let mut slot = self.shared.lock();
        if let Some(before) = slot.handed_at.replace(now) {
            slot.between.add(now.saturating_duration_since(before));
        }
        slot.picture_at = Some(now);
        let mut excess = None;
        if let Some(waiting) = slot.picture.replace(picture) {
            slot.replaced += 1;
            excess = keep(&mut slot.spares, waiting);
        }
        drop(slot);
        self.shared.pictures.notify_one();
        drop(excess);
    }

    /// Convert the pictures from now on to full levels, or video: taken up
    /// with the next picture, the output running on.
    pub fn set_full_range(&self, full_range: bool) {
        self.shared.lock().full_range = full_range;
    }

    /// Pictures put up, and let go for a newer before they were.
    pub fn counts(&self) -> (u64, u64) {
        let slot = self.shared.lock();
        (slot.shown, slot.replaced)
    }

    /// Tell the log how the output has fared, every [`REPORT_EVERY`] — from
    /// the show's side, the output's own threads kept to their pictures.
    pub fn tell_if_due(&self) {
        let line = self
            .told
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .due(&self.shared, &self.name, std::time::Instant::now());
        if let Some(line) = line {
            log::info!("{line}");
        }
    }

    /// Why the device would not open, stopped taking pictures, or stopped
    /// answering, if so.
    pub fn failed(&self) -> Option<String> {
        let (failed, showing_since) = {
            let slot = self.shared.lock();
            (slot.failed.clone(), slot.showing_since)
        };
        if let Some(why) = failed {
            return Some(why.to_string());
        }
        showing_since
            .filter(|at| at.elapsed() >= HUNG_AFTER)
            .map(|_| "the Blackmagic device stopped responding — unplugged?".to_string())
    }
}

impl Drop for DeckLinkSender {
    /// Tells the output's threads to stop, and does not wait for them: the
    /// device is let go there, the show's frames going on meanwhile.
    fn drop(&mut self) {
        let shown = {
            let mut slot = self.shared.lock();
            slot.stop = true;
            slot.shown
        };
        self.shared.pictures.notify_all();
        self.shared.converted.notify_all();
        self.threads.clear();
        // Its thread lets the device go — or, stuck in it, never does: then
        // the next output to want the device takes it over (`hold_device`).
        let claim = self
            .claim
            .as_ref()
            .and_then(|c| c.lock().unwrap_or_else(PoisonError::into_inner).clone());
        if let Some((device, token)) = claim {
            let_go(&device, token);
        }
        log::info!(
            "Blackmagic output '{}': its device let go, {shown} pictures put up",
            self.name
        );
    }
}

/// How long an output waits for a device another is letting go of.
const DEVICE_FREED_WITHIN: std::time::Duration = std::time::Duration::from_secs(3);

/// Where an output's thread puts its claim — device and token — once it
/// has one, for the output to let go of when stopped.
type ClaimSlot = Arc<Mutex<Option<(String, u64)>>>;

/// A running output's claim on a device: one output a device.
struct Claim {
    device: String,
    /// The arrival of the device it was made on (see `DeckLinkDevice::arrival`).
    arrival: u64,
    token: u64,
    /// When its output was stopped, its thread told to let the device go.
    let_go_at: Option<std::time::Instant>,
}

static CLAIMS: Mutex<Vec<Claim>> = Mutex::new(Vec::new());
static NEXT_CLAIM: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn claims() -> std::sync::MutexGuard<'static, Vec<Claim>> {
    CLAIMS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A device claimed by an output; given up as dropped — its own claim only,
/// never one taken over from it meanwhile.
struct DeviceHold {
    device: String,
    token: u64,
}

impl Drop for DeviceHold {
    fn drop(&mut self) {
        claims().retain(|c| !(c.device == self.device && c.token == self.token));
    }
}

/// Claim the device named `device`, as it arrived `arrival`, for an output:
/// at once when free; else once the output that has it lets it go — a stop
/// just before a start. An output stopped on an earlier arrival of the device
/// — unplugged mid-picture, its thread stuck in the device for good — has
/// its claim taken over; one on this same arrival never: its thread may yet
/// let go of the device under the new output's feet. Still not free
/// `within`, not at all: it is another output's.
fn hold_device(
    device: &str,
    arrival: u64,
    within: std::time::Duration,
) -> Result<DeviceHold, String> {
    let until = std::time::Instant::now() + within;
    loop {
        {
            let mut held = claims();
            let free = match held.iter().position(|c| c.device == device) {
                None => true,
                Some(at) if held[at].arrival != arrival && held[at].let_go_at.is_some() => {
                    log::warn!(
                        "Blackmagic: '{device}' was never let go by the output stopped on it \
                         before it was plugged in again (stuck in the device); taken over"
                    );
                    held.remove(at);
                    true
                }
                Some(_) => false,
            };
            if free {
                let token = NEXT_CLAIM.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                held.push(Claim {
                    device: device.to_string(),
                    arrival,
                    token,
                    let_go_at: None,
                });
                return Ok(DeviceHold {
                    device: device.to_string(),
                    token,
                });
            }
        }
        if std::time::Instant::now() >= until {
            return Err(format!("'{device}' is in use by another output"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The claim `token` on `device` is let go: its output was stopped.
fn let_go(device: &str, token: u64) {
    if let Some(claim) = claims()
        .iter_mut()
        .find(|c| c.device == device && c.token == token)
    {
        claim.let_go_at = Some(std::time::Instant::now());
    }
}

/// A screen and the hold on its device, let go after it.
struct Held {
    screen: Box<dyn Screen>,
    _held: DeviceHold,
}

impl Screen for Held {
    fn show(&mut self, uyvy: &[u8], row_bytes: usize) -> Result<(), Refused> {
        self.screen.show(uyvy, row_bytes)
    }

    fn report(&mut self) -> Option<String> {
        self.screen.report()
    }
}

/// The output's conversion thread: each picture handed over turned into
/// 8-bit YUV for the device's thread — while that thread puts the one before
/// up — until told to stop.
fn convert_until_stopped(shared: &Shared, converter: &mut Converter, bytes: usize) {
    // Levels the converter could not be made for: not tried again, nor
    // said again, for every picture.
    let mut refused_levels = None;
    loop {
        let (picture, handed_at, mut yuv, full_range) = {
            let mut slot = shared.lock();
            loop {
                if slot.stop {
                    return;
                }
                if let Some(picture) = slot.picture.take() {
                    let yuv = slot.spare_converted.pop().unwrap_or_default();
                    break (picture, slot.picture_at.take(), yuv, slot.full_range);
                }
                slot = shared
                    .pictures
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        if full_range != converter.full_range && refused_levels != Some(full_range) {
            match Converter::new(converter.width, converter.height, full_range) {
                Ok(other) => {
                    *converter = other;
                    refused_levels = None;
                }
                Err(why) => {
                    log::warn!("Blackmagic: levels not changed: {why}");
                    refused_levels = Some(full_range);
                }
            }
        }
        yuv.resize(bytes, 0);
        // The CPU time measured within the time taken, never past it.
        let started = std::time::Instant::now();
        let cpu = thread_cpu();
        let done = converter.run(&picture, &mut yuv);
        let cpu = thread_cpu().saturating_sub(cpu);
        let took = started.elapsed();
        let mut slot = shared.lock();
        let excess = keep(&mut slot.spares, picture);
        if done.is_ok() {
            slot.convert.add(took);
            slot.convert_cpu.add(cpu);
            slot.converted_at = handed_at;
            if let Some(older) = slot.converted.replace(yuv) {
                slot.replaced += 1;
                slot.spare_converted.push(older);
            }
            drop(slot);
            shared.converted.notify_one();
        } else {
            // Not the format's size: handed over as the output changed.
            slot.spare_converted.push(yuv);
            drop(slot);
        }
        drop(excess);
    }
}

/// The output's device thread: each converted picture put up, until told to
/// stop. Noted in the present trace as the output `name`'s frames, taken and
/// presented, as a window's are; what the device says it is doing asked
/// again every [`REPORT_EVERY`], for the log.
fn show_until_stopped(shared: &Shared, screen: &mut dyn Screen, name: &str, row_bytes: usize) {
    use crate::renderer::present_trace::{Step, TRACE};
    // Shared, for the trace: nothing copied per frame.
    let name: Arc<str> = Arc::from(name);
    let mut frame: u64 = 0;
    let mut asked = std::time::Instant::now();
    loop {
        let (converted, handed_at) = {
            let mut slot = shared.lock();
            loop {
                if slot.stop {
                    return;
                }
                if let Some(converted) = slot.converted.take() {
                    break (converted, slot.converted_at.take());
                }
                slot = shared
                    .converted
                    .wait(slot)
                    .unwrap_or_else(PoisonError::into_inner);
            }
        };
        frame += 1;
        TRACE.note_if_free(&name, frame, Step::Taken);
        let started = std::time::Instant::now();
        {
            let mut slot = shared.lock();
            slot.showing_since = Some(started);
            if let Some(at) = handed_at {
                slot.age.add(started.saturating_duration_since(at));
            }
        }
        let cpu = thread_cpu();
        let shown = screen.show(&converted, row_bytes);
        let cpu = thread_cpu().saturating_sub(cpu);
        let took = started.elapsed();
        if shown.is_ok() {
            TRACE.note_if_free(&name, frame, Step::Presented);
        }
        let gave_up = {
            let mut slot = shared.lock();
            slot.showing_since = None;
            slot.spare_converted.push(converted);
            match shown {
                Ok(()) => {
                    slot.shown += 1;
                    slot.failing = 0;
                    slot.device.add(took);
                    slot.device_cpu.add(cpu);
                    None
                }
                Err(why) => {
                    slot.failing += 1;
                    (slot.failing >= GIVE_UP_AFTER && slot.failed.is_none()).then_some(why)
                }
            }
        };
        // Said in words once, with the lock let go.
        if let Some(why) = gave_up {
            let failed: Arc<str> = Arc::from(format!(
                "the Blackmagic device stopped taking pictures ({why}) — unplugged?"
            ));
            shared.lock().failed = Some(failed);
        }
        if asked.elapsed() >= REPORT_EVERY {
            asked = std::time::Instant::now();
            let doing = screen.report().map(Arc::from);
            // The report before let go outside the lock.
            let before = std::mem::replace(&mut shared.lock().doing, doing);
            drop(before);
        }
    }
}

/// Where the log was last told how an output fares.
struct Told {
    at: std::time::Instant,
    shown: u64,
    replaced: u64,
}

impl Told {
    fn new() -> Self {
        Self {
            at: std::time::Instant::now(),
            shown: 0,
            replaced: 0,
        }
    }

    /// How the output `name` has fared, once [`REPORT_EVERY`] has passed
    /// by `now`: pictures put up and let go; how often the show handed them
    /// over, and how long they waited before the device took them; how long
    /// converting and the device took, and of that on a CPU; what the device
    /// says it is doing.
    fn due(&mut self, shared: &Shared, name: &str, now: std::time::Instant) -> Option<String> {
        let since = now.saturating_duration_since(self.at);
        if since < REPORT_EVERY {
            return None;
        }
        let mut slot = shared.lock();
        let (shown, replaced) = (slot.shown, slot.replaced);
        let between = std::mem::take(&mut slot.between);
        let age = std::mem::take(&mut slot.age);
        let (convert, convert_cpu) = (
            std::mem::take(&mut slot.convert),
            std::mem::take(&mut slot.convert_cpu),
        );
        let (device, device_cpu) = (
            std::mem::take(&mut slot.device),
            std::mem::take(&mut slot.device_cpu),
        );
        let doing = slot.doing.clone();
        drop(slot);
        let doing = doing.map(|r| format!("; {r}")).unwrap_or_default();
        let put_up = shown - self.shown;
        #[allow(clippy::cast_precision_loss)]
        let rate = put_up as f64 / since.as_secs_f64();
        let line = format!(
            "Blackmagic output '{name}': {put_up} pictures put up in {:.1} s ({rate:.1} a second), \
             {} let go for newer; handed over every {between}, waiting {age} till the device took \
             them; converting each {convert}, on a CPU {convert_cpu}; the device taking each \
             {device}, on a CPU {device_cpu}{doing}",
            since.as_secs_f64(),
            replaced - self.replaced,
        );
        *self = Self {
            at: now,
            shown,
            replaced,
        };
        Some(line)
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod sdk {
    use super::{DeckLinkDevice, DeckLinkMode, Screen};
    use std::ffi::{c_char, c_void, CStr};

    #[repr(C)]
    struct Mode {
        code: u32,
        width: i32,
        height: i32,
        frame_duration: i64,
        time_scale: i64,
        name: [c_char; 64],
    }

    #[repr(C)]
    struct State {
        mode_name: [c_char; 64],
        mode: u32,
        frame_duration: i64,
        time_scale: i64,
        current_mode: u32,
        ticks_per_frame: i64,
    }

    extern "C" {
        fn nida_dl_state(handle: *mut c_void, state: *mut State) -> i32;
        #[cfg(target_os = "macos")]
        fn nida_dl_converter_new(height: i32, full: i32) -> *mut c_void;
        #[cfg(target_os = "macos")]
        fn nida_dl_converter_run(
            converter: *mut c_void,
            rgba: *const u8,
            row_bytes: i32,
            width: i32,
            height: i32,
            out: *mut u8,
            out_row_bytes: i32,
        ) -> i32;
        #[cfg(target_os = "macos")]
        fn nida_dl_converter_free(converter: *mut c_void);
        fn nida_dl_api_version(buf: *mut c_char, len: i32) -> i32;
        fn nida_dl_all_device_count() -> i32;
        fn nida_dl_device_count() -> i32;
        fn nida_dl_device_snapshot(
            index: i32,
            id: *mut u64,
            name: *mut c_char,
            len: i32,
            modes: *mut Mode,
            max: i32,
        ) -> i32;
        fn nida_dl_open(id: u64, mode: u32, err: *mut c_char, errlen: i32) -> *mut c_void;
        fn nida_dl_show(
            handle: *mut c_void,
            data: *const u8,
            row_bytes: i32,
            width: i32,
            height: i32,
        ) -> i32;
        fn nida_dl_close(handle: *mut c_void);
    }

    /// Desktop Video's DeckLink API version, when it is installed.
    pub(super) fn api_version() -> Option<String> {
        let mut version = [0 as c_char; 64];
        // SAFETY: a buffer of the length given.
        if unsafe { nida_dl_api_version(version.as_mut_ptr(), 64) } != 0 {
            return None;
        }
        // SAFETY: NUL-terminated by the shim.
        Some(
            unsafe { CStr::from_ptr(version.as_ptr()) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    /// Every DeckLink device, whether it can put out video or not.
    pub(super) fn all_device_count() -> i32 {
        // SAFETY: no arguments.
        unsafe { nida_dl_all_device_count() }
    }

    pub(super) fn devices() -> Vec<DeckLinkDevice> {
        // SAFETY: no arguments; loads the API when there is one.
        let count = unsafe { nida_dl_device_count() };
        (0..count)
            .filter_map(|index| {
                let mut name = [0 as c_char; 256];
                let mut modes: Vec<Mode> = (0..128)
                    .map(|_| Mode {
                        code: 0,
                        width: 0,
                        height: 0,
                        frame_duration: 0,
                        time_scale: 0,
                        name: [0; 64],
                    })
                    .collect();
                let mut arrival = 0;
                // SAFETY: live scalar and name buffer, and room for 128 modes.
                let found = unsafe {
                    nida_dl_device_snapshot(
                        index,
                        &raw mut arrival,
                        name.as_mut_ptr(),
                        256,
                        modes.as_mut_ptr(),
                        128,
                    )
                };
                if found < 0 || arrival == 0 {
                    return None;
                }
                // SAFETY: NUL-terminated by the shim, copied with a bound.
                let name = unsafe { CStr::from_ptr(name.as_ptr()) }
                    .to_string_lossy()
                    .into_owned();
                modes.truncate(usize::try_from(found).unwrap_or(0));
                let modes = modes
                    .iter()
                    .map(|m| DeckLinkMode {
                        // SAFETY: NUL-terminated by the shim.
                        name: unsafe { CStr::from_ptr(m.name.as_ptr()) }
                            .to_string_lossy()
                            .into_owned(),
                        width: u32::try_from(m.width).unwrap_or(0),
                        height: u32::try_from(m.height).unwrap_or(0),
                        #[allow(clippy::cast_precision_loss)]
                        fps: if m.frame_duration > 0 {
                            m.time_scale as f64 / m.frame_duration as f64
                        } else {
                            0.0
                        },
                        code: m.code,
                    })
                    .collect();
                Some(DeckLinkDevice {
                    name,
                    modes,
                    arrival,
                })
            })
            .collect()
    }

    struct Device {
        handle: *mut c_void,
        width: i32,
        height: i32,
    }

    // SAFETY: the handle is used by one thread at a time: the output's.
    unsafe impl Send for Device {}

    impl Screen for Device {
        fn show(&mut self, uyvy: &[u8], row_bytes: usize) -> Result<(), super::Refused> {
            let lines = usize::try_from(self.height).unwrap_or(0);
            if uyvy.len() < row_bytes.saturating_mul(lines) {
                return Err(super::Refused::TooSmall);
            }
            let row = i32::try_from(row_bytes).map_err(|_| super::Refused::TooSmall)?;
            // SAFETY: a picture of `height` lines of `row_bytes`.
            let shown =
                unsafe { nida_dl_show(self.handle, uyvy.as_ptr(), row, self.width, self.height) };
            if shown == 0 {
                Ok(())
            } else {
                Err(super::Refused::Code(shown))
            }
        }

        fn report(&mut self) -> Option<String> {
            let mut state = State {
                mode_name: [0; 64],
                mode: 0,
                frame_duration: 0,
                time_scale: 0,
                current_mode: 0,
                ticks_per_frame: 0,
            };
            // SAFETY: an open handle, on the thread that keeps it.
            if unsafe { nida_dl_state(self.handle, &raw mut state) } != 0 {
                return None;
            }
            // SAFETY: NUL-terminated by the shim (zeroed, copied with a bound).
            let name = unsafe { CStr::from_ptr(state.mode_name.as_ptr()) }.to_string_lossy();
            #[allow(clippy::cast_precision_loss)]
            let fps = if state.frame_duration > 0 {
                state.time_scale as f64 / state.frame_duration as f64
            } else {
                0.0
            };
            #[allow(clippy::cast_precision_loss)]
            let frame = if state.ticks_per_frame > 0 {
                format!("{:.3} ms", state.ticks_per_frame as f64 / 1000.0)
            } else {
                "not said".to_string()
            };
            Some(format!(
                "format {name} ({fps:.3} a second, {}); the device says it puts out {}, its clock's frame {frame}",
                fourcc(state.mode),
                fourcc(state.current_mode),
            ))
        }
    }

    /// A Blackmagic code as its four letters, quoted ('Hp60'); none, as not
    /// said.
    fn fourcc(code: u32) -> String {
        if code == 0 {
            return "nothing it says".to_string();
        }
        let letters: String = code
            .to_be_bytes()
            .iter()
            .map(|&b| {
                if b.is_ascii_graphic() || b == b' ' {
                    char::from(b)
                } else {
                    '?'
                }
            })
            .collect();
        format!("'{letters}'")
    }

    #[cfg(target_os = "windows")]
    pub(super) use super::convert::Converter;

    /// RGBA to '2vuy' with Apple's Accelerate (vImage), on the calling thread.
    #[cfg(target_os = "macos")]
    pub(super) struct Converter(*mut c_void);

    // SAFETY: the conversion is used by one thread at a time: the output's.
    #[cfg(target_os = "macos")]
    unsafe impl Send for Converter {}

    #[cfg(target_os = "macos")]
    impl Converter {
        pub(super) fn new(height: u32, full_range: bool) -> Result<Self, String> {
            let height = i32::try_from(height).map_err(|e| e.to_string())?;
            // SAFETY: no pointers; null when it cannot be made.
            let made = unsafe { nida_dl_converter_new(height, i32::from(full_range)) };
            if made.is_null() {
                Err("vImage would not make the YUV conversion".into())
            } else {
                Ok(Self(made))
            }
        }

        pub(super) fn run(
            &mut self,
            rgba: &[u8],
            uyvy: &mut [u8],
            width: u32,
            height: u32,
        ) -> Result<(), String> {
            let w = i32::try_from(width).map_err(|e| e.to_string())?;
            let h = i32::try_from(height).map_err(|e| e.to_string())?;
            let (row, out_row) = w
                .checked_mul(4)
                .zip(w.checked_mul(2))
                .ok_or_else(|| "a picture too wide".to_string())?;
            // SAFETY: the caller checked both buffers hold `width`×`height`
            // pictures at 4 and 2 bytes a pixel.
            let done = unsafe {
                nida_dl_converter_run(self.0, rgba.as_ptr(), row, w, h, uyvy.as_mut_ptr(), out_row)
            };
            if done == 0 {
                Ok(())
            } else {
                Err(format!("vImage did not convert the picture ({done})"))
            }
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for Converter {
        fn drop(&mut self) {
            // SAFETY: made by `nida_dl_converter_new`, freed once.
            unsafe { nida_dl_converter_free(self.0) };
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: opened by `nida_dl_open`, closed once.
            unsafe { nida_dl_close(self.handle) };
        }
    }

    pub(super) fn open(
        arrival: u64,
        code: u32,
        width: u32,
        height: u32,
    ) -> Result<Box<dyn Screen>, String> {
        let width = i32::try_from(width).map_err(|e| e.to_string())?;
        let height = i32::try_from(height).map_err(|e| e.to_string())?;
        let mut err = [0 as c_char; 256];
        // SAFETY: a buffer of the length given.
        let handle = unsafe { nida_dl_open(arrival, code, err.as_mut_ptr(), 256) };
        if handle.is_null() {
            // SAFETY: NUL-terminated by the shim.
            return Err(unsafe { CStr::from_ptr(err.as_ptr()) }
                .to_string_lossy()
                .into_owned());
        }
        let device = Device {
            handle,
            width,
            height,
        };
        Ok(Box::new(device))
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod sdk {
    use super::{DeckLinkDevice, Screen};

    pub(super) fn api_version() -> Option<String> {
        None
    }

    pub(super) fn all_device_count() -> i32 {
        0
    }

    pub(super) fn devices() -> Vec<DeckLinkDevice> {
        Vec::new()
    }

    pub(super) use super::convert::Converter;

    pub(super) fn open(
        _arrival: u64,
        _code: u32,
        _width: u32,
        _height: u32,
    ) -> Result<Box<dyn Screen>, String> {
        Err("Blackmagic outputs require macOS or Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn device() -> DeckLinkDevice {
        let mode = |name: &str, width, height, fps| DeckLinkMode {
            name: name.into(),
            width,
            height,
            fps,
            code: 0,
        };
        DeckLinkDevice {
            name: "UltraStudio Monitor 3G".into(),
            arrival: 1,
            modes: vec![
                mode("NTSC", 720, 486, 29.97),
                mode("1080i50", 1920, 1080, 25.0),
                mode("1080p50", 1920, 1080, 50.0),
                mode("1080p5994", 1920, 1080, 59.94),
                mode("1080p60", 1920, 1080, 60.0),
            ],
        }
    }

    /// A new output takes 1080p at the show's rate, else the 1080p nearest.
    #[test]
    fn a_new_output_takes_1080p_at_the_shows_rate() {
        let d = device();
        assert_eq!(d.mode_for(60).map(|m| m.name.as_str()), Some("1080p60"));
        assert_eq!(d.mode_for(50).map(|m| m.name.as_str()), Some("1080p50"));
        assert_eq!(
            d.mode_for(25).map(|m| m.name.as_str()),
            Some("1080p50"),
            "interlaced passed over"
        );
        let none = DeckLinkDevice {
            name: "x".into(),
            modes: vec![],
            arrival: 0,
        };
        assert!(none.mode_for(60).is_none());
    }

    /// A screen for tests: says when it starts on a picture and when it has
    /// shown it, holding each until let go when told to, failing when told
    /// to, and when it is closed.
    struct Recorder {
        started: mpsc::Sender<()>,
        shown: mpsc::Sender<Vec<u8>>,
        hold: Option<mpsc::Receiver<()>>,
        fail: Arc<std::sync::atomic::AtomicBool>,
        closed: mpsc::Sender<()>,
    }

    impl Screen for Recorder {
        fn show(&mut self, uyvy: &[u8], row_bytes: usize) -> Result<(), Refused> {
            assert_eq!(row_bytes, 4 * 2, "a line of 4 pixels, 2 bytes each");
            // Read before saying it started: the test changes it only after.
            let failing = self.fail.load(std::sync::atomic::Ordering::SeqCst);
            let _ = self.started.send(());
            if let Some(hold) = &self.hold {
                let _ = hold.recv_timeout(Duration::from_secs(5));
            }
            if failing {
                return Err(Refused::Code(-3));
            }
            let _ = self.shown.send(uyvy.to_vec());
            Ok(())
        }
    }

    impl Drop for Recorder {
        fn drop(&mut self) {
            let _ = self.closed.send(());
        }
    }

    /// The test's ends of a [`Recorder`].
    struct Ends {
        started: mpsc::Receiver<()>,
        shown: mpsc::Receiver<Vec<u8>>,
        release: mpsc::Sender<()>,
        fail: Arc<std::sync::atomic::AtomicBool>,
        closed: mpsc::Receiver<()>,
    }

    fn recorder(holding: bool) -> (Recorder, Ends) {
        let (started_tx, started) = mpsc::channel();
        let (shown_tx, shown) = mpsc::channel();
        let (release, hold) = mpsc::channel();
        let (closed_tx, closed) = mpsc::channel();
        let fail = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let screen = Recorder {
            started: started_tx,
            shown: shown_tx,
            hold: holding.then_some(hold),
            fail: Arc::clone(&fail),
            closed: closed_tx,
        };
        (
            screen,
            Ends {
                started,
                shown,
                release,
                fail,
                closed,
            },
        )
    }

    /// A frame at 60 a second: the threads run in real time, as a
    /// device's.
    const FRAME: Duration = Duration::from_micros(16_667);

    fn start(screen: Recorder) -> DeckLinkSender {
        DeckLinkSender::start_with(
            "Test",
            4,
            2,
            false,
            Some(FRAME),
            move || Ok(Box::new(screen) as Box<dyn Screen>),
        )
        .expect("started")
    }

    fn rgba(value: u8) -> Vec<u8> {
        vec![value; 4 * 2 * 4]
    }

    const WAIT: Duration = Duration::from_secs(5);

    /// Each picture handed over is put up on the device as 8-bit YUV, the
    /// format's size; one handed over while the device is busy with the one
    /// before is replaced by a newer — only the newest goes up after.
    #[test]
    fn the_newest_picture_goes_up_and_one_still_waiting_is_replaced() {
        let (screen, ends) = recorder(true);
        let sender = start(screen);
        assert_eq!((sender.width, sender.height), (4, 2));
        // White: the device takes it, and holds it.
        sender.send(&rgba(255));
        ends.started.recv_timeout(WAIT).expect("taken");
        // Two more while it is busy: the first replaced by the second.
        sender.send(&rgba(10));
        sender.send(&rgba(0));
        let until = std::time::Instant::now() + WAIT;
        while sender.counts().1 < 1 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        ends.release.send(()).unwrap();
        let first = ends.shown.recv_timeout(WAIT).expect("shown");
        assert_eq!(first.len(), 4 * 2 * 2, "UYVY: 2 bytes a pixel");
        assert_eq!(first[1], 235, "white is luma 235");
        ends.started.recv_timeout(WAIT).expect("taken");
        ends.release.send(()).unwrap();
        let next = ends.shown.recv_timeout(WAIT).expect("shown");
        assert_eq!(
            next[1], 16,
            "the newest — black, luma 16 — not the one replaced"
        );
        assert!(
            ends.started
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "nothing more"
        );
        let until = std::time::Instant::now() + WAIT;
        while sender.counts().0 < 2 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sender.counts(), (2, 1));
        assert_eq!(sender.failed(), None);
    }

    /// The next picture is converted while the device is still taking the
    /// one before — some 12 ms for an UltraStudio Monitor 3G at 1080p — not
    /// after it: one after the other, the two did not fit in a 60th of a
    /// second, and the output ran at 48 a second.
    #[test]
    fn the_next_picture_is_converted_while_the_device_takes_the_one_before() {
        let (screen, ends) = recorder(true);
        let sender = start(screen);
        sender.send(&rgba(255));
        ends.started.recv_timeout(WAIT).expect("taken");
        // The device holds white; black comes, and is converted meanwhile.
        sender.send(&rgba(0));
        let until = std::time::Instant::now() + WAIT;
        while sender.shared.lock().converted.is_none() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(
            sender.shared.lock().converted.is_some(),
            "converted only once the device was free"
        );
        ends.release.send(()).unwrap();
        ends.started
            .recv_timeout(WAIT)
            .expect("black taken at once");
        ends.release.send(()).unwrap();
    }

    /// Accelerate's conversion is the one Nida's own loop does — BT.709,
    /// video range, '2vuy' — within a step of rounding, at 1080p and below.
    #[test]
    fn the_conversion_is_bt709_video_range_2vuy() {
        for (width, height) in [(1920u32, 1080u32), (720, 486), (64, 8)] {
            let mut seed = 0x2545_f491_u32;
            let mut next = || {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed
            };
            // Flat colours first, then noise: chroma is the pair's average.
            let flat: [[u8; 4]; 6] = [
                [255, 255, 255, 255],
                [0, 0, 0, 255],
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [128, 128, 128, 255],
            ];
            let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height as usize {
                for x in 0..width as usize {
                    if let Some(colour) = flat.get(y) {
                        rgba.extend_from_slice(colour);
                    } else if y % 2 == 0 {
                        let v = next().to_le_bytes();
                        rgba.extend_from_slice(&[v[0], v[1], v[2], 255]);
                    } else {
                        // A gradient: neighbours alike, as pictures mostly are.
                        let v = u8::try_from((x * 255) / width as usize).unwrap_or(255);
                        rgba.extend_from_slice(&[v, 255 - v, v / 2, 255]);
                    }
                }
            }
            let mut ours = vec![0u8; width as usize * height as usize * 2];
            crate::ndi::rgba_to_uyvy(&rgba, &mut ours, width, height);
            let mut theirs = vec![0u8; ours.len()];
            Converter::new(width, height, false)
                .and_then(|mut c| c.run(&rgba, &mut theirs))
                .expect("converted");
            let row = width as usize * 2;
            for (i, (a, b)) in ours.iter().zip(&theirs).enumerate() {
                let (y, x) = (i / row, i % row);
                let luma = x % 2 == 1;
                let off = a.abs_diff(*b);
                // Flat rows and the gradient: a rounding step. Noise rows'
                // chroma: how the pair is averaged may differ a little more.
                let most = if luma || y < 6 || y % 2 == 1 { 1 } else { 3 };
                assert!(
                    off <= most,
                    "{width}×{height}: byte {x} of line {y} ({}): ours {a}, Accelerate's {b}",
                    if luma { "Y" } else { "Cb/Cr" }
                );
            }
        }
    }

    /// At full levels white is Y 254 and black Y 1 — the whole range but the
    /// two codes the signal keeps for its own timing — chroma still 128.
    #[test]
    fn full_levels_span_the_range() {
        let convert = |value: u8| {
            let rgba = vec![value; 4 * 2 * 4];
            let mut uyvy = vec![0u8; 4 * 2 * 2];
            Converter::new(4, 2, true)
                .and_then(|mut c| c.run(&rgba, &mut uyvy))
                .expect("converted");
            uyvy
        };
        let white = convert(255);
        let black = convert(0);
        assert_eq!((white[1], white[0], white[2]), (254, 128, 128), "white");
        assert_eq!((black[1], black[0], black[2]), (1, 128, 128), "black");
        let grey = convert(128)[1];
        assert!((126..=130).contains(&grey), "grey at {grey}");
    }

    #[test]
    fn conversion_checks_dimensions_and_preserves_buffer_tails() {
        let mut converter = Converter::new(4, 2, true).unwrap();
        assert!(converter.run(&[0; 31], &mut [0; 16]).is_err());
        assert!(converter.run(&[0; 32], &mut [0; 15]).is_err());
        for (width, height) in [(0, 2), (3, 2), (4, 0)] {
            assert!(Converter::new(width, height, true)
                .unwrap()
                .run(&[0; 32], &mut [0; 16])
                .is_err());
        }
        let mut out = [99; 20];
        converter.run(&[255; 40], &mut out).unwrap();
        assert_eq!(&out[16..], &[99; 4]);
        assert_eq!(out[1], 254);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn portable_full_range_matches_accelerate_for_colour_patches() {
        for height in [576, 1080] {
            let patches = [
                [0, 0, 0, 255],
                [255; 4],
                [255, 0, 0, 255],
                [0, 255, 0, 255],
                [0, 0, 255, 255],
                [128, 128, 128, 255],
            ];
            let rgba = patches.concat().repeat(height as usize);
            let mut ours = vec![0; 12 * height as usize];
            let mut theirs = vec![0; ours.len()];
            convert::Converter::new(height, true)
                .unwrap()
                .run(&rgba, &mut ours, 6, height)
                .unwrap();
            Converter::new(6, height, true)
                .unwrap()
                .run(&rgba, &mut theirs)
                .unwrap();
            for (a, b) in ours.iter().zip(theirs) {
                assert!(a.abs_diff(b) <= 1, "portable {a}, Accelerate {b}");
            }
        }
    }

    /// A running output's levels change with the next picture.
    #[test]
    fn levels_change_with_the_next_picture() {
        let (screen, ends) = recorder(false);
        let sender = start(screen);
        sender.send(&rgba(255));
        assert_eq!(ends.shown.recv_timeout(WAIT).expect("shown")[1], 235, "video");
        sender.set_full_range(true);
        sender.send(&rgba(255));
        assert_eq!(ends.shown.recv_timeout(WAIT).expect("shown")[1], 254, "full");
        sender.set_full_range(false);
        sender.send(&rgba(255));
        assert_eq!(ends.shown.recv_timeout(WAIT).expect("shown")[1], 235, "video again");
    }

    /// How long a 1080p conversion takes: about a millisecond, against the
    /// 8.6 ms Nida's own loop took on an M1 Pro. Run by hand:
    /// `cargo test --lib converting_1080p_is_quick -- --ignored --nocapture`.
    #[test]
    #[ignore = "a measurement, not a check"]
    fn converting_1080p_is_quick() {
        let rgba: Vec<u8> = (0..1920 * 1080 * 4)
            .map(|i| u8::try_from(i % 251).unwrap_or(0))
            .collect();
        let mut uyvy = vec![0u8; 1920 * 1080 * 2];
        for full_range in [false, true] {
            let mut converter = Converter::new(1920, 1080, full_range).expect("made");
            converter.run(&rgba, &mut uyvy).expect("converted");
            let runs = 100;
            let started = std::time::Instant::now();
            for _ in 0..runs {
                converter.run(&rgba, &mut uyvy).expect("converted");
            }
            let each = started.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
            println!("1080p RGBA → 2vuy (full={full_range}): {each:.2} ms each");
        }
    }

    /// Stopping an output does not wait for its device — busy putting a
    /// picture up, say — and the device is let go once it is done.
    #[test]
    fn stopping_does_not_wait_for_the_device_which_is_let_go_after() {
        let (screen, ends) = recorder(true);
        let sender = start(screen);
        sender.send(&rgba(255));
        ends.started.recv_timeout(WAIT).expect("taken");
        let stopping = std::time::Instant::now();
        drop(sender);
        assert!(
            stopping.elapsed() < Duration::from_secs(1),
            "the stop waited for the device"
        );
        assert!(
            ends.closed.try_recv().is_err(),
            "not closed under its own feet"
        );
        ends.release.send(()).unwrap();
        ends.closed.recv_timeout(WAIT).expect("let go once done");
    }

    /// Both of an output's threads run in real time, a frame being given,
    /// each on its own budget — the device's once its screen is open; none
    /// given, not.
    #[cfg(target_os = "macos")]
    #[test]
    fn an_outputs_threads_run_in_real_time() {
        use crate::renderer::presenter::real_time_of;
        let near = |(period, computation, constraint): (Duration, Duration, Duration),
                    want: (Duration, Duration)| {
            let close = |a: Duration, b: Duration| a.abs_diff(b) < Duration::from_micros(5);
            close(period, FRAME) && close(computation, want.0) && close(constraint, want.1)
        };
        let (screen, ends) = recorder(false);
        let sender = start(screen);
        sender.send(&rgba(0));
        ends.shown.recv_timeout(WAIT).expect("shown");
        let device = real_time_of(&sender.threads[0]).expect("the device's thread in real time");
        assert!(near(device, DEVICE_BUDGET), "{device:?}");
        let convert = real_time_of(&sender.threads[1]).expect("the conversion in real time");
        assert!(near(convert, CONVERT_BUDGET), "{convert:?}");

        let (screen, ends) = recorder(false);
        let sender = DeckLinkSender::start_with("Test", 4, 2, false, None, move || {
            Ok(Box::new(screen) as Box<dyn Screen>)
        })
        .expect("started");
        sender.send(&rgba(0));
        ends.shown.recv_timeout(WAIT).expect("shown");
        assert_eq!(real_time_of(&sender.threads[0]), None);
        assert_eq!(real_time_of(&sender.threads[1]), None);
    }

    /// The threads' budgets are 1080p's for it and less; more pixels, more,
    /// never past the frame.
    #[test]
    fn bigger_pictures_get_more_of_a_cpu() {
        let ms = |ms: u64| Duration::from_millis(ms);
        assert_eq!(budget_for(CONVERT_BUDGET, FRAME, 1920, 1080), (ms(3), ms(6)));
        assert_eq!(budget_for(DEVICE_BUDGET, FRAME, 1280, 720), (ms(2), ms(4)));
        let at_30 = Duration::from_micros(33_333);
        assert_eq!(budget_for(CONVERT_BUDGET, at_30, 3840, 2160), (ms(12), ms(24)));
        assert_eq!(budget_for(CONVERT_BUDGET, FRAME, 3840, 2160), (ms(12), FRAME));
    }

    /// Every [`REPORT_EVERY`] the log is told how the output fares: pictures
    /// put up and let go; how often the show handed them over and how long
    /// they waited for the device; how long converting and the device took,
    /// and of it on a CPU — once, then not till the next.
    #[test]
    fn the_log_is_told_how_the_output_fares() {
        let (screen, ends) = recorder(false);
        let sender = start(screen);
        for value in [0, 128, 255] {
            sender.send(&rgba(value));
            ends.shown.recv_timeout(WAIT).expect("shown");
            std::thread::sleep(Duration::from_millis(2));
        }
        let until = std::time::Instant::now() + WAIT;
        while sender.counts().0 < 3 && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(1));
        }
        {
            let slot = sender.shared.lock();
            assert_eq!(slot.between.count, 2, "two times between three pictures");
            assert!(slot.between.most >= Duration::from_millis(2));
            assert_eq!(
                (slot.age.count, slot.convert_cpu.count, slot.device_cpu.count),
                (3, 3, 3)
            );
            assert!(slot.convert_cpu.most <= slot.convert.most);
        }
        let now = std::time::Instant::now();
        let mut told = sender.told.lock().unwrap();
        assert_eq!(told.due(&sender.shared, "Test", now), None, "not yet");
        let line = told
            .due(&sender.shared, "Test", now + REPORT_EVERY)
            .expect("told");
        for part in [
            "Blackmagic output 'Test': 3 pictures put up",
            "0 let go for newer",
            "handed over every ",
            "waiting ",
            "converting each ",
            "the device taking each ",
        ] {
            assert!(line.contains(part), "{part:?} in {line}");
        }
        assert_eq!(line.matches(", on a CPU ").count(), 2, "{line}");
        assert_eq!(sender.shared.lock().between.count, 0, "counted afresh");
        assert_eq!(
            told.due(&sender.shared, "Test", now + REPORT_EVERY),
            None,
            "once"
        );
    }

    /// Pictures' memory is kept for the next, at most a few — any more let
    /// go — and a device's own error said in words only when told.
    #[test]
    fn pictures_memory_is_kept_but_never_piles_up() {
        let mut spares = Vec::new();
        assert!(keep(&mut spares, vec![1]).is_none());
        assert!(keep(&mut spares, vec![2]).is_none());
        assert_eq!(keep(&mut spares, vec![3]), Some(vec![3]), "no room for a third");
        assert_eq!(spares.len(), SPARE_PICTURES);

        let (screen, ends) = recorder(true);
        let sender = start(screen);
        sender.send(&rgba(255));
        ends.started.recv_timeout(WAIT).expect("taken");
        // The device holding one: more handed over than it takes.
        for value in 0..20 {
            sender.send(&rgba(value));
        }
        assert!(sender.shared.lock().spares.len() <= SPARE_PICTURES);
        ends.release.send(()).unwrap();
        assert_eq!(Refused::Code(-3).to_string(), "error -3");
    }

    /// A device that will not open says why — the output's frames not waiting.
    #[test]
    fn a_device_that_will_not_open_says_why() {
        let sender =
            DeckLinkSender::start_with("Test", 4, 2, false, None, || {
                Err("in use by another app".into())
            })
            .expect("started");
        let until = std::time::Instant::now() + WAIT;
        while sender.failed().is_none() && std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
        let why = sender.failed().expect("said why");
        assert!(why.contains("in use"), "{why}");
    }

    /// A device not taking a picture now and then keeps its output; one not
    /// taking any, half a second on, is given up, saying why.
    #[test]
    fn a_device_not_taking_pictures_is_given_up_only_when_it_keeps_not() {
        let (screen, ends) = recorder(true);
        let sender = start(screen);
        // Each picture handed over, taken and let go: how the one before
        // went is noted by the time the device's thread takes the next.
        let take = || {
            sender.send(&rgba(0));
            ends.started.recv_timeout(WAIT).expect("taken");
        };
        let put_up = |n: u32| {
            for _ in 0..n {
                take();
                ends.release.send(()).unwrap();
            }
        };
        ends.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        put_up(GIVE_UP_AFTER - 1);
        ends.fail.store(false, std::sync::atomic::Ordering::SeqCst);
        put_up(1);
        assert_eq!(sender.failed(), None, "a moment's hiccup");
        ends.fail.store(true, std::sync::atomic::Ordering::SeqCst);
        put_up(GIVE_UP_AFTER - 1);
        take();
        assert_eq!(sender.failed(), None, "counted from the last one taken");
        ends.release.send(()).unwrap();
        let noted = std::time::Instant::now() + WAIT;
        while sender.failed().is_none() && std::time::Instant::now() < noted {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            sender.failed().as_deref(),
            Some("the Blackmagic device stopped taking pictures (error -3) — unplugged?")
        );
        assert_eq!(sender.counts().0, 1);
    }

    /// One output a device: a start just after a stop waits for the device
    /// to be let go; a device another running output has is not taken.
    #[test]
    fn a_device_is_held_by_one_output_at_a_time() {
        let name = "Device for a_device_is_held_by_one_output_at_a_time";
        let first = hold_device(name, 1, WAIT).expect("free");
        let taken = hold_device(name, 1, Duration::from_millis(50));
        assert!(taken.err().expect("not taken").contains("in use"));
        let waiting = std::thread::spawn(move || hold_device(name, 1, WAIT).map(drop));
        std::thread::sleep(Duration::from_millis(100));
        drop(first);
        waiting.join().unwrap().expect("taken once let go");
        drop(hold_device(name, 1, Duration::ZERO).expect("free again"));
    }

    /// A device an output stopped on, its thread stuck in it for good — the
    /// device unplugged mid-picture — is taken over once the device has
    /// arrived again; the stuck thread, should it ever end, does not take the
    /// new claim with it. On the same arrival it is never taken over: its
    /// thread may yet let go of the device under the new output's feet. A
    /// running output's device is never taken over.
    #[test]
    fn a_device_an_output_never_let_go_is_taken_over_once_plugged_in_again() {
        let name = "Device for a_device_an_output_never_let_go_is_taken_over";
        let short = Duration::from_millis(50);
        let stuck = hold_device(name, 1, WAIT).expect("free");
        // Running, on an earlier arrival: not taken over.
        assert!(hold_device(name, 2, short).is_err());
        // Stopped, but on this same arrival: still not.
        let_go(&stuck.device, stuck.token);
        assert!(hold_device(name, 1, short).is_err());
        // Plugged in again: taken over at once.
        let taken = hold_device(name, 2, short).expect("taken over");
        drop(stuck);
        assert!(
            hold_device(name, 2, short).is_err(),
            "the stuck thread ending gave up the new claim"
        );
        drop(taken);
        drop(hold_device(name, 2, Duration::ZERO).expect("free again"));
    }

    /// A device that stops answering — a picture handed to it, never taken:
    /// unplugged mid-picture — is said to have stopped a second on, so its
    /// output is stopped and started again when the device comes back.
    #[test]
    fn a_device_that_stops_answering_is_said_to() {
        let (screen, ends) = recorder(true);
        let sender = start(screen);
        sender.send(&rgba(0));
        ends.started.recv_timeout(WAIT).expect("taken");
        assert_eq!(sender.failed(), None, "a moment is no matter");
        std::thread::sleep(HUNG_AFTER + Duration::from_millis(100));
        assert_eq!(
            sender.failed().as_deref(),
            Some("the Blackmagic device stopped responding — unplugged?")
        );
        drop(sender);
        ends.release.send(()).unwrap();
    }

    /// Asked for devices with no Desktop Video — as here, building — there
    /// are none, at once.
    #[test]
    fn without_desktop_video_there_are_no_devices() {
        if sdk::api_version().is_some() {
            return;
        }
        assert!(devices().is_empty());
        assert!(DeckLinkSender::start("Test", "UltraStudio Monitor 3G", "1080p60", false).is_err());
    }
}
