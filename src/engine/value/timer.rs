//! Timer value types: a count down, a count to a time of day, a count up or
//! the time of day, drawn as text in a layer — in any font installed, any
//! colours, on a see-through background or on a box of its own.
//!
//! What a timer reads is worked out here from how long it has run and the
//! time of day, so it can be tested without a clock; the layer that keeps
//! the time and draws it is [`crate::deck::timer`].

/// What a timer shows.
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
pub enum TimerMode {
    /// Down from a length of time, started and paused by hand.
    #[default]
    Countdown,
    /// Down to a time of day: "starts in".
    CountTo,
    /// Up from zero, started and paused by hand: how long a talk has run.
    CountUp,
    /// The time of day.
    Clock,
}

impl TimerMode {
    pub const ALL: [Self; 4] = [Self::Countdown, Self::CountTo, Self::CountUp, Self::Clock];

    pub fn label(self) -> &'static str {
        match self {
            Self::Countdown => "Count down",
            Self::CountTo => "Count to a time",
            Self::CountUp => "Count up",
            Self::Clock => "Time of day",
        }
    }

    /// Started, paused and reset by hand; the others run by the clock.
    pub fn has_transport(self) -> bool {
        matches!(self, Self::Countdown | Self::CountUp)
    }

    /// Counts towards a zero, and can warn before it.
    pub fn counts_down(self) -> bool {
        matches!(self, Self::Countdown | Self::CountTo)
    }
}

/// What a count down does at zero.
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
pub enum AtZero {
    /// Stops on 0:00.
    #[default]
    Stop,
    /// Counts on past zero, with a minus: how far over time it is.
    CountOver,
}

impl AtZero {
    pub const ALL: [Self; 2] = [Self::Stop, Self::CountOver];

    pub fn label(self) -> &'static str {
        match self {
            Self::Stop => "Stop on 0:00",
            Self::CountOver => "Count on over time (-0:12)",
        }
    }
}

/// How a length of time is written.
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
pub enum TimeFormat {
    /// 4:59, and 1:04:59 once there is an hour.
    #[default]
    Auto,
    /// 00:04:59.
    Hours,
    /// 04:59, and 64:59 past the hour.
    Minutes,
}

impl TimeFormat {
    pub const ALL: [Self; 3] = [Self::Auto, Self::Hours, Self::Minutes];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "4:59 (hours when there are)",
            Self::Hours => "00:04:59",
            Self::Minutes => "04:59",
        }
    }
}

/// Where on the frame the timer sits.
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
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    #[default]
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    /// Row by row, as the picker lays them out.
    pub const ALL: [Self; 9] = [
        Self::TopLeft,
        Self::Top,
        Self::TopRight,
        Self::Left,
        Self::Center,
        Self::Right,
        Self::BottomLeft,
        Self::Bottom,
        Self::BottomRight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::TopLeft => "Top left",
            Self::Top => "Top",
            Self::TopRight => "Top right",
            Self::Left => "Left",
            Self::Center => "Centre",
            Self::Right => "Right",
            Self::BottomLeft => "Bottom left",
            Self::Bottom => "Bottom",
            Self::BottomRight => "Bottom right",
        }
    }

    /// How far across and down the frame: 0, a half or 1 of each.
    pub fn fractions(self) -> (f32, f32) {
        let x = match self {
            Self::TopLeft | Self::Left | Self::BottomLeft => 0.0,
            Self::Top | Self::Center | Self::Bottom => 0.5,
            Self::TopRight | Self::Right | Self::BottomRight => 1.0,
        };
        let y = match self {
            Self::TopLeft | Self::Top | Self::TopRight => 0.0,
            Self::Left | Self::Center | Self::Right => 0.5,
            Self::BottomLeft | Self::Bottom | Self::BottomRight => 1.0,
        };
        (x, y)
    }

    /// Where a `size` box goes on a `frame`, `margin` in from the edges it
    /// keeps to — its top left corner, in whole pixels.
    pub fn place(self, size: (f32, f32), frame: (f32, f32), margin: f32) -> (f32, f32) {
        let (fx, fy) = self.fractions();
        // A side's box is kept `margin` in from it; a middle one centred.
        let along = |f: f32, size: f32, frame: f32| {
            let room = frame - size;
            let at = if f < 0.25 {
                margin
            } else if f > 0.75 {
                room - margin
            } else {
                room / 2.0
            };
            at.round()
        };
        (along(fx, size.0, frame.0), along(fy, size.1, frame.1))
    }
}

/// How a timer looks.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct TimerLook {
    /// A font family installed on the computer; one not found falls back
    /// to the system's sans serif.
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "yes")]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    /// The numbers' colour, sRGB.
    #[serde(default = "white")]
    pub color: [u8; 3],
    /// The numbers' height, as a share of the frame's.
    #[serde(default = "default_size")]
    pub size: f32,
    #[serde(default)]
    pub anchor: Anchor,
    /// Room kept from the frame's edges, as a share of its height.
    #[serde(default = "default_margin")]
    pub margin: f32,
    /// The box behind the numbers — none at 0 opacity, which leaves the
    /// background see-through.
    #[serde(default)]
    pub box_color: [u8; 3],
    #[serde(default)]
    pub box_opacity: f32,
    /// An outline round the numbers, for reading them over a picture: its
    /// width as a share of their height, none at 0.
    #[serde(default)]
    pub outline_color: [u8; 3],
    #[serde(default)]
    pub outline: f32,
    /// A line of text over the numbers ("Back in"), none when empty.
    #[serde(default)]
    pub caption: String,
    /// The colour in the last seconds before zero.
    #[serde(default = "amber")]
    pub warn_color: [u8; 3],
    /// The colour at zero, and over time.
    #[serde(default = "red")]
    pub over_color: [u8; 3],
}

impl Default for TimerLook {
    fn default() -> Self {
        Self {
            font: default_font(),
            bold: true,
            italic: false,
            color: white(),
            size: default_size(),
            anchor: Anchor::default(),
            margin: default_margin(),
            box_color: [0, 0, 0],
            box_opacity: 0.0,
            outline_color: [0, 0, 0],
            outline: 0.0,
            caption: String::new(),
            warn_color: amber(),
            over_color: red(),
        }
    }
}

/// The smallest and largest the numbers can be, as a share of the frame.
pub const MIN_SIZE: f32 = 0.02;
pub const MAX_SIZE: f32 = 0.8;
/// The widest outline, as a share of the numbers' height.
pub const MAX_OUTLINE: f32 = 0.1;
/// The longest count down: a day, less a second.
pub const MAX_DURATION_SECS: u32 = 24 * 3600 - 1;
const DAY_SECS: u32 = 24 * 3600;

fn default_font() -> String {
    "Helvetica Neue".to_string()
}

const fn yes() -> bool {
    true
}

const fn white() -> [u8; 3] {
    [255, 255, 255]
}

const fn amber() -> [u8; 3] {
    [255, 190, 0]
}

const fn red() -> [u8; 3] {
    [235, 45, 45]
}

const fn default_size() -> f32 {
    0.15
}

const fn default_margin() -> f32 {
    0.04
}

const fn default_duration() -> u32 {
    5 * 60
}

const fn default_target() -> u32 {
    9 * 3600
}

const fn default_warn() -> u32 {
    60
}

/// What a timer counts and how it writes it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct TimerSettings {
    #[serde(default)]
    pub mode: TimerMode,
    /// How long a count down is, in seconds.
    #[serde(default = "default_duration")]
    pub duration_secs: u32,
    /// The time of day counted to, in seconds after midnight.
    #[serde(default = "default_target")]
    pub target_secs: u32,
    #[serde(default)]
    pub at_zero: AtZero,
    /// Seconds before zero it turns the warning colour; never at 0.
    #[serde(default = "default_warn")]
    pub warn_secs: u32,
    #[serde(default)]
    pub format: TimeFormat,
    /// The time of day as 14:05, or as 2:05 PM.
    #[serde(default = "yes")]
    pub clock_24h: bool,
    /// Seconds on the time of day.
    #[serde(default = "yes")]
    pub clock_seconds: bool,
    #[serde(default)]
    pub look: TimerLook,
}

impl Default for TimerSettings {
    fn default() -> Self {
        Self {
            mode: TimerMode::default(),
            duration_secs: default_duration(),
            target_secs: default_target(),
            at_zero: AtZero::default(),
            warn_secs: default_warn(),
            format: TimeFormat::default(),
            clock_24h: true,
            clock_seconds: true,
            look: TimerLook::default(),
        }
    }
}

/// Which colour a timer is in.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TimerState {
    #[default]
    Normal,
    /// The last seconds before zero.
    Warn,
    /// At zero, or over time.
    Over,
}

/// What a timer shows at a moment.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    pub text: String,
    pub state: TimerState,
    /// Seconds left before zero, for a count down — below zero over time.
    pub remaining: Option<f64>,
}

impl TimerSettings {
    /// Within bounds, whatever was asked.
    #[must_use]
    pub fn clamped(mut self) -> Self {
        self.duration_secs = self.duration_secs.min(MAX_DURATION_SECS);
        self.target_secs %= DAY_SECS;
        self.warn_secs = self.warn_secs.min(MAX_DURATION_SECS);
        let look = &mut self.look;
        look.size = finite_or(look.size, default_size()).clamp(MIN_SIZE, MAX_SIZE);
        look.margin = finite_or(look.margin, default_margin()).clamp(0.0, 0.4);
        look.box_opacity = finite_or(look.box_opacity, 0.0).clamp(0.0, 1.0);
        look.outline = finite_or(look.outline, 0.0).clamp(0.0, MAX_OUTLINE);
        if look.font.trim().is_empty() {
            look.font = default_font();
        }
        self
    }

    /// Seconds left before zero, from how long the timer has run and the
    /// time of day (seconds after midnight) — `None` for what counts up.
    pub fn remaining(&self, elapsed: f64, time_of_day: f64) -> Option<f64> {
        match self.mode {
            TimerMode::Countdown => Some(f64::from(self.duration_secs) - elapsed),
            TimerMode::CountTo => Some(until(f64::from(self.target_secs), time_of_day)),
            TimerMode::CountUp | TimerMode::Clock => None,
        }
    }

    /// What the timer reads.
    pub fn reading(&self, elapsed: f64, time_of_day: f64) -> Reading {
        let remaining = self.remaining(elapsed, time_of_day);
        let (text, state) = match (self.mode, remaining) {
            (TimerMode::Clock, _) => (
                clock_text(time_of_day, self.clock_24h, self.clock_seconds),
                TimerState::Normal,
            ),
            (TimerMode::CountUp, _) | (_, None) => (
                span_text(whole_secs(elapsed.max(0.0).floor()), self.format),
                TimerState::Normal,
            ),
            (_, Some(left)) if left > 0.0 => {
                // Shown rounded up: 5:00 at the start, 0:00 only at zero.
                let text = span_text(whole_secs(left.ceil()), self.format);
                let warn = self.warn_secs > 0 && left <= f64::from(self.warn_secs);
                (
                    text,
                    if warn {
                        TimerState::Warn
                    } else {
                        TimerState::Normal
                    },
                )
            }
            (_, Some(left)) => {
                let over = whole_secs((-left).floor());
                let text = if self.at_zero == AtZero::CountOver && over > 0 {
                    format!("-{}", span_text(over, self.format))
                } else {
                    span_text(0, self.format)
                };
                (text, TimerState::Over)
            }
        };
        Reading {
            text,
            state,
            remaining,
        }
    }

    /// The colour for `state`.
    pub fn color(&self, state: TimerState) -> [u8; 3] {
        match state {
            TimerState::Normal => self.look.color,
            TimerState::Warn => self.look.warn_color,
            TimerState::Over => self.look.over_color,
        }
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn whole_secs(secs: f64) -> u64 {
    secs.max(0.0) as u64
}

/// Seconds from `now` to `target`, both times of day. A time already past
/// is over time for six hours, then tomorrow's.
fn until(target: f64, now: f64) -> f64 {
    let day = f64::from(DAY_SECS);
    let left = target - now;
    if left < -6.0 * 3600.0 {
        left + day
    } else {
        left
    }
}

/// `secs` written as `format` asks.
pub fn span_text(secs: u64, format: TimeFormat) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    match format {
        TimeFormat::Auto if h > 0 => format!("{h}:{m:02}:{s:02}"),
        TimeFormat::Auto => format!("{m}:{s:02}"),
        TimeFormat::Hours => format!("{h:02}:{m:02}:{s:02}"),
        TimeFormat::Minutes => format!("{:02}:{s:02}", secs / 60),
    }
}

/// A time of day, `secs` after midnight.
pub fn clock_text(secs: f64, h24: bool, seconds: bool) -> String {
    let secs = whole_secs(secs.floor()) % u64::from(DAY_SECS);
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    let tail = if seconds {
        format!(":{s:02}")
    } else {
        String::new()
    };
    if h24 {
        format!("{h:02}:{m:02}{tail}")
    } else {
        let (h12, half) = match h {
            0 => (12, "AM"),
            1..=11 => (h, "AM"),
            12 => (12, "PM"),
            _ => (h - 12, "PM"),
        };
        format!("{h12}:{m:02}{tail} {half}")
    }
}

/// A time of day as 09:30 or 09:30:15, for editing a target.
pub fn target_text(secs: u32) -> String {
    let secs = secs % DAY_SECS;
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if s == 0 {
        format!("{h:02}:{m:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

/// "9:30", "09:30:15" or "930" as seconds after midnight.
pub fn parse_time_of_day(text: &str) -> Option<u32> {
    let text = text.trim();
    let parts: Vec<&str> = if text.contains(':') || text.contains('.') {
        text.split([':', '.']).collect()
    } else if text.len() > 2 && text.chars().all(|c| c.is_ascii_digit()) {
        let (h, m) = text.split_at(text.len() - 2);
        vec![h, m]
    } else {
        vec![text]
    };
    let number = |s: &str| s.trim().parse::<u32>().ok();
    let (h, m, s) = match parts.as_slice() {
        [h] => (number(h)?, 0, 0),
        [h, m] => (number(h)?, number(m)?, 0),
        [h, m, s] => (number(h)?, number(m)?, number(s)?),
        _ => return None,
    };
    (h < 24 && m < 60 && s < 60).then_some(h * 3600 + m * 60 + s)
}

/// "5", "5:00", "1:30:00" — minutes, minutes and seconds, or hours too — as
/// seconds, for editing a count down's length.
pub fn parse_span(text: &str) -> Option<u32> {
    let number = |s: &str| s.trim().parse::<u32>().ok();
    let secs = match text.trim().split(':').collect::<Vec<_>>().as_slice() {
        [m] => number(m)?.checked_mul(60)?,
        [m, s] if number(s)? < 60 => number(m)?.checked_mul(60)?.checked_add(number(s)?)?,
        [h, m, s] if number(m)? < 60 && number(s)? < 60 => {
            number(h)? * 3600 + number(m)? * 60 + number(s)?
        }
        _ => return None,
    };
    (secs <= MAX_DURATION_SECS).then_some(secs)
}

/// The number a timer's control panel goes by in a screen layout, from its
/// layer's id: the same each time the show opens, and another for a copy.
pub fn panel_of(deck_uuid: &str) -> u64 {
    // FNV-1a: the same in every build, as std's hasher is not promised to be.
    deck_uuid.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// A timer's state, for the UI and the API.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct TimerSnapshot {
    pub settings: TimerSettings,
    /// Counting — a count down or up started and not paused. The time of
    /// day and a count to a time always run.
    pub running: bool,
    /// What it shows now.
    pub text: String,
    pub state: TimerState,
    /// Seconds left before zero, for a count down; below zero over time.
    pub remaining: Option<f64>,
    /// How long a count down or up has run, in seconds.
    pub elapsed: f64,
    /// Its control panel's number, which the screen layout keeps.
    pub panel: u64,
}

/// Starting, pausing and resetting a timer.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimerAction {
    /// Count on from where it is; a count down at zero starts again from
    /// the top.
    Start,
    Pause,
    /// Back to the top (a count down's length, a count up's zero), paused.
    Reset,
    /// Put seconds on what it shows (take them off, below zero): a count
    /// down gets more time left, a count up more time run.
    Adjust { secs: i64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn countdown(secs: u32) -> TimerSettings {
        TimerSettings {
            duration_secs: secs,
            ..TimerSettings::default()
        }
    }

    #[test]
    fn a_count_down_starts_on_its_length_and_ends_on_zero() {
        let t = countdown(300);
        assert_eq!(t.reading(0.0, 0.0).text, "5:00");
        assert_eq!(t.reading(0.2, 0.0).text, "5:00", "rounded up");
        assert_eq!(t.reading(1.0, 0.0).text, "4:59");
        assert_eq!(t.reading(299.5, 0.0).text, "0:01");
        let end = t.reading(300.0, 0.0);
        assert_eq!((end.text.as_str(), end.state), ("0:00", TimerState::Over));
        assert_eq!(t.reading(330.0, 0.0).text, "0:00", "stops on zero");
    }

    #[test]
    fn over_time_counts_on_with_a_minus() {
        let t = TimerSettings {
            at_zero: AtZero::CountOver,
            ..countdown(60)
        };
        assert_eq!(t.reading(60.5, 0.0).text, "0:00");
        assert_eq!(t.reading(72.0, 0.0).text, "-0:12");
        assert_eq!(t.reading(72.0, 0.0).state, TimerState::Over);
    }

    #[test]
    fn the_last_minute_warns() {
        let t = countdown(300);
        assert_eq!(t.reading(239.0, 0.0).state, TimerState::Normal);
        assert_eq!(t.reading(240.0, 0.0).state, TimerState::Warn);
        let never = TimerSettings {
            warn_secs: 0,
            ..countdown(300)
        };
        assert_eq!(never.reading(299.0, 0.0).state, TimerState::Normal);
    }

    #[test]
    fn lengths_are_written_as_asked() {
        assert_eq!(span_text(3 * 3600 + 4 * 60 + 5, TimeFormat::Auto), "3:04:05");
        assert_eq!(span_text(65, TimeFormat::Auto), "1:05");
        assert_eq!(span_text(65, TimeFormat::Hours), "00:01:05");
        assert_eq!(span_text(65, TimeFormat::Minutes), "01:05");
        assert_eq!(span_text(3700, TimeFormat::Minutes), "61:40");
    }

    #[test]
    fn counting_to_a_time_counts_to_today_or_tomorrow() {
        let t = TimerSettings {
            mode: TimerMode::CountTo,
            target_secs: 14 * 3600,
            ..TimerSettings::default()
        };
        // 13:55:00 → five minutes.
        assert_eq!(t.reading(0.0, 13.0 * 3600.0 + 55.0 * 60.0).text, "5:00");
        // 14:02 → two minutes over.
        let late = t.reading(0.0, 14.0 * 3600.0 + 120.0);
        assert_eq!((late.text.as_str(), late.state), ("0:00", TimerState::Over));
        // 22:00 → tomorrow's, sixteen hours on.
        assert_eq!(t.reading(0.0, 22.0 * 3600.0).text, "16:00:00");
    }

    #[test]
    fn counting_up_and_the_time_of_day() {
        let up = TimerSettings {
            mode: TimerMode::CountUp,
            ..TimerSettings::default()
        };
        assert_eq!(up.reading(61.9, 0.0).text, "1:01");
        let clock = TimerSettings {
            mode: TimerMode::Clock,
            ..TimerSettings::default()
        };
        let at = 14.0 * 3600.0 + 5.0 * 60.0 + 9.4;
        assert_eq!(clock.reading(0.0, at).text, "14:05:09");
        assert_eq!(clock_text(at, false, false), "2:05 PM");
        assert_eq!(clock_text(0.0, false, true), "12:00:00 AM");
    }

    #[test]
    fn times_and_lengths_are_read_as_typed() {
        assert_eq!(parse_time_of_day("9:30"), Some(9 * 3600 + 30 * 60));
        assert_eq!(parse_time_of_day("0930"), Some(9 * 3600 + 30 * 60));
        assert_eq!(parse_time_of_day("14.05.10"), Some(14 * 3600 + 5 * 60 + 10));
        assert_eq!(parse_time_of_day("25:00"), None);
        assert_eq!(target_text(9 * 3600 + 30 * 60), "09:30");
        assert_eq!(parse_span("5"), Some(300));
        assert_eq!(parse_span("2:30"), Some(150));
        assert_eq!(parse_span("1:00:00"), Some(3600));
        assert_eq!(parse_span("2:75"), None);
        assert_eq!(parse_span("x"), None);
    }

    #[test]
    fn a_box_is_placed_inside_the_frame() {
        let frame = (1920.0, 1080.0);
        assert_eq!(Anchor::TopLeft.place((200.0, 100.0), frame, 40.0), (40.0, 40.0));
        assert_eq!(
            Anchor::BottomRight.place((200.0, 100.0), frame, 40.0),
            (1680.0, 940.0)
        );
        assert_eq!(Anchor::Center.place((200.0, 100.0), frame, 40.0), (860.0, 490.0));
    }

    #[test]
    fn settings_are_kept_in_bounds_and_fill_in_when_missing() {
        let wild = TimerSettings {
            duration_secs: u32::MAX,
            look: TimerLook {
                size: f32::NAN,
                box_opacity: 7.0,
                font: " ".into(),
                ..TimerLook::default()
            },
            ..TimerSettings::default()
        }
        .clamped();
        assert_eq!(wild.duration_secs, MAX_DURATION_SECS);
        assert!((wild.look.size - default_size()).abs() < f32::EPSILON);
        assert!((wild.look.box_opacity - 1.0).abs() < f32::EPSILON);
        assert_eq!(wild.look.font, default_font());
        let read: TimerSettings = serde_json::from_str(r#"{"mode":"clock"}"#).unwrap();
        assert_eq!(read.mode, TimerMode::Clock);
        assert_eq!(read.duration_secs, 300);
        assert!(read.look.bold);
    }
}
