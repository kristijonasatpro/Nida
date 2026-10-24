//! Driving PowerPoint for Mac: open a deck, play it in a window of a given
//! size, move it on and back, read where it is.
//!
//! PowerPoint is told by AppleScript, one `osascript` per command (30–50 ms
//! to start, measured), on the presentation worker's thread, never the
//! show's: a PowerPoint that hangs or asks something costs a timeout, and
//! the process is killed at it. The file itself is opened through Launch
//! Services (`open -g`), which hands sandboxed PowerPoint the right to read
//! it; opened by AppleScript, PowerPoint would ask for access first.
//!
//! macOS asks once whether Nida may control PowerPoint (Privacy & Security →
//! Automation); refused, every command fails with [`ScriptError::NotAllowed`].

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// PowerPoint's bundle id: found wherever it is installed, whatever it is
/// called.
pub const POWERPOINT_ID: &str = "com.microsoft.Powerpoint";

/// How long a command may take before its `osascript` is killed. Opening a
/// large deck takes longest; a step should take a few milliseconds.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a walk to a slide may take (see `scripts::walk_to`): a step at a
/// time, each build one, about 55 ms each (measured) — up to its 1000 steps.
pub const WALK_TIMEOUT: Duration = Duration::from_secs(70);

/// Why PowerPoint did not do what it was told.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScriptError {
    #[error("PowerPoint is not installed")]
    NotInstalled,
    #[error(
        "macOS does not allow Nida to control PowerPoint: System Settings → Privacy & Security → \
         Automation → Nida → Microsoft PowerPoint"
    )]
    NotAllowed,
    #[error("PowerPoint is not running")]
    NotRunning,
    /// The deck is not open in PowerPoint (closed there, say).
    #[error("the deck is not open in PowerPoint")]
    NotOpen,
    /// The deck is open but no slide show of it is running.
    #[error("the slide show is not running")]
    NoShow,
    #[error("PowerPoint did not answer in time")]
    TimedOut,
    #[error("PowerPoint: {0}")]
    Failed(String),
}

impl ScriptError {
    /// Read `osascript`'s complaint: its message ends with the Apple Event
    /// error number in brackets.
    fn from_stderr(stderr: &str) -> Self {
        let code = stderr
            .trim_end()
            .strip_suffix(')')
            .and_then(|s| s.rsplit_once('('))
            .and_then(|(_, n)| n.parse::<i32>().ok());
        match code {
            // errAEEventNotPermitted, errAEEventWouldRequireUserConsent.
            Some(-1743 | -1744) => Self::NotAllowed,
            // procNotFound, connectionInvalid.
            Some(-600 | -609) => Self::NotRunning,
            // errAETimeout.
            Some(-1712) => Self::TimedOut,
            // An application id unknown at compile time.
            Some(-2700 | -10814) if stderr.contains("application") => Self::NotInstalled,
            _ => {
                let message = stderr
                    .trim()
                    .split_once("error: ")
                    .map_or(stderr.trim(), |(_, m)| m);
                Self::Failed(message.to_string())
            }
        }
    }
}

pub use crate::engine::value::presentation::{Go, Screen, VideoState};

/// A video (or sound) on the slide on screen, as PowerPoint plays it: the
/// slide, the shape (PowerPoint's numbering, from 1) — kept to ask of it
/// again without looking for it — what it is doing and where it has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Video {
    pub slide: u32,
    pub shape: u32,
    pub state: VideoState,
    pub position_ms: u32,
}

/// What to tell a video to do (see [`PowerPoint::video`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaAction {
    Play,
    Pause,
    /// To this many milliseconds from its start, playing or not as it was.
    Seek(u32),
    /// From its start, playing.
    Restart,
}

/// What a video script finds: the slide on screen, and its video if it has
/// one; `None` for no show or the end screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoReading {
    pub slide: Option<u32>,
    pub video: Option<Video>,
}

impl VideoReading {
    /// Parse a video script's reply (see [`scripts::video`]): `video`, the
    /// slide (0: none), the shape (0: no video), and for a video its state
    /// and position.
    fn parse(reply: &str) -> Result<Self, ScriptError> {
        let fields: Vec<&str> = reply.trim().split('\t').collect();
        match fields.first().copied() {
            Some("video") => {}
            Some("nopres") => return Err(ScriptError::NotOpen),
            Some("notrunning") => return Err(ScriptError::NotRunning),
            _ => return Err(ScriptError::Failed(format!("unexpected reply: {reply}"))),
        }
        let count = |i: usize| -> Result<u32, ScriptError> {
            fields
                .get(i)
                .and_then(|f| f.trim().replace(',', ".").parse::<f64>().ok())
                .map(|v| u32::try_from(v.round().max(0.0) as i64).unwrap_or(u32::MAX))
                .ok_or_else(|| ScriptError::Failed(format!("unexpected reply: {reply}")))
        };
        let slide = count(1)?;
        let shape = count(2)?;
        if slide == 0 {
            return Ok(Self {
                slide: None,
                video: None,
            });
        }
        if shape == 0 {
            return Ok(Self {
                slide: Some(slide),
                video: None,
            });
        }
        let state = match fields.get(3).copied().unwrap_or("") {
            s if s.contains("playing") => VideoState::Playing,
            s if s.contains("paused") => VideoState::Paused,
            s if s.contains("stopped") => VideoState::Stopped,
            _ => VideoState::NotReady,
        };
        Ok(Self {
            slide: Some(slide),
            video: Some(Video {
                slide,
                shape,
                state,
                position_ms: count(4).unwrap_or(0),
            }),
        })
    }
}

/// A slide show's window, in screen points from the top left of the main
/// display, as PowerPoint reports and takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Placement {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

/// Where a running slide show is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShowState {
    /// The slide on screen, in PowerPoint's numbering (from 1, hidden
    /// slides counted): its notes are slide `slide`'s.
    pub slide: u32,
    /// Its place among the slides the show plays (from 1).
    pub position: u32,
    /// Slides in the deck, hidden ones too.
    pub slides: u32,
    pub screen: Screen,
    pub window: Placement,
    /// Played full screen rather than in a window: not one Nida started.
    pub full_screen: bool,
}

/// What a state read finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reading {
    Showing(ShowState),
    /// The deck is open, no show running; it has this many slides.
    Open {
        slides: u32,
    },
}

/// Where a step left the show: the slide, its place in the show, the
/// screen — all a step changes; the window stays as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Moved {
    /// 0 on the end screen when PowerPoint does not say which slide was
    /// last: the one before is kept.
    pub slide: u32,
    pub position: u32,
    pub screen: Screen,
}

impl Moved {
    /// Parse a step's reply (see [`scripts::STEP_REPLY`]).
    fn parse(reply: &str) -> Result<Self, ScriptError> {
        let fields: Vec<&str> = reply.trim().split('\t').collect();
        let count = |i: usize| -> Result<u32, ScriptError> {
            fields
                .get(i)
                .and_then(|f| f.trim().parse::<u32>().ok())
                .ok_or_else(|| ScriptError::Failed(format!("unexpected reply: {reply}")))
        };
        match fields.first().copied() {
            Some("step") => Ok(Self {
                slide: count(1)?,
                position: count(2)?,
                screen: screen_of(fields.get(3).copied()),
            }),
            Some("noshow") => Err(ScriptError::NoShow),
            Some("nopres") => Err(ScriptError::NotOpen),
            Some("notrunning") => Err(ScriptError::NotRunning),
            _ => Err(ScriptError::Failed(format!("unexpected reply: {reply}"))),
        }
    }

    /// `last` moved: the window and the deck's slides as they were.
    #[must_use]
    pub fn onto(self, last: ShowState) -> ShowState {
        ShowState {
            slide: if self.slide == 0 {
                last.slide
            } else {
                self.slide
            },
            position: self.position,
            screen: self.screen,
            ..last
        }
    }
}

/// The screen a reply names.
fn screen_of(field: Option<&str>) -> Screen {
    match field {
        Some("black") => Screen::Black,
        Some("white") => Screen::White,
        Some("done") => Screen::Ended,
        _ => Screen::Slide,
    }
}

/// A deck open in PowerPoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opened {
    pub slides: u32,
    /// Nida opened it, rather than finding it open: Nida closes it again.
    pub by_nida: bool,
}

impl Reading {
    #[must_use]
    pub fn slides(&self) -> u32 {
        match self {
            Self::Showing(state) => state.slides,
            Self::Open { slides } => *slides,
        }
    }

    /// Parse a state script's reply (see [`scripts::STATE_REPLY`]).
    fn parse(reply: &str) -> Result<Self, ScriptError> {
        let fields: Vec<&str> = reply.trim().split('\t').collect();
        let number = |i: usize| -> Result<i64, ScriptError> {
            fields
                .get(i)
                .and_then(|f| f.trim().replace(',', ".").parse::<f64>().ok())
                .map(|v| v.round() as i64)
                .ok_or_else(|| ScriptError::Failed(format!("unexpected reply: {reply}")))
        };
        let count = |i: usize| number(i).map(|v| u32::try_from(v.max(0)).unwrap_or(u32::MAX));
        let coord = |i: usize| number(i).map(|v| v.clamp(i32::MIN.into(), i32::MAX.into()) as i32);
        match fields.first().copied() {
            Some("show") => Ok(Self::Showing(ShowState {
                slide: count(1)?,
                position: count(2)?,
                slides: count(3)?,
                screen: screen_of(fields.get(4).copied()),
                window: Placement {
                    left: coord(5)?,
                    top: coord(6)?,
                    width: coord(7)?,
                    height: coord(8)?,
                },
                full_screen: fields.get(9).is_some_and(|f| f.trim() == "full"),
            })),
            Some("noshow") => Ok(Self::Open { slides: count(1)? }),
            Some("nopres") => Err(ScriptError::NotOpen),
            Some("notrunning") => Err(ScriptError::NotRunning),
            _ => Err(ScriptError::Failed(format!("unexpected reply: {reply}"))),
        }
    }
}

/// Runs scripts and opens files: `osascript` and `open` in Nida, a stand-in
/// in tests.
pub trait Runner: Send + 'static {
    /// Run AppleScript `script`, its result as text.
    ///
    /// # Errors
    ///
    /// Why it did not run or what it complained of, as [`ScriptError`].
    fn run(&self, script: &str, timeout: Duration) -> Result<String, ScriptError>;
    /// Open `path` in PowerPoint without bringing it forward.
    ///
    /// # Errors
    ///
    /// [`ScriptError::NotInstalled`] without PowerPoint; why `open` failed.
    fn open(&self, path: &Path) -> Result<(), ScriptError>;
    /// PowerPoint came forward to start a show: Nida asks to be in front
    /// again, so its keys (Space cuts) do not go to PowerPoint. macOS may
    /// say no; then a click in Nida does it.
    fn refocus(&self) {}
}

/// The real runner.
#[derive(Debug, Default, Clone, Copy)]
pub struct Osascript;

impl Runner for Osascript {
    fn run(&self, script: &str, timeout: Duration) -> Result<String, ScriptError> {
        let mut child = Command::new("/usr/bin/osascript")
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| ScriptError::Failed(format!("osascript: {e}")))?;
        // The script on its standard input: no length limit, no quoting.
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(script.as_bytes())
                .map_err(|e| ScriptError::Failed(format!("osascript: {e}")))?;
        }
        let read = |pipe: Option<Box<dyn Read + Send>>| {
            std::thread::spawn(move || {
                let mut text = String::new();
                if let Some(mut pipe) = pipe {
                    let _ = pipe.read_to_string(&mut text);
                }
                text
            })
        };
        let stdout = read(
            child
                .stdout
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let stderr = read(
            child
                .stderr
                .take()
                .map(|p| Box::new(p) as Box<dyn Read + Send>),
        );
        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if started.elapsed() > timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(ScriptError::TimedOut);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(2)),
                Err(e) => return Err(ScriptError::Failed(format!("osascript: {e}"))),
            }
        };
        let out = stdout.join().unwrap_or_default();
        let err = stderr.join().unwrap_or_default();
        if status.success() {
            Ok(out.trim_end_matches('\n').to_string())
        } else {
            Err(ScriptError::from_stderr(&err))
        }
    }

    fn refocus(&self) {
        #[cfg(target_os = "macos")]
        objc2::rc::autoreleasepool(|_| {
            use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};
            let id = objc2_foundation::NSString::from_str(POWERPOINT_ID);
            let forward = NSRunningApplication::runningApplicationsWithBundleIdentifier(&id)
                .iter()
                .any(|app| app.isActive());
            if forward {
                let me = NSRunningApplication::currentApplication();
                if !me.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows) {
                    log::info!(
                        "PowerPoint is in front after starting its show; macOS kept it there"
                    );
                }
            }
        });
    }

    fn open(&self, path: &Path) -> Result<(), ScriptError> {
        let output = Command::new("/usr/bin/open")
            .args(["-g", "-b", POWERPOINT_ID])
            .arg(path)
            .output()
            .map_err(|e| ScriptError::Failed(format!("open: {e}")))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("Unable to find application") {
            Err(ScriptError::NotInstalled)
        } else {
            Err(ScriptError::Failed(stderr.trim().to_string()))
        }
    }
}

/// PowerPoint, through a runner.
///
/// Each command fails with the [`ScriptError`] that says why: PowerPoint
/// missing, not allowed, not running, the deck not open or its show not
/// running, no answer in time, or PowerPoint's own complaint.
pub struct PowerPoint<R: Runner = Osascript> {
    runner: R,
}

impl Default for PowerPoint<Osascript> {
    fn default() -> Self {
        Self::new(Osascript)
    }
}

impl<R: Runner> PowerPoint<R> {
    pub fn new(runner: R) -> Self {
        Self { runner }
    }

    fn run(&self, script: &str) -> Result<String, ScriptError> {
        self.runner.run(script, COMMAND_TIMEOUT)
    }

    /// As `run`, given longer when it walks to a slide.
    fn run_walking(&self, script: &str, walks: bool) -> Result<String, ScriptError> {
        let timeout = if walks { WALK_TIMEOUT } else { COMMAND_TIMEOUT };
        self.runner.run(script, timeout)
    }

    /// Open `path` (absolute, as the file system names it) and wait, up to
    /// `wait`, for PowerPoint to have it open.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn open(&self, path: &Path, wait: Duration) -> Result<Opened, ScriptError> {
        // Already open (by hand, or a show reopened): nothing to wait for,
        // and not Nida's to close.
        match self.read(path) {
            Ok(reading) => {
                return Ok(Opened {
                    slides: reading.slides(),
                    by_nida: false,
                })
            }
            Err(ScriptError::NotOpen | ScriptError::NotRunning) => {}
            Err(e) => return Err(e),
        }
        self.runner.open(path)?;
        let started = Instant::now();
        loop {
            match self.read(path) {
                Ok(reading) => {
                    return Ok(Opened {
                        slides: reading.slides(),
                        by_nida: true,
                    })
                }
                // PowerPoint starting, or still reading the file.
                Err(ScriptError::NotOpen | ScriptError::NotRunning | ScriptError::TimedOut)
                    if started.elapsed() < wait =>
                {
                    std::thread::sleep(Duration::from_millis(250));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// Open `path` in PowerPoint without waiting for it: [`Self::read`]
    /// finds it there once PowerPoint has read it.
    ///
    /// # Errors
    ///
    /// [`ScriptError::NotInstalled`] without PowerPoint; why `open` failed.
    pub fn launch(&self, path: &Path) -> Result<(), ScriptError> {
        self.runner.open(path)
    }

    /// Nida in front again, if PowerPoint came forward (see
    /// [`Runner::refocus`]).
    pub fn refocus(&self) {
        self.runner.refocus();
    }

    /// Play the open deck at `path` in a window placed at `at`, from
    /// `slide` (from 1) or its first.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn start(
        &self,
        path: &Path,
        at: Placement,
        slide: Option<u32>,
    ) -> Result<ShowState, ScriptError> {
        let walks = slide.is_some_and(|n| n > 1);
        Self::showing(&self.run_walking(&scripts::start(path, at, slide), walks)?)
    }

    /// A step, answered briefly: a whole reading took twice as long
    /// (measured: 400 ms, against 170 for the step alone).
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn go(&self, path: &Path, to: Go) -> Result<Moved, ScriptError> {
        let walks = matches!(to, Go::Slide(_));
        Moved::parse(&self.run_walking(&scripts::go(path, to), walks)?)
    }

    /// Black or white the screen out, or show the slide again.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn screen(&self, path: &Path, screen: Screen) -> Result<Moved, ScriptError> {
        Moved::parse(&self.run(&scripts::screen(path, screen))?)
    }

    /// Move the show's window.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn place(&self, path: &Path, at: Placement) -> Result<ShowState, ScriptError> {
        Self::showing(&self.run(&scripts::place(path, at))?)
    }

    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn read(&self, path: &Path) -> Result<Reading, ScriptError> {
        Reading::parse(&self.run(&scripts::read(path))?)
    }

    /// End the show, leaving the deck open.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn end(&self, path: &Path) -> Result<(), ScriptError> {
        self.run(&scripts::end(path)).map(|_| ())
    }

    /// Close the deck without saving: Nida only changed how it plays.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn close(&self, path: &Path) -> Result<(), ScriptError> {
        self.run(&scripts::close(path)).map(|_| ())
    }

    /// The video on the slide on screen — the shape `known` holds when its
    /// slide is still on screen, else the slide's first video found — with
    /// `action` done to it first.
    ///
    /// # Errors
    ///
    /// As for every command (see [`PowerPoint`]).
    pub fn video(
        &self,
        path: &Path,
        known: Option<(u32, u32)>,
        action: Option<MediaAction>,
    ) -> Result<VideoReading, ScriptError> {
        VideoReading::parse(&self.run(&scripts::video(path, known, action))?)
    }

    fn showing(reply: &str) -> Result<ShowState, ScriptError> {
        match Reading::parse(reply)? {
            Reading::Showing(state) => Ok(state),
            Reading::Open { .. } => Err(ScriptError::NoShow),
        }
    }
}

/// AppleScript text for each command. Each finds the deck among
/// PowerPoint's open presentations by its path and answers with a state
/// line ([`STATE_REPLY`](scripts::STATE_REPLY)).
pub mod scripts {
    use super::{Go, Placement, Screen, POWERPOINT_ID};
    use std::fmt::Write as _;
    use std::path::Path;

    /// How every command answers: `show`, the slide, its place in the show,
    /// the deck's slides, `running`/`black`/`white`/`done`, the window's
    /// left, top, width and height, and `window` or `full` — tab-separated;
    /// or `noshow` and the slides; or `nopres`; or `notrunning`.
    pub const STATE_REPLY: &str = r#"
on stateOf(thePres)
	tell application id "com.microsoft.Powerpoint"
		set theCount to count of slides of thePres
		try
			set theWindow to slide show window of thePres
			set theView to slideshow view of theWindow
			set theState to slide state of theView
		on error
			return "noshow" & tab & theCount
		end try
		if theState is slide show state done then
			-- The end screen has no slide: the last one shown.
			set theIndex to theCount
			try
				set theIndex to slide index of (last slide viewed of theView)
			end try
		else
			set theIndex to slide index of slide of theView
		end if
		if theState is slide show state black screen then
			set theScreen to "black"
		else if theState is slide show state white screen then
			set theScreen to "white"
		else if theState is slide show state done then
			set theScreen to "done"
		else
			set theScreen to "running"
		end if
		set thePosition to current show position of theView
		set theLeft to (left position of theWindow) as integer
		set theTop to (top of theWindow) as integer
		set theWidth to (width of theWindow) as integer
		set theHeight to (height of theWindow) as integer
		if is full screen of theWindow then
			set theKind to "full"
		else
			set theKind to "window"
		end if
	end tell
	return "show" & tab & theIndex & tab & thePosition & tab & theCount & tab & theScreen & tab & theLeft & tab & theTop & tab & theWidth & tab & theHeight & tab & theKind
end stateOf
"#;

    /// How a step answers, briefly: `step`, the slide (0 on the end screen
    /// when PowerPoint does not say which was last), its place in the show,
    /// `running`/`black`/`white`/`done` — tab-separated; or as a reading.
    pub const STEP_REPLY: &str = r#"
on stepOf(thePres)
	tell application id "com.microsoft.Powerpoint"
		try
			set theView to slideshow view of (slide show window of thePres)
			set theState to slide state of theView
		on error
			return "noshow" & tab & (count of slides of thePres)
		end try
		if theState is slide show state done then
			set theScreen to "done"
			-- As a reading: the last one shown, else the deck's last.
			set theIndex to count of slides of thePres
			try
				set theIndex to slide index of (last slide viewed of theView)
			end try
		else
			if theState is slide show state black screen then
				set theScreen to "black"
			else if theState is slide show state white screen then
				set theScreen to "white"
			else
				set theScreen to "running"
			end if
			set theIndex to slide index of slide of theView
		end if
		set thePosition to current show position of theView
	end tell
	return "step" & tab & theIndex & tab & thePosition & tab & theScreen
end stepOf
"#;

    /// What a command answers with.
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Reply {
        /// A whole reading ([`STATE_REPLY`]).
        State,
        /// Where a step left it ([`STEP_REPLY`]).
        Step,
        /// `closed`.
        Closed,
        /// Its body answers for itself.
        Own,
    }

    /// Finds the presentation by its path: PowerPoint may name it the
    /// POSIX way or the old colon way, so both are tried.
    ///
    /// By index: `full name` of an element taken by `repeat with … in
    /// presentations` comes back empty (PowerPoint 16.114, tried). By any of
    /// the ways the deck's path is written — PowerPoint writes /tmp for
    /// /private/tmp; AppleScript compares accents composed or not, and
    /// capitals, as equal (tried) — else by its file name when one deck alone
    /// has it and PowerPoint does not say where it is: one that does is
    /// another file of that name (two talks' `presentation.pptx`).
    const FIND: &str = r#"
on findPres(thePaths, theFile)
	tell application id "com.microsoft.Powerpoint"
		set theCount to count of presentations
		repeat with i from 1 to theCount
			try
				if thePaths contains (full name of presentation i) then return presentation i
			end try
		end repeat
		set theFound to {}
		repeat with i from 1 to theCount
			if (name of presentation i) is theFile then
				set theFull to ""
				try
					set theFull to (full name of presentation i) as text
				end try
				if theFull is "" then set end of theFound to i
			end if
		end repeat
		if (count of theFound) is 1 then return presentation (item 1 of theFound)
	end tell
	return missing value
end findPres
"#;

    /// The ways `path` may be written back by PowerPoint: as given, resolved,
    /// and without macOS's /private before /tmp, /var and /etc.
    #[must_use]
    pub fn path_forms(path: &Path) -> Vec<String> {
        let mut forms = vec![path.to_string_lossy().into_owned()];
        if let Ok(real) = std::fs::canonicalize(path) {
            forms.push(real.to_string_lossy().into_owned());
        }
        for form in forms.clone() {
            if let Some(rest) = form.strip_prefix("/private") {
                if ["/tmp/", "/var/", "/etc/"]
                    .iter()
                    .any(|d| rest.starts_with(d))
                {
                    forms.push(rest.to_string());
                }
            }
        }
        let mut seen = std::collections::HashSet::new();
        forms.retain(|f| seen.insert(f.clone()));
        forms
    }

    /// An AppleScript string literal of `s`.
    #[must_use]
    pub fn literal(s: &str) -> String {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('"');
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// A script that finds the deck at `path` and, when PowerPoint has it,
    /// does `body` with it as `thePres` and answers as `reply` says — the
    /// handlers it does not use left out, so it compiles sooner.
    fn with_deck(path: &Path, body: &str, reply: Reply) -> String {
        let mut s = String::new();
        s.push_str(FIND);
        match reply {
            Reply::State => s.push_str(STATE_REPLY),
            Reply::Step => s.push_str(STEP_REPLY),
            Reply::Closed | Reply::Own => {}
        }
        let _ = writeln!(
            s,
            "if application id \"{POWERPOINT_ID}\" is not running then return \"notrunning\""
        );
        let forms: Vec<String> = path_forms(path).iter().map(|f| literal(f)).collect();
        let _ = writeln!(s, "set thePaths to {{{}}}", forms.join(", "));
        // The old colon way too, for a PowerPoint that answers so.
        s.push_str("repeat with aPath in (items of thePaths)\n");
        s.push_str("\tset end of thePaths to ((POSIX file (aPath as text)) as text)\n");
        s.push_str("end repeat\n");
        let file = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let _ = writeln!(s, "set theFile to {}", literal(&file));
        s.push_str("with timeout of 8 seconds\n");
        s.push_str("\tset thePres to findPres(thePaths, theFile)\n");
        s.push_str("\tif thePres is missing value then return \"nopres\"\n");
        if !body.is_empty() {
            let _ = writeln!(s, "\ttell application id \"{POWERPOINT_ID}\"");
            for line in body.lines() {
                let _ = writeln!(s, "\t\t{line}");
            }
            s.push_str("\tend tell\n");
        }
        s.push_str(match reply {
            Reply::State => "\treturn stateOf(thePres)\n",
            Reply::Step => "\treturn stepOf(thePres)\n",
            Reply::Closed => "\treturn \"closed\"\n",
            Reply::Own => "",
        });
        s.push_str("end timeout\n");
        s
    }

    /// To slide `n` in a running show, its view `theView`: PowerPoint will
    /// not go to a slide in a show (−50, tried), so it is stepped there —
    /// back to the first slide first when `n` is behind — its builds shown
    /// on the way, quickly. A hidden `n` stops at the next slide shown.
    fn walk_to(n: u32) -> String {
        format!(
            "set theTarget to {n}\n\
             if theTarget > (count of slides of thePres) then set theTarget to (count of slides of thePres)\n\
             if (slide state of theView) is slide show state done then\n\
             \tgo to first slide theView\n\
             else if (slide index of slide of theView) > theTarget then\n\
             \tgo to first slide theView\n\
             end if\n\
             repeat 1000 times\n\
             \tif (slide state of theView) is slide show state done then exit repeat\n\
             \tif (slide index of slide of theView) is greater than or equal to theTarget then exit repeat\n\
             \tgo to next slide theView\n\
             end repeat",
            n = n.max(1)
        )
    }

    fn place_lines(at: Placement) -> String {
        format!(
            "set left position of theWindow to {}\nset top of theWindow to {}\nset width of theWindow to {}\nset height of theWindow to {}",
            at.left, at.top, at.width, at.height
        )
    }

    #[must_use]
    pub fn read(path: &Path) -> String {
        with_deck(path, "", Reply::State)
    }

    #[must_use]
    pub fn start(path: &Path, at: Placement, slide: Option<u32>) -> String {
        // In a window, PowerPoint's own presenter view off: the window is
        // what Nida shows. Settings changed for playing only; never saved.
        let mut body = String::from(
            "set theSettings to slide show settings of thePres\n\
             set show type of theSettings to slide show type window\n\
             set show with presenter of theSettings to false\n\
             set theWindow to run slide show theSettings\n",
        );
        body.push_str(&place_lines(at));
        if let Some(n) = slide.filter(|n| *n > 1) {
            body.push_str("\nset theView to slideshow view of theWindow\n");
            body.push_str(&walk_to(n));
        }
        with_deck(path, &body, Reply::State)
    }

    #[must_use]
    pub fn go(path: &Path, to: Go) -> String {
        let step = match to {
            Go::Next => "go to next slide theView".to_string(),
            Go::Previous => "go to previous slide theView".to_string(),
            Go::First => "go to first slide theView".to_string(),
            Go::Last => "go to last slide theView".to_string(),
            Go::Slide(n) => walk_to(n),
        };
        with_deck(path, &in_show(&step), Reply::Step)
    }

    #[must_use]
    pub fn screen(path: &Path, screen: Screen) -> String {
        let state = match screen {
            Screen::Black => "slide show state black screen",
            Screen::White => "slide show state white screen",
            Screen::Slide | Screen::Ended => "slide show state running",
        };
        with_deck(
            path,
            &in_show(&format!("set slide state of theView to {state}")),
            Reply::Step,
        )
    }

    /// `step` done to the show's view, `theView`, when it has one: else
    /// nothing, and the reply says there is no show (rather than an error
    /// at the first thing asked of a view that is not there).
    fn in_show(step: &str) -> String {
        let mut s = String::from(
            "set theView to slideshow view of (slide show window of thePres)\n\
             if theView is not missing value then\n",
        );
        for line in step.lines() {
            let _ = writeln!(s, "\t{line}");
        }
        s.push_str("end if");
        s
    }

    #[must_use]
    pub fn place(path: &Path, at: Placement) -> String {
        with_deck(
            path,
            &format!(
                "set theWindow to slide show window of thePres\n{}",
                place_lines(at)
            ),
            Reply::State,
        )
    }

    #[must_use]
    pub fn end(path: &Path) -> String {
        with_deck(
            path,
            "try\n\texit slide show (slideshow view of (slide show window of thePres))\nend try",
            Reply::State,
        )
    }

    /// Close the deck unsaved, and answer `closed` rather than its state.
    #[must_use]
    pub fn close(path: &Path) -> String {
        with_deck(path, "close thePres saving no", Reply::Closed)
    }

    /// The video on the slide on screen, `action` done to it first: the
    /// shape `known` names while its slide is on screen, else the slide's
    /// first media shape, looked for. Answers `video`, the slide (0 for no
    /// show or the end screen), the shape (0 for no video), and the video's
    /// state and position in milliseconds — through PowerPoint's player for
    /// a shape in a show (`get player from`, 16.114, tried).
    #[must_use]
    pub fn video(
        path: &Path,
        known: Option<(u32, u32)>,
        action: Option<super::MediaAction>,
    ) -> String {
        use super::MediaAction;
        let (known_slide, known_shape) = known.unwrap_or((0, 0));
        let act = match action {
            None => String::new(),
            Some(MediaAction::Play) => "play player thePlayer\n".into(),
            Some(MediaAction::Pause) => "pause player thePlayer\n".into(),
            Some(MediaAction::Seek(ms)) => format!("set current position of thePlayer to {ms}\n"),
            Some(MediaAction::Restart) => {
                "set current position of thePlayer to 0\nplay player thePlayer\n".into()
            }
        };
        let body = format!(
            "-- the slide's video\n\
             try\n\
             \tset theView to slideshow view of (slide show window of thePres)\n\
             \tif (slide state of theView) is slide show state done then return \"video\" & tab & 0 & tab & 0\n\
             \tset theIndex to slide index of slide of theView\n\
             on error\n\
             \treturn \"video\" & tab & 0 & tab & 0\n\
             end try\n\
             set theShape to 0\n\
             if theIndex is {known_slide} then set theShape to {known_shape}\n\
             if theShape is not 0 then\n\
             \ttry\n\
             \t\tif shape type of (shape theShape of slide theIndex of thePres) is not shape type media then set theShape to 0\n\
             \ton error\n\
             \t\tset theShape to 0\n\
             \tend try\n\
             end if\n\
             if theShape is 0 then\n\
             \tset theShapes to shapes of slide theIndex of thePres\n\
             \trepeat with j from 1 to count of theShapes\n\
             \t\ttry\n\
             \t\t\tif shape type of (item j of theShapes) is shape type media then\n\
             \t\t\t\tset theShape to j\n\
             \t\t\t\texit repeat\n\
             \t\t\tend if\n\
             \t\tend try\n\
             \tend repeat\n\
             end if\n\
             if theShape is 0 then return \"video\" & tab & theIndex & tab & 0\n\
             set thePlayer to get player from theView for shape theShape of slide theIndex of thePres\n\
             {act}\
             return \"video\" & tab & theIndex & tab & theShape & tab & ((player state of thePlayer) as text) & tab & (current position of thePlayer)"
        );
        with_deck(path, &body, Reply::Own)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Answers scripts from a queue and records what it was given.
    #[derive(Clone, Default)]
    struct Fake {
        replies: Arc<Mutex<Vec<Result<String, ScriptError>>>>,
        scripts: Arc<Mutex<Vec<String>>>,
        timeouts: Arc<Mutex<Vec<Duration>>>,
        opened: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl Fake {
        fn answering(replies: Vec<Result<&str, ScriptError>>) -> Self {
            let fake = Self::default();
            *fake.replies.lock().unwrap() = replies
                .into_iter()
                .rev()
                .map(|r| r.map(str::to_string))
                .collect();
            fake
        }
    }

    impl Runner for Fake {
        fn run(&self, script: &str, timeout: Duration) -> Result<String, ScriptError> {
            self.scripts.lock().unwrap().push(script.to_string());
            self.timeouts.lock().unwrap().push(timeout);
            self.replies
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(Err(ScriptError::Failed("no reply queued".into())))
        }

        fn open(&self, path: &Path) -> Result<(), ScriptError> {
            self.opened.lock().unwrap().push(path.to_path_buf());
            Ok(())
        }
    }

    const SHOWING: &str = "show\t3\t2\t24\trunning\t0\t25\t960\t568\twindow";

    #[test]
    fn a_state_line_reads_as_where_the_show_is() {
        assert_eq!(
            Reading::parse(SHOWING).unwrap(),
            Reading::Showing(ShowState {
                slide: 3,
                position: 2,
                slides: 24,
                screen: Screen::Slide,
                window: Placement {
                    left: 0,
                    top: 25,
                    width: 960,
                    height: 568,
                },
                full_screen: false,
            })
        );
        // A comma for a decimal point, as some languages write it.
        let Reading::Showing(state) =
            Reading::parse("show\t1\t1\t5\tblack\t-1920\t0,4\t1920,6\t1080").unwrap()
        else {
            panic!("not showing");
        };
        assert_eq!(state.screen, Screen::Black);
        assert_eq!(state.window.left, -1920);
        assert_eq!(state.window.width, 1921);
        assert!(
            !state.full_screen,
            "an older reply without the kind is a window"
        );
        let Reading::Showing(full) =
            Reading::parse("show\t1\t1\t5\trunning\t0\t0\t1920\t1080\tfull").unwrap()
        else {
            panic!("not showing");
        };
        assert!(full.full_screen);
        assert_eq!(
            Reading::parse("noshow\t12\n").unwrap(),
            Reading::Open { slides: 12 }
        );
        assert_eq!(Reading::parse("nopres"), Err(ScriptError::NotOpen));
        assert_eq!(Reading::parse("notrunning"), Err(ScriptError::NotRunning));
        assert!(matches!(
            Reading::parse("show\t1"),
            Err(ScriptError::Failed(_))
        ));
    }

    #[test]
    fn a_deck_is_known_by_every_way_its_path_is_written() {
        let dir = tempfile::tempdir().unwrap();
        let deck = dir.path().join("talk.pptx");
        std::fs::write(&deck, b"x").unwrap();
        let forms = scripts::path_forms(&deck);
        assert_eq!(forms[0], deck.to_string_lossy());
        // A temporary folder is /var/folders/… resolved to /private/var/…:
        // both ways, as PowerPoint writes the short one.
        let real = std::fs::canonicalize(&deck).unwrap();
        assert!(
            forms.contains(&real.to_string_lossy().into_owned()),
            "{forms:?}"
        );
        if let Some(short) = real.to_string_lossy().strip_prefix("/private") {
            assert!(forms.contains(&short.to_string()), "{forms:?}");
        }
        let s = scripts::read(&deck);
        assert!(s.contains("set theFile to \"talk.pptx\""), "{s}");
        assert!(
            s.contains("repeat with i from 1 to theCount"),
            "found by index"
        );
    }

    #[test]
    fn osascript_complaints_are_told_apart() {
        let err = |s: &str| ScriptError::from_stderr(s);
        assert_eq!(
            err("0:150: execution error: Not authorized to send Apple events to Microsoft PowerPoint. (-1743)\n"),
            ScriptError::NotAllowed
        );
        assert_eq!(
            err("12:30: execution error: Application isn’t running. (-600)"),
            ScriptError::NotRunning
        );
        assert_eq!(
            err(
                "execution error: Microsoft PowerPoint got an error: AppleEvent timed out. (-1712)"
            ),
            ScriptError::TimedOut
        );
        assert_eq!(
            err("1:2: execution error: Microsoft PowerPoint got an error: Can’t get slide show window. (-1728)"),
            ScriptError::Failed("Microsoft PowerPoint got an error: Can’t get slide show window. (-1728)".into())
        );
    }

    #[test]
    fn strings_are_quoted_for_applescript() {
        assert_eq!(
            scripts::literal(r#"/Talks/"Best" \ deck.pptx"#),
            r#""/Talks/\"Best\" \\ deck.pptx""#
        );
        let s = scripts::read(Path::new("/Talks/Jonas \"J\" Č.pptx"));
        assert!(
            s.contains(r#"set thePaths to {"/Talks/Jonas \"J\" Č.pptx"}"#),
            "{s}"
        );
    }

    #[test]
    fn each_command_finds_its_deck_and_answers_its_state() {
        let path = Path::new("/Talks/a.pptx");
        let at = Placement {
            left: 0,
            top: 25,
            width: 960,
            height: 568,
        };
        // A whole reading, or a step's brief answer.
        let state = "return stateOf(thePres)";
        let step = "return stepOf(thePres)";
        for (script, reply, wants) in [
            (scripts::read(path), state, vec![]),
            (
                scripts::go(path, Go::Next),
                step,
                vec!["go to next slide theView"],
            ),
            (
                scripts::go(path, Go::Previous),
                step,
                vec!["go to previous slide theView"],
            ),
            (
                scripts::go(path, Go::First),
                step,
                vec!["go to first slide theView"],
            ),
            (
                scripts::go(path, Go::Last),
                step,
                vec!["go to last slide theView"],
            ),
            // PowerPoint will not go to a slide in a show: walked there.
            (
                scripts::go(path, Go::Slide(0)),
                step,
                vec![
                    "set theTarget to 1",
                    "is greater than or equal to theTarget then exit repeat",
                    "go to next slide theView",
                ],
            ),
            (
                scripts::screen(path, Screen::Black),
                step,
                vec!["to slide show state black screen"],
            ),
            (
                scripts::start(path, at, Some(4)),
                state,
                vec![
                    "set show type of theSettings to slide show type window",
                    "set show with presenter of theSettings to false",
                    "set theWindow to run slide show theSettings",
                    "set width of theWindow to 960",
                    "set theView to slideshow view of theWindow",
                    "set theTarget to 4",
                ],
            ),
            (
                scripts::start(path, at, Some(1)),
                state,
                vec!["set height of theWindow to 568"],
            ),
        ] {
            assert!(
                script.contains("set thePres to findPres(thePaths, theFile)"),
                "{script}"
            );
            assert!(script.contains(reply), "{reply} missing from:\n{script}");
            assert!(
                script.contains("is not running then return \"notrunning\""),
                "{script}"
            );
            for want in wants {
                assert!(script.contains(want), "{want} missing from:\n{script}");
            }
            // A step only in a show: else the reply says there is none.
            if reply == step {
                assert!(
                    script.contains("\t\tif theView is not missing value then\n"),
                    "{script}"
                );
            }
            // Only the reply's own handler: it compiles sooner.
            let other = if reply == state {
                "on stepOf"
            } else {
                "on stateOf"
            };
            assert!(!script.contains(other), "{other} in:\n{script}");
            assert!(
                !script.contains("go to slide"),
                "refused in a show:\n{script}"
            );
        }
        assert!(!scripts::start(path, at, Some(1)).contains("theTarget"));
        let close = scripts::close(path);
        assert!(close.contains("close thePres saving no") && close.contains("return \"closed\""));
        assert!(!close.contains("stateOf(thePres)") && !close.contains("stepOf(thePres)"));
    }

    #[test]
    fn opening_waits_for_powerpoint_to_have_the_deck() {
        let fake = Fake::answering(vec![
            Ok("notrunning"),
            Ok("notrunning"),
            Ok("nopres"),
            Ok("noshow\t24"),
        ]);
        let ppt = PowerPoint::new(fake.clone());
        let path = Path::new("/Talks/a.pptx");
        assert_eq!(
            ppt.open(path, Duration::from_secs(5)),
            Ok(Opened {
                slides: 24,
                by_nida: true
            })
        );
        assert_eq!(*fake.opened.lock().unwrap(), vec![path.to_path_buf()]);
        assert_eq!(fake.scripts.lock().unwrap().len(), 4);
    }

    #[test]
    fn a_deck_already_open_is_not_opened_again() {
        let fake = Fake::answering(vec![Ok(SHOWING)]);
        let ppt = PowerPoint::new(fake.clone());
        assert_eq!(
            ppt.open(Path::new("/Talks/a.pptx"), Duration::from_secs(5)),
            Ok(Opened {
                slides: 24,
                by_nida: false
            })
        );
        assert!(fake.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn refused_by_macos_is_reported_at_once() {
        let fake = Fake::answering(vec![Err(ScriptError::NotAllowed)]);
        let ppt = PowerPoint::new(fake.clone());
        assert_eq!(
            ppt.open(Path::new("/a.pptx"), Duration::from_secs(5)),
            Err(ScriptError::NotAllowed)
        );
        assert!(fake.opened.lock().unwrap().is_empty());
    }

    #[test]
    fn a_step_answers_where_the_show_now_is() {
        let fake = Fake::answering(vec![
            Ok("step\t3\t2\trunning"),
            Ok("step\t0\t25\tdone"),
            Ok("noshow\t24"),
        ]);
        let ppt = PowerPoint::new(fake);
        let moved = ppt.go(Path::new("/a.pptx"), Go::Next).unwrap();
        assert_eq!(
            moved,
            Moved {
                slide: 3,
                position: 2,
                screen: Screen::Slide
            }
        );
        let Reading::Showing(last) = Reading::parse(SHOWING).unwrap() else {
            panic!("not showing");
        };
        // The end screen, PowerPoint not saying which slide was last: the
        // one before kept, the window as it was.
        let ended = ppt.go(Path::new("/a.pptx"), Go::Next).unwrap().onto(last);
        assert_eq!(
            (ended.slide, ended.position, ended.screen),
            (3, 25, Screen::Ended)
        );
        assert_eq!((ended.window, ended.slides), (last.window, last.slides));
        assert_eq!(
            ppt.go(Path::new("/a.pptx"), Go::Next),
            Err(ScriptError::NoShow)
        );
    }

    /// A video script's replies: no show, a slide without a video, and a
    /// video playing, its state as PowerPoint words it (16.114, tried).
    #[test]
    fn a_video_reply_reads_its_slide_and_player() {
        assert_eq!(
            VideoReading::parse("video\t0\t0").unwrap(),
            VideoReading {
                slide: None,
                video: None
            }
        );
        assert_eq!(
            VideoReading::parse("video\t3\t0\n").unwrap(),
            VideoReading {
                slide: Some(3),
                video: None
            }
        );
        assert_eq!(
            VideoReading::parse("video\t2\t8\tplayer state playing\t2816")
                .unwrap()
                .video,
            Some(Video {
                slide: 2,
                shape: 8,
                state: VideoState::Playing,
                position_ms: 2816,
            })
        );
        assert_eq!(
            VideoReading::parse("video\t2\t8\tplayer state not ready\t0")
                .unwrap()
                .video
                .map(|v| v.state),
            Some(VideoState::NotReady)
        );
        assert_eq!(VideoReading::parse("nopres"), Err(ScriptError::NotOpen));
    }

    /// A known video is asked of by its shape while its slide is on screen;
    /// a restart moves it to its start and plays it.
    #[test]
    fn a_video_script_asks_the_shape_known_and_does_what_it_is_told() {
        let path = Path::new("/Talks/talk.pptx");
        let seek = scripts::video(path, Some((2, 8)), Some(MediaAction::Seek(10_000)));
        assert!(
            seek.contains("if theIndex is 2 then set theShape to 8"),
            "{seek}"
        );
        assert!(seek.contains("get player from theView for shape theShape"));
        assert!(seek.contains("set current position of thePlayer to 10000"));
        assert!(!seek.contains("play player thePlayer"));
        let restart = scripts::video(path, Some((2, 8)), Some(MediaAction::Restart));
        let at = restart
            .find("set current position of thePlayer to 0")
            .unwrap();
        assert!(restart[at..].contains("play player thePlayer"));
        let look = scripts::video(path, None, None);
        assert!(look.contains("if theIndex is 0 then set theShape to 0"));
        assert!(look.contains("shape type media"));
    }

    #[test]
    fn a_walk_to_a_slide_is_given_longer_than_a_step() {
        let fake = Fake::answering(vec![
            Ok("step\t2\t2\trunning"),
            Ok("step\t9\t9\trunning"),
            Ok(SHOWING),
            Ok(SHOWING),
        ]);
        let timeouts = Arc::clone(&fake.timeouts);
        let ppt = PowerPoint::new(fake);
        let path = Path::new("/a.pptx");
        let at = Placement {
            left: 0,
            top: 0,
            width: 960,
            height: 572,
        };
        ppt.go(path, Go::Next).unwrap();
        ppt.go(path, Go::Slide(9)).unwrap();
        ppt.start(path, at, Some(5)).unwrap();
        ppt.start(path, at, Some(1)).unwrap();
        assert_eq!(
            *timeouts.lock().unwrap(),
            vec![COMMAND_TIMEOUT, WALK_TIMEOUT, WALK_TIMEOUT, COMMAND_TIMEOUT]
        );
    }

    #[test]
    fn another_file_of_the_decks_name_is_not_taken_for_it() {
        // By file name only when PowerPoint does not say where the deck is:
        // /A/talk.pptx open is not /B/talk.pptx.
        let script = scripts::read(Path::new("/B/talk.pptx"));
        assert!(script.contains("set theFile to \"talk.pptx\""), "{script}");
        assert!(
            script.contains("\t\t\t\tif theFull is \"\" then set end of theFound to i"),
            "{script}"
        );
        // A deck whose full name cannot be read does not stop the search.
        assert!(script.contains(
            "\t\t\ttry\n\t\t\t\tif thePaths contains (full name of presentation i) then return presentation i\n\t\t\tend try"
        ));
    }

    #[test]
    fn steps_read_as_json_as_the_api_takes_them() {
        assert_eq!(serde_json::to_string(&Go::Next).unwrap(), r#""next""#);
        assert_eq!(
            serde_json::from_str::<Go>(r#"{"slide":4}"#).unwrap(),
            Go::Slide(4)
        );
        assert_eq!(serde_json::to_string(&Screen::Black).unwrap(), r#""black""#);
    }
}
