//! Text input value types: lines of text typed in, and how they look — font,
//! size, colour, alignment, place, a box behind them, an outline and a
//! shadow — and whether they show, faded in and out.
//!
//! The layer that draws them is [`crate::deck::text`].

use super::timer::Anchor;

/// How the lines line up with each other.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    #[default]
    Centre,
    Right,
}

impl TextAlign {
    pub const ALL: [Self; 3] = [Self::Left, Self::Centre, Self::Right];

    pub fn label(self) -> &'static str {
        match self {
            Self::Left => "Left",
            Self::Centre => "Centre",
            Self::Right => "Right",
        }
    }
}

/// How the text looks. Colours are sRGB.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct TextLook {
    /// A font family installed on the computer; empty, or one not found,
    /// is the system's sans serif.
    #[serde(default)]
    pub font: String,
    #[serde(default = "yes")]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default = "white")]
    pub colour: [u8; 3],
    /// The font's size, as a share of the frame's height.
    #[serde(default = "default_size")]
    pub size: f32,
    /// Line to line, as a share of the size.
    #[serde(default = "default_spacing")]
    pub spacing: f32,
    #[serde(default)]
    pub align: TextAlign,
    #[serde(default)]
    pub anchor: Anchor,
    /// Room kept from the frame's edges, as a share of its height.
    #[serde(default = "default_margin")]
    pub margin: f32,
    /// The box behind the text: none at 0 opacity.
    #[serde(default)]
    pub box_colour: [u8; 3],
    #[serde(default)]
    pub box_opacity: f32,
    /// Its corners' rounding and the room inside it, as shares of the size.
    #[serde(default = "default_radius")]
    pub box_radius: f32,
    #[serde(default = "default_padding")]
    pub box_padding: f32,
    /// An outline round the letters, as a share of the size: none at 0.
    #[serde(default)]
    pub outline: f32,
    #[serde(default)]
    pub outline_colour: [u8; 3],
    /// A soft shadow under the letters: none at 0 opacity.
    #[serde(default = "default_shadow")]
    pub shadow_opacity: f32,
}

fn yes() -> bool {
    true
}
fn white() -> [u8; 3] {
    [255, 255, 255]
}
fn default_size() -> f32 {
    0.07
}
fn default_spacing() -> f32 {
    1.2
}
fn default_margin() -> f32 {
    0.06
}
fn default_radius() -> f32 {
    0.25
}
fn default_padding() -> f32 {
    0.45
}
fn default_shadow() -> f32 {
    0.5
}

impl Default for TextLook {
    fn default() -> Self {
        Self {
            font: String::new(),
            bold: true,
            italic: false,
            colour: white(),
            size: default_size(),
            spacing: default_spacing(),
            align: TextAlign::default(),
            anchor: Anchor::default(),
            margin: default_margin(),
            box_colour: [0, 0, 0],
            box_opacity: 0.0,
            box_radius: default_radius(),
            box_padding: default_padding(),
            outline: 0.0,
            outline_colour: [0, 0, 0],
            shadow_opacity: default_shadow(),
        }
    }
}

/// The smallest and largest sizes, as shares of the frame's height.
pub const MIN_SIZE: f32 = 0.015;
pub const MAX_SIZE: f32 = 0.4;

/// A text input: its lines, how they look, and whether they show.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct TextSettings {
    /// The text, a line a line.
    #[serde(default = "default_text")]
    pub text: String,
    #[serde(default)]
    pub look: TextLook,
    /// Shown, or hidden (the input clear); a change fades over `fade_secs`.
    #[serde(default = "yes")]
    pub visible: bool,
    #[serde(default = "default_fade")]
    pub fade_secs: f32,
    /// The buses (by UUID) it is shown on too, over whatever each shows, in
    /// the part each one's screens show (an LED wall's band), laid out for
    /// that part. Main as ever: the input on Program, or an overlay.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
}

fn default_text() -> String {
    "Text".to_string()
}
fn default_fade() -> f32 {
    0.4
}

impl Default for TextSettings {
    fn default() -> Self {
        Self {
            text: default_text(),
            look: TextLook::default(),
            visible: true,
            fade_secs: default_fade(),
            targets: Vec::new(),
        }
    }
}

fn finite_or(v: f32, or: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        or
    }
}

impl TextSettings {
    /// In range: sizes, shares and times sensible, the text not huge.
    #[must_use]
    pub fn normalized(mut self) -> Self {
        const MOST_CHARS: usize = 4000;
        if self.text.chars().count() > MOST_CHARS {
            self.text = self.text.chars().take(MOST_CHARS).collect();
        }
        let look = &mut self.look;
        look.size = finite_or(look.size, default_size()).clamp(MIN_SIZE, MAX_SIZE);
        look.spacing = finite_or(look.spacing, default_spacing()).clamp(0.8, 3.0);
        look.margin = finite_or(look.margin, default_margin()).clamp(0.0, 0.4);
        look.box_opacity = finite_or(look.box_opacity, 0.0).clamp(0.0, 1.0);
        look.box_radius = finite_or(look.box_radius, default_radius()).clamp(0.0, 2.0);
        look.box_padding = finite_or(look.box_padding, default_padding()).clamp(0.0, 3.0);
        look.outline = finite_or(look.outline, 0.0).clamp(0.0, 0.2);
        look.shadow_opacity = finite_or(look.shadow_opacity, default_shadow()).clamp(0.0, 1.0);
        self.fade_secs = finite_or(self.fade_secs, default_fade()).clamp(0.0, 5.0);
        self
    }
}

/// What a text input is doing, as the panel and the API show it.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
pub struct TextSnapshot {
    pub settings: TextSettings,
    /// A current drawing is still being prepared for this input or a target bus.
    pub pending: bool,
    /// The latest drawing failed; any previous picture is retained.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Out-of-range and non-numbers come back sensible; a saved show without
    /// the newer fields reads with their defaults.
    #[test]
    fn settings_normalise_and_read_with_defaults() {
        let mut s = TextSettings::default();
        s.look.size = f32::NAN;
        s.look.outline = 5.0;
        s.fade_secs = -1.0;
        let s = s.normalized();
        assert!((s.look.size - default_size()).abs() < 1e-6);
        assert!(s.look.outline <= 0.2);
        assert!(s.fade_secs >= 0.0);
        let read: TextSettings = serde_json::from_str(r#"{"text":"Pertrauka"}"#).expect("read");
        assert_eq!(read.text, "Pertrauka");
        assert!(read.visible);
        assert_eq!(read.look, TextLook::default());
    }
}
