//! The presentation clicker, heard whatever app is in front.
//!
//! A clicker's keys go to the app in front: Nida in front, they reach it as
//! any keyboard's do (see the UI's `clicker_shortcuts`); Finder in front, to
//! add a talk, or a browser, they go there and the talk on air stops moving.
//! So the clicker is also read here, as the device it is (macOS's HID
//! manager), and its keys step the show whatever app is in front.
//!
//! Only the clicker: it is learned — a button pressed on it once — and only
//! that device is read from then on, so typing in another app (a B, a
//! `PageDown` to scroll) never moves the slides. The Mac's own keyboard is never
//! taken for it. macOS asks once to let Nida do this: Input Monitoring.
//!
//! While Nida reads it, the clicker's keys the show uses are Nida's alone:
//! for every other app they are turned into spare keys macOS makes no key
//! press of at all (F24 down to F21 — macOS's own key remapping, `hidutil`,
//! for that device only), so no app hears them: a `PageDown` no longer also
//! scrolls the browser in front, nor steps PowerPoint's own window a second
//! time; no alert sound in Finder, nothing typed into a document. Read here
//! as the device sends them, they still step the show — with Nida in front
//! too. Lifted when Nida stops reading it — Forget, learning again,
//! Keyboard learn, Nida quitting — and put back when its receiver is
//! plugged in again. Letters and digits are never turned: a keyboard
//! sharing the clicker's receiver still types.
//!
//! Each press is taken once, however the remap stands. A key macOS has
//! turned — the remap read back in place for that device, and not asked
//! again since — reaches no window, Nida's neither: it is taken here,
//! whatever has the keyboard. One that is not — a letter, or any while the
//! remap is being put in place or lifted — reaches the control window as a
//! keyboard's: the window's while it has the keyboard, taken here only
//! while it has not. The Mac's own keyboard is the window's alone.
//!
//! The clicker learned is this Mac's, not the show's: kept in Nida's folder
//! of the user's application data.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use crate::engine::value::presentation::{Go, Screen};
use crate::engine::{CommandEnvelope, EngineCommand};
use crate::keymap::{ActionId, KeyCombo, KeyTarget};

#[cfg(target_os = "macos")]
mod macos;

/// The clicker learned: the device, by maker and product, and its name.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ClickerDevice {
    pub vendor: u32,
    pub product: u32,
    pub name: String,
}

/// Whether macOS lets Nida read input devices (Input Monitoring).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Access {
    /// Not asked yet.
    #[default]
    Unknown,
    Granted,
    /// Refused, or not yet switched on: Nida is to be allowed in System
    /// Settings, and opened again.
    Denied,
}

/// How the clicker stands, for the Presentations panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClickerStatus {
    /// The clicker learned.
    pub device: Option<ClickerDevice>,
    /// Waiting for a button on the clicker.
    pub learning: bool,
    /// A key of the Mac's own keyboard was pressed while learning: not
    /// taken.
    pub mac_keyboard: bool,
    /// Learned a moment ago: the press that taught it — heard by the
    /// control window as a keyboard's too — is not also the show's.
    pub just_learned: bool,
    pub access: Access,
}

/// What the control window's keys are for, as it last drew.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowKeys {
    /// The keymap's Keyboard learn is on: the clicker's keys reach the
    /// window as they are, to be learned, and step nothing.
    pub keyboard_learn: bool,
    /// The stage editor is open: the window's keys are its.
    pub stage_editor: bool,
}

/// What the panel asks of the clicker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClickerAsk {
    /// Wait for a button on the clicker, and take its device.
    Learn,
    /// Stop waiting.
    Cancel,
    /// Read no clicker any more.
    Forget,
}

/// A key-board device a key came from, as macOS describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub vendor: u32,
    pub product: u32,
    pub name: String,
    /// The Mac's own keyboard.
    pub built_in: bool,
}

/// What the device reader needs to change: read only the clicker learned,
/// or every keyboard (learning), or nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Reading {
    Nothing,
    Keyboards,
    Clicker(ClickerDevice),
}

#[derive(Default)]
struct State {
    status: ClickerStatus,
    /// Where the clicker learned is kept.
    file: Option<PathBuf>,
    /// Where the show's commands go.
    sender: Option<tokio::sync::mpsc::UnboundedSender<CommandEnvelope>>,
    /// The keys bound to the presentation actions, as the keymap has them.
    bindings: Vec<(KeyCombo, ActionId)>,
    /// The modifier keys held, per device.
    held: HashMap<usize, Modifiers>,
    /// What the reader was last set to read, and what it is to read now.
    reading: Option<Reading>,
    /// The control window has the keyboard: the clicker's keys reach it as
    /// a keyboard's.
    focused: bool,
    /// When the clicker was learned, and the control window's frames since:
    /// the press that taught it reaches the window as a keyboard's in one of
    /// the next, however late they come.
    learned_at: Option<std::time::Instant>,
    frames_since_learned: u32,
    /// What the control window's keys are for.
    window: WindowKeys,
    /// The clicker's keys the show uses, each turned into a spare key for
    /// every other app: as last asked of macOS — kept while it is lifted, a
    /// spare key still read as its key meanwhile.
    remap: Remap,
    /// The remap is wanted in place, and for which device (to lift it from).
    remap_on: Option<(u32, u32)>,
    /// Each remap asked of macOS — put in place or lifted — counted.
    remap_asked: u64,
    /// The clicker's keys macOS has turned, by the device they come from and
    /// the key: its spare, and the time (macOS's clock, as a key press
    /// carries it) from which a press of it reaches no window — the control
    /// window's neither. Emptied as a lift is asked.
    placed: HashMap<(usize, u32), (u32, u64)>,
    /// A device of the clicker came since the remap was asked: asked again.
    remap_due: bool,
    /// The clicker's devices here.
    present: HashSet<usize>,
    /// Every device remapped while Nida has run: given back as it quits.
    touched: Vec<(u32, u32)>,
    /// Asks macOS for remaps; `hidutil` on a thread unless a test gives
    /// another, answered at once.
    remapper: Option<Remapper>,
}

/// Asks macOS for remaps, and tells its clock.
#[derive(Clone, Copy)]
struct Remapper {
    /// Turn a device's keys — usages, each into its spare's — for every app
    /// (none: lifted); whether macOS took it.
    set: fn(u32, u32, &[(u32, u32)]) -> bool,
    /// Whether every one of the device's services has those keys turned so,
    /// as macOS reads them back.
    in_place: fn(u32, u32, &[(u32, u32)]) -> bool,
    /// macOS's clock, as key presses carry it.
    now: fn() -> u64,
}

/// The keys the clicker's own are turned into for every other app, one for
/// each key the show uses: F24 down to F21, then Execute — keys macOS makes
/// no key press of (no key code in its keyboard table), so no app hears
/// them. Not F13 to F20, which it does: an app with no use for one (Finder)
/// plays the alert sound, and F20 types an invisible character into text.
const SPARE_KEYS: [u32; 5] = [0x73, 0x72, 0x71, 0x70, 0x74];

/// Keys turned into spare ones: each one's usage, its spare's, its name.
type Remap = Vec<(u32, u32, &'static str)>;

/// The control window's keys are not the show's for this long after the
/// clicker is learned, and for this many of its frames.
const LEARNING_PRESS: std::time::Duration = std::time::Duration::from_millis(800);
const LEARNING_FRAMES: u32 = 3;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Modifiers {
    command: bool,
    shift: bool,
    alt: bool,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut guard = STATE.lock().unwrap_or_else(PoisonError::into_inner);
    f(guard.get_or_insert_with(State::default))
}

/// Where the clicker learned is kept: `VARDA_CLICKER_FILE` if set, else
/// `clicker.json` in Nida's folder of the user's application data.
#[must_use]
pub fn clicker_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("VARDA_CLICKER_FILE") {
        return Some(PathBuf::from(path));
    }
    dirs::data_dir().map(|dir| dir.join("Varda").join("clicker.json"))
}

/// The clicker kept in `path`; none if there is none, or it is damaged.
#[must_use]
pub fn load(path: &Path) -> Option<ClickerDevice> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Keep `device` in `path`, or none.
///
/// # Errors
///
/// Why it could not be written or taken away.
pub fn save(path: &Path, device: Option<&ClickerDevice>) -> std::io::Result<()> {
    match device {
        Some(device) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let text = serde_json::to_string_pretty(device).map_err(std::io::Error::other)?;
            std::fs::write(path, text)
        }
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        },
    }
}

/// Nida starting, on the main thread: the clicker learned before is read
/// again — when macOS already lets Nida (nothing is asked at start) — its
/// keys sent to `sender`.
pub fn start(sender: tokio::sync::mpsc::UnboundedSender<CommandEnvelope>) {
    start_with(sender, clicker_file());
}

/// [`start`], the clicker learned kept in `file`.
fn start_with(sender: tokio::sync::mpsc::UnboundedSender<CommandEnvelope>, file: Option<PathBuf>) {
    let device = file.as_deref().and_then(load);
    with_state(|s| {
        s.file = file;
        s.sender = Some(sender);
        s.status.access = access();
        s.status.device = device;
        // Taken as having the keyboard until the window says (see
        // `set_focused`): Keyboard learn, or the stage editor, holds the
        // clicker meanwhile.
        s.focused = true;
    });
    frame(&HashMap::new(), WindowKeys::default());
}

/// The control window took (`true`) or lost the keyboard, as it happens: on
/// the main thread, from the window's own event.
pub fn set_focused(focused: bool) {
    with_state(|s| s.focused = focused);
}

/// Each frame of the control window, on the main thread: the keys bound to
/// the presentation actions taken from `bindings`, what the window's keys
/// are for (`window`), the device reader set to what is asked of it, and
/// the clicker's keys kept from other apps while it is read.
pub fn frame<S: std::hash::BuildHasher>(
    bindings: &HashMap<KeyCombo, KeyTarget, S>,
    window: WindowKeys,
) {
    let wanted = with_state(|s| {
        s.frames_since_learned = s.frames_since_learned.saturating_add(1);
        s.window = window;
        let mut presentation: Vec<(KeyCombo, ActionId)> = bindings
            .iter()
            .filter_map(|(combo, target)| match target {
                KeyTarget::Action(id) if command_for(*id).is_some() => Some((combo.clone(), *id)),
                _ => None,
            })
            .collect();
        presentation.sort_by_key(|(c, _)| (c.key.clone(), c.command, c.shift, c.alt));
        if presentation != s.bindings {
            s.bindings = presentation;
        }
        let wanted = if s.status.learning {
            Reading::Keyboards
        } else if let Some(device) = &s.status.device {
            Reading::Clicker(device.clone())
        } else {
            Reading::Nothing
        };
        (s.reading.as_ref() != Some(&wanted)).then_some(wanted)
    });
    let Some(wanted) = wanted else {
        update_remap();
        return;
    };
    // Reading the clicker learned before is not worth a question at start:
    // only learning asks. Asked, macOS answers when the user has: the read
    // is tried again each frame till then — never waited for, the engine
    // held meanwhile.
    let ask = wanted == Reading::Keyboards;
    let (access, done) = read(&wanted, ask);
    with_state(|s| {
        s.status.access = access;
        if done {
            s.reading = Some(wanted);
            // A reader opened afresh says again which devices are here, by
            // new names: each is given the remap again.
            s.present.clear();
            s.placed.clear();
        }
    });
    update_remap();
}

/// A device of the clicker came (`here`) or went: on the main thread, from
/// the device reader. One come is given the remap: its keys are not turned
/// till then.
fn on_device(id: usize, here: bool) {
    with_state(|s| {
        if here {
            s.present.insert(id);
            s.remap_due = true;
        } else {
            s.present.remove(&id);
            s.held.remove(&id);
        }
        s.placed.retain(|(device, _), _| *device != id);
    });
    update_remap();
}

/// Nida quitting, on the main thread: every device remapped while it ran
/// given back to the other apps — after any remap still being asked (one
/// thread asks them, in order) — waited for, eight seconds at most.
pub fn stop() {
    let (asked, touched) = with_state(|s| {
        s.remap_on = None;
        s.remap_asked += 1;
        s.placed.clear();
        (s.remap_asked, std::mem::take(&mut s.touched))
    });
    let mut last = None;
    for (vendor, product) in touched {
        let (tx, rx) = std::sync::mpsc::channel();
        queue(RemapJob {
            vendor,
            product,
            pairs: Vec::new(),
            devices: Vec::new(),
            asked,
            done: Some(tx),
        });
        last = Some(rx);
    }
    if let Some(rx) = last {
        let _ = rx.recv_timeout(std::time::Duration::from_secs(8));
    }
}

/// The remap as it should be: asked of macOS — on a thread — when it has to
/// change: put in place for the clicker read (Keyboard learn not on), the
/// keys the show uses each turned into a spare one; lifted otherwise.
fn update_remap() {
    // Decided under the lock, asked of macOS after it.
    let jobs = with_state(|s| {
        let reading = match &s.reading {
            Some(Reading::Clicker(device)) if s.status.access == Access::Granted => {
                Some((device.vendor, device.product))
            }
            _ => None,
        };
        let wanted = reading.filter(|_| !s.window.keyboard_learn);
        let remap = remap_for(&s.bindings, &s.remap);
        let mut jobs: Vec<(u32, u32, Remap)> = Vec::new();
        match (wanted, s.remap_on) {
            (Some(device), on) if on != Some(device) || s.remap_due || remap != s.remap => {
                if let Some(old) = on.filter(|old| *old != device) {
                    // Another clicker learned: the old one's keys given back.
                    jobs.push((old.0, old.1, Vec::new()));
                    s.placed.clear();
                }
                s.remap_on = Some(device);
                s.remap_due = false;
                s.remap.clone_from(&remap);
                if !s.touched.contains(&device) {
                    s.touched.push(device);
                }
                jobs.push((device.0, device.1, remap));
            }
            (None, Some(old)) => {
                // Its keys may reach the windows again any moment: none taken
                // as turned from the ask on.
                s.remap_on = None;
                s.placed.clear();
                jobs.push((old.0, old.1, Vec::new()));
            }
            _ => {}
        }
        if !jobs.is_empty() {
            s.remap_asked += 1;
        }
        let devices: Vec<usize> = s.present.iter().copied().collect();
        (s.remap_asked, devices, jobs)
    });
    let (asked, devices, jobs) = jobs;
    for (vendor, product, remap) in jobs {
        queue(RemapJob {
            vendor,
            product,
            pairs: remap.iter().map(|(from, to, _)| (*from, *to)).collect(),
            devices: devices.clone(),
            asked,
            done: None,
        });
    }
}

/// A remap to ask of macOS: the device, its keys' usages each with its
/// spare's (none: lifted).
struct RemapJob {
    vendor: u32,
    product: u32,
    pairs: Vec<(u32, u32)>,
    /// The clicker's devices here as it was asked: those it turns the keys
    /// of. One come after is asked for again.
    devices: Vec<usize>,
    /// Which remap asked it is, by the count (see `State::remap_asked`).
    asked: u64,
    /// Told once it is asked (Nida quitting waits for it).
    done: Option<std::sync::mpsc::Sender<()>>,
}

/// Ask macOS for one remap, with `remapper`: on the remap thread, or at once
/// with a test's.
///
/// Read back on each of the device's services, its keys are taken as turned
/// from the moment macOS had taken it — a press after that reached no
/// window. Keys turned already stay so meanwhile; a key newly turned is not
/// taken so till read back (a press of it meanwhile goes unheard): one
/// service it did not take, and a press through it would reach a window
/// while taken here too. Not in place on all of them, it is lifted.
fn run(remapper: Remapper, job: &RemapJob) {
    let set = (remapper.set)(job.vendor, job.product, &job.pairs);
    let since = (remapper.now)();
    if job.pairs.is_empty() {
        return;
    }
    if set {
        if (remapper.in_place)(job.vendor, job.product, &job.pairs) {
            place(job, since);
            return;
        }
        log::warn!(
            "Clicker: its keys' remap not in place on every service; lifted, they reach other apps"
        );
    }
    with_state(|s| s.placed.clear());
    let _ = (remapper.set)(job.vendor, job.product, &[]);
}

/// `job` taken by macOS at `since`, and read back: its keys, on each of its
/// devices, taken as turned from then — a key turned so already keeping its
/// time — if no remap has been asked since (a later one decides).
fn place(job: &RemapJob, since: u64) {
    with_state(|s| {
        if s.remap_asked != job.asked {
            return;
        }
        let mut placed = HashMap::new();
        for &device in &job.devices {
            for &(key, spare) in &job.pairs {
                let at = match s.placed.get(&(device, key)) {
                    Some(&(was, at)) if was == spare => at,
                    _ => since,
                };
                placed.insert((device, key), (spare, at));
            }
        }
        s.placed = placed;
    });
}

/// The remaps' queue: one thread asks macOS, job after job.
fn queue(job: RemapJob) {
    static JOBS: std::sync::OnceLock<Option<Mutex<std::sync::mpsc::Sender<RemapJob>>>> =
        std::sync::OnceLock::new();
    if let Some(remapper) = with_state(|s| s.remapper) {
        run(remapper, &job);
        if let Some(done) = job.done {
            let _ = done.send(());
        }
        return;
    }
    let jobs = JOBS.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<RemapJob>();
        std::thread::Builder::new()
            .name("clicker-remap".into())
            .spawn(move || {
                while let Ok(job) = rx.recv() {
                    run(MACOS, &job);
                    if let Some(done) = job.done {
                        let _ = done.send(());
                    }
                }
            })
            .map_err(|e| log::warn!("Clicker: its keys not kept from other apps: {e}"))
            .ok()
            .map(|_| Mutex::new(tx))
    });
    if let Some(jobs) = jobs {
        let _ = jobs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .send(job);
    }
}

/// The keys the show uses — those the keymap binds to its actions, letters
/// and digits left out — each with a spare key, as many as there are
/// spares. A key still bound keeps the spare it had (`before`): a press on
/// its way as that spare is still read as that key; one newly bound takes a
/// spare free.
fn remap_for(bindings: &[(KeyCombo, ActionId)], before: &Remap) -> Remap {
    let mut names: Vec<&str> = bindings.iter().map(|(c, _)| c.key.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    let keys: Vec<(u32, &'static str)> = names
        .into_iter()
        .filter_map(|name| {
            let usage = usage_of(name)?;
            Some((usage, key_name(usage)?))
        })
        // Letters and digits never: a keyboard sharing the receiver types.
        .filter(|(usage, _)| !(0x04..=0x27).contains(usage) && !(0x59..=0x62).contains(usage))
        .collect();
    let mut remap: Remap = before
        .iter()
        .filter(|(usage, _, _)| keys.iter().any(|(u, _)| u == usage))
        .copied()
        .collect();
    for (usage, name) in keys {
        if remap.iter().any(|(u, _, _)| *u == usage) {
            continue;
        }
        let Some(spare) = SPARE_KEYS
            .iter()
            .find(|spare| !remap.iter().any(|(_, to, _)| to == *spare))
        else {
            break;
        };
        remap.push((usage, *spare, name));
    }
    remap.sort_unstable();
    remap
}

/// A key's keyboard-page usage, by the keymap's name for it (the row's for a
/// digit or minus, not the keypad's).
fn usage_of(name: &str) -> Option<u32> {
    (0x04..0x74).find(|&usage| key_name(usage) == Some(name))
}

/// The keyboard page's usages as macOS's key remapping writes them.
const PAGE: u64 = 0x7_0000_0000;

/// Whether `read` — `hidutil property --get UserKeyMapping` for a device:
/// a line for each of its services, with what it turns — names a service at
/// least, and each one turns every key of `pairs` (usages, each with its
/// spare's) into its spare. One service without them, and a press through
/// it would reach a window while taken as turned.
fn turned_on_every_service(read: &str, pairs: &[(u32, u32)]) -> bool {
    let mut services: Vec<String> = Vec::new();
    for line in read.lines() {
        // A service's line starts with its registry id; what it turns
        // follows, indented, over the next lines.
        if line.starts_with(|c: char| c.is_ascii_hexdigit()) {
            services.push(line.to_string());
        } else if let Some(service) = services.last_mut() {
            service.push('\n');
            service.push_str(line);
        }
    }
    let number = |text: &str| -> Option<u64> {
        let text = text.trim().trim_end_matches(';').trim();
        match text.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16).ok(),
            None => text.parse().ok(),
        }
    };
    let turned = |service: &str| -> HashSet<(u64, u64)> {
        service
            .split('{')
            .skip(1)
            .filter_map(|entry| {
                let entry = entry.split('}').next()?;
                let value = |key: &str| {
                    entry.lines().find_map(|line| {
                        number(line.trim().strip_prefix(key)?.trim().strip_prefix('=')?)
                    })
                };
                Some((
                    value("HIDKeyboardModifierMappingSrc")?,
                    value("HIDKeyboardModifierMappingDst")?,
                ))
            })
            .collect()
    };
    !services.is_empty()
        && services.iter().all(|service| {
            let turned = turned(service);
            pairs
                .iter()
                .all(|&(key, spare)| turned.contains(&(PAGE | u64::from(key), PAGE | u64::from(spare))))
        })
}

/// macOS's own remapping (`hidutil`), and its clock.
#[cfg(target_os = "macos")]
const MACOS: Remapper = Remapper {
    set: macos::remap,
    in_place: macos::remapped,
    now: macos::now,
};

/// No remapping off macOS.
#[cfg(not(target_os = "macos"))]
const MACOS: Remapper = Remapper {
    set: |_, _, _| false,
    in_place: |_, _, _| false,
    now: || 0,
};

/// What the panel asked, on the main thread.
pub fn ask(ask: ClickerAsk) {
    let forgotten = with_state(|s| {
        s.status.mac_keyboard = false;
        match ask {
            ClickerAsk::Learn => {
                s.status.learning = true;
                None
            }
            ClickerAsk::Cancel => {
                s.status.learning = false;
                None
            }
            ClickerAsk::Forget => {
                s.status.learning = false;
                s.status.device = None;
                s.held.clear();
                s.file.clone()
            }
        }
    });
    if let Some(file) = forgotten {
        if let Err(e) = save(&file, None) {
            log::warn!("Clicker: {} not taken away: {e}", file.display());
        }
    }
}

/// How the clicker stands.
#[must_use]
pub fn status() -> ClickerStatus {
    with_state(|s| ClickerStatus {
        just_learned: s.just_learned(),
        ..s.status.clone()
    })
}

impl State {
    /// Learned a moment ago (see [`ClickerStatus::just_learned`]).
    fn just_learned(&self) -> bool {
        self.learned_at.is_some_and(|at| {
            at.elapsed() < LEARNING_PRESS || self.frames_since_learned < LEARNING_FRAMES
        })
    }

    /// Whether the clicker's key of `usage`, pressed on its device `id` at
    /// `at` (macOS's clock), reached no window — turned by macOS, for the
    /// control window too: its spare come here (the remap at work), or a key
    /// macOS had turned on that device before it was pressed. One pressed
    /// before — or on a device come since — reached the windows.
    fn turned(&self, id: usize, usage: u32, at: u64) -> bool {
        self.remap.iter().any(|&(key, spare, _)| {
            spare == usage
                || (key == usage
                    && self
                        .placed
                        .get(&(id, key))
                        .is_some_and(|&(was, since)| was == spare && at > since))
        })
    }
}

/// A key of keyboard page `usage` pressed (`down`) or let go on `device`
/// (its device `id`) at `at` — macOS's clock, as the press carries it —
/// Nida the app in front (`active`) or not: learned as the clicker while
/// learning; else, the clicker's, the show's command sent — unless the
/// control window, holding the keyboard, gets the same press as a
/// keyboard's (a key not turned). What it did, for the tests.
fn on_key(device: &DeviceInfo, id: usize, usage: u32, down: bool, active: bool, at: u64) -> Heard {
    with_state(|s| {
        let in_front = active && s.focused;
        if let Some(modifier) = modifier_of(usage) {
            let held = s.held.entry(id).or_default();
            match modifier {
                Modifier::Command => held.command = down,
                Modifier::Shift => held.shift = down,
                Modifier::Alt => held.alt = down,
                Modifier::Control => {}
            }
            return Heard::Nothing;
        }
        if !down || !(0x04..0xE0).contains(&usage) {
            return Heard::Nothing;
        }
        // Any key teaches: a clicker's may be one the keymap does not name.
        if s.status.learning {
            if device.built_in {
                s.status.mac_keyboard = true;
                return Heard::Nothing;
            }
            let learned = ClickerDevice {
                vendor: device.vendor,
                product: device.product,
                name: device.name.clone(),
            };
            if let Some(file) = &s.file {
                if let Err(e) = save(file, Some(&learned)) {
                    log::warn!("Clicker: {} not kept: {e}", file.display());
                }
            }
            log::info!("Clicker learned: {}", learned.name);
            s.learned_at = Some(std::time::Instant::now());
            s.frames_since_learned = 0;
            s.status.device = Some(learned);
            s.status.learning = false;
            s.status.mac_keyboard = false;
            return Heard::Learned;
        }
        // A key turned into a spare one, read as itself either way.
        let Some(key) = s
            .remap
            .iter()
            .find(|(_, spare, _)| *spare == usage)
            .map(|(_, _, name)| *name)
            .or_else(|| key_name(usage))
        else {
            return Heard::Nothing;
        };
        let is_clicker = s
            .status
            .device
            .as_ref()
            .is_some_and(|c| c.vendor == device.vendor && c.product == device.product);
        // The press that taught it is not also the show's.
        if !is_clicker || device.built_in || s.just_learned() {
            return Heard::Nothing;
        }
        if in_front {
            // The control window holding the keyboard for keys of its own:
            // its Keyboard learn (the clicker's key to be learned), the stage
            // editor.
            if s.window.keyboard_learn || s.window.stage_editor {
                return Heard::Nothing;
            }
            // A key not turned reaches the window as a keyboard's: its.
            if !s.turned(id, usage, at) {
                return Heard::Nothing;
            }
        }
        let held = s.held.get(&id).copied().unwrap_or_default();
        let combo = KeyCombo {
            key: key.to_string(),
            command: held.command,
            shift: held.shift,
            alt: held.alt,
        };
        let Some(command) = s
            .bindings
            .iter()
            .find(|(c, _)| *c == combo)
            .and_then(|(_, id)| command_for(*id))
        else {
            return Heard::Nothing;
        };
        if let Some(sender) = &s.sender {
            let _ = sender.send((command, None));
        }
        Heard::Sent
    })
}

/// What a key did (see [`on_key`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Heard {
    Nothing,
    Learned,
    /// The show's command sent.
    Sent,
}

/// The show's command for a presentation action: the show on air (else
/// the one moved last) stepped or blacked out.
#[must_use]
pub fn command_for(id: ActionId) -> Option<EngineCommand> {
    match id {
        ActionId::PresentationNext => Some(EngineCommand::PresentationGo {
            deck_uuid: None,
            to: Go::Next,
        }),
        ActionId::PresentationPrevious => Some(EngineCommand::PresentationGo {
            deck_uuid: None,
            to: Go::Previous,
        }),
        ActionId::PresentationBlack => Some(EngineCommand::TogglePresentationScreen {
            deck_uuid: None,
            screen: Screen::Black,
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Modifier {
    Command,
    Shift,
    Alt,
    Control,
}

/// A modifier key of the keyboard page.
fn modifier_of(usage: u32) -> Option<Modifier> {
    match usage {
        0xE0 | 0xE4 => Some(Modifier::Control),
        0xE1 | 0xE5 => Some(Modifier::Shift),
        0xE2 | 0xE6 => Some(Modifier::Alt),
        0xE3 | 0xE7 => Some(Modifier::Command),
        _ => None,
    }
}

/// A keyboard-page key as the keymap names it; none for keys it does not
/// name.
#[must_use]
pub fn key_name(usage: u32) -> Option<&'static str> {
    const LETTERS: [&str; 26] = [
        "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R",
        "S", "T", "U", "V", "W", "X", "Y", "Z",
    ];
    const DIGITS: [&str; 10] = [
        "Num1", "Num2", "Num3", "Num4", "Num5", "Num6", "Num7", "Num8", "Num9", "Num0",
    ];
    const FUNCTIONS: [&str; 12] = [
        "F1", "F2", "F3", "F4", "F5", "F6", "F7", "F8", "F9", "F10", "F11", "F12",
    ];
    let at = |from: u32| usize::try_from(usage - from).ok();
    match usage {
        0x04..=0x1D => at(0x04).map(|i| LETTERS[i]),
        0x1E..=0x27 => at(0x1E).map(|i| DIGITS[i]),
        0x3A..=0x45 => at(0x3A).map(|i| FUNCTIONS[i]),
        // The keypad's: as the keymap names them, the same as the row's.
        0x59..=0x62 => at(0x59).map(|i| DIGITS[i]),
        0x57 => Some("Plus"),
        0x28 | 0x58 => Some("Enter"),
        0x29 => Some("Escape"),
        0x2A => Some("Backspace"),
        0x2B => Some("Tab"),
        0x2C => Some("Space"),
        0x2D | 0x56 => Some("Minus"),
        0x49 => Some("Insert"),
        0x4A => Some("Home"),
        0x4B => Some("PageUp"),
        0x4C => Some("Delete"),
        0x4D => Some("End"),
        0x4E => Some("PageDown"),
        0x4F => Some("ArrowRight"),
        0x50 => Some("ArrowLeft"),
        0x51 => Some("ArrowDown"),
        0x52 => Some("ArrowUp"),
        _ => None,
    }
}

/// The tests that touch the clicker's one state, one at a time.
#[cfg(test)]
pub(crate) static TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Whether macOS lets Nida read input devices.
fn access() -> Access {
    #[cfg(target_os = "macos")]
    {
        macos::access()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Access::Denied
    }
}

/// Set the device reader to `reading`, asking macOS when `ask` and it has
/// not been asked; how macOS stands then, and whether the reader is set
/// (not while macOS's answer is awaited).
fn read(reading: &Reading, ask: bool) -> (Access, bool) {
    #[cfg(target_os = "macos")]
    {
        macos::read(reading, ask)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (reading, ask);
        (Access::Denied, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clicker() -> DeviceInfo {
        DeviceInfo {
            vendor: 0x046d,
            product: 0xc538,
            name: "USB Receiver".into(),
            built_in: false,
        }
    }

    fn mac_keyboard() -> DeviceInfo {
        DeviceInfo {
            vendor: 0x05ac,
            product: 0x0342,
            name: "Apple Internal Keyboard / Trackpad".into(),
            built_in: true,
        }
    }

    /// The state as a test sets it, the keymap's defaults bound.
    fn fresh(file: Option<PathBuf>) -> tokio::sync::mpsc::UnboundedReceiver<CommandEnvelope> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let mut guard = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        *guard = Some(State {
            file,
            sender: Some(tx),
            bindings: crate::keymap::KeymapStore::defaults()
                .into_iter()
                .filter_map(|(combo, target)| match target {
                    KeyTarget::Action(id) if command_for(id).is_some() => Some((combo, id)),
                    _ => None,
                })
                .collect(),
            focused: true,
            remapper: Some(FAKE),
            ..State::default()
        });
        REMAPS.lock().unwrap_or_else(PoisonError::into_inner).clear();
        REMAP_READ_BACK.store(true, std::sync::atomic::Ordering::SeqCst);
        rx
    }

    /// A press of `usage` long after any remap was taken.
    fn press(device: &DeviceInfo, id: usize, usage: u32, down: bool, active: bool) -> Heard {
        on_key(device, id, usage, down, active, u64::MAX)
    }

    /// Learned long enough ago: the teaching press is behind it.
    fn learned_a_while_ago() {
        with_state(|s| {
            s.learned_at = std::time::Instant::now().checked_sub(LEARNING_PRESS * 2);
            s.frames_since_learned = LEARNING_FRAMES;
        });
    }

    fn page_down() -> KeyCombo {
        KeyCombo {
            key: "PageDown".into(),
            command: false,
            shift: false,
            alt: false,
        }
    }

    /// The remaps a test's macOS was asked for.
    type Asked = (u32, u32, Vec<(u32, u32)>);
    static REMAPS: Mutex<Vec<Asked>> = Mutex::new(Vec::new());

    /// Whether a test's macOS reads a remap back in place.
    static REMAP_READ_BACK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

    /// A test's macOS clock: on by one each time it is read.
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);

    /// A test's macOS: takes every remap, reads it back as `REMAP_READ_BACK`
    /// says, its clock `CLOCK`.
    const FAKE: Remapper = Remapper {
        set: |vendor, product, pairs| {
            REMAPS
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((vendor, product, pairs.to_vec()));
            true
        },
        in_place: |_, _, _| REMAP_READ_BACK.load(std::sync::atomic::Ordering::SeqCst),
        now: || CLOCK.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1,
    };

    /// The test clock's time now, not moved.
    fn clock() -> u64 {
        CLOCK.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn remaps() -> Vec<Asked> {
        std::mem::take(&mut *REMAPS.lock().unwrap_or_else(PoisonError::into_inner))
    }

    const PAGE_DOWN: u32 = 0x4E;
    const B: u32 = 0x05;

    /// The tests share the one state: one at a time.
    use super::TEST_SERIAL as SERIAL;

    /// Learning takes the first key pressed on a device that is not the
    /// Mac's own keyboard, and keeps it; from then on its `PageDown`, `PageUp`
    /// and B step and black out the show whatever app is in front — and
    /// nothing else's keys do.
    #[test]
    fn the_clicker_learned_moves_the_show_with_another_app_in_front_and_nothing_else_does() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clicker.json");
        let mut rx = fresh(Some(file.clone()));
        ask(ClickerAsk::Learn);
        assert_eq!(press(&mac_keyboard(), 1, PAGE_DOWN, true, true), Heard::Nothing);
        assert!(status().mac_keyboard && status().learning, "the Mac's keyboard not taken");
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Learned);
        assert!(status().just_learned, "the teaching press not the show's");
        let learned = status().device.expect("learned");
        assert_eq!((learned.vendor, learned.product), (0x046d, 0xc538));
        assert_eq!(load(&file), Some(learned), "kept");
        // The teaching press come again (a reader opened afresh): nothing.
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Nothing);
        assert!(rx.try_recv().is_err(), "learning moves nothing");
        learned_a_while_ago();

        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
        assert!(matches!(
            rx.try_recv(),
            Ok((EngineCommand::PresentationGo { to: Go::Next, .. }, None))
        ));
        assert_eq!(press(&clicker(), 2, B, true, false), Heard::Sent);
        assert!(matches!(
            rx.try_recv(),
            Ok((
                EngineCommand::TogglePresentationScreen {
                    screen: Screen::Black,
                    ..
                },
                None
            ))
        ));
        // Let go: nothing.
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, false, false), Heard::Nothing);
        // Nida in front, its keys not turned by macOS: the control window
        // gets them as a keyboard's — its.
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Nothing);
        assert!(rx.try_recv().is_err());
        // Typing in another app: the Mac's keyboard, another keyboard.
        assert_eq!(press(&mac_keyboard(), 1, B, true, false), Heard::Nothing);
        let other = DeviceInfo {
            product: 0x0001,
            ..clicker()
        };
        assert_eq!(press(&other, 3, PAGE_DOWN, true, false), Heard::Nothing);
        // A key bound to nothing of the show's.
        assert_eq!(press(&clicker(), 2, 0x04, true, false), Heard::Nothing);
        // Held with Shift, PageDown is another key.
        press(&clicker(), 2, 0xE1, true, false);
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Nothing);
        press(&clicker(), 2, 0xE1, false, false);
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "each press once");

        ask(ClickerAsk::Forget);
        assert_eq!(status().device, None);
        assert_eq!(load(&file), None, "taken away");
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Nothing);
    }

    /// The keys bound to the show's actions come from the keymap: a key
    /// learned for Next steps the show, the default one no longer does.
    #[test]
    fn the_keys_follow_the_keymap() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        with_state(|s| {
            s.status.device = Some(ClickerDevice {
                vendor: 0x046d,
                product: 0xc538,
                name: "USB Receiver".into(),
            });
            // Read as asked already: nothing opened in a test.
            s.reading = Some(Reading::Clicker(s.status.device.clone().unwrap()));
        });
        let mut bindings = HashMap::new();
        let right = KeyCombo {
            key: "ArrowRight".into(),
            command: false,
            shift: false,
            alt: false,
        };
        bindings.insert(right, KeyTarget::Action(ActionId::PresentationNext));
        frame(&bindings, WindowKeys::default());
        assert_eq!(press(&clicker(), 2, 0x4F, true, false), Heard::Sent);
        assert!(matches!(
            rx.try_recv(),
            Ok((EngineCommand::PresentationGo { to: Go::Next, .. }, None))
        ));
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Nothing);
    }

    /// Learned a while ago but no frame of the control window run since —
    /// a stalled window — the teaching press is still not the show's: it
    /// takes frames, not only time; and any key teaches, one the keymap does
    /// not name too.
    #[test]
    fn the_teaching_press_waits_on_the_windows_frames_and_any_key_teaches() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        ask(ClickerAsk::Learn);
        // A full stop: no name in the keymap, a clicker's all the same.
        assert_eq!(press(&clicker(), 2, 0x37, true, true), Heard::Learned);
        let device = status().device.expect("learned");
        with_state(|s| {
            s.learned_at = std::time::Instant::now().checked_sub(std::time::Duration::from_secs(5));
            s.reading = Some(Reading::Clicker(device.clone()));
        });
        assert!(status().just_learned, "no frame since: still the teaching press's");
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Nothing);
        let bindings: HashMap<KeyCombo, KeyTarget> = crate::keymap::KeymapStore::defaults();
        for _ in 0..LEARNING_FRAMES {
            frame(&bindings, WindowKeys::default());
        }
        assert!(!status().just_learned);
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
        assert!(rx.try_recv().is_ok());
    }

    /// The control window holding the keyboard for keys of its own — its
    /// Keyboard learn, the stage editor — the clicker steps nothing there;
    /// another app in front, or another of Nida's windows (an output) with
    /// the keyboard, it does.
    #[test]
    fn the_windows_own_uses_of_its_keys_hold_the_clicker_there() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        learned_a_while_ago();
        with_state(|s| s.status.device = Some(learned_clicker()));
        let bindings: HashMap<KeyCombo, KeyTarget> = crate::keymap::KeymapStore::defaults();
        for window in [
            WindowKeys {
                keyboard_learn: true,
                ..WindowKeys::default()
            },
            WindowKeys {
                stage_editor: true,
                ..WindowKeys::default()
            },
        ] {
            frame(&bindings, window);
            assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Nothing);
            assert!(rx.try_recv().is_err(), "{window:?}");
            assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
            assert!(rx.try_recv().is_ok());
            set_focused(false);
            assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Sent);
            assert!(rx.try_recv().is_ok());
            set_focused(true);
        }
    }

    fn learned_clicker() -> ClickerDevice {
        ClickerDevice {
            vendor: 0x046d,
            product: 0xc538,
            name: "USB Receiver".into(),
        }
    }

    /// The clicker read: a key macOS has turned on its device reaches no
    /// window, so with Nida in front too it is taken here — pressed after
    /// macOS took the remap; one pressed before reached the control window,
    /// and is the window's. A key not turned — B, a letter — is the window's
    /// with Nida in front, taken here with another app in front.
    #[test]
    fn a_turned_key_is_read_here_with_nida_in_front_and_one_not_turned_is_the_windows() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        learned_a_while_ago();
        let device = learned_clicker();
        with_state(|s| {
            s.status.device = Some(device.clone());
            s.status.access = Access::Granted;
            s.reading = Some(Reading::Clicker(device.clone()));
        });
        let bindings = crate::keymap::KeymapStore::defaults();
        let before = clock();
        on_device(2, true);
        frame(&bindings, WindowKeys::default());
        let taken = clock();
        assert_eq!(remaps().len(), 1);

        // Pressed before macOS took it: it reached the window — its.
        assert_eq!(on_key(&clicker(), 2, PAGE_DOWN, true, true, before), Heard::Nothing);
        assert!(rx.try_recv().is_err());
        // Pressed after: taken here, Nida in front or not.
        for active in [true, false] {
            assert_eq!(on_key(&clicker(), 2, PAGE_DOWN, true, active, taken + 1), Heard::Sent);
            assert!(rx.try_recv().is_ok());
        }
        // B, never turned: the window's with Nida in front, here else.
        assert_eq!(press(&clicker(), 2, B, true, true), Heard::Nothing);
        assert_eq!(press(&clicker(), 2, B, true, false), Heard::Sent);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "each once");
        // A device of it not given the remap (none come): the window's.
        assert_eq!(press(&clicker(), 9, PAGE_DOWN, true, true), Heard::Nothing);
    }

    /// Another device of the clicker come: the remap asked again for all of
    /// them. The device turned already keeps its keys turned throughout —
    /// none of its presses lost meanwhile; the one come is turned from when
    /// macOS took the remap again, its presses before that the window's.
    #[test]
    fn a_device_come_leaves_the_others_turned_and_is_turned_once_macos_takes_it() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        learned_a_while_ago();
        let device = learned_clicker();
        with_state(|s| {
            s.status.device = Some(device.clone());
            s.status.access = Access::Granted;
            s.reading = Some(Reading::Clicker(device.clone()));
        });
        let bindings = crate::keymap::KeymapStore::defaults();
        on_device(2, true);
        frame(&bindings, WindowKeys::default());
        let first = clock();
        let between = first + 1;
        on_device(3, true);
        let again = clock();
        assert_eq!(remaps().len(), 2, "asked again for the device come");
        assert!(between <= again);
        // The first device's press while it was asked again: still turned.
        assert_eq!(on_key(&clicker(), 2, PAGE_DOWN, true, true, between), Heard::Sent);
        assert!(rx.try_recv().is_ok());
        // The one come: the window's before macOS took it, here after.
        assert_eq!(on_key(&clicker(), 3, PAGE_DOWN, true, true, between), Heard::Nothing);
        assert_eq!(on_key(&clicker(), 3, PAGE_DOWN, true, true, again + 1), Heard::Sent);
        assert!(rx.try_recv().is_ok());
        // Gone: its keys no longer taken as turned.
        on_device(3, false);
        assert_eq!(on_key(&clicker(), 3, PAGE_DOWN, true, true, u64::MAX), Heard::Nothing);
        assert!(rx.try_recv().is_err());
    }

    /// A lift asked — Keyboard learn, Forget — and no key is taken as turned
    /// from the ask on; put back, turned again from when macOS took it. A
    /// remap not read back in place on every service is lifted, its keys
    /// the window's.
    #[test]
    fn a_lift_or_a_remap_not_in_place_leaves_the_clickers_keys_to_the_window() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        learned_a_while_ago();
        let device = learned_clicker();
        with_state(|s| {
            s.status.device = Some(device.clone());
            s.status.access = Access::Granted;
            s.reading = Some(Reading::Clicker(device.clone()));
        });
        let bindings = crate::keymap::KeymapStore::defaults();
        on_device(2, true);
        frame(&bindings, WindowKeys::default());
        let learning = WindowKeys {
            keyboard_learn: true,
            ..WindowKeys::default()
        };
        frame(&bindings, learning);
        assert!(with_state(|s| s.placed.is_empty()), "none turned from the lift's ask on");
        let back = clock();
        frame(&bindings, WindowKeys::default());
        assert_eq!(remaps().len(), 3, "put, lifted, put back");
        assert_eq!(on_key(&clicker(), 2, PAGE_DOWN, true, true, back), Heard::Nothing);
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Sent);
        assert!(rx.try_recv().is_ok());

        // Not read back in place: lifted.
        REMAP_READ_BACK.store(false, std::sync::atomic::Ordering::SeqCst);
        on_device(3, true);
        let asked = remaps();
        assert_eq!(asked.len(), 2, "asked, then lifted: {asked:?}");
        assert!(asked[1].2.is_empty(), "the lift");
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Nothing);
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    /// The read-back counts only when every service of the device turns
    /// every key into its own spare: one service without, or a key turned
    /// on one service and another on the other, or a key turned into
    /// another spare, is not in place.
    #[test]
    fn a_remap_is_in_place_only_on_every_service() {
        let both = "RegistryID  Key                   Value
10009f73d   UserKeyMapping   (
        {
        HIDKeyboardModifierMappingDst = 30064771186;
        HIDKeyboardModifierMappingSrc = 30064771147;
    },
        {
        HIDKeyboardModifierMappingDst = 30064771187;
        HIDKeyboardModifierMappingSrc = 30064771150;
    }
)
10009f735   UserKeyMapping   (
        {
        HIDKeyboardModifierMappingDst = 0x700000072;
        HIDKeyboardModifierMappingSrc = 0x70000004b;
    },
        {
        HIDKeyboardModifierMappingSrc = 30064771150;
        HIDKeyboardModifierMappingDst = 30064771187;
    }
)
";
        let pairs = [(0x4B, 0x72), (0x4E, 0x73)];
        assert!(turned_on_every_service(both, &pairs));
        let one_without = format!("{both}10009f740   UserKeyMapping   (null)\n");
        assert!(!turned_on_every_service(&one_without, &pairs), "a service without");
        let split = "10009f73d   UserKeyMapping   (
        {
        HIDKeyboardModifierMappingDst = 30064771186;
        HIDKeyboardModifierMappingSrc = 30064771147;
    }
)
10009f735   UserKeyMapping   (
        {
        HIDKeyboardModifierMappingDst = 30064771187;
        HIDKeyboardModifierMappingSrc = 30064771150;
    }
)
";
        assert!(!turned_on_every_service(split, &pairs), "one key on each service");
        let crossed = both.replace("30064771186", "30064771185");
        assert!(!turned_on_every_service(&crossed, &pairs), "into another spare");
        assert!(!turned_on_every_service("RegistryID  Key  Value\n", &pairs), "no service");
    }

    /// What a press heard while macOS read a remap back did.
    static HEARD_WHILE_READ: Mutex<Option<Heard>> = Mutex::new(None);

    /// A press of a newly turned device while its remap is read back — not
    /// on every service, as it turns out — is not taken here: through the
    /// service macOS did not turn, it reaches the window too.
    #[test]
    fn a_press_while_a_remap_is_read_back_is_not_taken_as_turned() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        learned_a_while_ago();
        let device = learned_clicker();
        with_state(|s| {
            s.status.device = Some(device.clone());
            s.status.access = Access::Granted;
            s.reading = Some(Reading::Clicker(device.clone()));
            s.remapper = Some(Remapper {
                in_place: |_, _, _| {
                    let heard = on_key(&clicker(), 2, PAGE_DOWN, true, true, u64::MAX);
                    *HEARD_WHILE_READ.lock().unwrap_or_else(PoisonError::into_inner) = Some(heard);
                    false
                },
                ..FAKE
            });
        });
        on_device(2, true);
        frame(&crate::keymap::KeymapStore::defaults(), WindowKeys::default());
        assert_eq!(
            *HEARD_WHILE_READ.lock().unwrap_or_else(PoisonError::into_inner),
            Some(Heard::Nothing)
        );
        assert!(rx.try_recv().is_err(), "the window's alone");
        assert!(with_state(|s| s.placed.is_empty()), "lifted");
    }

    /// A remap taken after another was asked places nothing — the later one
    /// decides, and may lift it.
    #[test]
    fn only_the_remap_asked_last_places_its_keys() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let _rx = fresh(None);
        let job = |asked| RemapJob {
            vendor: 1,
            product: 2,
            pairs: vec![(0x4E, 0x73)],
            devices: vec![2],
            asked,
            done: None,
        };
        with_state(|s| s.remap_asked = 2);
        run(FAKE, &job(1));
        assert!(with_state(|s| s.placed.is_empty()), "an older one");
        run(FAKE, &job(2));
        assert_eq!(with_state(|s| s.placed.len()), 1);
    }

    /// Nida starting with a clicker learned before: the control window is
    /// taken as having the keyboard until it says otherwise — its Keyboard
    /// learn holds a press with Nida in front, not the show's.
    #[test]
    fn nida_starting_takes_the_window_as_having_the_keyboard() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clicker.json");
        let learned = ClickerDevice {
            vendor: 0x046d,
            product: 0xc538,
            name: "USB Receiver".into(),
        };
        save(&file, Some(&learned)).unwrap();
        *STATE.lock().unwrap_or_else(PoisonError::into_inner) = None;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        // Read as asked already: nothing opened, nor remapped, in a test.
        with_state(|s| {
            s.reading = Some(Reading::Clicker(learned.clone()));
            s.remapper = Some(FAKE);
        });
        start_with(tx, Some(file));
        assert_eq!(status().device, Some(learned));
        with_state(|s| {
            s.bindings = vec![(page_down(), ActionId::PresentationNext)];
            s.window.keyboard_learn = true;
        });
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Nothing);
        assert!(rx.try_recv().is_err());
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
    }

    /// The clicker read, its keys the show uses — letters and digits left
    /// out — are turned into spare ones for every other app, keys macOS
    /// makes no press of: asked of macOS for its device, again for each of
    /// its devices that comes; lifted for Keyboard learn, on Forget, and
    /// given back as Nida quits — even after a lift still being asked. A
    /// spare key, should the reader be given it, is read as the key it
    /// stands for, Nida in front or not.
    #[test]
    fn the_clickers_keys_are_kept_from_other_apps_while_it_is_read() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let mut rx = fresh(None);
        learned_a_while_ago();
        let device = learned_clicker();
        with_state(|s| {
            s.status.device = Some(device.clone());
            s.status.access = Access::Granted;
            s.reading = Some(Reading::Clicker(device.clone()));
        });
        let bindings = crate::keymap::KeymapStore::defaults();
        frame(&bindings, WindowKeys::default());
        // PageDown and PageUp into F24 and F23; B — a letter — never.
        let asked = vec![(0x4B, 0x72), (0x4E, 0x73)];
        assert_eq!(remaps(), vec![(0x046d, 0xc538, asked.clone())]);
        frame(&bindings, WindowKeys::default());
        assert!(remaps().is_empty(), "asked once");
        on_device(2, true);
        assert_eq!(remaps(), vec![(0x046d, 0xc538, asked.clone())], "a device come");

        // The spare key read as its key, whatever app is in front.
        for active in [true, false] {
            assert_eq!(press(&clicker(), 2, 0x73, true, active), Heard::Sent);
            assert!(matches!(
                rx.try_recv(),
                Ok((EngineCommand::PresentationGo { to: Go::Next, .. }, None))
            ));
        }
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, false), Heard::Sent);
        assert!(rx.try_recv().is_ok());
        // A spare key from another keyboard: nothing.
        let other = DeviceInfo {
            product: 0x0001,
            ..clicker()
        };
        assert_eq!(press(&other, 3, 0x73, true, false), Heard::Nothing);

        // Keyboard learn: lifted, put back after.
        let learning = WindowKeys {
            keyboard_learn: true,
            ..WindowKeys::default()
        };
        frame(&bindings, learning);
        assert_eq!(remaps(), vec![(0x046d, 0xc538, vec![])]);
        frame(&bindings, WindowKeys::default());
        assert_eq!(remaps(), vec![(0x046d, 0xc538, asked)]);

        // Forgotten: its lift asked.
        ask(ClickerAsk::Forget);
        frame(&bindings, WindowKeys::default());
        assert_eq!(remaps(), vec![(0x046d, 0xc538, vec![])]);
        assert_eq!(press(&clicker(), 2, PAGE_DOWN, true, true), Heard::Nothing);
        // Nida quitting gives it back all the same.
        stop();
        assert_eq!(remaps(), vec![(0x046d, 0xc538, vec![])], "given back at quit");
    }

    /// The remap waits for macOS to let Nida read the clicker: unread, its
    /// keys turned for every app would be heard nowhere.
    #[test]
    fn the_remap_waits_for_macos_to_let_nida_read_the_clicker() {
        let _one = SERIAL.lock().unwrap_or_else(PoisonError::into_inner);
        let _rx = fresh(None);
        with_state(|s| {
            s.status.device = Some(learned_clicker());
            s.reading = Some(Reading::Clicker(learned_clicker()));
            s.status.access = Access::Denied;
        });
        let bindings = crate::keymap::KeymapStore::defaults();
        frame(&bindings, WindowKeys::default());
        assert!(remaps().is_empty());
        with_state(|s| s.status.access = Access::Granted);
        frame(&bindings, WindowKeys::default());
        assert_eq!(remaps().len(), 1);
    }

    /// A key still bound keeps its spare as others are bound: a press on its
    /// way as that spare is still read as that key. One newly bound takes a
    /// spare free; one no longer bound frees its own.
    #[test]
    fn a_key_keeps_its_spare_as_others_are_bound() {
        let combo = |key: &str| KeyCombo {
            key: key.into(),
            command: false,
            shift: false,
            alt: false,
        };
        let defaults = vec![
            (combo("PageDown"), ActionId::PresentationNext),
            (combo("PageUp"), ActionId::PresentationPrevious),
            (combo("B"), ActionId::PresentationBlack),
        ];
        let first = remap_for(&defaults, &Vec::new());
        assert_eq!(first, vec![(0x4B, 0x72, "PageUp"), (0x4E, 0x73, "PageDown")]);
        let mut more = defaults.clone();
        more.push((combo("ArrowLeft"), ActionId::PresentationPrevious));
        let second = remap_for(&more, &first);
        assert!(second.contains(&(0x4E, 0x73, "PageDown")), "kept: {second:?}");
        assert!(second.contains(&(0x50, 0x71, "ArrowLeft")), "a free one: {second:?}");
        let fewer = vec![(combo("ArrowLeft"), ActionId::PresentationPrevious)];
        let third = remap_for(&fewer, &second);
        assert_eq!(third, vec![(0x50, 0x71, "ArrowLeft")]);
    }

    /// Every spare key is one macOS makes no key press of — none of F13 to
    /// F20, which it does (an app with no use for one beeps, F20 types) — and
    /// none a key the show could use.
    #[test]
    fn the_spare_keys_are_ones_macos_makes_no_press_of() {
        for spare in SPARE_KEYS {
            assert!(!(0x68..=0x6F).contains(&spare), "{spare:#x}: F13 to F20");
            assert!((0x70..=0x74).contains(&spare), "{spare:#x}: F21 to F24, Execute");
            assert_eq!(key_name(spare), None, "{spare:#x}: a key the keymap names");
        }
    }

    #[test]
    fn keys_are_named_as_the_keymap_names_them() {
        assert_eq!(key_name(0x04), Some("A"));
        assert_eq!(key_name(0x1D), Some("Z"));
        assert_eq!(key_name(0x1E), Some("Num1"));
        assert_eq!(key_name(0x27), Some("Num0"));
        assert_eq!(key_name(0x3E), Some("F5"));
        assert_eq!(key_name(0x4B), Some("PageUp"));
        assert_eq!(key_name(0x4E), Some("PageDown"));
        assert_eq!(key_name(0x37), None, "a full stop: not a key the keymap names");
        assert_eq!(key_name(0x59), Some("Num1"), "the keypad's 1, as the row's");
        assert_eq!(key_name(0x62), Some("Num0"));
        assert_eq!(key_name(0x56), Some("Minus"));
        assert_eq!(key_name(0x57), Some("Plus"));
        for usage in 0..0x100 {
            if let Some(name) = key_name(usage) {
                assert!(crate::keymap::is_supported_key_name(name), "{name}");
            }
        }
    }

    #[test]
    fn a_damaged_file_is_no_clicker() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("clicker.json");
        std::fs::write(&file, "{ not json").unwrap();
        assert_eq!(load(&file), None);
        assert_eq!(load(&dir.path().join("none.json")), None);
    }
}
