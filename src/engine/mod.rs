//! Engine layer — domain contracts (traits + types).
//!
//! This module defines the public API for the Varda engine.
//! NO implementation, NO GPU types. Pure contracts.
//!
//! Consumers (UI, HTTP API, CLI) program against these traits.
//! The concrete implementation lives in `src/app/`.

pub mod traits;
pub mod types;
pub mod value;

pub use traits::*;
pub use types::*;

/// Result of processing an `EngineCommand`. Sent back to the caller
/// via the optional `oneshot::Sender` in the command envelope.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub enum CommandResult {
    /// Command succeeded with no additional data.
    Ok,
    /// Command succeeded and created an entity with the given UUID.
    OkWithId { uuid: String },
    /// Command succeeded with additional data payload.
    OkWithData { data: serde_json::Value },
    /// Command failed.
    Err { code: ErrorCode, message: String },
}

/// Typed, in-process result of executing a command through the GUI drain.
///
/// Distinct from [`CommandResult`] (the serializable HTTP/WS wire type): the
/// windowed consumer needs same-frame, strongly-typed data to complete a
/// mutation (register a preview texture by UUID, refresh state after undo).
/// The bus consumers never see this — they get `CommandResult` over the
/// oneshot reply. See [`/spec/ui-engine-boundary.md`] Decision #9.
#[derive(Debug, Clone)]
pub enum CommandOutcome {
    /// No GUI side-channel data; carries the wire result verbatim.
    Plain(CommandResult),
    /// One or more decks were created. The GUI registers a preview texture for
    /// each UUID. Mirrors `OkWithId` for the single-deck case.
    DecksCreated { uuids: Vec<String> },
    /// Undo/redo restored engine state. `structural_changed` tells the GUI to
    /// re-register all preview textures; `dome_layout` carries the UI-local
    /// dome flags to sync back into layout state.
    HistoryRestored {
        structural_changed: bool,
        dome_layout: DomeLayoutFields,
    },
}

/// Dome layout flags that live in UI layout state (not engine state) and must
/// be synced back after an undo/redo restore.
#[derive(Debug, Clone, Copy)]
pub struct DomeLayoutFields {
    pub dome_mode_active: bool,
    pub dome_preset: crate::engine::value::dome::DomePreset,
    pub dome_geometry: crate::engine::value::dome::DomeGeometry,
}

/// What a new input shows, for [`EngineCommand::AddInput`]: a live source,
/// as vMix's Add Input window offers them. Files come in through the file
/// dialog instead, a new input each.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputSource {
    /// A camera or capture card, by id from `GET /api/library/cameras`, at
    /// one of its formats (`GET /api/state/cameras`) or its best.
    Camera {
        camera_id: CameraId,
        #[serde(default)]
        mode: Option<crate::engine::value::camera::CameraMode>,
    },
    /// An NDI source on the network, by its full name.
    Ndi {
        source_name: String,
    },
    /// A display or window.
    ScreenCapture {
        /// `{"kind":"display","name":…}` or `{"kind":"window","app":…,"title":…}`.
        #[schema(value_type = Object)]
        target: crate::scene::CaptureTargetConfig,
        #[serde(default)]
        rate: Option<f32>,
        #[serde(default)]
        show_cursor: Option<bool>,
    },
    /// A solid colour, straight RGBA 0 to 1.
    Colour {
        color: [f32; 4],
    },
    Srt {
        url: String,
        mode: crate::stream::SrtMode,
    },
    Rtmp {
        url: String,
        mode: crate::stream::RtmpMode,
    },
    Hls {
        url: String,
    },
    /// A web page.
    Html {
        url: String,
    },
    /// A generator from the shader library (`GET /api/library/generators`),
    /// by name.
    Generator {
        shader_name: String,
    },
    /// Another input's picture, by its channel's UUID: as a layer, that input
    /// inside this one; as a new input, a copy of it that can be sized and
    /// placed on its own (vMix's virtual input). Drawn once for both.
    Input {
        channel_uuid: String,
    },
    /// Clips, sound files and pictures played one after another, as vMix's
    /// List input.
    Playlist {
        #[serde(default)]
        items: Vec<crate::engine::value::playlist::PlaylistItem>,
    },
    /// A count down, count to a time, count up or the time of day, drawn
    /// as text over clear.
    Timer {
        #[serde(default)]
        settings: Option<crate::engine::value::timer::TimerSettings>,
    },
    /// A PowerPoint deck, played by PowerPoint on this computer in a window
    /// Nida captures — its transitions, animations and videos as PowerPoint
    /// plays them — and moved on from Nida (macOS).
    #[serde(rename = "powerpoint")]
    PowerPoint {
        /// The deck's file, absolute.
        path: String,
    },
    /// A presenter view of a PowerPoint input, for the speaker's screen: its
    /// slide, live, with the speaker notes, what comes next and the talk's
    /// clocks. `deck_uuid` is the PowerPoint input's layer.
    PresenterView {
        deck_uuid: String,
    },
    /// A Slides input: a conference's presentations — PowerPoint decks,
    /// PDFs, web presentations, clips, pictures — in running order, one on
    /// at a time, each started with one press. `paths` are its talks' files.
    Slides {
        #[serde(default)]
        paths: Vec<String>,
    },
}

impl InputSource {
    /// The command that adds this source to a channel as a layer.
    pub fn deck_command(&self, channel_uuid: &str) -> EngineCommand {
        let channel_uuid = channel_uuid.to_string();
        match self.clone() {
            Self::Camera { camera_id, mode } => EngineCommand::AddCameraDeck {
                channel_uuid,
                camera_id,
                mode,
            },
            Self::Ndi { source_name } => EngineCommand::AddNdiDeck {
                channel_uuid,
                source_name,
            },
            Self::ScreenCapture {
                target,
                rate,
                show_cursor,
            } => EngineCommand::AddScreenCaptureDeck {
                channel_uuid,
                target,
                rate,
                crop: None,
                show_cursor,
                exclude_varda: None,
            },
            Self::Colour { color } => EngineCommand::AddSolidColorDeck {
                channel_uuid,
                color,
            },
            Self::Srt { url, mode } => EngineCommand::AddSrtDeck {
                channel_uuid,
                url,
                mode,
            },
            Self::Rtmp { url, mode } => EngineCommand::AddRtmpDeck {
                channel_uuid,
                url,
                mode,
            },
            Self::Hls { url } => EngineCommand::AddHlsDeck { channel_uuid, url },
            Self::Html { url } => EngineCommand::AddHtmlDeck { channel_uuid, url },
            Self::Generator { shader_name } => EngineCommand::AddDeck {
                channel_uuid,
                shader_name,
            },
            Self::Input {
                channel_uuid: shown,
            } => EngineCommand::AddTapDeck {
                channel_uuid,
                source: crate::scene::TapSourceConfig::Channel { uuid: shown },
            },
            Self::Playlist { items } => EngineCommand::AddPlaylistDeck {
                channel_uuid,
                items,
                settings: None,
            },
            Self::Timer { settings } => EngineCommand::AddTimerDeck {
                channel_uuid,
                settings,
            },
            Self::PowerPoint { path } => EngineCommand::AddPresentationDeck { channel_uuid, path },
            Self::PresenterView { deck_uuid } => EngineCommand::AddPresenterDeck {
                channel_uuid,
                deck_uuid,
            },
            Self::Slides { paths } => EngineCommand::AddSlidesDeck {
                channel_uuid,
                paths,
                settings: None,
            },
        }
    }
}

/// A new input's own sound: a sound device, or two of its channels, heard
/// as the input goes on air — a camera or capture card's sound, vMix's
/// "Audio" when adding a camera.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct InputAudio {
    /// Sound device, by name (`GET /api/audio-mix`'s `input_devices`).
    pub device: String,
    /// Device channel for the left side (0-based).
    #[serde(default)]
    pub left: u16,
    /// Device channel for the right side; the same as `left` for mono.
    #[serde(default = "second_channel")]
    pub right: u16,
}

fn second_channel() -> u16 {
    1
}

fn yes() -> bool {
    true
}

/// Error codes for command failures.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
pub enum ErrorCode {
    NotFound,
    InvalidInput,
    InternalError,
    Unavailable,
}

/// A command envelope: the command itself plus an optional reply channel.
/// UI consumers send `None` (fire-and-forget). HTTP API sends `Some(tx)`.
pub type CommandEnvelope = (
    EngineCommand,
    Option<tokio::sync::oneshot::Sender<CommandResult>>,
);

/// Cross-thread command envelope for message-passing consumers.
///
/// Each variant mirrors a trait method 1:1. Cross-thread consumers
/// (HTTP API, CLI) send these via `mpsc::Sender<EngineCommand>`.
/// The engine processes them once per frame.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub enum EngineCommand {
    // ── Mixer ──────────────────────────────────────────────────
    SetCrossfader(f32),
    SetTonemapMode(crate::engine::value::render::TonemapMode),
    LoadLut {
        filename: String,
    },
    UnloadLut,
    AutoCrossfade {
        target: f32,
        duration_secs: f32,
        easing: CrossfadeEasing,
    },
    BeatCrossfade {
        target: f32,
        beats: f32,
    },
    AddDeck {
        channel_uuid: String,
        shader_name: String,
    },
    AddImageDeck {
        channel_uuid: String,
        path: std::path::PathBuf,
    },
    AddVideoDeck {
        channel_uuid: String,
        path: std::path::PathBuf,
    },
    AddSolidColorDeck {
        channel_uuid: String,
        color: [f32; 4],
    },
    AddCameraDeck {
        channel_uuid: String,
        camera_id: CameraId,
        /// The size and rate to open it at; its best when left out.
        #[serde(default)]
        mode: Option<crate::engine::value::camera::CameraMode>,
    },
    AddDepthSensorDeck {
        channel_uuid: String,
        depth_sensor_id: DepthSensorId,
    },
    /// Add a screen / window capture deck. The target is named, not handled, so
    /// the same payload works from the UI, HTTP, and a restored scene.
    AddScreenCaptureDeck {
        channel_uuid: String,
        target: crate::scene::CaptureTargetConfig,
        #[serde(default)]
        rate: Option<f32>,
        #[serde(default)]
        crop: Option<crate::scene::CaptureCropConfig>,
        #[serde(default)]
        show_cursor: Option<bool>,
        #[serde(default)]
        exclude_varda: Option<bool>,
    },
    /// Add a deck that re-enters Varda's own output. See spec/program-tap.md.
    AddTapDeck {
        channel_uuid: String,
        source: crate::scene::TapSourceConfig,
    },
    /// Repoint an existing tap deck at a different source.
    SetTapSource {
        deck_uuid: String,
        source: crate::scene::TapSourceConfig,
    },
    RemoveDeck {
        deck_uuid: String,
    },
    /// Move a deck into a channel (its own included), before
    /// `before_deck_uuid` there, or at the end when that is `None`.
    MoveDeck {
        deck_uuid: String,
        dst_channel_uuid: String,
        #[serde(default)]
        before_deck_uuid: Option<String>,
    },
    /// Reposition a deck within its channel. `from_idx`/`to_idx` are ordinals,
    /// not addresses — the position is the payload. See `/spec/api-addressing.md`.
    ReorderDeck {
        channel_uuid: String,
        from_idx: usize,
        to_idx: usize,
    },
    SetDeckOpacity {
        deck_uuid: String,
        opacity: f32,
    },
    SetDeckBlendMode {
        deck_uuid: String,
        mode: BlendMode,
    },
    SetDeckSolo {
        deck_uuid: String,
        solo: bool,
    },
    SetDeckMute {
        deck_uuid: String,
        mute: bool,
    },
    SetDeckRenderFps {
        deck_uuid: String,
        render_fps: DeckRenderFps,
    },
    /// Render a deck at a fraction of the master resolution — the way to make
    /// a heavy generator affordable at 4K.
    SetDeckRenderScale {
        deck_uuid: String,
        scale: crate::engine::types::DeckRenderScale,
    },
    SetDeckScalingMode {
        deck_uuid: String,
        mode: ScalingMode,
    },
    SetDeckTransparent {
        deck_uuid: String,
        transparent: bool,
    },
    /// A colour layer's colour, straight RGBA 0 to 1. The layer keeps a name
    /// it was given; one still called after its colour follows it.
    SetDeckColor {
        deck_uuid: String,
        color: [f32; 4],
    },
    SetChannelOpacity {
        channel_uuid: String,
        opacity: f32,
    },
    SetChannelBlendMode {
        channel_uuid: String,
        mode: BlendMode,
    },
    AddChannel,
    /// A new input, as vMix's Add Input: a channel at the end of the row
    /// showing `source`, called `name` or after the source. Replies with
    /// the channel's UUID; nothing is left behind if the source fails.
    AddInput {
        source: InputSource,
        #[serde(default)]
        name: Option<String>,
        /// Its own sound, on and off air with it (Audio Auto).
        #[serde(default)]
        audio: Option<InputAudio>,
    },
    RemoveChannel {
        channel_uuid: String,
    },
    /// Call a channel something else.
    RenameChannel {
        channel_uuid: String,
        name: String,
    },
    /// Give a channel a colour of its own — red, green, blue — to find it
    /// at a glance on its tile and its switcher buttons; `None` gives it
    /// back the one its place gives.
    SetChannelColor {
        channel_uuid: String,
        #[serde(default)]
        color: Option<[u8; 3]>,
    },
    /// Move a channel to just before another, or to the end when
    /// `before_channel_uuid` is left out.
    MoveChannel {
        channel_uuid: String,
        #[serde(default)]
        before_channel_uuid: Option<String>,
    },
    AddEffect {
        target: EffectTarget,
        shader_name: String,
    },
    RemoveEffect {
        effect_uuid: String,
    },
    ToggleEffect {
        effect_uuid: String,
    },
    /// Reposition an effect within its chain. `target` scopes the ordinals; the
    /// indices are positions, not addresses.
    MoveEffect {
        target: EffectTarget,
        from_idx: usize,
        to_idx: usize,
    },

    // ── Clipboard (see /spec/clipboard.md) ───────────────────
    /// Capture an object's config onto the clipboard. Mutates nothing on stage,
    /// so it is not undoable.
    ///
    /// `include_arrangement` carries a deck's regions, which the UI sets when
    /// the copy was made on the timeline: in the mixer a deck is a source, and
    /// in Arrangement mode it is a source and a placement.
    Copy {
        source: ClipboardSource,
        #[serde(default)]
        include_arrangement: bool,
    },
    /// Rebuild what the clipboard holds, with a fresh identity throughout.
    Paste {
        target: PasteTarget,
    },
    /// Paste beside the original in one step, leaving the clipboard untouched.
    Duplicate {
        source: ClipboardSource,
    },
    SetTransition {
        shader_name: Option<String>,
    },
    SetParam {
        path: String,
        value: ParamValue,
    },
    /// Toggle a parameter between its two extremes by path (keyboard-shortcut
    /// affordance): crossfader 0↔1, opacity 0↔1, mute/solo flip, etc. The
    /// two-value logic lives in `param_router::toggle_param_by_path`.
    ToggleParam {
        path: String,
    },

    // ── Audio ──────────────────────────────────────────────────
    OpenAudioSource {
        source_id: AudioSourceId,
    },
    CloseAudioSource {
        source_id: AudioSourceId,
    },
    ScanAudioDevices,

    // ── Modulation ─────────────────────────────────────────────
    AddLfo {
        waveform: LFOWaveform,
        frequency: f32,
    },
    AddAudioBand {
        preset: AudioBandPreset,
        source_id: Option<AudioSourceId>,
    },
    AddAdsr {
        attack: f32,
        decay: f32,
        sustain: f32,
        release: f32,
    },
    AddStepSequencer {
        num_steps: usize,
        rate: f32,
    },
    /// Create an automation envelope and assign it to `target` in `Absolute`
    /// mode, which is the "Add automation lane" gesture.
    /// See /spec/automation.md.
    AddAutomationLane {
        target: String,
        /// Timebase the curve is drawn against. Arrangement-authored lanes use
        /// `Transport`.
        timebase: crate::timebase::Timebase,
    },
    /// Replace an envelope's breakpoints wholesale. The engine sorts them, so
    /// callers do not have to maintain the ordering invariant.
    SetEnvelopeBreakpoints {
        uuid: String,
        breakpoints: Vec<crate::modulation::Breakpoint>,
    },
    RemoveModulationSource {
        uuid: String,
    },
    AssignModulation {
        target: String,
        source_id: String,
        amount: f32,
    },
    ClearModulation {
        target: String,
    },
    ClearModulationSource {
        target: String,
        source_id: String,
    },

    // ── Video Playback ────────────────────────────────────────────
    VideoTogglePlay {
        deck_uuid: String,
    },
    VideoSeek {
        deck_uuid: String,
        position_secs: f64,
    },
    VideoSetSpeed {
        deck_uuid: String,
        speed: f64,
    },
    VideoSetLoopMode {
        deck_uuid: String,
        mode: crate::engine::value::video::LoopMode,
    },
    /// The clip's in-point, seconds; one at or past its out-point lets the
    /// out-point go (played to the end).
    VideoSetInPoint {
        deck_uuid: String,
        secs: f64,
    },
    /// The clip's out-point, seconds (0: none, played to the end); one at or
    /// before its in-point lets the in-point go (played from the start).
    VideoSetOutPoint {
        deck_uuid: String,
        secs: f64,
    },
    VideoClearInOutPoints {
        deck_uuid: String,
    },
    VideoSetTransportSync {
        deck_uuid: String,
        sync: crate::engine::value::video::DeckTransportSync,
    },
    /// What a clip does by itself going on and off air: Auto Play, Auto
    /// Pause, Auto Restart.
    VideoSetAutoPlayback {
        deck_uuid: String,
        auto: crate::engine::value::video::AutoPlayback,
    },

    // ── Playlists ─────────────────────────────────────────────────
    /// Add a playlist layer: clips, sound files and pictures shown one after
    /// another, as vMix's List input. It starts stopped on its first item.
    AddPlaylistDeck {
        channel_uuid: String,
        #[serde(default)]
        items: Vec<crate::engine::value::playlist::PlaylistItem>,
        #[serde(default)]
        settings: Option<crate::engine::value::playlist::PlaylistSettings>,
    },
    /// A playlist's items, all of them: added, taken out or moved. What is
    /// on screen stays there and the list plays on from it.
    SetPlaylistItems {
        deck_uuid: String,
        items: Vec<crate::engine::value::playlist::PlaylistItem>,
    },
    /// How a playlist moves on: its transition and its time, how long a
    /// picture stays, whether it loops.
    SetPlaylistSettings {
        deck_uuid: String,
        settings: crate::engine::value::playlist::PlaylistSettings,
    },
    /// Play or stop a playlist.
    SetPlaylistPlaying {
        deck_uuid: String,
        playing: bool,
    },
    /// Stop a clip or playlist as its Stop button does — on its tile and in
    /// the Audio Mixer: its sound faded out, over `secs` or else the Stop
    /// fade set (`SetStopFade`), then paused. A second Stop while it fades
    /// stops it at once; `secs` 0 cuts.
    StopWithFade {
        deck_uuid: String,
        #[serde(default)]
        secs: Option<f32>,
    },
    /// How long Stop fades a clip's or playlist's sound before pausing it,
    /// 0 (a cut) to 10 seconds.
    SetStopFade {
        secs: f32,
    },
    /// Show one item of a playlist now — with its transition while playing,
    /// at once while stopped — and play on from it.
    ShowPlaylistItem {
        deck_uuid: String,
        index: usize,
    },

    // ── Slides ───────────────────────────────────────────────────
    /// Add a Slides layer: `paths`' files as talks in running order,
    /// nothing on yet.
    AddSlidesDeck {
        channel_uuid: String,
        #[serde(default)]
        paths: Vec<String>,
        #[serde(default)]
        settings: Option<crate::engine::value::slides::SlidesSettings>,
    },
    /// A Slides layer's talks, all of them: added, taken out, moved,
    /// renamed, given another file. The talk on stays on.
    SetSlidesTalks {
        deck_uuid: String,
        talks: Vec<crate::engine::value::slides::Talk>,
    },
    /// Add the files at `paths` as talks: at place `at` in the list, else
    /// at its end. Without a `deck_uuid`, to the show's first Slides input,
    /// made on a new input when there is none.
    AddTalks {
        #[serde(default)]
        deck_uuid: Option<String>,
        paths: Vec<String>,
        #[serde(default)]
        at: Option<usize>,
    },
    /// One talk renamed (`""` for its file's name), or given another file
    /// — it keeps its place, its name and its id; the talk on plays its new
    /// file once started again. A field left out stays as it is.
    EditTalk {
        deck_uuid: String,
        talk_id: String,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    /// One talk moved to place `to` in the running order (from 0).
    MoveTalk {
        deck_uuid: String,
        talk_id: String,
        to: usize,
    },
    /// One talk taken out of the running order; the talk on stays on.
    RemoveTalk {
        deck_uuid: String,
        talk_id: String,
    },
    /// How a Slides input goes from one talk to the next, and where its
    /// PowerPoint shows play.
    SetSlidesSettings {
        deck_uuid: String,
        settings: crate::engine::value::slides::SlidesSettings,
    },
    /// Start a talk: `"next"`, `"previous"` or `{"id": "…"}`. What is on
    /// stays until its picture is there. Without a `deck_uuid`, the Slides
    /// input on air, else the one started last, else the first.
    StartTalk {
        #[serde(default)]
        deck_uuid: Option<String>,
        to: crate::engine::value::slides::TalkRef,
    },
    /// The talk Start next takes, picked by hand; `null` for the talk
    /// after the one on.
    SetNextTalk {
        deck_uuid: String,
        #[serde(default)]
        talk_id: Option<String>,
    },
    /// Look the talks' files over again — a Slides input's, or every one's
    /// — for what may go wrong in the show: a video a deck links to not
    /// there, fonts this Mac lacks, a talk of another shape, a page that
    /// needs the internet. A talk is looked over as it is added and when its
    /// file changes; this is for what changes no talk's file (a video copied
    /// beside its deck, a font installed). What is found is each talk's
    /// `warnings` in the state.
    CheckTalks {
        #[serde(default)]
        deck_uuid: Option<String>,
    },
    /// Have PowerPoint save the PowerPoint talks of a Slides input — or of
    /// every one — as slide pictures (a PDF each, in Nida's folder for them)
    /// for the presenter view's next slide: those without them as their
    /// files are now. Refused while a PowerPoint talk is on: the first time,
    /// PowerPoint asks for access to that folder (Grant Access) and holds
    /// its show till answered. What is made is each talk's `pictures` in the
    /// state.
    MakeSlidePictures {
        #[serde(default)]
        deck_uuid: Option<String>,
    },

    // ── Timers ───────────────────────────────────────────────────
    /// Add a timer layer: a count down, a count to a time of day, a count
    /// up or the time of day, drawn as text over clear. A count starts
    /// paused at its top.
    AddTimerDeck {
        channel_uuid: String,
        #[serde(default)]
        settings: Option<crate::engine::value::timer::TimerSettings>,
    },
    /// What a timer counts and how it looks; the time it has run stays.
    SetTimerSettings {
        deck_uuid: String,
        settings: crate::engine::value::timer::TimerSettings,
    },
    /// Start, pause, reset or put time on a count down or up.
    ControlTimer {
        deck_uuid: String,
        action: crate::engine::value::timer::TimerAction,
    },

    // ── PowerPoint ───────────────────────────────────────────────
    /// Add a PowerPoint input's layer: the deck at `path`, opened in
    /// PowerPoint and played in a window that is captured.
    AddPresentationDeck {
        channel_uuid: String,
        path: String,
    },
    /// Move a PowerPoint show on or back: `{"to": "next"}`, `"previous"`,
    /// `"first"`, `"last"` or `{"slide": 4}`. Without a `deck_uuid`, the
    /// show on air (on Main or a bus), else the one moved last.
    PresentationGo {
        #[serde(default)]
        deck_uuid: Option<String>,
        to: crate::engine::value::presentation::Go,
    },
    /// Play, pause or move the video on a PowerPoint show's slide on screen:
    /// `"toggle"`, `"play"`, `"pause"`, `"restart"` or `{"skip": 10}`
    /// (seconds; negative back). Without a `deck_uuid`, as for
    /// `PresentationGo`.
    PresentationVideo {
        #[serde(default)]
        deck_uuid: Option<String>,
        control: crate::engine::value::presentation::VideoControl,
    },
    /// Black or white a PowerPoint show's screen out (`"black"`, `"white"`),
    /// or show its slide again (`"slide"`). Without a `deck_uuid`, as for
    /// `PresentationGo`.
    SetPresentationScreen {
        #[serde(default)]
        deck_uuid: Option<String>,
        screen: crate::engine::value::presentation::Screen,
    },
    /// Black (or white) a PowerPoint show's screen out, or show its slide
    /// again when it already is: a clicker's black-screen button. Without a
    /// `deck_uuid`, as for `PresentationGo`.
    TogglePresentationScreen {
        #[serde(default)]
        deck_uuid: Option<String>,
        screen: crate::engine::value::presentation::Screen,
    },
    /// Play a PowerPoint show again where it was.
    RestartPresentation {
        deck_uuid: String,
    },
    /// How a presenter view (its own layer, `deck_uuid`) is laid out: its
    /// talk timer and time of day, on or off and at the `top` or `bottom`,
    /// what comes next on or off, and the sizes of slide, notes and the next
    /// line.
    SetPresenterLayout {
        deck_uuid: String,
        layout: crate::engine::value::presentation::PresenterLayout,
    },
    /// Which screen a PowerPoint input's show plays on: `"auto"` (where it
    /// plays sharpest — a spare screen when as sharp as this Mac's),
    /// `"main"` (this Mac's screen) or `{"screen": "<identity>"}` (one of
    /// `outputs.monitors`).
    SetPresentationPlaysOn {
        deck_uuid: String,
        plays_on: crate::engine::value::presentation::PlaysOn,
    },
    /// Add a presenter view layer showing the PowerPoint input whose layer
    /// is `deck_uuid`.
    AddPresenterDeck {
        channel_uuid: String,
        deck_uuid: String,
    },
    /// A presenter view's talk clock back to 0:00 (its own layer's UUID).
    RestartPresenterClock {
        deck_uuid: String,
    },

    // ── Deck Auto-Transitions ──────────────────────────────────
    SetAutoTransitionEnabled {
        deck_uuid: String,
        enabled: bool,
    },
    SetAutoTransitionTrigger {
        deck_uuid: String,
        clip_end: bool,
    },
    SetAutoTransitionPlayDuration {
        deck_uuid: String,
        value: f64,
        unit: crate::channel::DurationUnit,
    },
    SetAutoTransitionDuration {
        deck_uuid: String,
        value: f64,
        unit: crate::channel::DurationUnit,
    },
    SetAutoTransitionShader {
        deck_uuid: String,
        shader_name: Option<String>,
    },
    ToggleAutoTransitionPlayDurationUnit {
        deck_uuid: String,
    },
    ToggleAutoTransitionDurationUnit {
        deck_uuid: String,
    },
    SetAutoTransitionPlayDurationValue {
        deck_uuid: String,
        value: f64,
    },
    SetAutoTransitionDurationValue {
        deck_uuid: String,
        value: f64,
    },

    // ── External I/O Deck Sources ──────────────────────────────
    AddNdiDeck {
        channel_uuid: String,
        source_name: String,
    },
    AddSyphonDeck {
        channel_uuid: String,
        server_name: String,
    },
    AddSrtDeck {
        channel_uuid: String,
        url: String,
        mode: crate::stream::SrtMode,
    },
    AddHlsDeck {
        channel_uuid: String,
        url: String,
    },
    AddDashDeck {
        channel_uuid: String,
        url: String,
    },
    AddRtmpDeck {
        channel_uuid: String,
        url: String,
        mode: crate::stream::RtmpMode,
    },
    AddHtmlDeck {
        channel_uuid: String,
        url: String,
    },
    ReloadHtmlDeck {
        deck_uuid: String,
    },
    /// Open the interactive window for an HTML deck.
    OpenHtmlInteractive {
        deck_uuid: String,
    },
    /// Close the interactive HTML window (if any).
    CloseHtmlInteractive,

    // ── Transition Sequences ───────────────────────────────────
    CreateSequence,
    DeleteSequence {
        sequence_uuid: String,
    },
    PlaySequence {
        sequence_uuid: String,
    },
    StopSequence {
        sequence_uuid: String,
    },
    ToggleSequence {
        sequence_uuid: String,
    },
    // Steps are positional within their sequence: `step_idx` is an ordinal, not
    // an address. See `/spec/api-addressing.md`.
    AddFadeStep {
        sequence_uuid: String,
        from_channel_uuid: String,
        to_channel_uuid: String,
    },
    AddWaitStep {
        sequence_uuid: String,
    },
    AddGoToStep {
        sequence_uuid: String,
        step_index: usize,
    },
    RemoveStep {
        sequence_uuid: String,
        step_idx: usize,
    },
    SetStepDuration {
        sequence_uuid: String,
        step_idx: usize,
        value: f64,
        unit: crate::channel::DurationUnit,
    },
    SetStepEasing {
        sequence_uuid: String,
        step_idx: usize,
        easing: String,
    },
    SetStepTransitionShader {
        sequence_uuid: String,
        step_idx: usize,
        shader_name: Option<String>,
    },
    MoveStep {
        sequence_uuid: String,
        from: usize,
        to: usize,
    },
    SetStepDurationUnit {
        sequence_uuid: String,
        step_idx: usize,
        unit: crate::channel::DurationUnit,
    },
    SetStepFromCh {
        sequence_uuid: String,
        step_idx: usize,
        channel_uuid: String,
    },
    SetStepToCh {
        sequence_uuid: String,
        step_idx: usize,
        channel_uuid: String,
    },
    SetGoToTarget {
        sequence_uuid: String,
        step_idx: usize,
        target: usize,
    },
    ToggleStepDurationUnit {
        sequence_uuid: String,
        step_idx: usize,
    },
    SetStepDurationValue {
        sequence_uuid: String,
        step_idx: usize,
        value: f64,
    },
    SetStepTargetAmount {
        sequence_uuid: String,
        step_idx: usize,
        amount: f32,
    },

    // ── Stream Library ─────────────────────────────────────────
    AddStreamLibraryEntry {
        url: String,
        mode: crate::stream::SrtMode,
    },
    RemoveStreamLibraryEntry {
        url: String,
    },
    AddHlsLibraryEntry {
        url: String,
    },
    RemoveHlsLibraryEntry {
        url: String,
    },
    AddDashLibraryEntry {
        url: String,
    },
    RemoveDashLibraryEntry {
        url: String,
    },
    AddRtmpLibraryEntry {
        url: String,
        mode: crate::stream::RtmpMode,
    },
    RemoveRtmpLibraryEntry {
        url: String,
    },
    AddHtmlLibraryEntry {
        url: String,
    },
    RemoveHtmlLibraryEntry {
        url: String,
    },

    // ── Output ─────────────────────────────────────────────────
    CreateOutput,
    CreateHeadlessOutput {
        target: crate::engine::value::render::OutputTarget,
    },
    CloseOutput {
        output_uuid: String,
    },
    /// Close an output's window and make it again from its settings, in its
    /// place: what Nida does by itself when the window's screen stops showing
    /// its frames (see `renderer::on_screen::Unshown`).
    ReopenOutput {
        output_uuid: String,
    },
    SetOutputDisplay {
        output_uuid: String,
        monitor_name: String,
    },
    SetOutputTarget {
        output_uuid: String,
        target: crate::engine::value::render::OutputTarget,
    },
    StartOutput {
        output_uuid: String,
    },
    StopOutput {
        output_uuid: String,
    },
    /// Set the calibration display mode for an output (Off / Projector / Surfaces).
    SetCalibrationMode {
        output_uuid: String,
        mode: crate::engine::value::render::CalibrationMode,
    },
    /// Move one corner-pin corner of a surface's warp (per-surface).
    SetWarpCorner {
        surface_uuid: String,
        corner_idx: usize,
        position: [f32; 2],
    },
    /// Clear a surface's warp (back to no-warp / native position).
    ResetWarp {
        surface_uuid: String,
    },
    /// Set the warp grid resolution for a surface, converting its warp to a
    /// `cols` × `rows` mesh (preserving the current deformation). Dimensions ≥2.
    SetWarpSubdivisions {
        surface_uuid: String,
        cols: u32,
        rows: u32,
    },
    /// Move a single mesh grid point (row-major) of a surface's mesh warp.
    /// No-op if the surface's warp is not currently a mesh.
    SetWarpMeshPoint {
        surface_uuid: String,
        row: usize,
        col: usize,
        position: [f32; 2],
    },
    /// Bind or unbind a surface's warp from its shape (auto-warp). Binding
    /// re-derives the warp from the outline; unbinding materialises it for
    /// manual fine-tuning.
    SetWarpBound {
        surface_uuid: String,
        bound: bool,
    },
    /// Convert a surface's warp into a smooth bezier patch grid (8i.6), seeding
    /// the control cage from the current warp so the shape is preserved.
    ConvertWarpToBezier {
        surface_uuid: String,
    },
    /// Move a bezier-warp control anchor (row-major grid coords).
    MoveWarpAnchor {
        surface_uuid: String,
        row: usize,
        col: usize,
        position: [f32; 2],
    },
    /// Move a bezier-warp tangent handle. `horizontal` selects a horizontal edge
    /// (`(r,c)→(r,c+1)`) vs a vertical edge (`(r,c)→(r+1,c)`); `which` is 0/1.
    MoveWarpHandle {
        surface_uuid: String,
        horizontal: bool,
        row: usize,
        col: usize,
        which: usize,
        position: [f32; 2],
    },
    /// Set the bezier-warp control-cage resolution (anchor `cols` × `rows`).
    SetBezierCageSubdivisions {
        surface_uuid: String,
        cols: u32,
        rows: u32,
    },
    SetEdgeBlend {
        output_uuid: String,
        config: crate::engine::value::render::EdgeBlendConfig,
    },
    SetEdgeBlendMode {
        output_uuid: String,
        mode: crate::engine::value::render::EdgeBlendMode,
    },
    SetOutputRotation {
        output_uuid: String,
        rotation: crate::engine::value::render::OutputRotation,
    },
    SetOutputPresentation {
        output_uuid: String,
        request: crate::engine::value::render::PresentationRequest,
    },
    /// Route a headless output (recording, stream, NDI, Syphon) from the
    /// master or from one bus's program — `Master` or `BusProgram(uuid)`.
    /// Window outputs are routed through the switcher's screen picker instead.
    SetOutputSource {
        output_uuid: String,
        source: OutputSource,
    },

    // ── Surfaces ────────────────────────────────────────────────
    AddSurface {
        name: String,
        source: OutputSource,
    },
    AddPolygonSurface {
        name: String,
        vertices: Vec<[f32; 2]>,
        source: OutputSource,
    },
    AddCircleSurface {
        name: String,
        center: [f32; 2],
        radius: f32,
        sides: u32,
        aspect_ratio: f32,
        source: OutputSource,
    },
    RemoveSurface {
        uuid: String,
    },
    /// Change a surface's global stacking order (8i.12).
    ReorderSurface {
        uuid: String,
        op: SurfaceReorderOp,
    },
    SetSurfaceSource {
        uuid: String,
        source: OutputSource,
    },
    SetSurfaceOutputType {
        uuid: String,
        output_type: SurfaceOutputType,
    },
    SetSurfaceContentMapping {
        uuid: String,
        mapping: ContentMapping,
    },
    RenameSurface {
        uuid: String,
        name: String,
    },
    UpdateSurfaceVertices {
        uuid: String,
        vertices: Vec<[f32; 2]>,
    },
    DuplicateSurface {
        uuid: String,
    },
    FlipSurfaceHorizontal {
        uuid: String,
    },
    FlipSurfaceVertical {
        uuid: String,
    },
    InsertSurfaceVertex {
        uuid: String,
        after_vert_idx: usize,
        position: [f32; 2],
    },
    SetCircleRadius {
        uuid: String,
        radius: f32,
    },
    SetCircleSides {
        uuid: String,
        sides: u32,
    },
    ConvertSurfaceToPolygon {
        uuid: String,
    },
    CombineSurfaces {
        uuids: Vec<String>,
    },
    MoveSurface {
        uuid: String,
        dx: f32,
        dy: f32,
    },
    RotateSurface {
        uuid: String,
        /// Rotation in radians (clockwise in canvas space, y-down).
        angle: f32,
        /// Pivot the rotation is applied around, in normalized canvas coords.
        pivot: [f32; 2],
    },
    ScaleSurface {
        uuid: String,
        sx: f32,
        sy: f32,
        /// Pivot the scale is applied around, in normalized canvas coords.
        pivot: [f32; 2],
    },
    UpdateSurfaceContourVertices {
        uuid: String,
        contour: usize,
        vertices: Vec<[f32; 2]>,
    },
    /// Convert a curve-path edge to a cubic bezier (`to_cubic`) or back to a
    /// straight line. Lazily builds a path from the polygon if absent.
    ConvertSurfaceEdge {
        uuid: String,
        edge_idx: usize,
        to_cubic: bool,
    },
    /// Move a curve-path anchor to `pos` (normalized coords).
    MovePathAnchor {
        uuid: String,
        anchor_idx: usize,
        pos: [f32; 2],
    },
    /// Move a cubic control handle of a curve-path segment to `pos`.
    MovePathHandle {
        uuid: String,
        segment_idx: usize,
        handle: CubicHandle,
        pos: [f32; 2],
    },
    /// Add a subtractive cut-out hole (8i.7) to a surface from a closed path.
    AddSurfaceHole {
        uuid: String,
        hole: SurfacePath,
    },
    /// Remove the hole at `hole_index` from a surface.
    RemoveSurfaceHole {
        uuid: String,
        hole_index: usize,
    },
    /// "Make Hole" (8i.7): convert an existing surface into a cut-out hole in the
    /// topmost other surface under its centroid, then remove the source surface.
    /// Atomic (single command — no half-punched state).
    PunchSurfaceHole {
        source_uuid: String,
    },
    AssignSurfaceToOutput {
        output_uuid: String,
        surface_uuid: String,
    },
    UnassignSurfaceFromOutput {
        output_uuid: String,
        surface_uuid: String,
    },

    // ── Surface Auto-Detection ──────────────────────────────────
    /// Detect contours from a raster image and create surfaces from them.
    DetectFromImage {
        image_data: Vec<u8>,
        params: crate::engine::value::detect::DetectionParams,
    },
    /// Detect contours from an SVG file.
    DetectFromSvg {
        svg_data: Vec<u8>,
    },
    /// Detect contours from a DXF file.
    DetectFromDxf {
        dxf_data: Vec<u8>,
    },
    /// Confirm detected contours: create surfaces from them.
    ConfirmDetectedContours {
        contours: Vec<crate::engine::value::detect::DetectedContour>,
    },
    /// Import surfaces from a stage-plan file (image/SVG/DXF): detect contours
    /// and create surfaces. Composite of detect + confirm.
    ImportSurfacesFromFile {
        path: std::path::PathBuf,
    },
    /// Generate per-projector dome surfaces with warp meshes from a dome setup.
    /// Removes existing "Dome P*" surfaces, computes meshes, creates new ones.
    GenerateDomeSlices {
        setup: crate::engine::value::dome::DomeSetup,
    },
    /// Detect contours from a camera snapshot.
    DetectFromCamera {
        camera_id: CameraId,
        params: crate::engine::value::detect::DetectionParams,
    },

    // ── Transport ──────────────────────────────────────────────
    // Absolute show position. See /spec/transport.md.
    /// Start the show position advancing. Rejected while chasing timecode.
    TransportPlay,
    /// Hold the show position. Anything reading it freezes rather than
    /// releasing, so a stop keeps the current look.
    TransportStop,
    /// Jump to an absolute position in seconds. Rejected while chasing timecode.
    TransportLocate {
        position: f64,
    },
    /// Choose whether position advances locally or chases incoming timecode.
    SetTransportSource {
        source: crate::transport::TransportSource,
    },
    /// Set or clear the range internal playback wraps within.
    SetTransportLoop {
        region: Option<crate::transport::LoopRegion>,
    },
    /// Frame rate positions are displayed and quantised at.
    SetTimecodeRate {
        rate: crate::transport::TimecodeRate,
    },
    /// Which incoming timecode signal the transport should follow.
    /// See /spec/timecode.md § Preference and Priority.
    SetTimecodePreference {
        preference: crate::timecode::TimecodePreference,
    },
    /// Name the audio input carrying LTC, or stop listening for it.
    SetLtcInput {
        input: Option<crate::timecode::LtcInput>,
    },
    /// Keep live parameter writes as automation curves while the transport
    /// runs. Arming from a stop also rolls the transport. See
    /// /spec/automation-recording.md.
    SetRecordArmed {
        armed: bool,
    },
    /// Locate to the cue before the playhead, or to zero when there is none.
    TransportPrevCue,
    /// Locate to the cue after the playhead, or stay put when there is none.
    TransportNextCue,
    /// Locate to one named cue, leaving the transport running or stopped as it
    /// was. What the Performance-mode cue bank's buttons send.
    TriggerCue {
        uuid: String,
    },

    // ── Arrangement ────────────────────────────────────────────
    // Deck activity positioned against transport time. See /spec/arrangement.md.
    /// Give a deck a row in the arrangement. Idempotent: a lane *is* the deck.
    AddLane {
        deck_uuid: String,
    },
    /// Drop a row and the envelopes it owned, returning the deck to
    /// Performance mode.
    RemoveLane {
        deck_uuid: String,
    },
    /// Add a visibility span, creating the lane if the deck has none.
    AddRegion {
        deck_uuid: String,
        region: crate::arrangement::RegionConfig,
    },
    /// Replace a span in place, for a move, a resize, or a fade drag.
    UpdateRegion {
        deck_uuid: String,
        index: usize,
        region: crate::arrangement::RegionConfig,
    },
    RemoveRegion {
        deck_uuid: String,
        index: usize,
    },
    /// Fold a lane's automation rows away. View state, but it belongs to the
    /// scene: which curves a show wants open is a property of the show.
    SetLaneCollapsed {
        deck_uuid: String,
        collapsed: bool,
    },
    /// What renders before the transport reaches the arranged range.
    SetIdleBehaviour {
        idle: crate::arrangement::IdleBehaviour,
    },
    /// Hand one overridden parameter back to the arrangement, ramping over
    /// `seconds` rather than snapping.
    RearmParam {
        param_key: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<f64>,
    },
    /// Hand every overridden parameter back at once.
    RearmAll {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        seconds: Option<f64>,
    },
    /// Mark an instant worth returning to. Returns the cue's UUID.
    AddCue {
        at: f64,
        /// Left empty to be named by how many cues exist.
        #[serde(default)]
        name: String,
    },
    /// Move or rename a cue. Absent fields are left alone.
    UpdateCue {
        uuid: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    RemoveCue {
        uuid: String,
    },

    // ── Modulation Updates ─────────────────────────────────────
    /// Choose which notion of time a modulation source follows.
    /// See /spec/timebase.md.
    UpdateModulationTimebase {
        uuid: String,
        timebase: crate::timebase::Timebase,
    },
    UpdateLfoFrequency {
        uuid: String,
        frequency: f32,
    },
    UpdateLfoWaveform {
        uuid: String,
        waveform: LFOWaveform,
    },
    UpdateLfoPhase {
        uuid: String,
        phase: f32,
    },
    UpdateLfoAmplitude {
        uuid: String,
        amplitude: f32,
    },
    UpdateLfoBipolar {
        uuid: String,
        bipolar: bool,
    },
    UpdateAudioSmoothing {
        uuid: String,
        smoothing: f32,
    },
    UpdateAudioFreqRange {
        uuid: String,
        freq_low: f32,
        freq_high: f32,
    },
    UpdateAudioFreqLow {
        uuid: String,
        freq_low: f32,
    },
    UpdateAudioFreqHigh {
        uuid: String,
        freq_high: f32,
    },
    UpdateAudioGain {
        uuid: String,
        gain: f32,
    },
    UpdateAudioPreset {
        uuid: String,
        preset: AudioBandPreset,
    },
    UpdateAudioMode {
        uuid: String,
        mode: crate::modulation::AudioReactMode,
    },
    UpdateAudioSource {
        uuid: String,
        source_id: Option<AudioSourceId>,
    },
    UpdateAudioNoiseGate {
        uuid: String,
        noise_gate: f32,
    },
    UpdateAdsrAttack {
        uuid: String,
        attack: f32,
    },
    UpdateAdsrDecay {
        uuid: String,
        decay: f32,
    },
    UpdateAdsrSustain {
        uuid: String,
        sustain: f32,
    },
    UpdateAdsrRelease {
        uuid: String,
        release: f32,
    },
    TriggerAdsr {
        uuid: String,
    },
    ReleaseAdsr {
        uuid: String,
    },
    UpdateStepSeqSteps {
        uuid: String,
        steps: Vec<f32>,
    },
    UpdateStepSeqRate {
        uuid: String,
        rate: f32,
    },
    UpdateStepSeqInterpolation {
        uuid: String,
        interpolation: crate::modulation::StepInterpolation,
    },
    UpdateStepSeqBipolar {
        uuid: String,
        bipolar: bool,
    },
    SetStepSeqCount {
        uuid: String,
        count: usize,
    },
    UpdateStepSeqValue {
        uuid: String,
        step_idx: usize,
        value: f32,
    },
    AssignModOnMod {
        target_source_id: String,
        param_name: String,
        modulator_id: String,
        amount: f32,
    },
    RemoveModOnMod {
        target_source_id: String,
        param_name: String,
    },

    // ── Macros ─────────────────────────────────────────────────
    AddMacro {
        kind: crate::macros::MacroKind,
    },
    RemoveMacro {
        uuid: String,
    },
    RenameMacro {
        uuid: String,
        name: String,
    },
    SetMacroKind {
        uuid: String,
        kind: crate::macros::MacroKind,
    },
    /// Live macro turn — fans out to all targets. Not undoable.
    SetMacroValue {
        uuid: String,
        value: f32,
    },
    AddMacroTarget {
        uuid: String,
        path: String,
    },
    RemoveMacroTarget {
        uuid: String,
        target_idx: usize,
    },
    UpdateMacroTarget {
        uuid: String,
        target_idx: usize,
        min: f32,
        max: f32,
        curve: crate::macros::MacroCurve,
        invert: bool,
    },
    SetMacroButtonBehavior {
        uuid: String,
        behavior: crate::macros::ButtonBehavior,
    },
    SetMacroTriggers {
        uuid: String,
        actions: Vec<crate::macros::TriggerAction>,
    },

    // ── Analyzers ──────────────────────────────────────────────────
    RequestAnalyzer {
        deck_id: String,
        analyzer_type: String,
        options: serde_json::Value,
    },
    ReleaseAnalyzer {
        deck_id: String,
        analyzer_type: String,
    },
    AddAnalyzerModSource {
        deck_id: String,
        analyzer_type: String,
        output_name: String,
    },
    UpdateAnalyzerSmoothing {
        uuid: String,
        smoothing: f32,
    },

    // ── Device Scanning ────────────────────────────────────────
    RescanNdi,
    RescanSyphon,
    RescanCameras,
    RescanDepthSensors,
    /// Re-enumerate displays and windows. Manual: window lists churn constantly
    /// and polling them would thrash the library panel.
    RescanCaptureTargets,
    /// Trigger the platform screen-recording permission request.
    RequestScreenCapturePermission,
    /// Open the OS's camera Video Effects for this app (macOS Control
    /// Center), where Portrait, Studio Light, Reactions and the rest are
    /// switched off. Nothing elsewhere.
    ShowCameraVideoEffects,
    /// How a camera layer's picture is read into colours: range and matrix
    /// as the camera says (`auto`), or set by hand — for a camera that says
    /// one thing and sends another. Blacks crushed and highlights clipped
    /// next to the camera's own output: `full`; washed out: `limited`. Every
    /// layer on that camera takes it: they share its capture.
    SetCameraColour {
        deck_uuid: String,
        colour: crate::engine::value::camera::CameraColour,
    },
    /// Show a camera layer's newest picture every frame — the lowest latency,
    /// about a frame sooner, but a judder whenever a picture comes late (a
    /// camera over USB: about two a second) — or, `false`, each in turn at
    /// the delay that keeps motion even. Every layer on that camera takes
    /// it: they share its capture.
    SetCameraLowestLatency { deck_uuid: String, on: bool },
    /// What a camera layer's picture is coded as, so the show gets Rec.709
    /// video from it: `normal` (shown as it comes), `sony_hlg`,
    /// `sony_s_log2`, `sony_s_log3_cine`, `sony_s_log3`,
    /// `blackmagic_film_gen5`, or `{"kind": "lut", "file": "..."}` — a LUT
    /// file in the show's LUT folder, or a path. Every layer on that camera
    /// takes it: they share its capture.
    SetCameraProfile {
        deck_uuid: String,
        profile: crate::engine::value::camera::CameraProfile,
    },
    RescanMidi,
    RescanAudio,
    ToggleAudioSource {
        source_id: u32,
        enabled: bool,
    },
    SetMidiDeviceEnabled {
        device_id: crate::midi::DeviceId,
        enabled: bool,
    },

    // ── MIDI Mappings ──────────────────────────────────────────
    ClearMidiMappings,
    RemoveMidiMapping {
        key: crate::midi::MidiKey,
    },

    // ── Clock ──────────────────────────────────────────────────
    SetClockPreference {
        preference: crate::clock::ClockPreference,
    },
    SetManualBpm {
        bpm: f32,
    },

    // ── Parameters ───────────────────────────────────────────────
    SetGeneratorParam {
        deck_uuid: String,
        name: String,
        value: ParamValue,
    },
    /// Set a parameter on any effect — deck, channel, or master chain. Effect
    /// UUIDs are globally unique, so one variant covers all three scopes.
    SetEffectParam {
        effect_uuid: String,
        name: String,
        value: ParamValue,
    },
    /// Show an effect only inside (or outside) a shape, or everywhere again
    /// for `None`. A mask with no area left is refused.
    SetEffectMask {
        effect_uuid: String,
        mask: Option<crate::engine::value::mask::EffectMask>,
    },
    /// Cut a layer's own mask: transparent inside the shape, so what is
    /// under it shows through — or, inverted, only the shape kept. `None`
    /// takes it away. Carried by an effect kept last in the layer's chain
    /// (see `mask::LAYER_MASK_EFFECT`), made the first time; answered with
    /// its UUID.
    SetLayerMask {
        deck_uuid: String,
        mask: Option<crate::engine::value::mask::EffectMask>,
    },
    ResetGeneratorParamsToDefaults {
        deck_uuid: String,
    },
    /// Put an effect's parameters back to their defaults: all of them, or
    /// only those named (the crop's border, say, without its crop).
    ResetEffectParams {
        effect_uuid: String,
        #[serde(default)]
        names: Vec<String>,
    },

    // ── Resolution ─────────────────────────────────────────────
    SetRenderResolution {
        width: u32,
        height: u32,
    },
    /// Auto resolution: the master follows the largest screen an output is
    /// on, so a 4K TV gets a 4K picture and Full HD screens a Full HD one.
    /// `SetRenderResolution` turns it off again.
    SetRenderResolutionAuto {
        enabled: bool,
    },

    /// Set the domemaster output size. Separate from the render resolution
    /// because a domemaster image is square by definition — it is sized by the
    /// dome's projector, not by the master canvas it samples from.
    SetDomemasterResolution {
        resolution: crate::engine::value::dome::DomemasterResolution,
    },

    // ── Frame pacing ─────────────────────────────────────────
    SetTargetFps {
        fps: u32,
    },
    /// How many frames the GPU may still be drawing when the next starts
    /// (1–3): at 1 (the default) a frame starts once the GPU has finished
    /// the one before; at 2 the next is prepared meanwhile, which keeps a
    /// busy GPU from holding the frame loop up.
    SetFramesInFlight {
        frames: u32,
    },
    /// What starts each frame: `refresh` (the default) — the screen's
    /// refresh; `ahead` — the frame before being done, output windows
    /// holding up to two ready, so a frame drawn late still finds its
    /// refresh, at a frame or two of delay.
    SetFramePacing {
        pacing: crate::engine::value::render::FramePacing,
    },
    /// Ask for frames by the refresh each display-link callback is stamped
    /// for — one frame per slot, whichever callback brings it — rather than
    /// by counting callbacks, which a late or doubled callback throws off;
    /// and leave redraws macOS asks for by itself undrawn while a screen's
    /// refresh starts frames. Off by default.
    SetClockTimestamps {
        on: bool,
    },
    /// Hold each frame on screen for a frame's time — macOS's own frame
    /// pacing (Metal's `presentDrawable:afterMinimumDuration:`), which on a
    /// variable-refresh screen keeps every frame up equally long whatever its
    /// refresh signal does — with what moves by whole frames stepping a
    /// frame per frame. Off by default; macOS only.
    SetFrameHold {
        on: bool,
    },
    /// Present the control window in step with its screen's refresh (vsync),
    /// rather than as soon as each frame is drawn. On a variable-refresh
    /// (`ProMotion`) screen, presents out of step pull its refresh about for
    /// every app, the display link that starts frames included. Off by
    /// default.
    SetControlVsync {
        on: bool,
    },
    /// Set the screen the control window is on to the show's rate (60 Hz
    /// for a 60 fps show) for as long as Nida runs — on by default, kept
    /// with the show. A `ProMotion` screen at 120 Hz made macOS miss frames on
    /// every screen while a screen was captured. macOS puts the screen back
    /// when Nida quits; output screens are never changed. macOS only.
    SetControlScreenRate {
        on: bool,
    },
    /// Keep Night Shift off while an output window is on a screen — on by
    /// default, kept with the show. macOS warms every screen with it, the
    /// outputs too, and lets no app leave one out; Nida gives it back when
    /// no output is on a screen or it quits, on again if it is due on then.
    /// macOS only.
    SetNightShiftOff {
        on: bool,
    },

    // ── Performance profiling ──────────────────────────────────
    /// Start GPU performance profiling for the next N frames.
    /// Inserts device.poll(Wait) between GPU stages to measure actual
    /// GPU execution time per category. Logs every frame.
    StartPerfProfile {
        frames: u32,
    },
    /// Note every output frame's way to its screen for `secs` seconds (at
    /// most 60) — drawn, kept back or handed to the window's presenter, taken,
    /// a frame to draw into had, submitted, presented, shown — and the GPU's
    /// command buffers meanwhile, all on the host clock, then write it to a
    /// JSON file beside the log. Answers with the file's path.
    TracePresents {
        secs: f32,
    },

    // ── Presets ────────────────────────────────────────────────
    /// Load a named deck preset as a new deck appended to a channel. Presets are
    /// addressed by name: the library is rescanned from disk, so its ordering is
    /// not stable across scans.
    LoadDeckPreset {
        channel_uuid: String,
        preset_name: String,
    },
    /// Load a named channel preset. Fills `target_channel_uuid` if it is given
    /// and empty; otherwise appends a new channel.
    LoadChannelPreset {
        target_channel_uuid: Option<String>,
        preset_name: String,
    },
    /// Save a deck's current config as a named deck preset (writes to disk).
    SaveDeckPreset {
        deck_uuid: String,
        name: String,
    },
    /// Save a channel's current config as a named channel preset (writes to disk).
    SaveChannelPreset {
        channel_uuid: String,
        name: String,
    },

    // ── Production buses ───────────────────────────────────────
    // Every variant names an explicit bus, so a command can never switch a
    // bus the caller did not address. See `src/internal/bus`.
    AddBus {
        name: String,
        width: u32,
        height: u32,
    },
    RemoveBus {
        uuid: String,
    },
    RenameBus {
        uuid: String,
        name: String,
    },
    SetBusPreview {
        uuid: String,
        input: BusInputRef,
    },
    SetBusCanvas {
        uuid: String,
        width: u32,
        height: u32,
    },
    /// Place a bus's region within the master raster, size and position.
    SetBusCanvasRegion {
        uuid: String,
        width: u32,
        height: u32,
        origin_x: u32,
        origin_y: u32,
    },
    SetBusAutoDuration {
        uuid: String,
        duration_secs: f32,
    },
    BusCut {
        uuid: String,
    },
    BusAuto {
        uuid: String,
        duration_secs: Option<f32>,
    },
    /// Put a bus fullscreen on a monitor (by name), or take it off screen
    /// with `None` — off a Blackmagic device too. Makes the bus's output
    /// window the first time.
    ShowBusOnScreen {
        uuid: String,
        monitor_name: Option<String>,
    },
    /// Give a screen (by the name outputs address it by) a name of the
    /// operator's own — kept on this computer — or, with `None` or a blank
    /// one, take it away.
    RenameScreen {
        monitor_name: String,
        name: Option<String>,
    },
    /// Put each screen's name big on it for a few seconds, so two alike can
    /// be told apart.
    IdentifyScreens,
    /// Put a bus on a Blackmagic device (by name) — its HDMI and SDI — as on
    /// a screen, taking it off any other; or off the device with `None`. The
    /// device's output becomes the bus's, named after it, and is started;
    /// made the first time, in 1080p at the show's rate. Replies with the
    /// output's UUID.
    ShowBusOnBlackmagic {
        uuid: String,
        device: Option<String>,
    },
    /// Make an output an LED feed carrying these screens, each at its own
    /// pixels of the one signal an LED processor takes — or, with `None`, an
    /// ordinary output again.
    SetOutputLed {
        output_uuid: String,
        layout: Option<crate::engine::value::led::LedLayout>,
    },
    /// Open a new LED feed carrying `layout`: fullscreen on a monitor (by
    /// name), or a window with `None`. Replies with the output's UUID.
    CreateLedOutput {
        monitor_name: Option<String>,
        layout: crate::engine::value::led::LedLayout,
    },
    /// Give one screen of an LED feed a bus of its own, named after it, and
    /// point the screen at it — its place in the switcher. Replies with the
    /// new bus's UUID.
    CreateLedScreenBus {
        output_uuid: String,
        screen_uuid: String,
    },
    /// Leave one mixer layer (channel) out of a bus's Master, or put it back —
    /// vMix's per-output overlay switch. Only affects the bus while it shows
    /// Master.
    SetBusLayerHidden {
        uuid: String,
        channel_uuid: String,
        hidden: bool,
    },
    /// Take a source's button off a bus's row in the switcher, or put it
    /// back: Black, Master or a channel. Only the button — what is on the
    /// bus is untouched.
    SetBusSourceHidden {
        uuid: String,
        input: crate::bus::BusInputRef,
        hidden: bool,
    },

    // ── Audio mixer ─────────────────────────────────────────────
    /// Add an audio bus; replies with its UUID.
    AddAudioBus {
        name: String,
    },
    /// Remove an audio bus and every send to it.
    RemoveAudioBus {
        uuid: String,
    },
    RenameAudioBus {
        uuid: String,
        name: String,
    },
    /// An audio bus's fader, in dB (-96 is off, at most +12).
    SetAudioBusGain {
        uuid: String,
        gain_db: f32,
    },
    SetAudioBusCompressor {
        uuid: String,
        config: crate::audio_mix::CompressorConfig,
    },
    ResetAudioMeter {
        uuid: String,
        bus: bool,
    },
    /// Start a bus's loudness readings again, as a show starts. Its held
    /// peak stays (that is `ResetAudioMeter`).
    ResetAudioLoudness {
        uuid: String,
    },
    SetAudioSourceCompressor {
        uuid: String,
        config: crate::audio_mix::CompressorConfig,
    },
    /// A source's low cut: rumble and hum under a voice.
    SetAudioSourceLowCut {
        uuid: String,
        config: crate::audio_mix::LowCutConfig,
    },
    /// A source's leveler: each speaker brought to the same loudness.
    SetAudioSourceLeveler {
        uuid: String,
        config: crate::audio_mix::LevelerConfig,
    },
    /// A bus's limiter, the last thing before its recordings and streams.
    SetAudioBusLimiter {
        uuid: String,
        config: crate::audio_mix::LimiterConfig,
    },
    SetAudioBusMute {
        uuid: String,
        muted: bool,
    },
    /// Delay an audio bus, 0–1000 ms, to line it up with a late picture.
    SetAudioBusDelay {
        uuid: String,
        delay_ms: f32,
    },
    /// Play an audio bus on a sound device (by name), on the output pair
    /// starting at `first_channel` (0 = outputs 1-2); `None` plays nowhere.
    SetAudioBusDevice {
        uuid: String,
        device: Option<String>,
        first_channel: u16,
    },
    /// Add an input device (microphone, line, USB mixer) as a source; replies
    /// with its UUID. `left`/`right` are 0-based device channels, the same
    /// for a mono microphone.
    AddAudioInput {
        name: String,
        device: String,
        left: u16,
        right: u16,
    },
    /// Add an app's sound — a Zoom or Teams call's, by the app's bundle ID
    /// ("us.zoom.xos") — as a source, taken straight from the app while it
    /// runs (macOS 14.2 and later); replies with its UUID.
    AddAppSound {
        app: String,
        name: String,
        /// The input whose sound it is (PowerPoint's, of a PowerPoint
        /// input): heard with that picture on air (Audio Auto), removed
        /// with it.
        #[serde(default)]
        deck_uuid: Option<String>,
    },
    RemoveAudioInput {
        uuid: String,
    },
    /// A source's fader, in dB. `uuid` is the deck's for a clip.
    SetAudioSourceGain {
        uuid: String,
        gain_db: f32,
    },
    SetAudioSourceMute {
        uuid: String,
        muted: bool,
    },
    /// Send a source to an audio bus, or stop.
    SetAudioSend {
        uuid: String,
        bus_uuid: String,
        on: bool,
    },
    /// Where a source's Audio Auto looks for it being on air: a video bus,
    /// `"main"` for Main, or `None` for Main and every bus.
    SetAudioFollow {
        uuid: String,
        bus_uuid: Option<String>,
    },
    /// Audio Auto (vMix): the source's sound switches on as it goes on air
    /// and off as it leaves, fading with the picture.
    SetAudioAuto {
        uuid: String,
        auto: bool,
    },
    /// Left–right balance, -1 (left only) to 1 (right only).
    SetAudioBalance {
        uuid: String,
        balance: f32,
    },
    /// Hold a source's sound back, 0–1000 ms: a microphone's, to meet the
    /// picture of a camera that arrives later (OBS's "Sync Offset", vMix's
    /// input audio delay).
    SetAudioSourceDelay {
        uuid: String,
        delay_ms: f32,
    },
    /// The audio bus a recording or stream carries; `None` for none (or the
    /// target's own capture device, if it names one).
    SetOutputAudioBus {
        output_uuid: String,
        bus_uuid: Option<String>,
    },
    /// List sound devices again.
    RescanAudioDevices,
    /// Solo a source to the monitor (pre-fader listen, heard even when muted
    /// or off air), or stop.
    SetAudioSolo {
        uuid: String,
        solo: bool,
    },
    /// Play the monitor (the operator's headphones) on a sound device, on the
    /// output pair starting at `first_channel`; `None` plays nowhere.
    SetAudioMonitorDevice {
        device: Option<String>,
        first_channel: u16,
    },
    /// Monitor level, in dB.
    SetAudioMonitorGain {
        gain_db: f32,
    },
    /// The audio bus the monitor hears when nothing is soloed; `None` is the
    /// first bus.
    SetAudioMonitorListen {
        bus_uuid: Option<String>,
    },

    // ── Main mode ──────────────────────────────────────────────
    /// Make the master Main with overlays (`program`) — one channel or input
    /// on air, overlay channels on top — or the layered channel mix
    /// (`layered`).
    SetMixMode {
        mode: crate::mixer::MixMode,
    },
    /// Put a channel, a deck, or black on Main: a cut, or with `fade` a
    /// dissolve over the fade time. An overlay channel is switched on
    /// instead.
    TakeToMain {
        source: crate::mixer::MainSource,
        #[serde(default)]
        fade: bool,
    },
    /// Make a channel an overlay — switched on and off over Main — or a
    /// normal channel, taken to Main.
    SetChannelOverlay {
        channel_uuid: String,
        overlay: bool,
    },
    /// Switch an overlay channel on or off over Main, at once or fading.
    SetOverlayOn {
        channel_uuid: String,
        on: bool,
        #[serde(default)]
        fade: bool,
    },
    /// Seconds a fade to Main takes, 0–10.
    SetMainFadeTime {
        secs: f32,
    },
    /// What a take to Main that is not a cut does: a fade, a merge, a
    /// wipe… (see `MainTransition`).
    SetMainTransition {
        transition: crate::mixer::MainTransition,
    },

    // ── Rundown ────────────────────────────────────────────────
    // The show in order, as steps taken with one press: every bus changed
    // together, Main taken, clips started. See `src/internal/rundown`.
    /// Add a step at the end of the rundown, keeping what is on now: what
    /// each of `buses` shows — every bus but one the stream or the recording
    /// takes, when none are named — and, with `main`, what is on Main and
    /// which overlays are on and off. Replies with the step's UUID.
    AddRundownStep {
        name: String,
        #[serde(default)]
        buses: Option<Vec<String>>,
        #[serde(default)]
        main: bool,
    },
    /// Put a copy of a step right after it. Replies with the copy's UUID.
    DuplicateRundownStep {
        uuid: String,
    },
    RemoveRundownStep {
        uuid: String,
    },
    RenameRundownStep {
        uuid: String,
        name: String,
    },
    /// Move a step one place later in the rundown, or earlier.
    MoveRundownStep {
        uuid: String,
        later: bool,
    },
    /// What a bus shows in a step — or, with `None`, leave that bus as it
    /// is when the step is taken.
    SetRundownScreen {
        uuid: String,
        bus_uuid: String,
        input: Option<BusInputRef>,
    },
    /// What a step takes to Main — or, with `None`, leave Main as it is.
    SetRundownMain {
        uuid: String,
        main: Option<crate::mixer::MainSource>,
    },
    /// Whether a step switches an overlay channel on or off — or, with
    /// `None`, leaves it as it is.
    SetRundownOverlay {
        uuid: String,
        channel_uuid: String,
        on: Option<bool>,
    },
    /// What a step does to a clip, a playlist or a timer: start it from its
    /// beginning, carry on, pause, stop — or, with `None`, nothing.
    SetRundownPlay {
        uuid: String,
        deck_uuid: String,
        action: Option<crate::rundown::PlayAction>,
    },
    /// Whether a clip or playlist a step starts or carries on is faded out —
    /// its sound over the next step's fade — and stopped when another step
    /// is taken.
    SetRundownUntilNext {
        uuid: String,
        deck_uuid: String,
        on: bool,
    },
    /// How a step sets a timer it starts: counting down from a length of its
    /// own, or up — or, with `None`, as the timer is set.
    SetRundownTimer {
        uuid: String,
        deck_uuid: String,
        timer: Option<crate::rundown::TimerSet>,
    },
    /// How many seconds after a step is taken the next is taken by itself
    /// (more than none, an hour at most) — or, with `None`, not until the
    /// operator says.
    SetRundownThen {
        uuid: String,
        secs: Option<f32>,
        /// Instead of a time: go on when this clip or playlist — one the
        /// step starts or carries on — ends, by deck UUID.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        when_ends: Option<String>,
    },
    /// The time of day a step is taken by itself at while GO would take it
    /// next, seconds after midnight — or, with `None`, not. `always`, if
    /// given, is whether it is taken at its time even when not next.
    SetRundownAt {
        uuid: String,
        at_secs: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        always: Option<bool>,
    },
    /// Make a step what is on now, for the parts it sets: the buses it
    /// lists, and Main and the overlays if it sets them.
    CaptureRundownStep {
        uuid: String,
    },
    /// Take a step, all of it or none: its buses together — all cut, or
    /// with `fade` all dissolved over the rundown's fade time — Main as its
    /// own take does, its overlays, and what it starts and stops.
    TakeRundownStep {
        uuid: String,
        #[serde(default)]
        fade: bool,
    },
    /// Take the next step: the one after the step taken last, or the one
    /// `SetRundownNext` named. With `expect`, only if that is the step —
    /// so a press meant for one step never lands on the one after it, when
    /// the rundown went on by itself in between.
    RundownGo {
        #[serde(default = "yes")]
        fade: bool,
        #[serde(default)]
        expect: Option<String>,
    },
    /// Make this the step `RundownGo` takes next, without taking anything.
    SetRundownNext {
        uuid: String,
    },
    /// Drop the wait of a step that goes on to the next by itself: the
    /// rundown stays where it is until the operator says.
    RundownHold,
    /// Seconds the buses take to fade to a step.
    SetRundownFade {
        secs: f32,
    },

    // ── Stream ─────────────────────────────────────────────────
    /// What is streamed and how: source, audio bus, size, bit rates,
    /// encoder, keyframe interval. While the stream is on only the source
    /// can change; the rest is refused until it is stopped.
    SetStreamOptions {
        options: crate::streaming::StreamOptions,
    },
    /// One of the four destinations, by position 0–3. `key` and `password`
    /// left out keep the ones saved. Takes effect at once while live.
    SetStreamDestination {
        index: usize,
        enabled: bool,
        #[serde(default)]
        name: String,
        url: String,
        #[serde(default)]
        key: Option<String>,
        #[serde(default)]
        username: String,
        #[serde(default)]
        password: Option<String>,
    },
    /// Start streaming to every destination switched on.
    StartStream,
    /// Stop streaming.
    StopStream,

    // ── Record ─────────────────────────────────────────────────
    /// What is recorded and how: source, sound, folder, format, codec,
    /// quality, size, frame rate. While recording only the source changes.
    SetRecordOptions {
        options: crate::record::RecordOptions,
    },
    /// Start recording into a new file named by the date and time.
    StartRecording,
    /// Stop recording; the file is finished in the background.
    StopRecording,

    // ── Persistence ────────────────────────────────────────────
    SaveWorkspace,
    LoadWorkspace,
    /// Save the show into another folder (a show is a folder holding
    /// `.varda/`), and carry on there.
    SaveShowAs {
        folder: std::path::PathBuf,
    },
    /// Save the show, then start a fresh one — in `folder`, or a new
    /// "Untitled" in `~/Documents/Nida Shows` — as one never set up: no
    /// outputs, the one bus, the mixer's Master alone, stream, recording and
    /// timecode settings as new. MIDI mappings, keys, OSC, presets, LUTs and
    /// shaders come along.
    NewShow {
        #[serde(default)]
        folder: Option<std::path::PathBuf>,
    },
    /// Save the show, then open the one in `folder`.
    OpenShow {
        folder: std::path::PathBuf,
    },

    // ── History ─────────────────────────────────────────────────
    Undo,
    Redo,

    // ── System ──────────────────────────────────────────────────
    Shutdown,
}
