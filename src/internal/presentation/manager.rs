//! The presentation worker: one thread talks to PowerPoint for every
//! PowerPoint input, so a slow, busy or asking PowerPoint never holds the
//! show. The engine hands it jobs and takes back what it found: each deck's
//! notes, where its show is, and a capture of the show's window — opened
//! here, the slow part, for the engine to adopt in a moment.
//!
//! Nothing waits on PowerPoint for long: a deck being opened is looked for
//! at each reading rather than waited on, so the show on air moves on
//! meanwhile. A show that stops by itself — Esc pressed in PowerPoint, the
//! deck closed there, PowerPoint quit — is started again where it was, a
//! little later: while the input exists, it shows its slides.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use super::powerpoint::{
    Go, MediaAction, Moved, Osascript, Placement, PowerPoint, Reading, Runner, Screen, ScriptError,
    ShowState, Video, VideoReading, VideoState, POWERPOINT_ID,
};
use super::pptx;
use super::screens::ScreenSpot;
use crate::engine::value::presentation::VideoControl;
use crate::screen_capture::backend::{
    CaptureConfig, CaptureTargetInfo, CaptureTargetKind, ScreenCaptureBackend,
};

/// How often a show is read when nothing is asked of it.
const POLL: Duration = Duration::from_millis(300);
/// How long PowerPoint may take to open a deck.
const OPEN_WAIT: Duration = Duration::from_secs(60);
/// How long a new show's window is looked for before saying so.
const WINDOW_WAIT: Duration = Duration::from_secs(10);
/// After a failure, the first try again; each further one waits twice as
/// long, up to the last.
const RETRY_FIRST: Duration = Duration::from_secs(2);
const RETRY_MOST: Duration = Duration::from_secs(16);
/// The engine asks again for a deck's window at most this often.
const ASK_WINDOW_EVERY: Duration = Duration::from_secs(2);
/// How long Nida's closing waits for shows to end and decks to close.
const SHUT_DOWN_WAIT: Duration = Duration::from_secs(3);
/// A show's capture rate: animations and video move smoothly.
pub const SHOW_CAPTURE_RATE: f32 = 60.0;

/// What the engine asks of the worker.
#[derive(Debug, Clone, PartialEq)]
pub enum Job {
    /// Open the deck at `path` and play it in a window `pixels` big (the
    /// slide's shape kept within it) on the screen `spot` (this Mac's when
    /// `None`), from `slide` or its first.
    Open {
        deck: String,
        path: PathBuf,
        slide: Option<u32>,
        pixels: (u32, u32),
        spot: Option<ScreenSpot>,
    },
    /// Play the deck's show on the screen `spot` from now on: moved there
    /// now when it runs.
    Place {
        deck: String,
        spot: ScreenSpot,
    },
    Go {
        deck: String,
        to: Go,
    },
    Screen {
        deck: String,
        screen: Screen,
    },
    /// End the show and play it again where it was.
    Restart {
        deck: String,
    },
    /// The input is gone: end its show, and close the deck if Nida opened it.
    Close {
        deck: String,
    },
    /// The engine has no picture of the deck's show (its capture failed, or
    /// an undo made the deck again): find and capture the window again.
    Window {
        deck: String,
    },
    /// Play, pause or move the video on the slide on screen.
    Video {
        deck: String,
        control: crate::engine::value::presentation::VideoControl,
    },
}

/// How a deck's show stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// PowerPoint is opening the deck or starting its show.
    Opening,
    /// The show is running and its window captured.
    Showing,
    /// It is not, and why; tried again by itself.
    Failed(ScriptError),
    /// The show runs but its window is not captured, and why.
    NoWindow(String),
}

/// A capture of a show's window, opened on the worker's thread.
pub struct Capture(pub Box<dyn ScreenCaptureBackend>);

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Capture({})", self.0.label())
    }
}

/// What the worker found.
#[derive(Debug)]
pub enum Event {
    /// The deck's notes and slide size, read from the file; or why not.
    Notes {
        deck: String,
        doc: Result<pptx::Document, String>,
    },
    Status {
        deck: String,
        status: Status,
    },
    Show {
        deck: String,
        state: ShowState,
    },
    /// The window the show plays in, and a capture of it to adopt, with
    /// how it was set (its title bar cut off).
    Window {
        deck: String,
        target: CaptureTargetInfo,
        capture: Capture,
        config: CaptureConfig,
    },
    /// The video on the slide on screen, or none, when that changes.
    Video {
        deck: String,
        video: Option<Video>,
    },
}

/// The windows macOS lists, and captures of them: the slow part of a
/// capture, done on the worker's thread.
pub trait WindowFinder: Send + 'static {
    /// The windows macOS lists.
    ///
    /// # Errors
    ///
    /// Why it lists none, in words: Nida not allowed to record the screen.
    fn windows(&self) -> Result<Vec<CaptureTargetInfo>, String>;
    /// A capture of `target`, set to `config`.
    ///
    /// # Errors
    ///
    /// Why it could not be opened, in words.
    fn open(
        &self,
        target: &CaptureTargetInfo,
        config: &CaptureConfig,
    ) -> Result<Box<dyn ScreenCaptureBackend>, String>;
}

/// `ScreenCaptureKit`'s windows and captures.
#[derive(Debug, Default, Clone, Copy)]
pub struct ScreenWindows;

/// What a show's input says when macOS lists Nida no windows: the switch
/// to turn on, and that Nida must be opened again to have it.
pub const SCREEN_RECORDING_OFF: &str = "Nida may not record the screen: switch Nida on in \
     System Settings → Privacy & Security → Screen & System Audio Recording, then quit and \
     reopen Nida";

impl WindowFinder for ScreenWindows {
    fn windows(&self) -> Result<Vec<CaptureTargetInfo>, String> {
        match crate::screen_capture::platform::enumerate() {
            Ok(windows) => Ok(windows),
            Err(crate::screen_capture::backend::CaptureError::PermissionDenied) => {
                Err(SCREEN_RECORDING_OFF.to_string())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn open(
        &self,
        target: &CaptureTargetInfo,
        config: &CaptureConfig,
    ) -> Result<Box<dyn ScreenCaptureBackend>, String> {
        crate::screen_capture::platform::open(target, config).map_err(|e| e.to_string())
    }
}

/// A window's title bar on this macOS, points: PowerPoint's show window has
/// one over the slide, which the capture leaves out. 32 from macOS 26 on —
/// `AppKit`'s own figure on 27, and what PowerPoint's window measured — 28
/// before.
#[must_use]
pub fn title_bar_points() -> u32 {
    #[cfg(target_os = "macos")]
    {
        let version = objc2_foundation::NSProcessInfo::processInfo().operatingSystemVersion();
        if version.majorVersion >= 26 {
            32
        } else {
            28
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        0
    }
}

/// How a show's window is captured, for a picture `pixels` big: the title
/// bar (`title_bar` points) cut off, the slide under it kept whole, at the
/// screen's own pixels (a Retina screen's 960 × 540 points are 1920 × 1080
/// pixels), no more than the picture needs.
///
/// The capture's `scale_to` bounds the whole window and the crop is taken
/// from that (see `screen_capture::platform::macos::output_size`): the bound
/// is the picture's size grown by what the crop takes away, so the slide
/// comes out at most `pixels`.
#[must_use]
pub fn capture_config(
    target: &CaptureTargetInfo,
    pixels: (u32, u32),
    title_bar: u32,
) -> CaptureConfig {
    let high = target.height.max(2);
    let bar = title_bar.min(high - 1);
    #[allow(clippy::cast_precision_loss)]
    let kept = (high - bar) as f32 / high as f32;
    let crop = crate::screen_capture::backend::CropRect {
        x: 0.0,
        #[allow(clippy::cast_precision_loss)]
        y: bar as f32 / high as f32,
        w: 1.0,
        h: kept,
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bound = (
        pixels.0.max(1),
        (pixels.1.max(1) as f32 / kept).ceil() as u32,
    );
    CaptureConfig {
        rate: SHOW_CAPTURE_RATE,
        crop,
        show_cursor: false,
        exclude_varda: false,
        scale_to: Some(bound),
    }
}

/// The show's window among `targets`: PowerPoint's, the size PowerPoint
/// says its show window is (screen points, within 2 for rounding), and not
/// one `taken` by another file's input. Of several that size: the one whose
/// title has the deck's file name (`talk.pptx`); else its name as a whole
/// word (`talk`, not `talk2`); else one naming a slide show; else the newest
/// (the highest window number: the show just started).
#[must_use]
pub fn pick_window<S: std::hash::BuildHasher>(
    targets: &[CaptureTargetInfo],
    window: Placement,
    deck: &Path,
    taken: &HashSet<u64, S>,
) -> Option<CaptureTargetInfo> {
    let near = |a: u32, b: i32| (i64::from(a) - i64::from(b)).abs() <= 2;
    let sized: Vec<&CaptureTargetInfo> = targets
        .iter()
        .filter(|t| t.kind == CaptureTargetKind::Window && !t.is_varda)
        .filter(|t| t.app.as_deref() == Some(POWERPOINT_ID))
        .filter(|t| !taken.contains(&t.platform_id))
        .filter(|t| near(t.width, window.width) && near(t.height, window.height))
        .collect();
    if sized.len() <= 1 {
        return sized.first().map(|t| (*t).clone());
    }
    let lower = |s: Option<&std::ffi::OsStr>| {
        s.map(|s| s.to_string_lossy().to_lowercase())
            .unwrap_or_default()
    };
    let (file, stem) = (lower(deck.file_name()), lower(deck.file_stem()));
    let title = |t: &CaptureTargetInfo| t.title.clone().unwrap_or_default().to_lowercase();
    let pick = |keep: &dyn Fn(&str) -> bool| -> Vec<&CaptureTargetInfo> {
        sized.iter().copied().filter(|t| keep(&title(t))).collect()
    };
    let tiers = [
        pick(&|t: &str| !file.is_empty() && t.contains(&file)),
        pick(&|t: &str| !stem.is_empty() && has_word(t, &stem)),
        pick(&|t: &str| t.contains("slide show")),
    ];
    let pool = tiers
        .into_iter()
        .find(|tier| !tier.is_empty())
        .unwrap_or_else(|| sized.clone());
    pool.into_iter()
        .max_by_key(|t| t.platform_id)
        .map(|t| (*t).clone())
}

/// Whether `word` stands in `text` on its own: not inside a longer word.
fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

/// A show's window size, screen points, for a capture `pixels` big on a
/// screen of `scale` pixels to the point: the slide's shape (`aspect`,
/// width over height) fitted inside, and the title bar (`title_bar` points)
/// over it.
#[must_use]
pub fn placement(pixels: (u32, u32), aspect: Option<f64>, scale: f64, title_bar: u32) -> Placement {
    let (w, h) = (f64::from(pixels.0.max(1)), f64::from(pixels.1.max(1)));
    let (w, h) = match aspect.filter(|a| a.is_finite() && *a > 0.0) {
        Some(a) if w / h > a => (h * a, h),
        Some(a) => (w, w / a),
        None => (w, h),
    };
    let scale = if scale.is_finite() && scale >= 1.0 {
        scale
    } else {
        1.0
    };
    let bar = i32::try_from(title_bar).unwrap_or(0);
    Placement {
        left: 0,
        top: 0,
        width: (w / scale).round() as i32,
        height: (h / scale).round() as i32 + bar,
    }
}

/// The window PowerPoint actually gave — macOS keeps it inside the screen,
/// above the Dock — made the slide's shape under its title bar, so the slide
/// fills it with no bars beside or above it: narrowed when the screen kept it
/// low, lowered when it kept it narrow; `None` when it is the shape already.
#[must_use]
pub fn fitted(window: Placement, aspect: Option<f64>, title_bar: u32) -> Option<Placement> {
    let aspect = aspect
        .filter(|a| a.is_finite() && *a > 0.0)
        .unwrap_or(16.0 / 9.0);
    let bar = i32::try_from(title_bar).unwrap_or(0);
    let content = (window.height - bar).max(1);
    let wide = (f64::from(content) * aspect).round() as i32;
    if window.width > wide + 1 {
        return Some(Placement {
            width: wide,
            ..window
        });
    }
    let high = (f64::from(window.width.max(1)) / aspect).round() as i32;
    (content > high + 1).then_some(Placement {
        height: high + bar,
        ..window
    })
}

/// The main display's pixels to a point: 2 on a Retina screen.
#[must_use]
pub fn main_display_scale() -> f64 {
    #[cfg(target_os = "macos")]
    {
        use objc2_core_graphics::{CGDisplayCopyDisplayMode, CGDisplayMode, CGMainDisplayID};
        if let Some(mode) = CGDisplayCopyDisplayMode(CGMainDisplayID()) {
            let points = CGDisplayMode::width(Some(&mode));
            let pixels = CGDisplayMode::pixel_width(Some(&mode));
            if points > 0 {
                return pixels as f64 / points as f64;
            }
        }
    }
    1.0
}

struct Session {
    path: PathBuf,
    pixels: (u32, u32),
    /// The screen its window goes on; this Mac's when `None`.
    spot: Option<ScreenSpot>,
    aspect: Option<f64>,
    slide: Option<u32>,
    /// Nida opened the deck (`Some(true)`), or found it open; `None` until
    /// it is open.
    opened: Option<bool>,
    /// Opened in PowerPoint, not yet there: since when.
    launching: Option<Instant>,
    show: Option<ShowState>,
    /// The show's window captured (its window number), for this run of it.
    window: Option<u64>,
    looking_since: Option<Instant>,
    /// Why its window's capture last failed to open, as logged: logged again
    /// only when it changes.
    capture_failed: Option<String>,
    /// The slide last looked at for a video, and its video: looked for
    /// again only when the slide changes.
    video: Option<(u32, Option<Video>)>,
    /// When to try again after a failure, and how long the wait was.
    retry_at: Option<Instant>,
    retry_wait: Duration,
}

/// The worker's side: PowerPoint, the windows, the shows. Its calls run on
/// the worker thread; tests call them directly.
pub struct Worker<R: Runner, F: WindowFinder> {
    ppt: PowerPoint<R>,
    finder: F,
    scale: f64,
    /// The height of a window's title bar, points.
    title_bar: u32,
    events: mpsc::Sender<Event>,
    sessions: HashMap<String, Session>,
    /// Decks Nida was opening for inputs since gone: closed once PowerPoint
    /// has them, given up on after a while.
    orphans: Vec<(PathBuf, Instant)>,
}

impl<R: Runner, F: WindowFinder> Worker<R, F> {
    pub fn new(runner: R, finder: F, scale: f64, events: mpsc::Sender<Event>) -> Self {
        Self {
            ppt: PowerPoint::new(runner),
            finder,
            scale,
            title_bar: title_bar_points(),
            events,
            sessions: HashMap::new(),
            orphans: Vec::new(),
        }
    }

    fn send(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn status(&self, deck: &str, status: Status) {
        self.send(Event::Status {
            deck: deck.to_string(),
            status,
        });
    }

    pub fn handle(&mut self, job: Job, now: Instant) {
        match job {
            Job::Open {
                deck,
                path,
                slide,
                pixels,
                spot,
            } => self.open(&deck, path, slide, pixels, spot, now),
            Job::Place { deck, spot } => self.move_to(&deck, spot, now),
            Job::Go { deck, to } => self.go(&deck, to, now),
            Job::Video { deck, control } => self.video(&deck, control),
            Job::Screen { deck, screen } => {
                let Some(path) = self.showing(&deck) else {
                    return;
                };
                let result = self.ppt.screen(&path, screen);
                self.after_step(&deck, result, now);
            }
            Job::Restart { deck } => {
                let Some(session) = self.sessions.get_mut(&deck) else {
                    return;
                };
                session.slide = session.show.map(|s| s.slide).or(session.slide);
                session.show = None;
                session.window = None;
                let path = session.path.clone();
                let _ = self.ppt.end(&path);
                self.bring_up(&deck, now);
            }
            Job::Close { deck } => self.close(&deck),
            Job::Window { deck } => {
                if let Some(session) = self.sessions.get_mut(&deck) {
                    session.window = None;
                    session.looking_since = Some(now);
                }
                self.look_for_window(&deck, now);
            }
        }
    }

    /// The file of a deck whose show is running.
    fn showing(&self, deck: &str) -> Option<PathBuf> {
        let session = self.sessions.get(deck)?;
        session.show.map(|_| session.path.clone())
    }

    fn open(
        &mut self,
        deck: &str,
        path: PathBuf,
        slide: Option<u32>,
        pixels: (u32, u32),
        spot: Option<ScreenSpot>,
        now: Instant,
    ) {
        // The deck showed another file: that one is ended (and closed, if
        // Nida opened it) first.
        if self.sessions.get(deck).is_some_and(|s| s.path != path) {
            self.close(deck);
        }
        let doc = pptx::read(&path).map_err(|e| e.to_string());
        let aspect = doc.as_ref().ok().map(pptx::Document::aspect);
        self.send(Event::Notes {
            deck: deck.to_string(),
            doc,
        });
        self.sessions.insert(
            deck.to_string(),
            Session {
                path,
                pixels,
                spot,
                aspect,
                slide,
                opened: None,
                launching: None,
                show: None,
                window: None,
                looking_since: None,
                capture_failed: None,
                video: None,
                retry_at: None,
                retry_wait: RETRY_FIRST,
            },
        );
        self.bring_up(deck, now);
    }

    /// Open the deck when not open — without waiting for PowerPoint to have
    /// it: [`Self::tick`] looks — and start its show when it is.
    fn bring_up(&mut self, deck: &str, now: Instant) {
        let Some(session) = self.sessions.get(deck) else {
            return;
        };
        if session.launching.is_some() {
            return;
        }
        let path = session.path.clone();
        let opened = session.opened;
        self.status(deck, Status::Opening);
        if opened.is_none() {
            match self.ppt.read(&path) {
                // Open already: by hand, or for another input.
                Ok(_) => {
                    if let Some(session) = self.sessions.get_mut(deck) {
                        session.opened = Some(false);
                    }
                }
                Err(ScriptError::NotOpen | ScriptError::NotRunning) => {
                    match self.ppt.launch(&path) {
                        Ok(()) => {
                            if let Some(session) = self.sessions.get_mut(deck) {
                                session.launching = Some(now);
                            }
                        }
                        Err(e) => self.failed(deck, e, now),
                    }
                    return;
                }
                Err(e) => return self.failed(deck, e, now),
            }
        }
        self.start(deck, now);
    }

    /// The show of `deck` on the screen `spot` from now on: moved there now
    /// when it runs, and captured again. Not when it is there already — the
    /// show is the file's, and another input of it may have moved it.
    fn move_to(&mut self, deck: &str, spot: ScreenSpot, now: Instant) {
        let Some(session) = self.sessions.get_mut(deck) else {
            return;
        };
        let there = session.show.is_some_and(|s| spot.holds(s.window));
        if session.spot == Some(spot) && there {
            return;
        }
        session.spot = Some(spot);
        if session.show.is_some() {
            self.start(deck, now);
        }
    }

    /// Play the open deck's show in a window, or take it as it plays: placed
    /// where Nida wants it — and played again in a window if it played full
    /// screen, which Nida cannot show.
    fn start(&mut self, deck: &str, now: Instant) {
        let Some(session) = self.sessions.get(deck) else {
            return;
        };
        let path = session.path.clone();
        // At the top-left of its screen, that screen's pixels to a point:
        // macOS moves it below the menu bar, and keeps it within the screen.
        let spot = session.spot.unwrap_or(ScreenSpot::main_at(self.scale));
        let at = Placement {
            left: spot.left,
            top: spot.top,
            ..placement(session.pixels, session.aspect, spot.scale, self.title_bar)
        };
        let aspect = session.aspect;
        let slide = session.slide;
        let result = match self.ppt.read(&path) {
            Ok(Reading::Showing(state)) if state.full_screen => {
                let from = Some(state.slide);
                let _ = self.ppt.end(&path);
                let started = self.ppt.start(&path, at, from);
                self.ppt.refocus();
                started
            }
            Ok(Reading::Showing(_)) => self.ppt.place(&path, at),
            Ok(Reading::Open { .. }) => {
                let started = self.ppt.start(&path, at, slide);
                self.ppt.refocus();
                started
            }
            Err(e) => Err(e),
        };
        // Kept smaller than asked (a screen too small for it): narrowed to the
        // slide's shape, so no bars stand beside the slide.
        let result = match result {
            Ok(state) => match fitted(state.window, aspect, self.title_bar) {
                Some(narrower) => self.ppt.place(&path, narrower).or(Ok(state)),
                None => Ok(state),
            },
            Err(e) => Err(e),
        };
        match result {
            Ok(state) => {
                if !spot.holds(state.window) {
                    log::warn!(
                        "PowerPoint: the show's window did not stay on the screen chosen \
                         (display {}, at {}, {}): it is at {}, {}",
                        spot.display,
                        spot.left,
                        spot.top,
                        state.window.left,
                        state.window.top
                    );
                }
                // A new run of the show, in a new window, for every input of
                // the file: each captures it again.
                let sharing: Vec<String> = self
                    .sessions
                    .iter()
                    .filter(|(_, s)| s.path == path)
                    .map(|(d, _)| d.clone())
                    .collect();
                for each in sharing {
                    if let Some(session) = self.sessions.get_mut(&each) {
                        let was_down = session.show.is_none();
                        session.show = Some(state);
                        session.slide = Some(state.slide);
                        session.window = None;
                        session.looking_since = Some(now);
                        session.retry_at = None;
                        session.retry_wait = RETRY_FIRST;
                        if was_down && each != deck {
                            session.launching = None;
                            session.opened.get_or_insert(false);
                        }
                    }
                    self.send(Event::Show {
                        deck: each.clone(),
                        state,
                    });
                    self.look_for_window(&each, now);
                }
            }
            Err(e) => self.failed(deck, e, now),
        }
    }

    fn failed(&mut self, deck: &str, error: ScriptError, now: Instant) {
        // A slow failure has spent time: the wait counts from its end.
        let now = now.max(Instant::now());
        // No show, no video: said, so the panel does not keep one.
        self.set_video(deck, None);
        if let Some(session) = self.sessions.get_mut(deck) {
            session.slide = session.show.map(|s| s.slide).or(session.slide);
            session.show = None;
            session.window = None;
            session.launching = None;
            if matches!(error, ScriptError::NotOpen | ScriptError::NotRunning) {
                // To be opened again; then it is Nida's to close.
                session.opened = None;
            }
            session.retry_at = Some(now + session.retry_wait);
            session.retry_wait = (session.retry_wait * 2).min(RETRY_MOST);
        }
        self.status(deck, Status::Failed(error));
    }

    fn go(&mut self, deck: &str, to: Go, now: Instant) {
        let Some(path) = self.showing(deck) else {
            // Not running (yet): starting it is already in hand.
            return;
        };
        // Past its end screen PowerPoint ends the show and closes its
        // window: the input would go black. It stays on the end screen —
        // whichever input of the file saw it last.
        let ended = self
            .sessions
            .values()
            .filter(|s| s.path == path)
            .any(|s| s.show.is_some_and(|st| st.screen == Screen::Ended));
        if to == Go::Next && ended {
            return;
        }
        let result = self.ppt.go(&path, to);
        self.after_step(deck, result, now);
    }

    fn after_step(&mut self, deck: &str, result: Result<Moved, ScriptError>, now: Instant) {
        match result {
            // A step changes the slide and the screen; the window and the
            // deck stay as last read.
            Ok(moved) => {
                if let Some(last) = self.sessions.get(deck).and_then(|s| s.show) {
                    self.update(deck, moved.onto(last), now);
                } else {
                    let path = self.sessions.get(deck).map(|s| s.path.clone());
                    if let Some(Ok(Reading::Showing(state))) = path.map(|p| self.ppt.read(&p)) {
                        self.update(deck, state, now);
                    }
                }
            }
            // Ended in PowerPoint meanwhile: start it again at once.
            Err(ScriptError::NoShow) => {
                if let Some(session) = self.sessions.get_mut(deck) {
                    session.slide = session.show.map(|s| s.slide).or(session.slide);
                    session.show = None;
                    session.window = None;
                }
                self.bring_up(deck, now);
            }
            Err(e @ (ScriptError::NotOpen | ScriptError::NotRunning | ScriptError::NotAllowed)) => {
                self.failed(deck, e, now);
            }
            // A step PowerPoint would not take (busy, or a slide it will not
            // go to): the show is still there; the next reading says where.
            Err(e) => log::warn!("PowerPoint: a step did not go: {e}"),
        }
    }

    /// A fresh reading of a running show, for every input of its file.
    fn update(&mut self, deck: &str, state: ShowState, now: Instant) {
        let Some(path) = self.sessions.get(deck).map(|s| s.path.clone()) else {
            return;
        };
        let sharing: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.path == path)
            .map(|(d, _)| d.clone())
            .collect();
        for each in sharing {
            let Some(session) = self.sessions.get_mut(&each) else {
                continue;
            };
            let resized = session.show.is_some_and(|old| {
                old.window.width != state.window.width || old.window.height != state.window.height
            });
            let changed = session.show != Some(state);
            let was_down = session.show.is_none();
            session.show = Some(state);
            session.slide = Some(state.slide);
            if resized {
                // Resized (by hand, or by PowerPoint): captured again.
                session.window = None;
                session.looking_since = Some(now);
            }
            if changed {
                self.send(Event::Show {
                    deck: each.clone(),
                    state,
                });
            }
            if was_down {
                self.status(&each, Status::Showing);
            }
            self.look_for_window(&each, now);
        }
    }

    fn look_for_window(&mut self, deck: &str, now: Instant) {
        let Some(session) = self.sessions.get(deck) else {
            return;
        };
        let (Some(state), None) = (session.show, session.window) else {
            return;
        };
        let deck_path = session.path.clone();
        let pixels = session.pixels;
        let since = session.looking_since.unwrap_or(now);
        // Windows held by inputs of other files are theirs.
        let taken: HashSet<u64> = self
            .sessions
            .iter()
            .filter(|(d, s)| d.as_str() != deck && s.path != session.path)
            .filter_map(|(_, s)| s.window)
            .collect();
        let waited = now.saturating_duration_since(since) >= WINDOW_WAIT;
        let windows = match self.finder.windows() {
            Ok(windows) => windows,
            // No window to wait for: what holds it up, said at once.
            Err(why) => {
                self.status(deck, Status::NoWindow(why));
                return;
            }
        };
        let Some(target) = pick_window(&windows, state.window, &deck_path, &taken) else {
            if waited {
                if let Some(session) = self.sessions.get_mut(deck) {
                    session.looking_since = Some(now);
                }
                self.status(deck, Status::NoWindow("its window was not found".into()));
            }
            return;
        };
        let config = capture_config(&target, pixels, self.title_bar);
        match self.finder.open(&target, &config) {
            Ok(backend) => {
                if let Some(session) = self.sessions.get_mut(deck) {
                    session.window = Some(target.platform_id);
                    session.looking_since = None;
                    session.capture_failed = None;
                }
                self.send(Event::Window {
                    deck: deck.to_string(),
                    target,
                    capture: Capture(backend),
                    config,
                });
                self.status(deck, Status::Showing);
            }
            // Tried again at the next reading; logged when the reason is new.
            Err(e) => {
                let session = self.sessions.get_mut(deck);
                if let Some(session) = session.filter(|s| s.capture_failed.as_ref() != Some(&e)) {
                    log::warn!(
                        "PowerPoint show's window '{}' not captured: {e}",
                        target.label
                    );
                    session.capture_failed = Some(e.clone());
                }
                if waited {
                    if let Some(session) = self.sessions.get_mut(deck) {
                        session.looking_since = Some(now);
                    }
                    self.status(deck, Status::NoWindow(e));
                }
            }
        }
    }

    /// Read every show; look for decks being opened; bring back any that
    /// stopped.
    pub fn tick(&mut self, now: Instant) {
        self.close_orphans(now);
        let decks: Vec<String> = self.sessions.keys().cloned().collect();
        // One file's show is read for its video once, for all its inputs.
        let mut videos_read: HashSet<PathBuf> = HashSet::new();
        for deck in decks {
            let Some(session) = self.sessions.get(&deck) else {
                continue;
            };
            let path = session.path.clone();
            if let Some(since) = session.launching {
                match self.ppt.read(&path) {
                    Ok(_) => {
                        if let Some(session) = self.sessions.get_mut(&deck) {
                            session.launching = None;
                            session.opened = Some(true);
                        }
                        self.start(&deck, now);
                    }
                    Err(ScriptError::NotOpen | ScriptError::NotRunning | ScriptError::TimedOut)
                        if now.saturating_duration_since(since) < OPEN_WAIT => {}
                    Err(ScriptError::NotOpen | ScriptError::NotRunning | ScriptError::TimedOut) => {
                        self.failed(
                            &deck,
                            ScriptError::Failed("PowerPoint did not open the deck".into()),
                            now,
                        );
                    }
                    Err(e) => self.failed(&deck, e, now),
                }
                continue;
            }
            if session.show.is_none() {
                if session.retry_at.is_some_and(|at| now >= at) {
                    self.bring_up(&deck, now);
                }
                continue;
            }
            match self.ppt.read(&path) {
                Ok(Reading::Showing(state)) => {
                    self.update(&deck, state, now);
                    if videos_read.insert(path.clone()) {
                        self.watch_video(&deck, &path);
                    }
                }
                // Ended in PowerPoint: back where it was, a little later.
                Ok(Reading::Open { .. }) => self.failed(&deck, ScriptError::NoShow, now),
                Err(e) => self.failed(&deck, e, now),
            }
        }
    }

    /// The video on the slide on screen: looked for when the slide changes,
    /// read while it is on screen — its position moves — and said when what
    /// it does changes. Not finding it is no failure of the show's.
    fn watch_video(&mut self, deck: &str, path: &Path) {
        let Some(session) = self.sessions.get(deck) else {
            return;
        };
        let Some(show) = session.show else {
            return;
        };
        let known = match session.video {
            // The end screen: none.
            _ if show.screen == Screen::Ended => {
                self.set_videos(path, Some((0, None)));
                return;
            }
            Some((slide, None)) if slide == show.slide => return,
            Some((slide, Some(video))) if slide == show.slide => Some((slide, video.shape)),
            _ => None,
        };
        match self.ppt.video(path, known, None) {
            Ok(reading) => self.took_video(path, reading),
            // Looked for afresh at the next reading: a shape number gone
            // stale (the slide edited under the show) is not kept.
            Err(e) => {
                log::debug!("PowerPoint: the slide's video not read: {e}");
                self.set_videos(path, None);
            }
        }
    }

    /// What a video script found, kept and said for every input of the file.
    fn took_video(&mut self, path: &Path, reading: VideoReading) {
        self.set_videos(path, Some((reading.slide.unwrap_or(0), reading.video)));
    }

    /// `video` for every input showing the file at `path`.
    fn set_videos(&mut self, path: &Path, video: Option<(u32, Option<Video>)>) {
        let decks: Vec<String> = self
            .sessions
            .iter()
            .filter(|(_, s)| s.path == path)
            .map(|(d, _)| d.clone())
            .collect();
        for deck in decks {
            self.set_video(&deck, video);
        }
    }

    fn set_video(&mut self, deck: &str, video: Option<(u32, Option<Video>)>) {
        let Some(session) = self.sessions.get_mut(deck) else {
            return;
        };
        let was = session.video.and_then(|(_, v)| v);
        session.video = video;
        let now = video.and_then(|(_, v)| v);
        if was != now {
            self.send(Event::Video {
                deck: deck.to_string(),
                video: now,
            });
        }
    }

    /// Play, pause or move the video on the slide on screen: what it does
    /// now told from what it did when last read.
    fn video(&mut self, deck: &str, control: VideoControl) {
        let Some(session) = self.sessions.get(deck) else {
            return;
        };
        let Some(show) = session.show else {
            return;
        };
        let path = session.path.clone();
        // Read afresh: the slide may have moved on since the last reading —
        // to one with another video, or none — and a skip counts from where
        // it is now.
        let known = session
            .video
            .and_then(|(slide, v)| v.map(|v| (slide, v.shape)))
            .filter(|(slide, _)| *slide == show.slide);
        let video = match self.ppt.video(&path, known, None) {
            Ok(reading) => {
                self.took_video(&path, reading);
                reading.video
            }
            Err(e) => {
                log::warn!("PowerPoint: the video did not {control:?}: {e}");
                self.set_videos(&path, None);
                return;
            }
        };
        let Some(video) = video else {
            return;
        };
        let action = match control {
            VideoControl::Toggle if video.state == VideoState::Playing => MediaAction::Pause,
            VideoControl::Toggle | VideoControl::Play => MediaAction::Play,
            VideoControl::Pause => MediaAction::Pause,
            VideoControl::Restart => MediaAction::Restart,
            VideoControl::Skip(seconds) => {
                let to = i64::from(video.position_ms) + i64::from(seconds) * 1000;
                MediaAction::Seek(u32::try_from(to.max(0)).unwrap_or(u32::MAX))
            }
        };
        match self
            .ppt
            .video(&path, Some((video.slide, video.shape)), Some(action))
        {
            Ok(reading) => self.took_video(&path, reading),
            Err(e) => {
                log::warn!("PowerPoint: the video did not {control:?}: {e}");
                self.set_videos(&path, None);
            }
        }
    }

    fn close(&mut self, deck: &str) {
        let Some(session) = self.sessions.remove(deck) else {
            return;
        };
        // Another input showing the same file keeps it — and closes it when
        // it goes, if Nida opened it for this one.
        if let Some(other) = self.sessions.values_mut().find(|s| s.path == session.path) {
            if session.opened == Some(true) || session.launching.is_some() {
                other.opened = Some(true);
            }
            return;
        }
        if let Some(since) = session.launching {
            // Opening still: closed once PowerPoint has it.
            self.orphans.push((session.path, since));
            return;
        }
        let _ = self.ppt.end(&session.path);
        if session.opened == Some(true) {
            let _ = self.ppt.close(&session.path);
        }
    }

    /// Close decks opened for inputs since gone, once they are open.
    fn close_orphans(&mut self, now: Instant) {
        let orphans = std::mem::take(&mut self.orphans);
        for (path, since) in orphans {
            // Wanted again meanwhile: an input's now.
            if self.sessions.values().any(|s| s.path == path) {
                continue;
            }
            match self.ppt.read(&path) {
                Ok(_) => {
                    let _ = self.ppt.end(&path);
                    let _ = self.ppt.close(&path);
                }
                Err(ScriptError::NotOpen | ScriptError::NotRunning | ScriptError::TimedOut)
                    if now.saturating_duration_since(since) < OPEN_WAIT =>
                {
                    self.orphans.push((path, since));
                }
                Err(_) => {}
            }
        }
    }

    /// Nida is closing: end every show, close what Nida opened.
    pub fn shut_down(&mut self) {
        let decks: Vec<String> = self.sessions.keys().cloned().collect();
        for deck in decks {
            self.close(&deck);
        }
    }

    /// Run until the engine's side goes; then end the shows and say so.
    fn run(mut self, jobs: &mpsc::Receiver<Job>, done: &mpsc::Sender<()>) {
        let mut last_tick = Instant::now();
        loop {
            match jobs.recv_timeout(POLL) {
                Ok(job) => self.handle(job, Instant::now()),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    self.shut_down();
                    let _ = done.send(());
                    return;
                }
            }
            // Read the shows every POLL even while steps keep coming.
            if last_tick.elapsed() >= POLL {
                self.tick(Instant::now());
                last_tick = Instant::now();
            }
        }
    }
}

/// What the engine knows of a deck's show.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub path: PathBuf,
    pub status: Status,
    pub show: Option<ShowState>,
    /// Notes and slide size from the file, once read.
    pub doc: Option<pptx::Document>,
    /// Why the file could not be read (an old .ppt): the show still plays,
    /// without notes.
    pub doc_error: Option<String>,
    /// What the log said last of why the show is not up: said again only
    /// when it changes.
    reason_logged: Option<String>,
    /// The video on the slide on screen, when it has one.
    pub video: Option<Video>,
}

/// A show's window found and its capture opened: the engine adopts it.
pub struct Found {
    pub deck: String,
    pub target: CaptureTargetInfo,
    pub capture: Box<dyn ScreenCaptureBackend>,
    /// How it was set: the deck takes the same.
    pub config: CaptureConfig,
}

impl std::fmt::Debug for Found {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Found({}, {})", self.deck, self.target.label)
    }
}

/// The engine's side: hands jobs to the worker thread, takes its events.
pub struct PresentationManager {
    jobs: Option<mpsc::Sender<Job>>,
    events: mpsc::Receiver<Event>,
    done: Option<mpsc::Receiver<()>>,
    views: HashMap<String, View>,
    /// When the engine last asked for each deck's window.
    asked_window: HashMap<String, Instant>,
}

impl PresentationManager {
    /// The worker on PowerPoint and the screen's windows.
    #[must_use]
    pub fn new() -> Self {
        Self::start(Osascript, ScreenWindows, main_display_scale())
    }

    pub fn start<R: Runner, F: WindowFinder>(runner: R, finder: F, scale: f64) -> Self {
        let (jobs_tx, jobs_rx) = mpsc::channel();
        let (events_tx, events_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let worker = Worker::new(runner, finder, scale, events_tx);
        let spawned = std::thread::Builder::new()
            .name("presentations".into())
            .spawn(move || worker.run(&jobs_rx, &done_tx));
        if let Err(e) = &spawned {
            log::warn!("Presentations: worker thread failed to start: {e}");
        }
        Self {
            jobs: Some(jobs_tx),
            events: events_rx,
            done: spawned.ok().map(|_| done_rx),
            views: HashMap::new(),
            asked_window: HashMap::new(),
        }
    }

    fn send(&self, job: Job) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(job);
        }
    }

    /// Show the deck at `path` for `deck` (again, when already showing a
    /// different file), on the screen `spot` (this Mac's when `None`).
    pub fn open(
        &mut self,
        deck: &str,
        path: &Path,
        slide: Option<u32>,
        pixels: (u32, u32),
        spot: Option<ScreenSpot>,
    ) {
        log::info!("PowerPoint input {deck}: opening {}", path.display());
        self.views.insert(
            deck.to_string(),
            View {
                path: path.to_path_buf(),
                status: Status::Opening,
                show: None,
                doc: None,
                doc_error: None,
                reason_logged: None,
                video: None,
            },
        );
        self.send(Job::Open {
            deck: deck.to_string(),
            path: path.to_path_buf(),
            slide,
            pixels,
            spot,
        });
    }

    /// Play `deck`'s show on the screen `spot` from now on.
    pub fn place(&self, deck: &str, spot: ScreenSpot) {
        if self.views.contains_key(deck) {
            self.send(Job::Place {
                deck: deck.to_string(),
                spot,
            });
        }
    }

    pub fn go(&self, deck: &str, to: Go) {
        if self.views.contains_key(deck) {
            self.send(Job::Go {
                deck: deck.to_string(),
                to,
            });
        }
    }

    pub fn screen(&self, deck: &str, screen: Screen) {
        if self.views.contains_key(deck) {
            self.send(Job::Screen {
                deck: deck.to_string(),
                screen,
            });
        }
    }

    /// Play, pause or move the video on `deck`'s slide on screen.
    pub fn video(&self, deck: &str, control: VideoControl) {
        if self.views.contains_key(deck) {
            self.send(Job::Video {
                deck: deck.to_string(),
                control,
            });
        }
    }

    pub fn restart(&self, deck: &str) {
        if self.views.contains_key(deck) {
            self.send(Job::Restart {
                deck: deck.to_string(),
            });
        }
    }

    pub fn close(&mut self, deck: &str) {
        self.asked_window.remove(deck);
        if self.views.remove(deck).is_some() {
            self.send(Job::Close {
                deck: deck.to_string(),
            });
        }
    }

    /// The engine has no picture of a deck whose show runs: ask for its
    /// window again — at most every couple of seconds.
    pub fn want_window(&mut self, deck: &str, now: Instant) {
        let showing = self
            .views
            .get(deck)
            .is_some_and(|v| v.status == Status::Showing);
        let asked = self
            .asked_window
            .get(deck)
            .is_some_and(|at| now.saturating_duration_since(*at) < ASK_WINDOW_EVERY);
        if showing && !asked {
            self.asked_window.insert(deck.to_string(), now);
            self.send(Job::Window {
                deck: deck.to_string(),
            });
        }
    }

    /// Take what the worker found; the windows found, to adopt.
    pub fn poll(&mut self) -> Vec<Found> {
        let mut found = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            match event {
                Event::Notes { deck, doc } => {
                    if let Some(view) = self.views.get_mut(&deck) {
                        match doc {
                            Ok(doc) => {
                                view.doc = Some(doc);
                                view.doc_error = None;
                            }
                            Err(e) => {
                                view.doc = None;
                                view.doc_error = Some(e);
                            }
                        }
                    }
                }
                Event::Status { deck, status } => {
                    if let Some(view) = self.views.get_mut(&deck) {
                        // In the log, for a desk the panel cannot be seen
                        // from: why the show is not up, when that is new — a
                        // retry's own Opening and the same reason again are
                        // not — and the show once it is.
                        let why = match &status {
                            Status::Failed(e) => Some(format!("not showing: {e}")),
                            Status::NoWindow(why) => Some(format!("the show plays, but {why}")),
                            Status::Opening | Status::Showing => None,
                        };
                        match why {
                            Some(why) if view.reason_logged.as_ref() != Some(&why) => {
                                log::warn!("PowerPoint input {deck}: {why}");
                                view.reason_logged = Some(why);
                            }
                            None if status == Status::Showing => {
                                if view.status != Status::Showing {
                                    log::info!("PowerPoint input {deck}: showing");
                                }
                                view.reason_logged = None;
                            }
                            _ => {}
                        }
                        view.status = status;
                    }
                }
                Event::Show { deck, state } => {
                    if let Some(view) = self.views.get_mut(&deck) {
                        view.show = Some(state);
                    }
                }
                Event::Video { deck, video } => {
                    if let Some(view) = self.views.get_mut(&deck) {
                        view.video = video;
                    }
                }
                Event::Window {
                    deck,
                    target,
                    capture,
                    config,
                } => {
                    if self.views.contains_key(&deck) {
                        found.push(Found {
                            deck,
                            target,
                            capture: capture.0,
                            config,
                        });
                    }
                }
            }
        }
        found
    }

    #[must_use]
    pub fn view(&self, deck: &str) -> Option<&View> {
        self.views.get(deck)
    }

    /// The decks with a show, and their files.
    pub fn decks(&self) -> impl Iterator<Item = (&str, &Path)> {
        self.views
            .iter()
            .map(|(d, v)| (d.as_str(), v.path.as_path()))
    }
}

impl Default for PresentationManager {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for PresentationManager {
    /// The worker ends the shows and closes what Nida opened before Nida
    /// goes — waited for a little, not for ever.
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(done) = self.done.take() {
            let _ = done.recv_timeout(SHUT_DOWN_WAIT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presentation::fake::{window, window_numbered, FakePowerPoint, FakeWindows};

    fn worker(
        ppt: &FakePowerPoint,
        scale: f64,
    ) -> (Worker<FakePowerPoint, FakeWindows>, mpsc::Receiver<Event>) {
        let (tx, rx) = mpsc::channel();
        let mut worker = Worker::new(ppt.clone(), ppt.windows(), scale, tx);
        // macOS 26's, whatever this runs on.
        worker.title_bar = 32;
        (worker, rx)
    }

    fn open_job(deck: &str, slide: Option<u32>) -> Job {
        Job::Open {
            deck: deck.into(),
            path: "/Talks/talk.pptx".into(),
            slide,
            pixels: (1920, 1080),
            spot: None,
        }
    }

    fn drain(rx: &mpsc::Receiver<Event>) -> Vec<Event> {
        rx.try_iter().collect()
    }

    fn has_status(events: &[Event], want: &Status) -> bool {
        events
            .iter()
            .any(|e| matches!(e, Event::Status { status, .. } if status == want))
    }

    fn windows(events: &[Event]) -> Vec<u64> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Window { target, .. } => Some(target.platform_id),
                _ => None,
            })
            .collect()
    }

    fn shown(events: &[Event]) -> Option<ShowState> {
        events.iter().rev().find_map(|e| match e {
            Event::Show { state, .. } => Some(*state),
            _ => None,
        })
    }

    #[test]
    fn a_show_plays_on_the_screen_asked_for_and_moves_when_asked() {
        let ppt = FakePowerPoint::with_slides(5);
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        // A spare 4K screen at 2×, right of this Mac's.
        let spare = ScreenSpot {
            display: 2,
            left: 1920,
            top: 0,
            width: 1920,
            height: 1080,
            scale: 2.0,
            main: false,
        };
        w.handle(
            Job::Open {
                deck: "d1".into(),
                path: "/Talks/talk.pptx".into(),
                slide: None,
                pixels: (1920, 1080),
                spot: Some(spare),
            },
            now,
        );
        w.tick(now);
        // At its corner, at its pixels: 1920 × 1080 pixels are 960 × 540
        // points, under a 32-point title bar.
        assert_eq!(ppt.window_place(), (1920, 0, 960, 572));
        let show = ppt.window_id();
        let captures = ppt.captured().len();
        drain(&rx);
        // Moved to this Mac's screen, at 1×: the same show, captured again.
        let main = ScreenSpot::main_at(1.0);
        w.handle(
            Job::Place {
                deck: "d1".into(),
                spot: main,
            },
            now,
        );
        assert_eq!(ppt.window_place(), (0, 0, 1920, 1112));
        assert_eq!(ppt.window_id(), show, "moved, not played again");
        assert_eq!(ppt.captured().len(), captures + 1, "captured again");
        assert!(drain(&rx)
            .iter()
            .any(|e| matches!(e, Event::Show { state, .. } if state.window.width == 1920)));
        // Asked for where it is: nothing done.
        w.handle(
            Job::Place {
                deck: "d1".into(),
                spot: main,
            },
            now,
        );
        assert_eq!(ppt.captured().len(), captures + 1);
        // Another input of the file moves the show; asked again for this
        // Mac's screen, the first takes it back — the show is the file's.
        w.handle(open_job("d2", None), now);
        w.handle(
            Job::Place {
                deck: "d2".into(),
                spot: spare,
            },
            now,
        );
        assert_eq!(ppt.window_place().0, 1920);
        w.handle(Job::Close { deck: "d2".into() }, now);
        w.handle(
            Job::Place {
                deck: "d1".into(),
                spot: main,
            },
            now,
        );
        assert_eq!(ppt.window_place(), (0, 0, 1920, 1112));
    }

    #[test]
    fn a_deck_opens_plays_in_a_window_and_its_window_is_captured() {
        let ppt = FakePowerPoint::with_slides(12);
        let (mut w, rx) = worker(&ppt, 2.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        // Opened in PowerPoint; looked for at the next reading.
        w.tick(now);
        let events = drain(&rx);
        // The file is not a real .pptx here: no notes, the show plays on.
        assert!(matches!(&events[0], Event::Notes { doc: Err(_), .. }));
        assert!(has_status(&events, &Status::Opening));
        // 1920 × 1080 pixels on a Retina screen: 960 × 540 points.
        assert_eq!(
            shown(&events).map(|s| (s.slide, s.slides, s.window.width, s.window.height)),
            Some((1, 12, 960, 572))
        );
        assert_eq!(windows(&events), vec![1001]);
        assert_eq!(ppt.captured(), vec![1001]);
        assert!(matches!(
            events.last(),
            Some(Event::Status {
                status: Status::Showing,
                ..
            })
        ));
        assert_eq!(ppt.opened_files(), vec![PathBuf::from("/Talks/talk.pptx")]);
    }

    fn videos(events: &[Event]) -> Vec<Option<Video>> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Video { video, .. } => Some(*video),
                _ => None,
            })
            .collect()
    }

    /// A slide's video: looked for once when its slide comes up, read while
    /// it is on screen, played, paused, moved and started again — and gone
    /// with its slide.
    #[test]
    fn a_slides_video_is_found_played_and_moved() {
        let ppt = FakePowerPoint::with_slides(4);
        ppt.with_video(2, 8);
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.tick(now);
        w.tick(now);
        assert!(videos(&drain(&rx)).is_empty(), "slide 1 has none");
        assert_eq!(ppt.video_looks(), 1, "looked through once");

        w.handle(
            Job::Go {
                deck: "d1".into(),
                to: Go::Next,
            },
            now,
        );
        w.tick(now);
        assert_eq!(
            videos(&drain(&rx)),
            vec![Some(Video {
                slide: 2,
                shape: 8,
                state: VideoState::NotReady,
                position_ms: 0,
            })]
        );
        let mut control = |control: VideoControl| {
            w.handle(
                Job::Video {
                    deck: "d1".into(),
                    control,
                },
                now,
            );
            videos(&drain(&rx))
                .last()
                .copied()
                .flatten()
                .map(|v| (v.state, v.position_ms))
        };
        assert_eq!(
            control(VideoControl::Toggle),
            Some((VideoState::Playing, 0))
        );
        assert_eq!(
            control(VideoControl::Skip(10)),
            Some((VideoState::Playing, 10_000))
        );
        assert_eq!(
            control(VideoControl::Toggle),
            Some((VideoState::Paused, 10_000))
        );
        assert_eq!(
            control(VideoControl::Skip(-30)),
            Some((VideoState::Paused, 0)),
            "not before its start"
        );
        assert_eq!(
            control(VideoControl::Restart),
            Some((VideoState::Playing, 0))
        );
        w.tick(now);
        assert!(videos(&drain(&rx)).is_empty(), "nothing new");
        assert_eq!(ppt.video_looks(), 2, "its shape kept, not looked for again");

        w.handle(
            Job::Go {
                deck: "d1".into(),
                to: Go::Next,
            },
            now,
        );
        w.tick(now);
        assert_eq!(videos(&drain(&rx)), vec![None], "gone with its slide");
    }

    /// A press straight after a step works the new slide's video, not the
    /// one before's; both inputs of the file hear what it does; a show
    /// ended in PowerPoint takes its video with it.
    #[test]
    fn a_video_press_after_a_step_works_the_new_slides_video_for_every_input() {
        let ppt = FakePowerPoint::with_slides(4);
        ppt.with_video(2, 8);
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.handle(open_job("d2", None), now);
        w.tick(now);
        drain(&rx);
        // Slide 1 had none; Play before any reading of slide 2.
        w.handle(
            Job::Go {
                deck: "d1".into(),
                to: Go::Next,
            },
            now,
        );
        w.handle(
            Job::Video {
                deck: "d1".into(),
                control: VideoControl::Play,
            },
            now,
        );
        assert_eq!(ppt.player(), (Some(true), 0), "slide 2's video plays");
        let heard: Vec<(String, Option<VideoState>)> = drain(&rx)
            .into_iter()
            .filter_map(|e| match e {
                Event::Video { deck, video } => Some((deck, video.map(|v| v.state))),
                _ => None,
            })
            .collect();
        for deck in ["d1", "d2"] {
            assert_eq!(
                heard.iter().rev().find(|(d, _)| d == deck).map(|(_, v)| *v),
                Some(Some(VideoState::Playing)),
                "{deck} hears it: {heard:?}"
            );
        }
        let reads = ppt.video_reads();
        w.tick(now);
        assert_eq!(
            ppt.video_reads(),
            reads + 1,
            "one file's video read once a tick"
        );

        // Esc in PowerPoint: no show, no video — said.
        ppt.end_show();
        w.tick(now);
        assert!(videos(&drain(&rx)).contains(&None));
    }

    /// macOS refusing Nida the screen: the show plays, and its input says at
    /// once what to switch on — not, ten seconds on, only that its window
    /// was not found. Allowed (and Nida reopened), it is captured.
    #[test]
    fn without_screen_recording_the_input_says_so_at_once() {
        let ppt = FakePowerPoint::with_slides(12);
        ppt.refuse_screen(true);
        let (mut w, rx) = worker(&ppt, 2.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.tick(now);
        let events = drain(&rx);
        assert!(shown(&events).is_some(), "the show plays");
        assert!(has_status(
            &events,
            &Status::NoWindow(SCREEN_RECORDING_OFF.into())
        ));
        assert!(ppt.captured().is_empty());

        ppt.refuse_screen(false);
        w.tick(now + Duration::from_millis(300));
        let events = drain(&rx);
        assert_eq!(ppt.captured(), vec![1001]);
        assert!(has_status(&events, &Status::Showing));
    }

    #[test]
    fn opening_a_deck_waits_on_nothing() {
        let ppt = FakePowerPoint::with_slides(5);
        ppt.open_slowly(3);
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        // Back at once: PowerPoint has not got it yet, and a step waits for
        // the show.
        assert!(!ppt.showing());
        w.handle(
            Job::Go {
                deck: "d1".into(),
                to: Go::Next,
            },
            now,
        );
        assert!(ppt.steps().is_empty());
        for _ in 0..3 {
            w.tick(now);
        }
        assert!(!ppt.showing(), "still opening");
        w.tick(now);
        assert!(ppt.showing());
        assert_eq!(windows(&drain(&rx)), vec![1001]);
        // Never there: given up on after a while, and tried again later.
        let ppt = FakePowerPoint::with_slides(5);
        ppt.open_slowly(u32::MAX);
        let (mut w, rx) = worker(&ppt, 1.0);
        w.handle(open_job("d1", None), now);
        w.tick(now + OPEN_WAIT);
        assert!(has_status(
            &drain(&rx),
            &Status::Failed(ScriptError::Failed(
                "PowerPoint did not open the deck".into()
            ))
        ));
    }

    #[test]
    fn steps_move_the_show_and_the_end_screen_is_kept() {
        let ppt = FakePowerPoint::with_slides(2);
        ppt.open_by_hand();
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        drain(&rx);
        let mut step = |to| {
            w.handle(
                Job::Go {
                    deck: "d1".into(),
                    to,
                },
                now,
            );
            shown(&drain(&rx)).map(|s| (s.slide, s.screen))
        };
        assert_eq!(step(Go::Next), Some((2, Screen::Slide)));
        assert_eq!(step(Go::Next), Some((2, Screen::Ended)));
        // A third press would end the show and close its window: not sent.
        assert_eq!(step(Go::Next), None);
        assert_eq!(ppt.steps(), vec!["next", "next"]);
        assert_eq!(step(Go::Previous), Some((1, Screen::Slide)));
    }

    #[test]
    fn a_show_ended_in_powerpoint_comes_back_where_it_was_and_its_new_window_is_captured() {
        let ppt = FakePowerPoint::with_slides(10);
        ppt.open_by_hand();
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", Some(4)), now);
        assert_eq!(ppt.slide(), 4);
        drain(&rx);
        ppt.end_show();
        w.tick(now);
        assert!(has_status(
            &drain(&rx),
            &Status::Failed(ScriptError::NoShow)
        ));
        // Not at once — a little later, at the slide it was on.
        w.tick(now + Duration::from_millis(500));
        assert!(!ppt.showing());
        w.tick(now + RETRY_FIRST + Duration::from_secs(1));
        assert!(ppt.showing());
        assert_eq!(ppt.slide(), 4);
        // The new show's window, not the old one's.
        assert_eq!(windows(&drain(&rx)), vec![1002]);
        assert_eq!(ppt.captured(), vec![1001, 1002]);
    }

    #[test]
    fn powerpoint_quitting_under_a_step_opens_the_deck_again() {
        let ppt = FakePowerPoint::with_slides(10);
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.tick(now);
        assert!(ppt.showing());
        drain(&rx);
        ppt.quit();
        w.handle(
            Job::Go {
                deck: "d1".into(),
                to: Go::Next,
            },
            now,
        );
        assert!(has_status(
            &drain(&rx),
            &Status::Failed(ScriptError::NotRunning)
        ));
        let later = now + RETRY_FIRST + Duration::from_secs(1);
        w.tick(later);
        w.tick(later);
        assert_eq!(ppt.opened_files().len(), 2, "opened again");
        assert!(ppt.showing());
    }

    #[test]
    fn refused_by_macos_says_so_and_tries_again_less_often() {
        let ppt = FakePowerPoint::with_slides(3);
        ppt.refuse(true);
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        assert!(has_status(
            &drain(&rx),
            &Status::Failed(ScriptError::NotAllowed)
        ));
        let session = w.sessions.get("d1").unwrap();
        assert!(session.retry_at.is_some_and(|at| at >= now + RETRY_FIRST));
        assert_eq!(session.retry_wait, RETRY_FIRST * 2);
        // Allowed meanwhile: the next try plays it.
        ppt.refuse(false);
        let later = now + RETRY_FIRST + Duration::from_secs(1);
        w.tick(later);
        w.tick(later);
        assert!(ppt.showing());
    }

    #[test]
    fn starting_a_show_asks_for_nida_to_be_in_front_again() {
        let ppt = FakePowerPoint::with_slides(4);
        ppt.open_by_hand();
        let (mut w, _rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        assert_eq!(ppt.refocused(), 1, "PowerPoint came forward to start it");
        // Only placed (a show already running): nothing came forward.
        let ppt = FakePowerPoint::with_slides(4);
        ppt.open_by_hand();
        let (mut w, _rx) = worker(&ppt, 1.0);
        w.handle(open_job("d1", None), now);
        w.handle(open_job("d2", None), now);
        assert_eq!(ppt.refocused(), 1);
    }

    #[test]
    fn a_show_playing_full_screen_is_played_again_in_a_window() {
        let ppt = FakePowerPoint::with_slides(6);
        ppt.show_full_screen_by_hand();
        let (mut w, rx) = worker(&ppt, 1.0);
        w.handle(open_job("d1", None), Instant::now());
        assert!(ppt.showing() && !ppt.full_screen());
        assert_eq!(shown(&drain(&rx)).map(|s| s.full_screen), Some(false));
    }

    #[test]
    fn closing_ends_the_show_and_closes_only_what_nida_opened() {
        let ppt = FakePowerPoint::with_slides(3);
        let (mut w, _rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.tick(now);
        w.handle(Job::Close { deck: "d1".into() }, now);
        assert!(ppt.closed());

        // Open in PowerPoint already, by hand: the show ends, the deck stays.
        let ppt = FakePowerPoint::with_slides(3);
        ppt.open_by_hand();
        let (mut w, _rx) = worker(&ppt, 1.0);
        w.handle(open_job("d1", None), now);
        assert!(ppt.showing());
        assert!(ppt.opened_files().is_empty());
        w.handle(Job::Close { deck: "d1".into() }, now);
        assert!(!ppt.showing() && ppt.open() && !ppt.closed());
    }

    #[test]
    fn a_deck_given_another_file_closes_the_first() {
        let ppt = FakePowerPoint::with_slides(3);
        let (mut w, _rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.tick(now);
        w.handle(
            Job::Open {
                deck: "d1".into(),
                path: "/Talks/other.pptx".into(),
                slide: None,
                pixels: (1920, 1080),
                spot: None,
            },
            now,
        );
        assert!(ppt.closed(), "the first file, opened by Nida, closed");
    }

    #[test]
    fn two_inputs_on_one_file_share_its_show_and_its_end_screen() {
        let ppt = FakePowerPoint::with_slides(1);
        ppt.open_by_hand();
        let (mut w, _rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.handle(open_job("d2", None), now);
        // d1 steps to the end screen; d2's next would close the show.
        w.handle(
            Job::Go {
                deck: "d1".into(),
                to: Go::Next,
            },
            now,
        );
        w.handle(
            Job::Go {
                deck: "d2".into(),
                to: Go::Next,
            },
            now,
        );
        assert_eq!(ppt.steps(), vec!["next"]);
        w.handle(Job::Close { deck: "d1".into() }, now);
        assert!(ppt.showing());
        w.handle(Job::Close { deck: "d2".into() }, now);
        assert!(!ppt.showing());
    }

    #[test]
    fn a_lost_picture_is_captured_again_when_asked() {
        let ppt = FakePowerPoint::with_slides(3);
        ppt.open_by_hand();
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        assert_eq!(windows(&drain(&rx)), vec![1001]);
        w.handle(Job::Window { deck: "d1".into() }, now);
        assert_eq!(windows(&drain(&rx)), vec![1001]);
        assert_eq!(ppt.captured(), vec![1001, 1001]);
    }

    #[test]
    fn the_window_is_picked_by_size_then_name_and_never_another_files() {
        let at = Placement {
            left: 0,
            top: 0,
            width: 960,
            height: 540,
        };
        let none = HashSet::new();
        let talk = Path::new("/Talks/talk.pptx");
        let editor = window("talk.pptx", 1440, 900);
        let show = window_numbered("PowerPoint-Bildschirmpräsentation - talk", 961, 539, 7);
        // Localized title: the size finds it.
        assert_eq!(
            pick_window(&[editor.clone(), show.clone()], at, talk, &none),
            Some(show.clone())
        );
        // Two shows of that size: the one naming the deck, whatever the other
        // is called.
        let other = window_numbered("PowerPoint Slide Show - other", 960, 540, 9);
        assert_eq!(
            pick_window(&[other.clone(), show.clone()], at, talk, &none),
            Some(show.clone())
        );
        assert_eq!(
            pick_window(
                &[other.clone(), show.clone()],
                at,
                Path::new("/x/other.pptx"),
                &none
            ),
            Some(other.clone())
        );
        // Held by another file's input: not this one's, even alone.
        let taken: HashSet<u64> = [9].into();
        assert_eq!(
            pick_window(std::slice::from_ref(&other), at, talk, &taken),
            None
        );
        // Neither names the deck: the newest.
        let a = window_numbered("Slide Show", 960, 540, 3);
        let b = window_numbered("Slide Show", 960, 540, 5);
        assert_eq!(pick_window(&[a, b.clone()], at, talk, &none), Some(b));
        let mut ours = window("PowerPoint Slide Show", 960, 540);
        ours.is_varda = true;
        assert_eq!(pick_window(&[ours, editor], at, talk, &none), None);
    }

    #[test]
    fn a_deck_named_like_another_takes_its_own_window() {
        let at = Placement {
            left: 0,
            top: 0,
            width: 960,
            height: 540,
        };
        let none = HashSet::new();
        let talk = Path::new("/Talks/talk.pptx");
        // talk2's show is the newer: "talk" is in its title too.
        let mine = window_numbered("PowerPoint Slide Show - talk.pptx", 960, 540, 7);
        let theirs = window_numbered("PowerPoint Slide Show - talk2.pptx", 960, 540, 9);
        assert_eq!(
            pick_window(&[theirs.clone(), mine.clone()], at, talk, &none),
            Some(mine)
        );
        // Titles without the extension: the name as a whole word.
        let mine = window_numbered("Slide Show - talk", 960, 540, 7);
        let theirs = window_numbered("Slide Show - talk2", 960, 540, 9);
        assert_eq!(
            pick_window(&[theirs, mine.clone()], at, talk, &none),
            Some(mine)
        );
        assert!(has_word("slide show - talk", "talk"));
        assert!(!has_word("slide show - talk2", "talk"));
        assert!(!has_word("smalltalk", "talk"));
    }

    #[test]
    fn a_show_started_again_is_captured_again_by_every_input_of_its_file() {
        let ppt = FakePowerPoint::with_slides(3);
        ppt.open_by_hand();
        let (mut w, rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.handle(open_job("d2", None), now);
        drain(&rx);
        w.handle(Job::Restart { deck: "d1".into() }, now);
        let events = drain(&rx);
        let mut got: Vec<(String, u64)> = events
            .iter()
            .filter_map(|e| match e {
                Event::Window { deck, target, .. } => Some((deck.clone(), target.platform_id)),
                _ => None,
            })
            .collect();
        got.sort();
        assert_eq!(got, vec![("d1".into(), 1002), ("d2".into(), 1002)]);
    }

    #[test]
    fn an_input_gone_while_its_deck_opens_closes_the_deck_when_it_is_there() {
        let ppt = FakePowerPoint::with_slides(3);
        ppt.open_slowly(2);
        let (mut w, _rx) = worker(&ppt, 1.0);
        let now = Instant::now();
        w.handle(open_job("d1", None), now);
        w.handle(Job::Close { deck: "d1".into() }, now);
        assert!(!ppt.closed(), "not there yet to close");
        for _ in 0..3 {
            w.tick(now);
        }
        assert!(ppt.closed());
        assert!(!ppt.showing());
    }

    #[test]
    fn the_window_keeps_the_slides_shape_in_points() {
        let p = |pixels, aspect, scale| {
            let at = placement(pixels, aspect, scale, 32);
            (at.width, at.height)
        };
        // The slide's shape, and the title bar over it.
        assert_eq!(p((1920, 1080), Some(16.0 / 9.0), 2.0), (960, 572));
        assert_eq!(p((1920, 1080), Some(4.0 / 3.0), 1.0), (1440, 1112));
        assert_eq!(p((1080, 1920), Some(16.0 / 9.0), 1.0), (1080, 640));
        assert_eq!(p((1920, 1080), None, 1.0), (1920, 1112));
        assert_eq!(p((1920, 1080), Some(f64::NAN), 0.0), (1920, 1112));
    }

    #[test]
    fn a_window_kept_small_is_narrowed_to_the_slide_and_its_title_bar_cut_off() {
        // On a 1080p screen at 1× macOS kept the window 1920 × 974 at the
        // top (measured): narrowed to 16:9 under its 32-point title bar.
        let kept = Placement {
            left: 0,
            top: 30,
            width: 1920,
            height: 974,
        };
        let narrower = fitted(kept, Some(16.0 / 9.0), 32).expect("narrowed");
        assert_eq!((narrower.width, narrower.height), (1675, 974));
        assert_eq!(fitted(narrower, Some(16.0 / 9.0), 32), None, "fits now");
        assert_eq!(
            fitted(
                Placement {
                    left: 0,
                    top: 30,
                    width: 1600,
                    height: 932
                },
                Some(16.0 / 9.0),
                32
            ),
            None
        );
        // Kept narrow (a screen narrower than the slide's shape): lowered.
        let narrow = Placement {
            left: 0,
            top: 30,
            width: 1080,
            height: 1112,
        };
        let lower = fitted(narrow, Some(16.0 / 9.0), 32).expect("lowered");
        assert_eq!((lower.width, lower.height), (1080, 608 + 32));
        assert_eq!(fitted(lower, Some(16.0 / 9.0), 32), None, "fits now");
        // A 4:3 slide in a window kept 16:9: narrowed to 4:3.
        let four_three = fitted(narrower, Some(4.0 / 3.0), 32).expect("4:3");
        assert_eq!((four_three.width, four_three.height), (1256, 974));
        // The capture: the title bar cut off, the slide whole — the size it
        // comes out at worked as the macOS capture does: the window's pixels
        // bounded by `scale_to`, then the crop.
        let delivered = |target: &CaptureTargetInfo, scale: u32, config: &CaptureConfig| {
            let native = (target.width * scale, target.height * scale);
            let (w, h) = config.scale_to.map_or(native, |(bw, bh)| {
                crate::screen_capture::resample::fit_within(native.0, native.1, bw, bh)
            });
            (
                ((w as f32) * config.crop.w).round() as u32 & !1,
                ((h as f32) * config.crop.h).round() as u32 & !1,
            )
        };
        let target = window("PowerPoint Slide Show - [talk]", 1600, 932);
        let config = capture_config(&target, (1920, 1080), 32);
        assert!((config.crop.y - 32.0 / 932.0).abs() < 1e-6);
        assert!((config.crop.h - 900.0 / 932.0).abs() < 1e-6);
        assert_eq!((config.crop.x, config.crop.w), (0.0, 1.0));
        // At its own pixels on a 1× screen; the deck scales it up.
        assert_eq!(delivered(&target, 1, &config), (1600, 900));
        // The window macOS kept on a 1080p screen, narrowed: 1675 × 942 (the
        // first version came out at 1620 × 910 — cropped twice, measured).
        let kept = window("PowerPoint Slide Show - [talk]", 1675, 974);
        assert_eq!(
            delivered(&kept, 1, &capture_config(&kept, (1920, 1080), 32)),
            (1674, 942)
        );
        // A Retina screen's 960 × 540 points under the title bar: the
        // picture's 1920 × 1080.
        let retina = window("PowerPoint Slide Show - [talk]", 960, 572);
        assert_eq!(
            delivered(&retina, 2, &capture_config(&retina, (1920, 1080), 32)),
            (1920, 1080)
        );
        // A bigger window than the picture: no more than the picture.
        let big = window("PowerPoint Slide Show - [talk]", 2560, 1472);
        assert_eq!(
            delivered(&big, 2, &capture_config(&big, (1920, 1080), 32)),
            (1920, 1080)
        );
    }

    #[test]
    fn the_manager_hands_jobs_over_and_keeps_what_comes_back() {
        let ppt = FakePowerPoint::with_slides(5);
        let mut m = PresentationManager::start(ppt.clone(), ppt.windows(), 1.0);
        m.open(
            "d1",
            Path::new("/Talks/talk.pptx"),
            None,
            (1920, 1080),
            None,
        );
        assert_eq!(
            m.view("d1").map(|v| v.status.clone()),
            Some(Status::Opening)
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut found = Vec::new();
        while Instant::now() < deadline {
            found.extend(m.poll());
            if m.view("d1").is_some_and(|v| v.status == Status::Showing) && !found.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(found.len(), 1, "{:?}", m.view("d1"));
        assert_eq!(found[0].capture.resolution(), (1920, 1080));
        assert_eq!(m.view("d1").and_then(|v| v.show).map(|s| s.slides), Some(5));
        // Asked again for its window: once, not every frame.
        let now = Instant::now();
        m.want_window("d1", now);
        m.want_window("d1", now);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && ppt.captured().len() < 2 {
            m.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(ppt.captured().len(), 2);
        m.go("d1", Go::Next);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline
            && m.view("d1").and_then(|v| v.show).map(|s| s.slide) != Some(2)
        {
            m.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(m.view("d1").and_then(|v| v.show).map(|s| s.slide), Some(2));
        m.close("d1");
        assert!(m.view("d1").is_none());
        // Steps for a deck it does not have go nowhere.
        m.go("d1", Go::Next);
        // Gone, the worker closes the deck Nida opened before Nida goes.
        drop(m);
        assert!(ppt.closed());
    }
}
