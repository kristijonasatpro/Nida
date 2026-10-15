//! Playlist value types: clips and pictures shown one after another in one
//! layer, as vMix's List input — each picture held a while, each clip played
//! to its end, and a cut, fade or slide from one to the next.

/// One thing in a playlist: a clip, a sound file or a picture, by its path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct PlaylistItem {
    pub path: String,
}

impl PlaylistItem {
    pub fn new(path: impl Into<String>) -> Self {
        Self { path: path.into() }
    }

    /// The file's name, as the list shows it.
    pub fn name(&self) -> &str {
        std::path::Path::new(&self.path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.path)
    }
}

/// How one item gives way to the next.
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
pub enum PlaylistTransition {
    /// At once.
    Cut,
    /// One dissolves into the next.
    #[default]
    Fade,
    /// The next pushes the last off the screen.
    Slide,
}

impl PlaylistTransition {
    pub const ALL: [Self; 3] = [Self::Cut, Self::Fade, Self::Slide];

    pub fn label(self) -> &'static str {
        match self {
            Self::Cut => "Cut",
            Self::Fade => "Fade",
            Self::Slide => "Slide",
        }
    }
}

/// The side a slide brings the next item in from.
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
pub enum SlideFrom {
    #[default]
    Right,
    Left,
    Top,
    Bottom,
}

impl SlideFrom {
    pub const ALL: [Self; 4] = [Self::Right, Self::Left, Self::Top, Self::Bottom];

    pub fn label(self) -> &'static str {
        match self {
            Self::Right => "From the right",
            Self::Left => "From the left",
            Self::Top => "From the top",
            Self::Bottom => "From the bottom",
        }
    }

    /// Where the next item starts, in UV of the frame (y down).
    pub fn offset(self) -> [f32; 2] {
        match self {
            Self::Right => [1.0, 0.0],
            Self::Left => [-1.0, 0.0],
            Self::Top => [0.0, -1.0],
            Self::Bottom => [0.0, 1.0],
        }
    }
}

/// How a playlist plays.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct PlaylistSettings {
    #[serde(default)]
    pub transition: PlaylistTransition,
    #[serde(default)]
    pub slide_from: SlideFrom,
    /// A transition starts slowly, speeds up and slows down into place,
    /// rather than moving at one speed — gently: at its fastest, in the
    /// middle, about 1.6 times its average speed.
    #[serde(default = "yes")]
    pub speed_ramp: bool,
    /// How long a fade or slide takes, in seconds.
    #[serde(default = "default_transition_secs")]
    pub transition_secs: f64,
    /// How long a picture stays on screen, in seconds, the transition not
    /// counted. A clip plays to its end.
    #[serde(default = "default_picture_secs")]
    pub picture_secs: f64,
    /// Start again from the first item after the last.
    #[serde(default = "yes", rename = "loop")]
    pub loop_list: bool,
}

const fn yes() -> bool {
    true
}

const fn default_transition_secs() -> f64 {
    1.0
}

const fn default_picture_secs() -> f64 {
    5.0
}

/// The shortest a picture is held, so a list cannot race.
pub const MIN_PICTURE_SECS: f64 = 0.5;
/// The longest a fade or slide takes.
pub const MAX_TRANSITION_SECS: f64 = 10.0;

impl Default for PlaylistSettings {
    fn default() -> Self {
        Self {
            transition: PlaylistTransition::default(),
            slide_from: SlideFrom::default(),
            speed_ramp: true,
            transition_secs: default_transition_secs(),
            picture_secs: default_picture_secs(),
            loop_list: true,
        }
    }
}

impl PlaylistSettings {
    /// These settings with times in range: a picture held at least
    /// [`MIN_PICTURE_SECS`], a transition from 0 to [`MAX_TRANSITION_SECS`].
    #[must_use]
    pub fn clamped(self) -> Self {
        let finite = |v: f64, default: f64| if v.is_finite() { v } else { default };
        Self {
            transition_secs: finite(self.transition_secs, default_transition_secs())
                .clamp(0.0, MAX_TRANSITION_SECS),
            picture_secs: finite(self.picture_secs, default_picture_secs()).max(MIN_PICTURE_SECS),
            ..self
        }
    }

    /// How long a transition takes: nothing for a cut.
    pub fn transition_time(&self) -> f64 {
        match self.transition {
            PlaylistTransition::Cut => 0.0,
            PlaylistTransition::Fade | PlaylistTransition::Slide => self.transition_secs,
        }
    }

    /// How far a transition has got, 0 to 1, `elapsed` seconds in — with the
    /// speed ramp, slow at both ends and fastest in the middle.
    ///
    /// The ramp is a sine's: fastest at π/2 (1.57) times the average speed.
    /// A cubic ramp reached three times it, and a slide that fast moved so
    /// far between frames that it looked to step rather than glide.
    pub fn progress(&self, elapsed: f64) -> f32 {
        let time = self.transition_time();
        let t = if time <= 0.0 {
            1.0
        } else {
            (elapsed / time).clamp(0.0, 1.0)
        };
        let eased = if self.speed_ramp {
            0.5 - 0.5 * (std::f64::consts::PI * t).cos()
        } else {
            t
        };
        eased as f32
    }
}

/// A playlist as the UI shows it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct PlaylistSnapshot {
    pub items: Vec<PlaylistItem>,
    pub settings: PlaylistSettings,
    pub playing: bool,
    /// The item on screen.
    pub shown: Option<usize>,
    /// The item it is giving way to, or picked to show next.
    pub coming: Option<usize>,
    /// The item after it, open and ready to show.
    pub next_ready: Option<usize>,
    /// Items that would not open, skipped.
    pub failed: Vec<usize>,
    /// Seconds before the item on screen gives way (a clip's time left).
    pub remaining: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ramped_transition_is_slow_at_both_ends_and_whole_at_the_end() {
        let settings = PlaylistSettings {
            transition: PlaylistTransition::Slide,
            transition_secs: 2.0,
            ..PlaylistSettings::default()
        };
        assert_eq!(settings.progress(0.0), 0.0);
        assert!((settings.progress(1.0) - 0.5).abs() < 1e-6, "half way");
        assert_eq!(settings.progress(2.0), 1.0);
        assert_eq!(settings.progress(5.0), 1.0);
        let even = PlaylistSettings {
            speed_ramp: false,
            ..settings
        };
        // A tenth of the way in, the ramp has barely moved; even speed has
        // gone a tenth.
        assert!(settings.progress(0.2) < 0.03);
        assert!((even.progress(0.2) - 0.1).abs() < 1e-6);
    }

    /// At its fastest the ramp goes little more than one and a half times
    /// its average speed: fast enough to feel, not so fast it strobes.
    #[test]
    fn the_ramp_is_never_much_faster_than_the_average() {
        let settings = PlaylistSettings {
            transition: PlaylistTransition::Slide,
            transition_secs: 1.0,
            ..PlaylistSettings::default()
        };
        let step = 0.001;
        let fastest = (0..1000)
            .map(|i| {
                let t = f64::from(i) * step;
                f64::from(settings.progress(t + step) - settings.progress(t)) / step
            })
            .fold(0.0, f64::max);
        assert!(fastest < 1.6, "fastest {fastest} times the average");
    }

    #[test]
    fn a_cut_takes_no_time() {
        let cut = PlaylistSettings {
            transition: PlaylistTransition::Cut,
            ..PlaylistSettings::default()
        };
        assert_eq!(cut.transition_time(), 0.0);
        assert_eq!(cut.progress(0.0), 1.0);
    }

    #[test]
    fn settings_are_kept_in_range() {
        let wild = PlaylistSettings {
            transition_secs: 99.0,
            picture_secs: 0.0,
            ..PlaylistSettings::default()
        }
        .clamped();
        assert_eq!(wild.transition_secs, MAX_TRANSITION_SECS);
        assert_eq!(wild.picture_secs, MIN_PICTURE_SECS);
        let nan = PlaylistSettings {
            transition_secs: f64::NAN,
            ..PlaylistSettings::default()
        }
        .clamped();
        assert_eq!(nan.transition_secs, 1.0);
    }

    #[test]
    fn settings_saved_before_a_field_existed_take_its_default() {
        let settings: PlaylistSettings = serde_json::from_str(r#"{"transition":"slide"}"#).unwrap();
        assert_eq!(settings.transition, PlaylistTransition::Slide);
        assert!(settings.loop_list && settings.speed_ramp);
        assert_eq!(settings.picture_secs, 5.0);
    }
}
