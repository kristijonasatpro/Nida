//! Add Input, as vMix's: one window for every kind of input — clips and
//! sound files, pictures, cameras and capture cards, NDI sources on the
//! network, a display or window, a stream, a web page, a colour, a
//! generator, a timer, another input, and a sound device on its own.
//!
//! Kinds down the left, what there is of the kind on the right; OK (or a
//! double-click on what there is) adds it. A new input is a channel of its
//! own at the end of the row, called after what it shows. Opened from a
//! channel's "+ Add layer" menu instead, the same window adds the source to
//! that channel as a layer.

use super::super::{UIActions, UIData};
use crate::engine::value::camera::CameraMode;
use crate::engine::{EngineCommand, InputAudio, InputSource};
use crate::scene::CaptureTargetConfig;
use crate::stream::{RtmpMode, SrtMode};

/// Seconds between looks at the network while the NDI list is showing:
/// sources appear as they announce themselves.
const NDI_LOOK_SECS: f64 = 1.0;
const KINDS_WIDTH: f32 = 150.0;
/// The kinds list and what there is of the kind, above Name and OK.
const BODY_SIZE: egui::Vec2 = egui::vec2(640.0, 340.0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum Kind {
    #[default]
    Video,
    Image,
    Playlist,
    Camera,
    Ndi,
    Screen,
    Stream,
    Web,
    Colour,
    Generator,
    Timer,
    PowerPoint,
    Input,
    Audio,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Self::Video => "Video / Sound",
            Self::Image => "Image",
            Self::Playlist => "Playlist",
            Self::Camera => "Camera",
            Self::Ndi => "NDI",
            Self::Screen => "Desktop Capture",
            Self::Stream => "Stream / SRT",
            Self::Web => "Web Browser",
            Self::Colour => "Colour",
            Self::Generator => "Generator",
            Self::Timer => "Timer / Clock",
            Self::PowerPoint => "PowerPoint",
            Self::Input => "Another input",
            Self::Audio => "Audio Input",
        }
    }

    /// The kinds on offer: a layer has no sound device of its own, and a
    /// web page needs the build that can draw one.
    fn offered(layer: bool) -> Vec<Self> {
        let mut kinds = vec![
            Self::Video,
            Self::Image,
            Self::Playlist,
            Self::Camera,
            Self::Ndi,
            Self::Screen,
            Self::Stream,
        ];
        if cfg!(feature = "html") {
            kinds.push(Self::Web);
        }
        kinds.extend([Self::Colour, Self::Generator, Self::Timer]);
        // PowerPoint for Mac plays the deck: macOS only.
        if cfg!(target_os = "macos") {
            kinds.push(Self::PowerPoint);
        }
        kinds.push(Self::Input);
        if !layer {
            kinds.push(Self::Audio);
        }
        kinds
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
enum StreamKind {
    #[default]
    Srt,
    Rtmp,
    Hls,
}

#[derive(Clone)]
struct State {
    open: bool,
    /// Adding a layer to this channel, not a new input.
    layer_of: Option<String>,
    kind: Kind,
    /// The kind whose device list was last asked for, so opening a kind
    /// looks for devices once.
    scanned: Option<Kind>,
    /// When the NDI list last looked at the network (egui time).
    ndi_look: f64,
    camera: Option<crate::camera::CameraId>,
    /// The camera the format and sound below were chosen for.
    camera_settled: Option<crate::camera::CameraId>,
    /// `None` takes the camera's best.
    camera_mode: Option<CameraMode>,
    /// The camera's own sound: a device and its left and right channels.
    camera_audio: Option<(String, u16, u16)>,
    ndi: Option<String>,
    screen: Option<CaptureTargetConfig>,
    screen_rate: f32,
    show_cursor: bool,
    stream: StreamKind,
    srt_url: String,
    srt_mode: SrtMode,
    rtmp_url: String,
    rtmp_mode: RtmpMode,
    hls_url: String,
    web_url: String,
    colour: [f32; 4],
    /// A generator, by shader name, and the text it is looked for by.
    generator: Option<String>,
    generator_search: String,
    /// Another input, by its channel's UUID.
    input: Option<String>,
    /// What a new timer counts.
    timer: crate::engine::value::timer::TimerSettings,
    /// Sound device and the pair of its channels taken, left and right.
    audio: Option<(String, u16, u16)>,
    /// What to call the input; after the source when empty.
    name: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            layer_of: None,
            kind: Kind::default(),
            scanned: None,
            ndi_look: f64::NEG_INFINITY,
            camera: None,
            camera_settled: None,
            camera_mode: None,
            camera_audio: None,
            ndi: None,
            screen: None,
            screen_rate: 30.0,
            show_cursor: false,
            stream: StreamKind::default(),
            srt_url: "srt://0.0.0.0:9000".into(),
            srt_mode: SrtMode::Listener,
            rtmp_url: "rtmp://0.0.0.0:1935/live/stream".into(),
            rtmp_mode: RtmpMode::Listen,
            hls_url: String::new(),
            web_url: "https://".into(),
            colour: [0.0, 0.0, 0.0, 1.0],
            generator: None,
            generator_search: String::new(),
            input: None,
            timer: crate::engine::value::timer::TimerSettings::default(),
            audio: None,
            name: String::new(),
        }
    }
}

fn state_id() -> egui::Id {
    egui::Id::new("add_input")
}

fn load(ctx: &egui::Context) -> State {
    ctx.data(|d| d.get_temp(state_id())).unwrap_or_default()
}

fn store(ctx: &egui::Context, state: State) {
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
}

/// Open Add Input for a new input.
pub(super) fn open(ctx: &egui::Context) {
    let mut state = load(ctx);
    state.open = true;
    state.layer_of = None;
    state.scanned = None;
    state.name.clear();
    store(ctx, state);
}

/// Open Add Input to add a layer to a channel: the same kinds as a new
/// input (but a sound device of its own).
pub(super) fn open_for_layer(ctx: &egui::Context, channel_uuid: &str) {
    let mut state = load(ctx);
    state.open = true;
    state.layer_of = Some(channel_uuid.to_string());
    state.scanned = None;
    if state.kind == Kind::Audio {
        state.kind = Kind::Video;
    }
    store(ctx, state);
}

/// Whether the window is open, for tests.
#[cfg(test)]
pub(super) fn is_open(ctx: &egui::Context) -> bool {
    load(ctx).open
}

pub(super) fn render_add_input(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    let mut state = load(ctx);
    if !state.open {
        return;
    }
    // A channel that went away takes its "add layer" with it.
    if let Some(ch) = &state.layer_of {
        if !data.channels.iter().any(|c| &c.uuid == ch) {
            state.open = false;
            store(ctx, state);
            return;
        }
    }
    let title = match &state.layer_of {
        Some(ch) => {
            let name = data
                .channels
                .iter()
                .find(|c| &c.uuid == ch)
                .map_or("", |c| c.name.as_str());
            format!("Add layer to {name}")
        }
        None => "Add Input".to_string(),
    };
    let mut open = true;
    egui::Window::new(title)
        .id(egui::Id::new("add_input_window"))
        .open(&mut open)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| body(ui, data, actions, &mut state));
    state.open &= open;
    store(ctx, state);
}

fn body(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions, state: &mut State) {
    let layer = state.layer_of.is_some();
    let kinds = Kind::offered(layer);
    if !kinds.contains(&state.kind) {
        state.kind = Kind::default();
    }
    look_for_devices(ui, state, actions);

    let height = BODY_SIZE.y;
    let mut chosen = false;
    // Held to its size: a vertical separator would stretch the row to the
    // whole screen.
    ui.allocate_ui(BODY_SIZE, |ui| {
        ui.set_max_height(height);
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                egui::vec2(KINDS_WIDTH, height),
                egui::Layout::top_down_justified(egui::Align::Min),
                |ui| {
                    ui.set_width(KINDS_WIDTH);
                    for kind in kinds {
                        let text = egui::RichText::new(kind.label()).size(14.0);
                        if ui.selectable_label(state.kind == kind, text).clicked() {
                            state.kind = kind;
                        }
                    }
                },
            );
            ui.separator();
            ui.allocate_ui_with_layout(
                egui::vec2(BODY_SIZE.x - KINDS_WIDTH, height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_size(egui::vec2(BODY_SIZE.x - KINDS_WIDTH, height));
                    egui::ScrollArea::vertical()
                        .id_salt("add_input_kind")
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            chosen = kind_body(ui, data, actions, state);
                        });
                },
            );
        });
    });
    ui.separator();
    ui.horizontal(|ui| {
        let ready = source(data, state).is_some() || audio_command(state).is_some();
        let files = matches!(state.kind, Kind::Video | Kind::Image);
        if !layer && !files {
            ui.label("Name:");
            ui.add(
                egui::TextEdit::singleline(&mut state.name)
                    .hint_text("after what it shows")
                    .desired_width(220.0),
            );
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button("Cancel").clicked() {
                state.open = false;
            }
            if !files
                && ui
                    .add_enabled(
                        ready,
                        egui::Button::new("OK").min_size(egui::vec2(72.0, 0.0)),
                    )
                    .clicked()
            {
                chosen = true;
            }
        });
    });
    if chosen && add(ui.ctx(), data, actions, state) {
        state.open = false;
    }
}

/// Ask for the devices of the kind showing: once as a kind opens, and for
/// NDI every second, as sources come and go on the network.
fn look_for_devices(ui: &egui::Ui, state: &mut State, actions: &mut UIActions) {
    let first = state.scanned != Some(state.kind);
    state.scanned = Some(state.kind);
    match state.kind {
        Kind::Camera if first => actions.commands.push(EngineCommand::RescanCameras),
        Kind::Screen if first => actions.commands.push(EngineCommand::RescanCaptureTargets),
        Kind::Audio if first => actions.commands.push(EngineCommand::RescanAudioDevices),
        Kind::Ndi => {
            let now = ui.input(|i| i.time);
            if now - state.ndi_look >= NDI_LOOK_SECS {
                state.ndi_look = now;
                actions.commands.push(EngineCommand::RescanNdi);
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs_f64(NDI_LOOK_SECS));
        }
        _ => {}
    }
}

/// The right-hand side for the kind chosen. True when something was
/// double-clicked: add it at once.
fn kind_body(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions, state: &mut State) -> bool {
    match state.kind {
        Kind::Video | Kind::Image => {
            files_body(ui, actions, state);
            false
        }
        Kind::Playlist => {
            playlist_body(ui, state);
            false
        }
        Kind::Camera => camera_body(ui, data, actions, state),
        Kind::Ndi => ndi_body(ui, data, state),
        Kind::Screen => screen_body(ui, data, actions, state),
        Kind::Stream => {
            stream_body(ui, state);
            false
        }
        Kind::Web => {
            ui.label("Address of the page:");
            ui.add(egui::TextEdit::singleline(&mut state.web_url).desired_width(f32::INFINITY));
            false
        }
        Kind::Colour => {
            ui.horizontal(|ui| {
                ui.label("Colour:");
                crate::usecases::ui::widgets::colour_edit(ui, "new input", &mut state.colour);
            });
            false
        }
        Kind::Generator => generator_body(ui, data, state),
        Kind::Timer => {
            timer_body(ui, state);
            false
        }
        Kind::PowerPoint => {
            powerpoint_body(ui, state);
            false
        }
        Kind::Input => input_body(ui, data, state),
        Kind::Audio => audio_body(ui, data, actions, state),
    }
}

/// The generators in the library, found by typing part of a name. True
/// when one was double-clicked.
fn generator_body(ui: &mut egui::Ui, data: &UIData, state: &mut State) -> bool {
    ui.add(
        egui::TextEdit::singleline(&mut state.generator_search)
            .hint_text("Find a generator")
            .desired_width(f32::INFINITY),
    );
    let needle = state.generator_search.to_lowercase().replace(' ', "_");
    let mut chosen = false;
    for (name, _) in &data.generators {
        if !needle.is_empty() && !name.to_lowercase().contains(&needle) {
            continue;
        }
        let selected = state.generator.as_deref() == Some(name.as_str());
        let (clicked, twice) = row(ui, selected, &super::channel_settings::pretty(name));
        if clicked || twice {
            state.generator = Some(name.clone());
        }
        chosen |= twice;
    }
    chosen
}

/// The other inputs. As a layer, that input's picture inside this one; as
/// a new input, a copy of it placed on its own. One that shows this input
/// already would show itself round a loop, and cannot be picked. True when
/// one was double-clicked.
fn input_body(ui: &mut egui::Ui, data: &UIData, state: &mut State) -> bool {
    ui.label(if state.layer_of.is_some() {
        "Another input's picture as a layer of this one, sized and placed like any layer — drawn once for both."
    } else {
        "A copy of an input that can be sized and placed on its own — drawn once for both."
    });
    ui.add_space(6.0);
    let mut chosen = false;
    for other in &data.channels {
        if !input_allowed(data, state, &other.uuid) {
            if state.layer_of.as_deref() != Some(other.uuid.as_str()) {
                ui.add_enabled(false, egui::Button::selectable(false, other.name.as_str()))
                    .on_disabled_hover_text(format!("{} shows this input already", other.name));
            }
            continue;
        }
        let selected = state.input.as_deref() == Some(other.uuid.as_str());
        let (clicked, twice) = row(ui, selected, &other.name);
        if clicked || twice {
            state.input = Some(other.uuid.clone());
        }
        chosen |= twice;
    }
    chosen
}

/// Whether the input `uuid` can be shown in the one a layer is being added
/// to: not that input itself, nor one showing it.
fn input_allowed(data: &UIData, state: &State, uuid: &str) -> bool {
    data.channels.iter().any(|c| c.uuid == uuid)
        && state
            .layer_of
            .as_deref()
            .is_none_or(|target| !shows(data, uuid, target))
}

/// Whether the channel `from` shows the channel `target` — is it, or has it
/// among its layers, directly or through the inputs it shows.
pub(super) fn shows(data: &UIData, from: &str, target: &str) -> bool {
    let mut seen = std::collections::HashSet::new();
    let mut next = vec![from.to_string()];
    while let Some(uuid) = next.pop() {
        if uuid == target {
            return true;
        }
        if !seen.insert(uuid.clone()) {
            continue;
        }
        let Some(channel) = data.channels.iter().find(|c| c.uuid == uuid) else {
            continue;
        };
        next.extend(
            channel
                .decks
                .iter()
                .filter_map(|d| d.tap.as_ref()?.channel_uuid.clone()),
        );
    }
    false
}

fn files_body(ui: &mut egui::Ui, actions: &mut UIActions, state: &mut State) {
    let video = state.kind == Kind::Video;
    let layer = state.layer_of.clone();
    ui.label(match (video, layer.is_some()) {
        (true, false) => "Clips and sound files — each file you pick becomes an input of its own.",
        (false, false) => "Pictures — each file you pick becomes an input of its own.",
        (true, true) => "A clip or sound file, as a layer of this input.",
        (false, true) => "A picture, as a layer of this input.",
    });
    ui.add_space(8.0);
    if ui
        .add(egui::Button::new(
            egui::RichText::new("Choose files…").size(15.0),
        ))
        .clicked()
    {
        let session = &mut actions.session;
        match (video, layer) {
            (true, None) => session.open_video_dialog_for_new_inputs = true,
            (false, None) => session.open_image_dialog_for_new_inputs = true,
            (true, Some(ch)) => session.open_video_dialog_for_channel = Some(ch),
            (false, Some(ch)) => session.open_image_dialog_for_channel = Some(ch),
        }
        state.open = false;
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new("Files dropped from Finder on a tile go into that input.")
            .small()
            .weak(),
    );
}

/// A playlist: its files chosen now, or an empty one filled in its window.
fn playlist_body(ui: &mut egui::Ui, state: &mut State) {
    ui.label(match &state.layer_of {
        None => {
            "Clips, sound files and pictures played one after another in one input, as \
                 vMix's List — each picture held a while, each clip to its end, with a cut, \
                 fade or slide between."
        }
        Some(_) => {
            "Clips, sound files and pictures played one after another, as a layer of \
                    this input."
        }
    });
    ui.add_space(8.0);
    if ui
        .add(egui::Button::new(
            egui::RichText::new("Choose files…").size(15.0),
        ))
        .clicked()
    {
        let into = match &state.layer_of {
            Some(ch) => super::playlist::FilesFor::Layer(ch.clone()),
            None => super::playlist::FilesFor::NewInput(
                Some(state.name.trim().to_string()).filter(|n| !n.is_empty()),
            ),
        };
        super::playlist::choose_files(ui.ctx(), into);
        state.open = false;
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Or OK for an empty list, filled in its window. Its tile gets ▶ to start it.",
        )
        .small()
        .weak(),
    );
}

/// A PowerPoint deck: chosen, it opens in PowerPoint, which plays it.
fn powerpoint_body(ui: &mut egui::Ui, state: &mut State) {
    ui.label(match &state.layer_of {
        None => {
            "A PowerPoint deck, played by PowerPoint on this Mac in a window Nida shows — \
             its transitions, animations and videos as PowerPoint plays them. ◀ ▶ on its \
             tile, PageDown and PageUp (a presentation clicker) move it on."
        }
        Some(_) => "A PowerPoint deck, played by PowerPoint, as a layer of this input.",
    });
    ui.add_space(8.0);
    if ui
        .add(egui::Button::new(
            egui::RichText::new("Choose a deck…").size(15.0),
        ))
        .clicked()
    {
        let into = match &state.layer_of {
            Some(ch) => super::presentation::DeckFor::Layer(ch.clone()),
            None => super::presentation::DeckFor::NewInput(
                Some(state.name.trim().to_string()).filter(|n| !n.is_empty()),
            ),
        };
        super::presentation::choose_deck(ui.ctx(), into);
        state.open = false;
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "Needs Microsoft PowerPoint on this Mac. The first time, macOS asks whether \
             Nida may control PowerPoint: allow it.",
        )
        .small()
        .weak(),
    );
}

/// A timer: what it counts, and its length or the time it counts to. Its
/// look is set in its window, after.
fn timer_body(ui: &mut egui::Ui, state: &mut State) {
    use crate::engine::value::timer::{
        parse_span, parse_time_of_day, span_text, target_text, TimeFormat, TimerMode,
        MAX_DURATION_SECS,
    };
    ui.label(
        "A count down, a count to a time of day, a count up or the clock, drawn over a \
         see-through background — its font, colours and place set in its window, and its \
         own control panel to put beside any other.",
    );
    ui.add_space(6.0);
    let timer = &mut state.timer;
    for mode in TimerMode::ALL {
        if row(ui, timer.mode == mode, mode.label()).0 {
            timer.mode = mode;
        }
    }
    ui.add_space(6.0);
    match timer.mode {
        TimerMode::Countdown => {
            ui.horizontal(|ui| {
                ui.label("Length");
                ui.add(
                    egui::DragValue::new(&mut timer.duration_secs)
                        .range(1..=MAX_DURATION_SECS)
                        .custom_formatter(|v, _| span_text(v.max(0.0) as u64, TimeFormat::Auto))
                        .custom_parser(|text| parse_span(text).map(f64::from)),
                )
                .on_hover_text("Drag, or click and type: 5, 5:00 or 1:30:00");
            });
        }
        TimerMode::CountTo => {
            ui.horizontal(|ui| {
                ui.label("Counts to");
                ui.add(
                    egui::DragValue::new(&mut timer.target_secs)
                        .range(0..=24 * 3600 - 1)
                        .speed(30.0)
                        .custom_formatter(|v, _| target_text(v.max(0.0) as u32))
                        .custom_parser(|text| parse_time_of_day(text).map(f64::from)),
                )
                .on_hover_text("Drag, or click and type: 9:30, 14:05:30 or 1405");
            });
        }
        TimerMode::CountUp | TimerMode::Clock => {}
    }
}

/// A list row: selected or not, and whether it was clicked or double-clicked.
fn row(ui: &mut egui::Ui, selected: bool, text: &str) -> (bool, bool) {
    let response = ui.add(
        egui::Button::selectable(selected, egui::RichText::new(text).size(14.0))
            .min_size(egui::vec2(ui.available_width(), 24.0)),
    );
    // Clicked just after something else, egui counts a double-click as a
    // third click.
    let twice = response.double_clicked() || response.triple_clicked();
    (response.clicked(), twice)
}

fn refresh(ui: &mut egui::Ui, actions: &mut UIActions, command: EngineCommand) {
    if ui.small_button("Refresh").clicked() {
        actions.commands.push(command);
    }
}

fn camera_body(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
    state: &mut State,
) -> bool {
    ui.horizontal(|ui| {
        ui.label("Cameras and capture cards");
        refresh(ui, actions, EngineCommand::RescanCameras);
    });
    if data.camera_access_denied {
        ui.label(
            egui::RichText::new(
                "macOS has not given this app camera access, so no camera can be \
                 opened. Allow it in System Settings → Privacy & Security → Camera \
                 (Terminal, when the app is started from Terminal), then start the \
                 app again.",
            )
            .color(egui::Color32::from_rgb(240, 120, 100)),
        );
    }
    if !data.camera_video_effects.is_empty() {
        ui.label(
            egui::RichText::new(format!(
                "macOS camera effects are on: {}. They change the camera's picture, \
                 on air too.",
                data.camera_video_effects.join(", ")
            ))
            .color(egui::Color32::from_rgb(240, 180, 90)),
        );
        if ui
            .button("Video Effects…")
            .on_hover_text("Opens macOS's Video Effects for this app, to switch them off")
            .clicked()
        {
            actions.commands.push(EngineCommand::ShowCameraVideoEffects);
        }
    }
    if data.cameras.is_empty() {
        ui.label(
            egui::RichText::new(
                "None found. Plug the camera or capture card in and press Refresh. \
                 On a Mac, the app needs Camera access (System Settings → Privacy \
                 & Security → Camera).",
            )
            .weak(),
        );
        return false;
    }
    if state
        .camera
        .is_none_or(|id| !data.cameras.iter().any(|(_, c)| *c == id))
    {
        state.camera = data.cameras.first().map(|(_, id)| *id);
    }
    let mut double = false;
    for (name, id) in &data.cameras {
        let (clicked, twice) = row(ui, state.camera == Some(*id), name);
        if clicked {
            state.camera = Some(*id);
        }
        double |= twice;
    }
    let Some((camera_name, id)) = data
        .cameras
        .iter()
        .find(|(_, id)| Some(*id) == state.camera)
        .cloned()
    else {
        return double;
    };
    // A camera chosen afresh starts at its best, with its own sound if a
    // sound device goes by its name (a capture card's usually does).
    if state.camera_settled != Some(id) {
        state.camera_settled = Some(id);
        state.camera_mode = None;
        state.camera_audio = own_sound(&camera_name, data).map(|dev| {
            let right = u16::from(dev.channels >= 2);
            (dev.name.clone(), 0, right)
        });
    }
    ui.add_space(8.0);
    camera_format(ui, data, id, state);
    if state.layer_of.is_none() {
        camera_sound(ui, data, state);
    }
    double
}

/// The sound device that is a camera's own: named alike.
fn own_sound<'a>(camera: &str, data: &'a UIData) -> Option<&'a crate::audio_mix::AudioInputDevice> {
    let camera = camera.to_lowercase();
    data.audio_mix.input_devices.iter().find(|dev| {
        let dev = dev.name.to_lowercase();
        !dev.is_empty() && (dev.contains(&camera) || camera.contains(&dev))
    })
}

fn camera_format(ui: &mut egui::Ui, data: &UIData, id: crate::camera::CameraId, state: &mut State) {
    let modes: &[CameraMode] = data
        .camera_modes
        .iter()
        .find(|(c, _)| *c == id)
        .map_or(&[], |(_, m)| m.as_slice());
    let best = crate::engine::value::camera::default_mode(modes);
    let auto = best.map_or_else(|| "Best".to_string(), |m| format!("Best ({})", m.label()));
    ui.horizontal(|ui| {
        let label = ui.label("Format:");
        egui::ComboBox::from_id_salt("add_input_camera_format")
            .width(220.0)
            .selected_text(state.camera_mode.map_or(auto.clone(), |m| m.label()))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.camera_mode, None, auto);
                for mode in modes {
                    ui.selectable_value(&mut state.camera_mode, Some(*mode), mode.label());
                }
            })
            .response
            .labelled_by(label.id);
    });
}

fn camera_sound(ui: &mut egui::Ui, data: &UIData, state: &mut State) {
    let label = |audio: &Option<(String, u16, u16)>| match audio {
        None => "None".to_string(),
        Some((dev, l, r)) if l == r => format!("{dev} ({} mono)", l + 1),
        Some((dev, l, r)) if (*l, *r) == (0, 1) => dev.clone(),
        Some((dev, l, r)) => format!("{dev} ({}-{})", l + 1, r + 1),
    };
    ui.horizontal(|ui| {
        let caption = ui.label("Audio:");
        egui::ComboBox::from_id_salt("add_input_camera_audio")
            .width(220.0)
            .selected_text(label(&state.camera_audio))
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.camera_audio, None, "None");
                for dev in &data.audio_mix.input_devices {
                    let pair = (dev.name.clone(), 0, u16::from(dev.channels >= 2));
                    let text = label(&Some(pair.clone()));
                    ui.selectable_value(&mut state.camera_audio, Some(pair), text);
                }
            })
            .response
            .labelled_by(caption.id)
            .on_hover_text("Heard as the camera goes on air, off as it leaves (Audio Auto)");
    });
}

fn ndi_body(ui: &mut egui::Ui, data: &UIData, state: &mut State) -> bool {
    if !data.ndi_available {
        ui.label(
            egui::RichText::new(
                "NDI is not installed on this computer. Install the NDI runtime \
                 (NDI Tools, from ndi.video) and start the app again.",
            )
            .weak(),
        );
        return false;
    }
    ui.label("NDI sources on the network");
    if data.ndi_sources.is_empty() {
        ui.label(egui::RichText::new("Looking…").weak());
        return false;
    }
    let mut double = false;
    for name in &data.ndi_sources {
        let (clicked, twice) = row(ui, state.ndi.as_ref() == Some(name), name);
        if clicked || twice {
            state.ndi = Some(name.clone());
        }
        double |= twice;
    }
    double
}

/// A capture target as a scene names it: a display by name, a window by its
/// app and title.
fn capture_target(target: &crate::engine::CaptureTargetSnapshot) -> CaptureTargetConfig {
    if target.kind == "display" {
        CaptureTargetConfig::Display {
            name: target.label.clone(),
        }
    } else {
        CaptureTargetConfig::Window {
            app: target.app.clone().unwrap_or_default(),
            title: target.title.clone().unwrap_or_default(),
        }
    }
}

fn screen_body(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
    state: &mut State,
) -> bool {
    if !data.screen_capture_available {
        ui.label(egui::RichText::new("Screen capture is not in this build of the app.").weak());
        return false;
    }
    ui.horizontal(|ui| {
        ui.label("Displays and windows");
        refresh(ui, actions, EngineCommand::RescanCaptureTargets);
    });
    super::library::render_capture_permission(ui, actions, &data.screen_capture_permission);
    let mut double = false;
    for (heading, display) in [("Displays", true), ("Windows", false)] {
        let targets: Vec<_> = data
            .capture_targets
            .iter()
            .filter(|t| (t.kind == "display") == display)
            .collect();
        if targets.is_empty() {
            continue;
        }
        ui.add_space(4.0);
        ui.label(egui::RichText::new(heading).small().weak());
        for target in targets {
            let config = capture_target(target);
            let text = if target.is_varda {
                format!("{} (this app)", target.label)
            } else {
                target.label.clone()
            };
            let (clicked, twice) = row(ui, state.screen.as_ref() == Some(&config), &text);
            if clicked || twice {
                state.screen = Some(config);
            }
            double |= twice;
        }
    }
    if data.capture_targets.is_empty() {
        ui.label(egui::RichText::new("None found yet — press Refresh.").weak());
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.label("Frame rate:");
        ui.add(
            egui::DragValue::new(&mut state.screen_rate)
                .range(1.0..=60.0)
                .speed(0.5)
                .suffix(" fps"),
        );
        ui.checkbox(&mut state.show_cursor, "Show the mouse pointer");
    });
    double
}

fn stream_body(ui: &mut egui::Ui, state: &mut State) {
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.stream, StreamKind::Srt, "SRT");
        ui.selectable_value(&mut state.stream, StreamKind::Rtmp, "RTMP");
        ui.selectable_value(&mut state.stream, StreamKind::Hls, "HLS");
    });
    ui.add_space(6.0);
    let url = match state.stream {
        StreamKind::Srt => {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.srt_mode, SrtMode::Listener, "Listen")
                    .on_hover_text("Wait here for the sender to call in");
                ui.selectable_value(&mut state.srt_mode, SrtMode::Caller, "Call")
                    .on_hover_text("Connect out to the sender");
            });
            &mut state.srt_url
        }
        StreamKind::Rtmp => {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut state.rtmp_mode, RtmpMode::Listen, "Listen")
                    .on_hover_text("OBS or an encoder pushes to this computer");
                ui.selectable_value(&mut state.rtmp_mode, RtmpMode::Pull, "Pull")
                    .on_hover_text("Play a stream from a server");
            });
            &mut state.rtmp_url
        }
        StreamKind::Hls => &mut state.hls_url,
    };
    ui.label("Address:");
    ui.add(
        egui::TextEdit::singleline(url)
            .hint_text("srt:// rtmp:// or https://…m3u8")
            .desired_width(f32::INFINITY),
    );
}

fn audio_body(
    ui: &mut egui::Ui,
    data: &UIData,
    actions: &mut UIActions,
    state: &mut State,
) -> bool {
    ui.horizontal(|ui| {
        ui.label("Sound devices — a microphone, a mixer, a capture card's sound");
        refresh(ui, actions, EngineCommand::RescanAudioDevices);
    });
    let devices = &data.audio_mix.input_devices;
    if devices.is_empty() {
        ui.label(egui::RichText::new("None found — press Refresh.").weak());
        return false;
    }
    let chosen = state.audio.as_ref().map(|(d, _, _)| d.clone());
    let mut double = false;
    for dev in devices {
        let (clicked, twice) = row(ui, chosen.as_ref() == Some(&dev.name), &dev.name);
        if clicked || twice {
            let pair = if dev.channels >= 2 { (0, 1) } else { (0, 0) };
            state.audio = Some((dev.name.clone(), pair.0, pair.1));
        }
        double |= twice;
    }
    if let Some((name, left, right)) = &mut state.audio {
        if let Some(dev) = devices.iter().find(|d| &d.name == name) {
            if dev.channels > 1 {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    ui.label("Channels:");
                    for pair in (0..dev.channels.saturating_sub(1)).step_by(2) {
                        let text = format!("{}-{}", pair + 1, pair + 2);
                        if ui
                            .selectable_label(*left == pair && *right == pair + 1, text)
                            .clicked()
                        {
                            (*left, *right) = (pair, pair + 1);
                        }
                    }
                    for ch in 0..dev.channels {
                        let text = format!("{} mono", ch + 1);
                        if ui
                            .selectable_label(*left == ch && *right == ch, text)
                            .clicked()
                        {
                            (*left, *right) = (ch, ch);
                        }
                    }
                });
            }
        }
    }
    double
}

/// What OK would add, for the live kinds.
fn source(data: &UIData, state: &State) -> Option<InputSource> {
    let url = |u: &str| Some(u.trim().to_string()).filter(|u| !u.is_empty() && !u.ends_with("//"));
    match state.kind {
        Kind::Camera => state
            .camera
            .filter(|id| data.cameras.iter().any(|(_, c)| c == id))
            .map(|camera_id| InputSource::Camera {
                camera_id,
                mode: state.camera_mode,
            }),
        Kind::Ndi => state
            .ndi
            .clone()
            .filter(|_| data.ndi_available)
            .map(|source_name| InputSource::Ndi { source_name }),
        Kind::Screen => state
            .screen
            .clone()
            .filter(|_| data.screen_capture_available)
            .map(|target| InputSource::ScreenCapture {
                target,
                rate: Some(state.screen_rate),
                show_cursor: Some(state.show_cursor),
            }),
        Kind::Stream => match state.stream {
            StreamKind::Srt => url(&state.srt_url).map(|url| InputSource::Srt {
                url,
                mode: state.srt_mode,
            }),
            StreamKind::Rtmp => url(&state.rtmp_url).map(|url| InputSource::Rtmp {
                url,
                mode: state.rtmp_mode,
            }),
            StreamKind::Hls => url(&state.hls_url).map(|url| InputSource::Hls { url }),
        },
        Kind::Web => url(&state.web_url).map(|url| InputSource::Html { url }),
        Kind::Colour => Some(InputSource::Colour {
            color: state.colour,
        }),
        Kind::Generator => state
            .generator
            .clone()
            .filter(|g| data.generators.iter().any(|(name, _)| name == g))
            .map(|shader_name| InputSource::Generator { shader_name }),
        Kind::Input => state
            .input
            .clone()
            .filter(|uuid| input_allowed(data, state, uuid))
            .map(|channel_uuid| InputSource::Input { channel_uuid }),
        // An empty list, filled in its window.
        Kind::Playlist => Some(InputSource::Playlist { items: Vec::new() }),
        Kind::Timer => Some(InputSource::Timer {
            settings: Some(state.timer.clone()),
        }),
        Kind::Video | Kind::Image | Kind::Audio | Kind::PowerPoint => None,
    }
}

fn audio_command(state: &State) -> Option<EngineCommand> {
    if state.kind != Kind::Audio {
        return None;
    }
    let (device, left, right) = state.audio.clone()?;
    let name = Some(state.name.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| device.clone());
    Some(EngineCommand::AddAudioInput {
        name,
        device,
        left,
        right,
    })
}

/// Add what is chosen. True when something was.
fn add(ctx: &egui::Context, data: &UIData, actions: &mut UIActions, state: &State) -> bool {
    if let Some(command) = audio_command(state) {
        actions.commands.push(command);
        return true;
    }
    let Some(source) = source(data, state) else {
        return false;
    };
    let command = match &state.layer_of {
        Some(channel_uuid) => source.deck_command(channel_uuid),
        None => EngineCommand::AddInput {
            audio: match &source {
                InputSource::Camera { .. } => {
                    state
                        .camera_audio
                        .clone()
                        .map(|(device, left, right)| InputAudio {
                            device,
                            left,
                            right,
                        })
                }
                _ => None,
            },
            source,
            name: Some(state.name.trim().to_string()).filter(|n| !n.is_empty()),
        },
    };
    actions.commands.push(command);
    match state.kind {
        Kind::Playlist => super::playlist::open_when_made(ctx, data),
        Kind::Timer => super::timer::open_panel_when_made(ctx, data),
        _ => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    #[derive(Default)]
    struct Seen {
        commands: Vec<EngineCommand>,
        video_new: bool,
        image_new: bool,
        video_for: Option<String>,
    }

    fn data() -> UIData {
        let mut data = UIData::test_fixture();
        data.cameras = vec![("FaceTime HD".into(), 0), ("Blackmagic Design".into(), 1)];
        data.ndi_available = true;
        data.ndi_sources = vec!["STUDIO (Camera 1)".into(), "LAPTOP (Screen)".into()];
        data.screen_capture_available = true;
        data.screen_capture_permission = "granted".into();
        data.capture_targets = vec![crate::engine::CaptureTargetSnapshot {
            kind: "display".into(),
            label: "Display 1".into(),
            app: None,
            title: None,
            width: 1920,
            height: 1080,
            is_varda: false,
        }];
        data.audio_mix.input_devices = vec![
            crate::audio_mix::AudioInputDevice {
                name: "Scarlett Solo".into(),
                channels: 2,
            },
            crate::audio_mix::AudioInputDevice {
                name: "Blackmagic Design".into(),
                channels: 2,
            },
        ];
        let mode = |width, height, fps| CameraMode { width, height, fps };
        data.camera_modes = vec![(
            1,
            vec![
                mode(1920, 1080, 60),
                mode(1920, 1080, 30),
                mode(1280, 720, 60),
            ],
        )];
        data
    }

    fn harness(data: UIData, layer_of: Option<&str>) -> egui_kittest::Harness<'static, Seen> {
        let data = std::rc::Rc::new(data);
        let layer_of = layer_of.map(str::to_string);
        let mut opened = false;
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 600.0))
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                move |ui, seen: &mut Seen| {
                    if !opened {
                        match &layer_of {
                            Some(ch) => open_for_layer(ui.ctx(), ch),
                            None => open(ui.ctx()),
                        }
                        opened = true;
                    }
                    let mut actions = UIActions::new();
                    render_add_input(ui.ctx(), &data, &mut actions);
                    seen.commands.extend(actions.commands);
                    seen.video_new |= actions.session.open_video_dialog_for_new_inputs;
                    seen.image_new |= actions.session.open_image_dialog_for_new_inputs;
                    if let Some(ch) = actions.session.open_video_dialog_for_channel {
                        seen.video_for = Some(ch);
                    }
                },
                Seen::default(),
            );
        harness.run_steps(2);
        harness
    }

    fn click(harness: &mut egui_kittest::Harness<'static, Seen>, label: &str) {
        harness.get_by_label(label).click();
        harness.run_steps(3);
    }

    fn added<'a>(harness: &'a egui_kittest::Harness<'static, Seen>) -> Vec<&'a EngineCommand> {
        harness
            .state()
            .commands
            .iter()
            .filter(|c| {
                !matches!(
                    c,
                    EngineCommand::RescanNdi
                        | EngineCommand::RescanCameras
                        | EngineCommand::RescanCaptureTargets
                        | EngineCommand::RescanAudioDevices
                )
            })
            .collect()
    }

    #[test]
    fn a_chosen_ndi_source_becomes_a_new_input() {
        let mut harness = harness(data(), None);
        click(&mut harness, "NDI");
        assert!(harness
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::RescanNdi)));
        click(&mut harness, "LAPTOP (Screen)");
        click(&mut harness, "OK");

        match added(&harness).as_slice() {
            [EngineCommand::AddInput {
                source: InputSource::Ndi { source_name },
                name: None,
                audio: None,
            }] => assert_eq!(source_name, "LAPTOP (Screen)"),
            other => panic!("expected one AddInput for the NDI source, got {other:?}"),
        }
        assert!(!is_open(&harness.ctx));
    }

    #[test]
    fn a_clock_is_added_as_a_timer_input() {
        let mut harness = harness(data(), None);
        click(&mut harness, "Timer / Clock");
        click(&mut harness, "Time of day");
        click(&mut harness, "OK");
        match added(&harness).as_slice() {
            [EngineCommand::AddInput {
                source:
                    InputSource::Timer {
                        settings: Some(settings),
                    },
                ..
            }] => assert_eq!(settings.mode, crate::engine::value::timer::TimerMode::Clock),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_camera_is_added_under_the_name_typed() {
        let mut harness = harness(data(), None);
        click(&mut harness, "Camera");
        click(&mut harness, "Blackmagic Design");
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .click();
        harness.run_steps(2);
        harness
            .get_by_role(egui::accesskit::Role::TextInput)
            .type_text("Stage cam");
        harness.run_steps(2);
        click(&mut harness, "OK");
        match added(&harness).as_slice() {
            [EngineCommand::AddInput {
                source:
                    InputSource::Camera {
                        camera_id: 1,
                        mode: None,
                    },
                name: Some(name),
                audio,
            }] => {
                assert_eq!(name, "Stage cam");
                assert_eq!(
                    audio.as_ref().map(|a| a.device.as_str()),
                    Some("Blackmagic Design"),
                    "the capture card's own sound, found by its name"
                );
            }
            other => panic!("expected the capture card as a named input, got {other:?}"),
        }
    }

    #[test]
    fn a_cameras_format_and_sound_can_be_chosen() {
        let mut harness = harness(data(), None);
        click(&mut harness, "Camera");
        click(&mut harness, "Blackmagic Design");
        // Starts at its best, with the sound device of the same name.
        let value = |harness: &egui_kittest::Harness<'static, Seen>, label: &str| {
            harness.get_by_label(label).accesskit_node().value()
        };
        assert_eq!(
            value(&harness, "Format:").as_deref(),
            Some("Best (1920×1080 60p)")
        );
        assert_eq!(
            value(&harness, "Audio:").as_deref(),
            Some("Blackmagic Design")
        );
        click(&mut harness, "Format:");
        click(&mut harness, "1280×720 60p");
        click(&mut harness, "Audio:");
        click(&mut harness, "None");
        click(&mut harness, "OK");
        match added(&harness).as_slice() {
            [EngineCommand::AddInput {
                source: InputSource::Camera { camera_id: 1, mode },
                audio: None,
                ..
            }] => assert_eq!(
                *mode,
                Some(CameraMode {
                    width: 1280,
                    height: 720,
                    fps: 60
                })
            ),
            other => panic!("expected the card at 720p60 without sound, got {other:?}"),
        }
    }

    #[test]
    fn double_clicking_a_display_adds_it_at_once() {
        let mut harness = harness(data(), None);
        click(&mut harness, "Desktop Capture");
        let at = harness.get_by_label("Display 1").rect().center();
        harness.hover_at(at);
        harness.run_steps(1);
        for _ in 0..2 {
            harness.drag_at(at);
            harness.run_steps(1);
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        match added(&harness).as_slice() {
            [EngineCommand::AddInput {
                source: InputSource::ScreenCapture { target, .. },
                ..
            }] => assert_eq!(
                *target,
                CaptureTargetConfig::Display {
                    name: "Display 1".into()
                }
            ),
            other => panic!("expected the display as a new input, got {other:?}"),
        }
    }

    #[test]
    fn video_files_are_picked_a_new_input_each() {
        let mut harness = harness(data(), None);
        click(&mut harness, "Video / Sound");
        click(&mut harness, "Choose files…");
        assert!(harness.state().video_new);
        assert!(added(&harness).is_empty());
        assert!(!is_open(&harness.ctx));
    }

    #[test]
    fn a_sound_device_is_an_audio_input() {
        let mut harness = harness(data(), None);
        click(&mut harness, "Audio Input");
        click(&mut harness, "Scarlett Solo");
        click(&mut harness, "OK");
        match added(&harness).as_slice() {
            [EngineCommand::AddAudioInput {
                device,
                left: 0,
                right: 1,
                ..
            }] => assert_eq!(device, "Scarlett Solo"),
            other => panic!("expected the sound device as an audio input, got {other:?}"),
        }
    }

    #[test]
    fn opened_from_a_channel_it_adds_a_layer_there() {
        let data = data();
        let channel = data.channels[0].uuid.clone();
        let mut harness = harness(data, Some(&channel));
        click(&mut harness, "Camera");
        click(&mut harness, "FaceTime HD");
        click(&mut harness, "OK");
        match added(&harness).as_slice() {
            [EngineCommand::AddCameraDeck {
                channel_uuid,
                camera_id: 0,
                mode: None,
            }] => assert_eq!(*channel_uuid, channel),
            other => panic!("expected a camera layer in the channel, got {other:?}"),
        }
        assert!(harness.query_by_label("Audio Input").is_none());
    }

    /// A layer's kinds are a new input's, but a sound device of its own.
    #[test]
    fn a_layer_is_offered_what_a_new_input_is() {
        let channel = data().channels[0].uuid.clone();
        let layer = harness(data(), Some(&channel));
        let input = harness(data(), None);
        for kind in Kind::offered(false) {
            let label = kind.label();
            assert!(input.query_by_label(label).is_some(), "{label}");
            assert_eq!(
                layer.query_by_label(label).is_some(),
                kind != Kind::Audio,
                "{label}"
            );
        }
    }

    #[test]
    fn a_generator_is_found_by_name_and_added() {
        let data = data();
        let (name, _) = data.generators[0].clone();
        let channel = data.channels[0].uuid.clone();
        let mut harness = harness(data, Some(&channel));
        click(&mut harness, "Generator");
        let at = harness
            .get_by_label(&super::super::channel_settings::pretty(&name))
            .rect()
            .center();
        harness.hover_at(at);
        harness.run_steps(1);
        for _ in 0..2 {
            harness.drag_at(at);
            harness.run_steps(1);
            harness.event(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(1);
        }
        match added(&harness).as_slice() {
            [EngineCommand::AddDeck {
                channel_uuid,
                shader_name,
            }] => assert_eq!((channel_uuid, shader_name), (&channel, &name)),
            other => panic!("expected the generator as a layer, got {other:?}"),
        }
    }

    /// Another input goes in as a layer; one that shows this one already
    /// would loop and cannot be picked; as a new input, it is a copy.
    #[test]
    fn another_input_is_a_layer_or_a_copy() {
        let fresh = data();
        let (a, b) = (
            fresh.channels[0].uuid.clone(),
            fresh.channels[1].uuid.clone(),
        );
        let (a_name, b_name) = (
            fresh.channels[0].name.clone(),
            fresh.channels[1].name.clone(),
        );
        let mut harness_ = harness(fresh, Some(&a));
        click(&mut harness_, "Another input");
        assert!(harness_.query_by_label(&a_name).is_none(), "not itself");
        click(&mut harness_, &b_name);
        click(&mut harness_, "OK");
        match added(&harness_).as_slice() {
            [EngineCommand::AddTapDeck {
                channel_uuid,
                source: crate::scene::TapSourceConfig::Channel { uuid },
            }] => assert_eq!((channel_uuid, uuid), (&a, &b)),
            other => panic!("expected the input as a layer, got {other:?}"),
        }

        // B shows A: adding B to A would loop.
        let mut shows_a = data();
        shows_a.channels[1].decks[0].tap = Some(crate::usecases::ui::TapUI {
            kind: "channel".into(),
            channel_uuid: Some(a.clone()),
            label: a_name.clone(),
            bound: true,
        });
        let mut looped = harness(shows_a, Some(&a));
        click(&mut looped, "Another input");
        click(&mut looped, &b_name);
        click(&mut looped, "OK");
        assert!(added(&looped).is_empty(), "{:?}", added(&looped));

        let mut copy = harness(data(), None);
        click(&mut copy, "Another input");
        click(&mut copy, &a_name);
        click(&mut copy, "OK");
        match added(&copy).as_slice() {
            [EngineCommand::AddInput {
                source: InputSource::Input { channel_uuid },
                ..
            }] => assert_eq!(channel_uuid, &a),
            other => panic!("expected a copy of the input, got {other:?}"),
        }
    }

    #[test]
    fn without_camera_access_it_says_where_to_allow_it() {
        let mut data = data();
        data.camera_access_denied = true;
        let mut harness = harness(data, None);
        click(&mut harness, "Camera");
        assert!(harness
            .query_by_label_contains("macOS has not given this app camera access")
            .is_some());
    }

    #[test]
    fn camera_effects_that_are_on_are_named_with_a_way_to_switch_them_off() {
        let mut data = data();
        data.camera_video_effects = vec!["Portrait".into(), "Reactions gestures".into()];
        let mut harness = harness(data, None);
        click(&mut harness, "Camera");
        assert!(harness
            .query_by_label_contains("macOS camera effects are on: Portrait, Reactions gestures")
            .is_some());
        click(&mut harness, "Video Effects…");
        assert!(
            added(&harness)
                .iter()
                .any(|c| matches!(c, EngineCommand::ShowCameraVideoEffects)),
            "the button opens Video Effects"
        );
    }

    #[test]
    fn without_the_ndi_runtime_it_says_how_to_get_it() {
        let mut data = data();
        data.ndi_available = false;
        let mut harness = harness(data, None);
        click(&mut harness, "NDI");
        assert!(harness
            .query_by_label_contains("NDI is not installed")
            .is_some());
        assert!(harness.get_by_label("OK").accesskit_node().is_disabled());
    }

    #[test]
    fn an_empty_playlist_is_a_new_input_and_a_layer() {
        let mut input = harness(data(), None);
        click(&mut input, "Playlist");
        click(&mut input, "OK");
        assert!(
            matches!(
                added(&input).as_slice(),
                [EngineCommand::AddInput {
                    source: InputSource::Playlist { items },
                    ..
                }] if items.is_empty()
            ),
            "{:?}",
            added(&input)
        );

        let mut layer = harness(data(), Some("ca000001"));
        click(&mut layer, "Playlist");
        click(&mut layer, "OK");
        assert!(
            matches!(
                added(&layer).as_slice(),
                [EngineCommand::AddPlaylistDeck { channel_uuid, .. }] if channel_uuid == "ca000001"
            ),
            "{:?}",
            added(&layer)
        );
    }
}
