//! Video playback value types.

/// Loop mode for video playback.
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
pub enum LoopMode {
    /// Standard loop — restart from in-point when reaching out-point.
    #[default]
    Loop,
    /// Play forward then reverse repeatedly.
    PingPong,
    /// Play once and stop at the out-point.
    OneShot,
    /// Play once and hold the last frame.
    HoldLast,
}

/// What a clip does by itself as it goes on air and comes off — vMix's Auto
/// Play, Auto Pause and Auto Restart. On air is on Main or on any bus's
/// Program (on Main only with [`AutoPlayback::main_only`]), as a layer of
/// what is there too.
// Four independent switches, each a checkbox; not a state machine.
#[allow(clippy::struct_excessive_bools)]
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub struct AutoPlayback {
    /// Start playing when it goes on air.
    #[serde(default = "yes")]
    pub play: bool,
    /// Pause when it comes off air.
    #[serde(default = "yes")]
    pub pause: bool,
    /// Go back to the start (the in-point) when it comes off air, ready for
    /// next time.
    #[serde(default = "yes")]
    pub restart: bool,
    /// Only Main counts as on air: on a bus the clip neither starts nor
    /// stops by itself — for one played on the LED but not on the stream.
    #[serde(default)]
    pub main_only: bool,
}

const fn yes() -> bool {
    true
}

impl Default for AutoPlayback {
    /// All three on, as vMix sets a new video input.
    fn default() -> Self {
        Self {
            play: true,
            pause: true,
            restart: true,
            main_only: false,
        }
    }
}

/// Whether a video deck maps its playhead onto the show transport.
///
/// See /spec/timecode.md § Consumer 2.
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
pub enum TransportSyncMode {
    /// Chase while the transport is running; free-run (with loop modes) when it is not.
    #[default]
    Auto,
    /// Chase even while the transport is stopped: freeze on the mapped frame.
    Always,
    /// Never chase.
    Never,
}

impl TransportSyncMode {
    /// Whether this mode chases given the transport's running flag.
    #[must_use]
    pub const fn is_chasing(self, transport_running: bool) -> bool {
        match self {
            Self::Auto => transport_running,
            Self::Always => true,
            Self::Never => false,
        }
    }

    /// Short label for the deck-detail combo.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto",
            Self::Always => "Always",
            Self::Never => "Never",
        }
    }
}

/// Per-clip mapping of a video deck onto the show transport.
///
/// `offset` is independent of arrangement regions. `delay_frames` is in
/// transport displayed frames, not clip frames.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct DeckTransportSync {
    #[serde(default)]
    pub mode: TransportSyncMode,
    /// Transport seconds at which this clip's in-point sits.
    #[serde(default)]
    pub offset: f64,
    /// Signed latency in transport displayed frames.
    #[serde(default)]
    pub delay_frames: i32,
}

impl Default for DeckTransportSync {
    fn default() -> Self {
        Self {
            mode: TransportSyncMode::Auto,
            offset: 0.0,
            delay_frames: 0,
        }
    }
}
