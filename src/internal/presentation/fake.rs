//! A stand-in PowerPoint for tests: answers the driver's scripts as
//! PowerPoint would, keeping a show's slide, screen and window, and lists the
//! show's window among the windows on screen. Nothing reaches the real one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::manager::WindowFinder;
use super::powerpoint::{Runner, ScriptError, POWERPOINT_ID};
use crate::screen_capture::backend::{
    CaptureConfig, CaptureTargetInfo, CaptureTargetKind, MockBackend, ScreenCaptureBackend,
};

/// PowerPoint played by a test.
#[derive(Clone, Default)]
pub struct FakePowerPoint {
    state: Arc<Mutex<FakeState>>,
}

/// One switch for each thing a test turns on or off.
#[allow(clippy::struct_excessive_bools)]
#[derive(Default)]
struct FakeState {
    running: bool,
    open: bool,
    showing: bool,
    slide: u32,
    slides: u32,
    ended: bool,
    black: bool,
    window: (i32, i32),
    /// Where the show's window was put, desktop points.
    at: (i32, i32),
    opened_files: Vec<PathBuf>,
    closed: bool,
    steps: Vec<String>,
    refuse: bool,
    /// Reads before a deck being opened is there: PowerPoint taking time.
    opening_reads: u32,
    full_screen: bool,
    /// The show's window number: a new one each time a show starts.
    window_id: u64,
    /// Captures of windows opened, by window number.
    captured: Vec<u64>,
    /// Times Nida asked to be in front again.
    refocused: u32,
    /// macOS refusing Nida the screen: no windows listed.
    no_screen_recording: bool,
    /// Slides with a video, and the video's shape.
    videos: std::collections::HashMap<u32, u32>,
    /// The video on screen: whether it plays, and where it has got.
    player: (Option<bool>, u32),
    /// The slide whose video the player is.
    player_slide: u32,
    /// Times a slide's shapes were looked through for a video.
    video_looks: u32,
    /// Times the video was asked of at all.
    video_reads: u32,
}

impl FakePowerPoint {
    /// Not running yet, with a deck of `slides` to open.
    #[must_use]
    pub fn with_slides(slides: u32) -> Self {
        let fake = Self::default();
        fake.state().slides = slides;
        fake
    }

    fn state(&self) -> MutexGuard<'_, FakeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[must_use]
    pub fn slide(&self) -> u32 {
        self.state().slide
    }

    #[must_use]
    pub fn showing(&self) -> bool {
        self.state().showing
    }

    #[must_use]
    pub fn open(&self) -> bool {
        self.state().open
    }

    /// The steps it took, `next` or `previous`, in order.
    #[must_use]
    pub fn steps(&self) -> Vec<String> {
        self.state().steps.clone()
    }

    /// The files opened through Launch Services.
    #[must_use]
    pub fn opened_files(&self) -> Vec<PathBuf> {
        self.state().opened_files.clone()
    }

    /// Whether the deck was closed (unsaved).
    #[must_use]
    pub fn closed(&self) -> bool {
        self.state().closed
    }

    /// macOS refusing Nida control of PowerPoint, or allowing it again.
    pub fn refuse(&self, refuse: bool) {
        self.state().refuse = refuse;
    }

    /// macOS refusing Nida the screen (no Screen Recording), or allowing it.
    pub fn refuse_screen(&self, refuse: bool) {
        self.state().no_screen_recording = refuse;
    }

    /// Slide `slide` with a video, its shape `shape`.
    pub fn with_video(&self, slide: u32, shape: u32) {
        self.state().videos.insert(slide, shape);
    }

    /// Times a slide was looked through for its video.
    #[must_use]
    pub fn video_looks(&self) -> u32 {
        self.state().video_looks
    }

    /// Times the video was asked of, looked for or not.
    #[must_use]
    pub fn video_reads(&self) -> u32 {
        self.state().video_reads
    }

    /// The video's player: playing (`None` not started) and its position.
    #[must_use]
    pub fn player(&self) -> (Option<bool>, u32) {
        self.state().player
    }

    /// The video's answer to a video script (see `scripts::video`): its
    /// slide's player, what the script tells it done first.
    fn video_line(s: &mut FakeState, script: &str) -> String {
        s.video_reads += 1;
        if !s.showing || s.ended {
            return "video\t0\t0".into();
        }
        let slide = s.slide;
        let known = number(script, "if theIndex is")
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| *n == slide);
        if known.is_none() {
            s.video_looks += 1;
        }
        let Some(&shape) = s.videos.get(&slide) else {
            return format!("video\t{slide}\t0");
        };
        if s.player_slide != slide {
            s.player_slide = slide;
            s.player = (None, 0);
        }
        if let Some(ms) = number(script, "set current position of thePlayer to") {
            s.player.1 = u32::try_from(ms.max(0)).unwrap_or(0);
        }
        if script.contains("play player thePlayer") {
            s.player.0 = Some(true);
        } else if script.contains("pause player thePlayer") {
            s.player.0 = Some(false);
        }
        let state = match s.player.0 {
            None => "player state not ready",
            Some(true) => "player state playing",
            Some(false) => "player state paused",
        };
        format!("video\t{slide}\t{shape}\t{state}\t{}", s.player.1)
    }

    /// Esc pressed in PowerPoint: the show ends, the deck stays open.
    pub fn end_show(&self) {
        self.state().showing = false;
    }

    /// The deck already open in PowerPoint, opened by hand.
    pub fn open_by_hand(&self) {
        let mut s = self.state();
        s.running = true;
        s.open = true;
    }

    /// A show of the deck already playing full screen, started by hand.
    pub fn show_full_screen_by_hand(&self) {
        let mut s = self.state();
        s.running = true;
        s.open = true;
        s.showing = true;
        s.full_screen = true;
        s.slide = 1;
        s.window = (1920, 1080);
        s.window_id += 1;
    }

    /// PowerPoint takes `reads` looks before a deck it is opening is there.
    pub fn open_slowly(&self, reads: u32) {
        self.state().opening_reads = reads;
    }

    /// Whether the show plays full screen.
    #[must_use]
    pub fn full_screen(&self) -> bool {
        self.state().full_screen
    }

    /// The windows a capture was opened of, by window number, in order.
    #[must_use]
    pub fn captured(&self) -> Vec<u64> {
        self.state().captured.clone()
    }

    /// The current show's window number.
    #[must_use]
    pub fn window_id(&self) -> u64 {
        self.state().window_id
    }

    /// Where the show's window was put, and its size, desktop points.
    #[must_use]
    pub fn window_place(&self) -> (i32, i32, i32, i32) {
        let s = self.state();
        (s.at.0, s.at.1, s.window.0, s.window.1)
    }

    /// How often Nida asked to be in front again.
    #[must_use]
    pub fn refocused(&self) -> u32 {
        self.state().refocused
    }

    /// PowerPoint quit (or the deck closed there).
    pub fn quit(&self) {
        let mut s = self.state();
        s.running = false;
        s.open = false;
        s.showing = false;
    }

    /// The windows on screen with this PowerPoint's.
    #[must_use]
    pub fn windows(&self) -> FakeWindows {
        FakeWindows { ppt: self.clone() }
    }

    fn line(s: &mut FakeState) -> String {
        if !s.running {
            return "notrunning".into();
        }
        if s.opening_reads > 0 {
            s.opening_reads -= 1;
            return "nopres".into();
        }
        if !s.open {
            return "nopres".into();
        }
        if !s.showing {
            return format!("noshow\t{}", s.slides);
        }
        let screen = if s.ended {
            "done"
        } else if s.black {
            "black"
        } else {
            "running"
        };
        let kind = if s.full_screen { "full" } else { "window" };
        format!(
            "show\t{}\t{}\t{}\t{screen}\t{}\t{}\t{}\t{}\t{kind}",
            s.slide, s.slide, s.slides, s.at.0, s.at.1, s.window.0, s.window.1
        )
    }

    /// A step's brief answer, as the step reply gives it.
    fn step_line(s: &mut FakeState) -> String {
        let line = Self::line(s);
        if !line.starts_with("show\t") {
            return line;
        }
        let screen = if s.ended {
            "done"
        } else if s.black {
            "black"
        } else {
            "running"
        };
        // Past the end, the position is one past the last.
        let position = if s.ended { s.slides + 1 } else { s.slide };
        format!("step\t{}\t{position}\t{screen}", s.slide)
    }
}

/// The number after `key` in `script`.
fn number(script: &str, key: &str) -> Option<i32> {
    let at = script.find(key)? + key.len();
    script[at..].split_whitespace().next()?.parse().ok()
}

impl Runner for FakePowerPoint {
    fn run(&self, script: &str, _timeout: Duration) -> Result<String, ScriptError> {
        let mut s = self.state();
        if s.refuse {
            return Err(ScriptError::NotAllowed);
        }
        if s.running && s.open && s.opening_reads == 0 {
            if script.contains("run slide show") {
                s.showing = true;
                s.slide = 1;
                s.ended = false;
                s.black = false;
                s.full_screen = false;
                s.window_id += 1;
            }
            if let (Some(left), Some(top)) = (
                number(script, "set left position of theWindow to"),
                number(script, "set top of theWindow to"),
            ) {
                s.at = (left, top);
            }
            if let (Some(w), Some(h)) = (
                number(script, "set width of theWindow to"),
                number(script, "set height of theWindow to"),
            ) {
                s.window = (w, h);
            }
            // Stepped to a slide, as PowerPoint is walked there: the slide,
            // or the deck's last.
            let walked = number(script, "set theTarget to");
            if let (Some(n), true) = (walked, s.showing) {
                s.slide = u32::try_from(n).unwrap_or(1).clamp(1, s.slides.max(1));
                s.ended = false;
                let to = format!("to {}", s.slide);
                s.steps.push(to);
            }
            if s.showing && walked.is_none() {
                if script.contains("go to next slide") {
                    s.steps.push("next".into());
                    if s.slide == s.slides {
                        s.ended = true;
                    } else {
                        s.slide += 1;
                    }
                } else if script.contains("go to previous slide") {
                    s.steps.push("previous".into());
                    s.ended = false;
                    s.slide = s.slide.saturating_sub(1).max(1);
                } else if script.contains("to slide show state black screen") {
                    s.black = true;
                } else if script.contains("to slide show state running") {
                    s.black = false;
                }
            }
            if script.contains("exit slide show") {
                s.showing = false;
                s.full_screen = false;
            }
            if script.contains("close thePres saving no") {
                s.open = false;
                s.showing = false;
                s.closed = true;
                return Ok("closed".into());
            }
        }
        if script.contains("return stepOf(thePres)") {
            return Ok(Self::step_line(&mut s));
        }
        if script.contains("-- the slide's video") {
            return Ok(Self::video_line(&mut s, script));
        }
        Ok(Self::line(&mut s))
    }

    fn open(&self, path: &Path) -> Result<(), ScriptError> {
        let mut s = self.state();
        s.running = true;
        s.open = true;
        s.opened_files.push(path.to_path_buf());
        Ok(())
    }

    fn refocus(&self) {
        self.state().refocused += 1;
    }
}

/// PowerPoint's editor window, and its show's window while there is a show,
/// at the size the stand-in has it.
#[derive(Clone)]
pub struct FakeWindows {
    ppt: FakePowerPoint,
}

/// A window of PowerPoint's.
#[must_use]
pub fn window(title: &str, width: u32, height: u32) -> CaptureTargetInfo {
    window_numbered(title, width, height, u64::from(width))
}

/// A window of PowerPoint's with its window number.
#[must_use]
pub fn window_numbered(title: &str, width: u32, height: u32, number: u64) -> CaptureTargetInfo {
    CaptureTargetInfo {
        kind: CaptureTargetKind::Window,
        platform_id: number,
        label: format!("Microsoft PowerPoint — {title}"),
        app: Some(POWERPOINT_ID.into()),
        title: Some(title.into()),
        width,
        height,
        is_varda: false,
    }
}

impl WindowFinder for FakeWindows {
    fn windows(&self) -> Result<Vec<CaptureTargetInfo>, String> {
        let s = self.ppt.state();
        if s.no_screen_recording {
            return Err(super::manager::SCREEN_RECORDING_OFF.to_string());
        }
        let mut list = vec![window_numbered("talk.pptx", 1440, 900, 1)];
        if s.showing {
            list.push(window_numbered(
                "PowerPoint Slide Show - talk.pptx",
                u32::try_from(s.window.0).unwrap_or(0),
                u32::try_from(s.window.1).unwrap_or(0),
                1000 + s.window_id,
            ));
        }
        Ok(list)
    }

    fn open(
        &self,
        target: &CaptureTargetInfo,
        config: &CaptureConfig,
    ) -> Result<Box<dyn ScreenCaptureBackend>, String> {
        self.ppt.state().captured.push(target.platform_id);
        Ok(Box::new(MockBackend::new(
            target.label.clone(),
            target.width,
            target.height,
            config.clone(),
        )))
    }
}
