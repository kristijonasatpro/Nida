//! A Slides input: a conference's presentations in one input, in the order
//! they are given — PowerPoint decks, PDFs, web presentations, a speaker's
//! clips and pictures — prepared before the show and each started with one
//! press. Whatever shows the Slides input (a picture-in-picture, a presenter
//! view) shows whichever talk is on, and never has to change.
//! See docs/26-presentations.md.

use super::playlist::PlaylistTransition;
use super::presentation::{PlaysOn, PresentationSnapshot};

/// The file extensions PowerPoint opens.
pub const POWERPOINT_EXTENSIONS: &[&str] = &[
    "pptx", "ppt", "pptm", "ppsx", "pps", "ppsm", "potx", "pot", "potm",
];

/// What plays a talk, by its file.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TalkKind {
    /// PowerPoint plays it, in a window Nida captures (macOS, with
    /// PowerPoint installed).
    #[serde(rename = "powerpoint")]
    PowerPoint,
    /// Nida draws its pages, one page a slide.
    Pdf,
    /// A web page — an HTML presentation — played by the Mac's own web
    /// engine in a window of Nida's that no screen shows (macOS).
    Web,
    /// A video or sound file, played as a clip.
    Clip,
    /// A still picture.
    Picture,
}

impl TalkKind {
    /// What plays `path`, by its extension; `None` for a file none plays.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        use super::media::{IMAGES, SOUNDS, VIDEOS};
        let ext = super::media::extension(std::path::Path::new(path))?;
        let ext = ext.as_str();
        if POWERPOINT_EXTENSIONS.contains(&ext) {
            Some(Self::PowerPoint)
        } else if ext == "pdf" {
            Some(Self::Pdf)
        } else if matches!(ext, "html" | "htm" | "xhtml") {
            Some(Self::Web)
        } else if IMAGES.contains(&ext) {
            Some(Self::Picture)
        } else if VIDEOS.contains(&ext) || SOUNDS.contains(&ext) {
            Some(Self::Clip)
        } else {
            None
        }
    }

    /// How the panel names it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PowerPoint => "PowerPoint",
            Self::Pdf => "PDF",
            Self::Web => "Web",
            Self::Clip => "Clip",
            Self::Picture => "Picture",
        }
    }

    /// Whether its slides are stepped (◀ ▶ and a clicker move it).
    #[must_use]
    pub fn steps(self) -> bool {
        matches!(self, Self::PowerPoint | Self::Pdf | Self::Web)
    }
}

/// One talk in the running order.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct Talk {
    /// Its own name in the list, kept when it is moved, renamed or given
    /// another file.
    pub id: String,
    /// Its file, absolute.
    pub path: String,
    /// What the panel calls it — the speaker, the talk; empty for its
    /// file's name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Where it was left: the slide (PowerPoint's numbering, a PDF's page,
    /// from 1). Started again, it starts there; `None` from its first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
}

impl Talk {
    /// A talk of the file at `path`, with a new id.
    #[must_use]
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            id: new_talk_id(),
            path: path.into(),
            name: String::new(),
            slide: None,
        }
    }

    /// What plays it.
    #[must_use]
    pub fn kind(&self) -> Option<TalkKind> {
        TalkKind::of(&self.path)
    }

    /// Its name as the panel shows it: the one typed, else its file's,
    /// without the extension.
    #[must_use]
    pub fn label(&self) -> String {
        if !self.name.trim().is_empty() {
            return self.name.trim().to_string();
        }
        let p = std::path::Path::new(&self.path);
        p.file_stem()
            .or_else(|| p.file_name())
            .map_or_else(|| self.path.clone(), |s| s.to_string_lossy().into_owned())
    }
}

/// A new talk id: eight hex digits, as layers have.
#[must_use]
pub fn new_talk_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..8].to_string()
}

/// How a Slides input goes from one talk to the next, and where its
/// PowerPoint shows play.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct SlidesSettings {
    /// From one talk to the next: a cut, a fade or a slide.
    #[serde(default = "cut")]
    pub transition: PlaylistTransition,
    /// How long a fade or slide between talks takes, seconds.
    #[serde(default = "default_transition_secs")]
    pub transition_secs: f64,
    /// The screen its PowerPoint shows play on (see
    /// docs/26-presentations.md, "Which screen it plays on").
    #[serde(default, skip_serializing_if = "PlaysOn::is_auto")]
    pub plays_on: PlaysOn,
}

fn cut() -> PlaylistTransition {
    PlaylistTransition::Cut
}

fn default_transition_secs() -> f64 {
    0.5
}

/// The longest fade or slide between talks, seconds.
pub const MAX_TALK_TRANSITION_SECS: f64 = 5.0;

impl Default for SlidesSettings {
    fn default() -> Self {
        Self {
            transition: cut(),
            transition_secs: default_transition_secs(),
            plays_on: PlaysOn::Auto,
        }
    }
}

impl SlidesSettings {
    /// Within bounds: a time that is no number, as a new input has it.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.transition_secs = if self.transition_secs.is_finite() {
            self.transition_secs.clamp(0.0, MAX_TALK_TRANSITION_SECS)
        } else {
            default_transition_secs()
        };
        self
    }

    /// How long going to the next talk takes: none for a cut.
    #[must_use]
    pub fn transition_time(&self) -> f64 {
        match self.transition {
            PlaylistTransition::Cut => 0.0,
            PlaylistTransition::Fade | PlaylistTransition::Slide => self.transition_secs,
        }
    }
}

/// Which talk a start takes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TalkRef {
    /// The one marked NEXT: the talk after the one on, unless another was
    /// picked to come next.
    Next,
    /// The talk on before this one — back to it for questions, at the slide
    /// it was left on.
    Previous,
    /// That talk, by its id.
    Id(String),
}

/// How a talk stands.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TalkState {
    /// Waiting its turn: nothing of it open.
    Waiting,
    /// Opened ahead — PowerPoint has the deck — so starting it is quick.
    Ready,
    /// Being started: what is on stays until its picture comes.
    Starting,
    /// On: the Slides input shows it.
    Live,
    /// It cannot be played: `message` says why.
    Failed,
}

/// A talk as the panel and `GET /api/state` see it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct TalkSnapshot {
    pub id: String,
    /// As the panel names it (see [`Talk::label`]).
    pub name: String,
    /// The name typed for it, empty for none.
    pub given_name: String,
    pub path: String,
    /// What plays it; `None` for a file nothing plays.
    pub kind: Option<TalkKind>,
    pub state: TalkState,
    /// Why it failed or cannot be played, in words to act on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Its file is not there.
    pub missing: bool,
    /// Where it is, "4 / 31": its slide among those it plays, and how many
    /// it plays — for a PDF its page and pages. `None` until known.
    pub position: Option<u32>,
    pub shown: Option<u32>,
    /// A PowerPoint talk's show, while PowerPoint has it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presentation: Option<PresentationSnapshot>,
    /// A clip's length and where it is, seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clip: Option<ClipPosition>,
    /// What may go wrong with its file in the show, found when it was looked
    /// over: a video the deck links to not there, fonts this Mac lacks, a
    /// talk of another shape, a page that needs the internet. It plays all
    /// the same.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// A PowerPoint talk's slides saved as pictures, for the presenter
    /// view's next slide — as its file is now (see `MakeSlidePictures`).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub pictures: bool,
}

/// Where a clip talk is.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct ClipPosition {
    pub at: f64,
    pub length: f64,
    pub playing: bool,
}

/// A Slides input's running order and where it is, for the panel and
/// `GET /api/state`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct SlidesSnapshot {
    pub talks: Vec<TalkSnapshot>,
    pub settings: SlidesSettings,
    /// The talk on, by its place in the list.
    pub live: Option<usize>,
    /// The talk on as it plays — in the list or not, with the file it was
    /// started with: what the panel's ON AIR shows and moves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub on: Option<TalkSnapshot>,
    /// Black or white over the talk on, or the slide.
    pub screen: super::presentation::Screen,
    /// The talk being started: shown once its picture comes.
    pub starting: Option<usize>,
    /// What Start next takes.
    pub next: Option<usize>,
    /// NEXT was picked by hand, not just the talk after the one on.
    pub next_picked: bool,
    /// The talk before the one on, which ⏮ goes back to.
    pub previous: Option<usize>,
    /// Seconds since the talk on started; `None` with none on.
    pub talk_secs: Option<f64>,
    /// Its PowerPoint talks PowerPoint is saving as slide pictures now.
    #[serde(skip_serializing_if = "is_zero")]
    pub making_pictures: usize,
}

// Serde hands the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_told_what_plays_it_by_its_extension() {
        assert_eq!(TalkKind::of("/T/keynote.PPTX"), Some(TalkKind::PowerPoint));
        assert_eq!(TalkKind::of("/T/old.ppt"), Some(TalkKind::PowerPoint));
        assert_eq!(TalkKind::of("/T/show.ppsx"), Some(TalkKind::PowerPoint));
        assert_eq!(TalkKind::of("/T/paper.pdf"), Some(TalkKind::Pdf));
        assert_eq!(TalkKind::of("/T/deck.html"), Some(TalkKind::Web));
        assert_eq!(TalkKind::of("/T/deck.HTM"), Some(TalkKind::Web));
        assert_eq!(TalkKind::of("/T/demo.mp4"), Some(TalkKind::Clip));
        assert_eq!(TalkKind::of("/T/jingle.wav"), Some(TalkKind::Clip));
        assert_eq!(TalkKind::of("/T/break.png"), Some(TalkKind::Picture));
        assert_eq!(TalkKind::of("/T/notes.txt"), None);
        assert_eq!(TalkKind::of("/T/keynote.key"), None);
        assert_eq!(TalkKind::of("/T/no-extension"), None);
    }

    #[test]
    fn a_talk_is_named_as_typed_else_by_its_file() {
        let mut talk = Talk::new("/Talks/2026 results final.pptx");
        assert_eq!(talk.label(), "2026 results final");
        assert_eq!(talk.id.len(), 8);
        talk.name = "  Ona — Results ".into();
        assert_eq!(talk.label(), "Ona — Results");
        assert_ne!(Talk::new("/a.pdf").id, Talk::new("/a.pdf").id);
    }

    #[test]
    fn settings_are_kept_in_bounds() {
        let s = SlidesSettings {
            transition_secs: f64::NAN,
            ..SlidesSettings::default()
        }
        .clamped();
        assert!((s.transition_secs - 0.5).abs() < 1e-9);
        let s = SlidesSettings {
            transition: PlaylistTransition::Fade,
            transition_secs: 60.0,
            ..SlidesSettings::default()
        }
        .clamped();
        assert!((s.transition_time() - MAX_TALK_TRANSITION_SECS).abs() < 1e-9);
        assert!(
            SlidesSettings::default().transition_time().abs() < 1e-9,
            "a cut"
        );
    }

    #[test]
    fn a_saved_list_reads_back_and_leaves_defaults_out() {
        let talk = Talk {
            id: "a1b2c3d4".into(),
            path: "/T/a.pdf".into(),
            name: String::new(),
            slide: None,
        };
        let json = serde_json::to_string(&talk).unwrap();
        assert_eq!(json, r#"{"id":"a1b2c3d4","path":"/T/a.pdf"}"#);
        assert_eq!(serde_json::from_str::<Talk>(&json).unwrap(), talk);
        let s: SlidesSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, SlidesSettings::default());
        let to: TalkRef = serde_json::from_str(r#"{"id":"a1b2c3d4"}"#).unwrap();
        assert_eq!(to, TalkRef::Id("a1b2c3d4".into()));
        let to: TalkRef = serde_json::from_str(r#""next""#).unwrap();
        assert_eq!(to, TalkRef::Next);
    }
}
