//! PowerPoint as an input: a deck played by PowerPoint itself, in a window
//! Nida captures, moved on from Nida — its transitions, animations and
//! videos as PowerPoint plays them. See docs/26-presentations.md.

#[cfg(any(test, feature = "test-fixtures"))]
pub mod fake;
pub mod manager;
pub mod powerpoint;
pub mod pptx;
pub mod screens;

/// A PowerPoint input's deck file, kept with the show on its capture deck.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct PresentationConfig {
    /// The deck's file, absolute.
    pub path: String,
    /// The slide it was on when the show was saved (PowerPoint's numbering,
    /// from 1): opened again, it goes back there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slide: Option<u32>,
    /// Which screen its show plays on.
    #[serde(
        default,
        skip_serializing_if = "crate::engine::value::presentation::PlaysOn::is_auto"
    )]
    pub plays_on: crate::engine::value::presentation::PlaysOn,
}
