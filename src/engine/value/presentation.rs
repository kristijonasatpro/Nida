//! A PowerPoint input's show: where a step goes, what the screen shows, and
//! how the show stands, as commands take them and the API and panels see
//! them. PowerPoint itself is driven in `crate::presentation`.

/// Where a step goes.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Go {
    /// The next animation, or the next slide when the slide has none left
    /// — a clicker's forward button.
    Next,
    /// Back one step.
    Previous,
    First,
    Last,
    /// Slide `n` in PowerPoint's numbering (from 1).
    Slide(u32),
}

/// What the screen shows besides the slide.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Screen {
    #[default]
    Slide,
    /// Blacked out (B on a clicker or keyboard).
    Black,
    /// Whited out (W).
    White,
    /// Past the last slide: PowerPoint's black end screen.
    Ended,
}

/// Which screen a PowerPoint input's show plays on. PowerPoint draws its
/// slide the size of its window, and macOS keeps a window inside its
/// screen, so a bigger or sharper screen shows it sharper.
#[derive(
    Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum PlaysOn {
    /// The screen it plays sharpest on — a spare one (connected, with no
    /// output on it) when that is as sharp as this Mac's own.
    #[default]
    Auto,
    /// This Mac's screen: the one with the menu bar.
    Main,
    /// That screen, by its identity (`outputs.monitors[].identity`), while
    /// it is connected and no output is on it; else as `auto`.
    Screen(String),
}

impl PlaysOn {
    #[must_use]
    pub fn is_auto(&self) -> bool {
        *self == Self::Auto
    }
}

/// Where a presenter view's bar — where the show is, and its clocks — goes.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Default,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ClocksAt {
    Top,
    #[default]
    Bottom,
}

/// How a presenter view is laid out: which of the talk's clock and the time
/// of day it shows, and where; whether it shows what comes next; how much of
/// its width the slide takes; how big the notes and what comes next are
/// drawn. A new one shows the talk's clock at its foot, not the time of day,
/// and the next slide — its picture as wide as the notes' column.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(default)]
pub struct PresenterLayout {
    /// How long the talk has run.
    pub timer: bool,
    /// The time of day.
    pub clock: bool,
    /// Where the bar with where the show is and the clocks goes.
    pub clocks_at: ClocksAt,
    /// What comes next: the next slide's picture beside the slide, where
    /// Nida has one (a PDF talk's next page), with its line under it; else
    /// the line under the slide.
    pub next: bool,
    /// The slide's share of the width, 0.35 to 0.8; the notes have the rest.
    pub slide_share: f32,
    /// The notes' size, 0.5 to 2 (1 as Nida draws them).
    pub notes_size: f32,
    /// The size of what comes next — its picture, or its line — 0.5 to 2;
    /// at 1 the picture is as wide as the notes' column, and no wider above.
    pub next_size: f32,
}

impl Default for PresenterLayout {
    fn default() -> Self {
        Self {
            timer: true,
            clock: false,
            clocks_at: ClocksAt::Bottom,
            next: true,
            slide_share: 0.6,
            notes_size: 1.0,
            next_size: 1.0,
        }
    }
}

impl PresenterLayout {
    /// Within its bounds; a size that is no number, as drawn by default.
    #[must_use]
    pub fn sanitized(self) -> Self {
        let within = |v: f32, lo: f32, hi: f32, default: f32| {
            if v.is_finite() {
                v.clamp(lo, hi)
            } else {
                default
            }
        };
        let default = Self::default();
        Self {
            slide_share: within(self.slide_share, 0.35, 0.8, default.slide_share),
            notes_size: within(self.notes_size, 0.5, 2.0, default.notes_size),
            next_size: within(self.next_size, 0.5, 2.0, default.next_size),
            ..self
        }
    }

    /// As a new presenter view has it: left out of a saved show.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// What a video on the slide on screen is doing, as PowerPoint plays it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VideoState {
    Playing,
    Paused,
    Stopped,
    /// Not started yet on this slide.
    NotReady,
}

/// The video (or sound) on the slide on screen — the first, when the slide
/// has more: what it is doing and where it has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct VideoSnapshot {
    pub state: VideoState,
    /// Milliseconds from its start.
    pub position_ms: u32,
}

/// What to do with the video on the slide on screen.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum VideoControl {
    /// Play when it is not playing, else pause.
    Toggle,
    Play,
    Pause,
    /// From its start, playing.
    Restart,
    /// Forward (or, negative, back) this many seconds; not before its start.
    Skip(i32),
}

/// How a PowerPoint input's show stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PresentationStatus {
    /// PowerPoint is opening the deck or starting its show.
    Opening,
    /// The show is running and its window captured.
    Showing,
    /// It is not: `message` says why. Tried again by itself.
    Failed,
    /// The show runs but its window was not found to capture.
    NoWindow,
    /// Open in PowerPoint, its show not started: a Slides input's next talk
    /// (or the one before the talk on), ready to start quickly.
    Ready,
}

/// A PowerPoint input's show, for `GET /api/state` and the panels.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct PresentationSnapshot {
    /// The deck's file.
    pub path: String,
    pub status: PresentationStatus,
    /// Why it failed, in words to act on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The slide on screen, in PowerPoint's numbering (from 1, hidden
    /// slides counted); `None` until the show runs.
    pub slide: Option<u32>,
    /// Slides in the deck, hidden ones too.
    pub slides: Option<u32>,
    /// The slide's place among those the show plays, and how many it plays
    /// (hidden ones left out): "3 / 24".
    pub position: Option<u32>,
    pub shown: Option<u32>,
    pub screen: Screen,
    /// The slide's title, when it has a title placeholder.
    pub title: Option<String>,
    /// The slide's speaker notes; empty for none.
    pub notes: String,
    /// The next slide the show plays: its number and title.
    pub next_slide: Option<u32>,
    pub next_title: Option<String>,
    /// Which screen it is to play on.
    pub plays_on: PlaysOn,
    /// The screen its window is on, as screens are named; `None` until it
    /// plays.
    pub playing_on: Option<String>,
    /// The slide's width and height as PowerPoint draws it there, pixels:
    /// the most its picture shows.
    pub slide_pixels: Option<[u32; 2]>,
    /// The video on the slide on screen, when it has one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoSnapshot>,
}
