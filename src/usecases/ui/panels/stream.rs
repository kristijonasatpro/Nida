//! The STREAM button and its window, as in vMix: the button sits in the top
//! bar and shows the stream's health at a glance — plain when off, red when
//! every destination is sending, yellow and blinking when something needs
//! attention. Clicking it opens the window with what is streamed, the four
//! destinations and Start / Stop.

use super::super::{UIActions, UIData};
use crate::engine::value::render::OutputSource;
use crate::engine::EngineCommand;
use super::monitoring::{BAD, GOOD, WARN};
use crate::streaming::{
    DestinationState, DestinationStatus, StreamDestination, StreamOptions, StreamStatus,
    VideoCodec,
};

pub(super) const LIVE_RED: egui::Color32 = egui::Color32::from_rgb(200, 30, 30);
pub(super) const WARN_YELLOW: egui::Color32 = egui::Color32::from_rgb(235, 190, 20);
pub(super) const WARN_DIM: egui::Color32 = egui::Color32::from_rgb(110, 90, 20);
const INVALID_ORANGE: egui::Color32 = egui::Color32::from_rgb(230, 130, 40);

fn open_id() -> egui::Id {
    egui::Id::new("stream_window_open")
}

/// Video bit rates offered, with what they suit.
const BITRATE_PRESETS: [(u32, &str); 7] = [
    (2500, "720p"),
    (4500, "1080p, talking heads"),
    (6000, "1080p"),
    (9000, "1080p60, fast motion"),
    (12000, "1440p"),
    (20000, "4K"),
    (35000, "4K60"),
];

fn stop_armed_id() -> egui::Id {
    egui::Id::new("stream_stop_armed")
}

/// How long a first click on Stop waits for the second.
const STOP_CONFIRM_SECS: f64 = 3.0;

/// `h:mm:ss`.
pub(super) fn clock(secs: f32) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// How the button looks: its fill and text colour, if not the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Look {
    fill: Option<egui::Color32>,
    text: Option<egui::Color32>,
    /// It blinks, so it needs repainting.
    blinking: bool,
}

/// Off: a plain button. Live and healthy: red. Live with anything wrong:
/// yellow, alternating bright and dim with `bright`.
fn look(status: &crate::streaming::StreamStatus, bright: bool) -> Look {
    if !status.live {
        Look {
            fill: None,
            text: None,
            blinking: false,
        }
    } else if status.is_healthy() {
        Look {
            fill: Some(LIVE_RED),
            text: Some(egui::Color32::WHITE),
            blinking: false,
        }
    } else {
        Look {
            fill: Some(if bright { WARN_YELLOW } else { WARN_DIM }),
            text: Some(egui::Color32::BLACK),
            blinking: true,
        }
    }
}

/// The top-bar button. Its colour is the stream's health; a click opens the
/// window.
pub(super) fn render_stream_button(ui: &mut egui::Ui, data: &UIData) {
    let status = &data.stream_status;
    // Twice a second between bright and dim.
    let bright = (ui.input(|i| i.time) * 2.0) as i64 % 2 == 0;
    let look = look(status, bright);
    let text = button_text(status);
    let hover = if status.live {
        summary(data)
    } else {
        "Streaming: set up and start (up to four destinations)".to_string()
    };
    if look.blinking {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
    let mut rich = egui::RichText::new(text).strong();
    if let Some(color) = look.text {
        rich = rich.color(color);
    }
    let mut button = egui::Button::new(rich);
    if let Some(fill) = look.fill {
        button = button.fill(fill);
    }
    let response = ui.add(button).on_hover_text(hover);
    if response.clicked() {
        let open = window_open(ui.ctx());
        ui.ctx()
            .memory_mut(|m| m.data.insert_temp(open_id(), !open));
    }
    if status.live {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(500));
    }
}

fn window_open(ctx: &egui::Context) -> bool {
    ctx.memory(|m| m.data.get_temp::<bool>(open_id()))
        .unwrap_or(false)
}

/// "STREAM", or with its clock while live.
fn button_text(status: &StreamStatus) -> String {
    if status.live {
        format!("STREAM {}", clock(status.live_secs))
    } else {
        "STREAM".to_string()
    }
}

/// How much of the top bar the STREAM button takes now — it widens by its
/// clock when the stream starts — the row's gap after it included.
pub(super) fn button_width(ui: &egui::Ui, data: &UIData) -> f32 {
    button_text_width(ui, button_text(&data.stream_status))
}

/// How much of the row a plain button of `text` takes: the text, its
/// padding either side, and the row's gap after it.
pub(super) fn button_text_width(ui: &egui::Ui, text: String) -> f32 {
    let text = egui::WidgetText::from(egui::RichText::new(text).strong()).into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::FontSelection::Default,
    );
    text.size().x + 2.0 * ui.spacing().button_padding.x + ui.spacing().item_spacing.x
}

/// The stream's health at a glance beside the STREAM button while live, as
/// OBS's status bar shows it: signal bars for the destination doing worst,
/// the rate of the slowest one sending, how many are down, and the frames
/// the viewers did not get — fewer of them in a narrow top bar (`tier`, see
/// [`stats_pieces`]). The details — every destination — are on its hover.
pub(super) fn render_stream_stats(ui: &mut egui::Ui, data: &UIData, tier: u8) {
    let pieces = stats_pieces(data, tier);
    if pieces.is_empty() {
        return;
    }
    let hover = stats_hover(data, needed_kbps(data));
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = STATS_SPACING;
        for piece in pieces {
            match piece {
                Piece::Bars(lit, colour) => {
                    signal_bars(ui, lit, colour).on_hover_text(&hover);
                }
                Piece::Text(text, colour) => {
                    ui.label(piece_text(text, colour)).on_hover_text(&hover);
                }
            }
        }
    });
}

/// How much of the top bar [`render_stream_stats`] takes at `tier`, for it
/// to choose the form that fits.
pub(super) fn stats_width(ui: &egui::Ui, data: &UIData, tier: u8) -> f32 {
    pieces_width(ui, &stats_pieces(data, tier))
}

/// Space between the pieces of the live stats.
pub(super) const STATS_SPACING: f32 = 4.0;

/// One piece of the live stats: signal bars, or a short text in a colour.
pub(super) enum Piece {
    Bars(u8, egui::Color32),
    Text(String, egui::Color32),
}

/// A text piece as its label draws it.
pub(super) fn piece_text(text: impl Into<String>, colour: egui::Color32) -> egui::RichText {
    egui::RichText::new(text).small().color(colour)
}

/// How much of the row `pieces` take as [`render_stream_stats`] lays them
/// out in `ui`: their widths — each text laid out as its label lays it out
/// — the gaps between them, and the row's own gap after them; nothing when
/// there are none, as nothing is drawn.
pub(super) fn pieces_width(ui: &egui::Ui, pieces: &[Piece]) -> f32 {
    if pieces.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_precision_loss)]
    let gaps = (pieces.len() - 1) as f32 * STATS_SPACING + ui.spacing().item_spacing.x;
    gaps + pieces
        .iter()
        .map(|piece| match piece {
            Piece::Bars(..) => BARS_WIDTH,
            Piece::Text(text, colour) => egui::WidgetText::from(piece_text(text.clone(), *colour))
                .into_galley(
                    ui,
                    Some(egui::TextWrapMode::Extend),
                    f32::INFINITY,
                    egui::FontSelection::Default,
                )
                .size()
                .x,
        })
        .sum::<f32>()
}

/// The stream's bit rate: what it was set to send, sound included.
fn needed_kbps(data: &UIData) -> u32 {
    let options = &data.stream_settings.options;
    let status = &data.stream_status;
    // Held Variable or Average, what the picture asks for, as measured.
    let measured = status
        .stream_kbps
        .filter(|_| status.rate_mode.is_some_and(|m| m != crate::streaming::RateMode::Constant));
    if let Some(kbps) = measured {
        return kbps;
    }
    // Following the upload, the stream needs what it is sent at now.
    let video = status.adaptive.map_or(options.video_kbps, |a| a.kbps);
    video + options.audio_kbps
}

/// The live stats at `tier`: 0 everything — bars, rate, destinations down,
/// "Dropped N (x.x%)"; 1 short — the drops as their share, only when there
/// are any; 2 only the bars and those down. None while not live.
fn stats_pieces(data: &UIData, tier: u8) -> Vec<Piece> {
    let status = &data.stream_status;
    if !status.live {
        return Vec::new();
    }
    let (bars, colour) = health(status, worst_destination(data).map(|(_, s)| s));
    let mut pieces = vec![Piece::Bars(bars, colour)];
    if tier < 2 {
        let (kbps, rate_colour) = sending_rate(data);
        pieces.push(Piece::Text(format!("{:.1} Mbps", kbps / 1000.0), rate_colour));
    }
    let down = down_count(data);
    if down > 0 {
        pieces.push(Piece::Text(format!("{down} down"), BAD));
    }
    let dropped = stream_dropped(data);
    match tier {
        0 => pieces.push(Piece::from(dropped_text(dropped))),
        1 if dropped.frames > 0 => {
            let (_, colour) = dropped_text(dropped);
            pieces.push(Piece::Text(format!("{:.1}%", dropped.percent), colour));
        }
        _ => {}
    }
    pieces
}

impl From<(String, egui::Color32)> for Piece {
    fn from((text, colour): (String, egui::Color32)) -> Self {
        Self::Text(text, colour)
    }
}

/// How wide [`signal_bars`] draws.
const BARS_WIDTH: f32 = 15.0;

/// Four bars, the first `lit` of them in `colour`.
fn signal_bars(ui: &mut egui::Ui, lit: u8, colour: egui::Color32) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(BARS_WIDTH, 12.0), egui::Sense::hover());
    let off = ui.visuals().weak_text_color().gamma_multiply(0.5);
    for i in 0..4u8 {
        let height = 3.0 * f32::from(i + 1);
        let bar = egui::Rect::from_min_size(
            egui::pos2(rect.left() + f32::from(i) * 4.0, rect.bottom() - height),
            egui::vec2(3.0, height),
        );
        ui.painter()
            .rect_filled(bar, 0.5, if i < lit { colour } else { off });
    }
    response
}

/// How bad a destination's state is, worst first.
fn rank(state: DestinationState) -> u8 {
    match state {
        DestinationState::Invalid => 0,
        DestinationState::Reconnecting => 1,
        DestinationState::Struggling => 2,
        DestinationState::Connecting => 3,
        DestinationState::Live => 4,
        DestinationState::Off => 5,
    }
}

/// The switched-on destination doing worst, with its place; of two alike,
/// the slower.
fn worst_destination(data: &UIData) -> Option<(usize, &DestinationStatus)> {
    enabled(data).min_by(|(_, a), (_, b)| {
        rank(a.state)
            .cmp(&rank(b.state))
            .then(a.kbps.total_cmp(&b.kbps))
    })
}

/// Every switched-on destination's status, with its place.
fn enabled(data: &UIData) -> impl Iterator<Item = (usize, &DestinationStatus)> {
    data.stream_settings
        .destinations
        .iter()
        .zip(&data.stream_status.destinations)
        .enumerate()
        .filter(|(_, (dest, _))| dest.enabled)
        .map(|(i, (_, status))| (i, status))
}

/// The rate going out where the stream is going out — the slowest of the
/// destinations live or struggling — and its colour. One still connecting
/// has sent nothing yet, so it does not stand for the rate; with none
/// sending, nothing, yellow while one connects, red otherwise.
fn sending_rate(data: &UIData) -> (f32, egui::Color32) {
    let slowest = enabled(data)
        .filter(|(_, s)| matches!(s.state, DestinationState::Live | DestinationState::Struggling))
        .min_by(|(_, a), (_, b)| a.kbps.total_cmp(&b.kbps));
    match slowest {
        Some((_, s)) => (s.kbps, health_of(s.state).1),
        None if enabled(data).any(|(_, s)| s.state == DestinationState::Connecting) => {
            (0.0, WARN)
        }
        None => (0.0, BAD),
    }
}

/// Switched-on destinations not sending: reconnecting, or waiting for their
/// settings.
fn down_count(data: &UIData) -> usize {
    enabled(data)
        .filter(|(_, s)| {
            matches!(
                s.state,
                DestinationState::Reconnecting | DestinationState::Invalid | DestinationState::Off
            )
        })
        .count()
}

/// A state's signal bars (0–4) and colour: four green while live, fewer and
/// yellow while connecting or struggling, one red while reconnecting.
fn health_of(state: DestinationState) -> (u8, egui::Color32) {
    match state {
        DestinationState::Live => (4, GOOD),
        DestinationState::Connecting => (3, WARN),
        DestinationState::Struggling => (2, WARN),
        DestinationState::Reconnecting => (1, BAD),
        DestinationState::Invalid | DestinationState::Off => (0, BAD),
    }
}

/// The stream's signal bars: the worst destination's, at most two while the
/// encoder itself has a problem.
fn health(status: &StreamStatus, worst: Option<&DestinationStatus>) -> (u8, egui::Color32) {
    let (bars, colour) = worst.map_or((0, BAD), |w| health_of(w.state));
    if status.encoder_problem.is_some() && bars > 2 {
        (2, WARN)
    } else {
        (bars, colour)
    }
}

/// Frames the viewers did not get, at the rate the stream is sent, and their
/// share of all sent; whether the count is an estimate.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Dropped {
    frames: u64,
    estimated: bool,
    percent: f32,
}

/// What the viewers did not get, counted as time first, since the two parts
/// come at different rates: frames the encoder could not take, which arrive
/// at Nida's own rate (the encoder's input — 60 when uncapped), and the
/// stream a slow upload threw away at the destination that lost most — its
/// time reckoned as it was lost, at the bit rate the stream had then. That
/// time is then frames at the rate the stream is sent at.
fn stream_dropped(data: &UIData) -> Dropped {
    let status = &data.stream_status;
    // The rates the encoder was started at — the target may have changed
    // since — or, not known, what they would be now.
    let input_fps = if status.encoder_fps > 0 {
        status.encoder_fps
    } else {
        crate::app::state::encoder_fps(data.target_fps)
    };
    let out_fps = if status.stream_fps > 0 {
        status.stream_fps
    } else {
        data.stream_settings.options.fps.unwrap_or(input_fps).max(1)
    };
    let upload_secs = enabled(data)
        .map(|(_, s)| f64::from(s.dropped_secs))
        .fold(0.0, f64::max);
    // The time the encoder's dropped frames covered, as measured when they
    // were dropped; not reported, their count at the encoder's rate.
    #[allow(clippy::cast_precision_loss)]
    let encoder_secs = if status.encoder_dropped_secs > 0.0 {
        f64::from(status.encoder_dropped_secs)
    } else {
        status.encoder_dropped as f64 / f64::from(input_fps)
    };
    let lost_secs = upload_secs + encoder_secs;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let frames = (lost_secs * f64::from(out_fps)).round() as u64;
    let live = f64::from(status.live_secs);
    #[allow(clippy::cast_possible_truncation)]
    let percent = if live > 0.0 {
        (lost_secs / live * 100.0).min(100.0) as f32
    } else {
        0.0
    };
    Dropped {
        frames,
        estimated: upload_secs > 0.0 || frames != status.encoder_dropped,
        percent,
    }
}

/// "Dropped 0 (0.0%)", "~" before an estimate; grey while none, yellow
/// below 1 %, red from there.
fn dropped_text(dropped: Dropped) -> (String, egui::Color32) {
    let tilde = if dropped.estimated { "~" } else { "" };
    let text = format!(
        "Dropped {tilde}{} ({:.1}%)",
        dropped.frames, dropped.percent
    );
    let colour = if dropped.frames == 0 {
        egui::Color32::GRAY
    } else if dropped.percent < 1.0 {
        WARN
    } else {
        BAD
    };
    (text, colour)
}

/// Every destination switched on, one line each, and the encoder: the
/// stats' hover.
fn stats_hover(data: &UIData, needed_kbps: u32) -> String {
    let status = &data.stream_status;
    let mut lines = Vec::new();
    for (i, (dest, s)) in data
        .stream_settings
        .destinations
        .iter()
        .zip(&status.destinations)
        .enumerate()
    {
        if !dest.enabled {
            continue;
        }
        let mut line = format!(
            "{} {}: {}{}",
            i + 1,
            name_of(i, dest),
            s.state.label(),
            rate_text(s, needed_kbps)
        );
        if let Some(trouble) = trouble_text(s) {
            line.push_str(" — ");
            line.push_str(&trouble);
        }
        lines.push(line);
    }
    if status.encoder_dropped > 0 {
        lines.push(format!(
            "Encoder: {} frames it could not take, each covered by the one before",
            status.encoder_dropped
        ));
    }
    if let Some(problem) = &status.encoder_problem {
        lines.push(format!("Encoder: {problem}"));
    }
    lines.join("\n")
}

/// One line per destination switched on, for the button's tooltip.
fn summary(data: &UIData) -> String {
    let mut lines = Vec::new();
    if let Some(problem) = &data.stream_status.encoder_problem {
        lines.push(format!("Encoder: {problem}"));
    }
    for (i, (dest, status)) in data
        .stream_settings
        .destinations
        .iter()
        .zip(&data.stream_status.destinations)
        .enumerate()
    {
        if !dest.enabled {
            continue;
        }
        let mut line = format!("{} {}: {}", i + 1, name_of(i, dest), status.state.label());
        if let Some(message) = &status.message {
            line.push_str(" — ");
            line.push_str(message);
        }
        lines.push(line);
    }
    lines.join("\n")
}

fn name_of(i: usize, dest: &StreamDestination) -> String {
    if dest.name.trim().is_empty() {
        format!("Destination {}", i + 1)
    } else {
        dest.name.clone()
    }
}

/// The stream window, when open.
pub(super) fn render_stream_window(ctx: &egui::Context, data: &UIData, actions: &mut UIActions) {
    if !window_open(ctx) {
        return;
    }
    let mut open = true;
    egui::Window::new("Stream")
        .id(egui::Id::new("stream_window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        // Tall enough for all four destinations, and below the top bar so
        // the STREAM button's colour stays in sight.
        .default_pos(egui::pos2(240.0, 44.0))
        .default_size(egui::vec2(620.0, 800.0))
        .show(ctx, |ui| {
            // Status and Start / Stop stay in view; the settings scroll.
            render_header(ui, data, actions);
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("stream_window_scroll")
                .show(ui, |ui| render_body(ui, data, actions));
        });
    if !open {
        ctx.memory_mut(|m| m.data.insert_temp(open_id(), false));
    }
}

fn render_header(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let status = &data.stream_status;
    ui.horizontal(|ui| {
        render_start_stop(ui, data, actions);
        if status.live {
            let color = if status.is_healthy() {
                LIVE_RED
            } else {
                WARN_YELLOW
            };
            ui.label(
                egui::RichText::new(format!(" LIVE {} ", clock(status.live_secs)))
                    .strong()
                    .color(egui::Color32::WHITE)
                    .background_color(color),
            );
        } else {
            ui.label(egui::RichText::new("Not streaming").strong());
        }
    });
    if let Some(problem) = &status.encoder_problem {
        ui.colored_label(WARN_YELLOW, format!("⚠ {problem}"));
    }
    if let Some((text, color)) = adapting_text(status) {
        ui.colored_label(color, text);
    }
    if status.live && status.encoder_dropped > 0 {
        ui.label(
            egui::RichText::new(format!(
                "Encoder: {} frames dropped since the start (each covered by repeating the one before)",
                status.encoder_dropped
            ))
            .small()
            .weak(),
        );
    }
}

/// The video bit rate while the upload holds it down: following it, or as
/// low as it goes and still too much. Nothing at the rate set.
fn adapting_text(status: &StreamStatus) -> Option<(String, egui::Color32)> {
    let adaptive = status.adaptive.filter(|a| status.live && a.lowered())?;
    let mbps = |kbps: u32| format!("{:.1}", f64::from(kbps) / 1000.0);
    // As low as it goes is news only while an upload still struggles.
    let struggling = status
        .destinations
        .iter()
        .any(|d| d.state == DestinationState::Struggling);
    Some(if adaptive.at_floor() && struggling {
        (
            format!(
                "⚠ Bit rate as low as it goes: {} of {} Mbps — the upload still cannot carry the stream",
                mbps(adaptive.kbps),
                mbps(adaptive.asked_kbps)
            ),
            LIVE_RED,
        )
    } else {
        (
            format!(
                "Adapting to the upload: {} of {} Mbps",
                mbps(adaptive.kbps),
                mbps(adaptive.asked_kbps)
            ),
            WARN_YELLOW,
        )
    })
}

fn render_body(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let status = &data.stream_status;
    let settings = &data.stream_settings;

    render_options(ui, &settings.options, status.live, data, actions);
    ui.add_space(6.0);
    ui.label(egui::RichText::new("Destinations").strong());
    ui.label(
        egui::RichText::new(
            "From the platform: the server URL (rtmp:// or rtmps://) and the stream key. \
             Username and password only if the server asks for a login.",
        )
        .small()
        .weak(),
    );
    // What the stream needs of each upload: its settings are locked while
    // live, so they are what is being sent — or, following the upload, the
    // rate it is sent at now.
    let needed_kbps = needed_kbps(data);
    for (i, dest) in settings.destinations.iter().enumerate() {
        let dest_status = status
            .destinations
            .get(i)
            .cloned()
            .unwrap_or_else(DestinationStatus::off);
        render_destination(ui, i, dest, &dest_status, needed_kbps, actions);
    }
}

pub(super) fn source_label(source: &OutputSource, data: &UIData) -> String {
    match source {
        OutputSource::BusProgram(uuid) => {
            data.buses.iter().find(|b| &b.bus.uuid == uuid).map_or_else(
                || "Missing bus".to_string(),
                |b| format!("{} (bus)", b.bus.name),
            )
        }
        _ => "Master (main output)".to_string(),
    }
}

fn render_options(
    ui: &mut egui::Ui,
    options: &StreamOptions,
    live: bool,
    data: &UIData,
    actions: &mut UIActions,
) {
    let set = |actions: &mut UIActions, options: StreamOptions| {
        actions
            .commands
            .push(EngineCommand::SetStreamOptions { options });
    };
    egui::Grid::new("stream_options")
        .num_columns(2)
        .spacing([8.0, 6.0])
        .show(ui, |ui| {
            ui.label("Picture");
            egui::ComboBox::from_id_salt("stream_source")
                .selected_text(source_label(&options.source, data))
                .width(220.0)
                .show_ui(ui, |ui| {
                    let mut pick = |ui: &mut egui::Ui, source: OutputSource| {
                        let label = source_label(&source, data);
                        if ui
                            .selectable_label(options.source == source, label)
                            .clicked()
                            && options.source != source
                        {
                            set(
                                actions,
                                StreamOptions {
                                    source,
                                    ..options.clone()
                                },
                            );
                        }
                    };
                    pick(ui, OutputSource::Master);
                    for bus in &data.buses {
                        pick(ui, OutputSource::BusProgram(bus.bus.uuid.clone()));
                    }
                })
                .response
                .on_hover_text("What goes out — can be changed while live");
            ui.end_row();

            ui.label("Sound");
            ui.add_enabled_ui(!live, |ui| {
                let buses = &data.audio_mix.buses;
                let current = options
                    .audio_bus
                    .as_deref()
                    .and_then(|u| buses.iter().find(|b| b.config.uuid == u))
                    .or(buses.first())
                    .map_or("Silence (no audio bus)".to_string(), |b| {
                        b.config.name.clone()
                    });
                egui::ComboBox::from_id_salt("stream_audio")
                    .selected_text(current)
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for (i, bus) in buses.iter().enumerate() {
                            let chosen = options.audio_bus.as_deref() == Some(&bus.config.uuid)
                                || (options.audio_bus.is_none() && i == 0);
                            if ui.selectable_label(chosen, &bus.config.name).clicked() && !chosen {
                                set(
                                    actions,
                                    StreamOptions {
                                        audio_bus: Some(bus.config.uuid.clone()),
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Size");
            ui.add_enabled_ui(!live, |ui| {
                let mut sizes = vec![(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];
                if !sizes.contains(&(data.render_width, data.render_height)) {
                    sizes.push((data.render_width, data.render_height));
                }
                egui::ComboBox::from_id_salt("stream_size")
                    .selected_text(format!("{}×{}", options.width, options.height))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for (w, h) in sizes {
                            let chosen = (options.width, options.height) == (w, h);
                            if ui.selectable_label(chosen, format!("{w}×{h}")).clicked() && !chosen
                            {
                                set(
                                    actions,
                                    StreamOptions {
                                        width: w,
                                        height: h,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Video");
            // The bit rate changes while live for a stream encoded in Nida;
            // the codec needs a new encoder.
            let rate_editable = !live || data.stream_status.rate_live;
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!live, |ui| {
                    egui::ComboBox::from_id_salt("stream_codec")
                        .selected_text(options.codec.label())
                        .width(120.0)
                        .show_ui(ui, |ui| {
                            for codec in [VideoCodec::H264, VideoCodec::Hevc] {
                                let chosen = options.codec == codec;
                                if ui.selectable_label(chosen, codec.label()).clicked() && !chosen {
                                    set(
                                        actions,
                                        StreamOptions {
                                            codec,
                                            ..options.clone()
                                        },
                                    );
                                }
                            }
                        });
                });
                ui.add_enabled_ui(rate_editable, |ui| {
                    let mut kbps = options.video_kbps;
                    let drag = ui
                        .add(
                            egui::DragValue::new(&mut kbps)
                                .range(300..=50_000)
                                .speed(50)
                                .suffix(" kbps"),
                        )
                        .on_hover_text(if live {
                            "Changes while live, from the next frame"
                        } else {
                            "The video bit rate: with the Hardware encoder it can be changed while live too"
                        })
                        .on_disabled_hover_text(
                            "This stream's encoder cannot change its bit rate while live: stop the stream to change it",
                        );
                    if drag.changed() && kbps != options.video_kbps {
                        set(
                            actions,
                            StreamOptions {
                                video_kbps: kbps,
                                ..options.clone()
                            },
                        );
                    }
                    ui.menu_button("Presets", |ui| {
                        for (kbps, what) in BITRATE_PRESETS {
                            if ui.button(format!("{kbps} kbps — {what}")).clicked() {
                                set(
                                    actions,
                                    StreamOptions {
                                        video_kbps: kbps,
                                        ..options.clone()
                                    },
                                );
                                ui.close();
                            }
                        }
                    });
                });
            });
            ui.end_row();

            ui.label("Rate");
            ui.add_enabled_ui(rate_editable, |ui| {
                ui.horizontal(|ui| {
                    use crate::streaming::RateMode;
                    // Live, Constant and Variable trade places (one encoding,
                    // padded or not); Average waits for a stop.
                    let held = |m: RateMode| matches!(m, RateMode::Constant | RateMode::Variable);
                    egui::ComboBox::from_id_salt("stream_rate_mode")
                        .selected_text(options.rate_mode.label())
                        .width(150.0)
                        .show_ui(ui, |ui| {
                            for mode in RateMode::ALL {
                                let chosen = options.rate_mode == mode;
                                // Live, as the encoder runs it: a Constant set but
                                // sent as Average (see the note) has nothing to
                                // trade with.
                                let running = data.stream_status.rate_mode.is_some_and(held);
                                let open = !live || (held(mode) && held(options.rate_mode) && running);
                                let item = ui
                                    .add_enabled(open, egui::Button::selectable(chosen, mode.label()))
                                    .on_hover_text(rate_mode_hover(mode))
                                    .on_disabled_hover_text(
                                        "Stop the stream to change to or from Average: it is encoded apart",
                                    );
                                if item.clicked() && !chosen {
                                    set(
                                        actions,
                                        StreamOptions {
                                            rate_mode: mode,
                                            ..options.clone()
                                        },
                                    );
                                }
                            }
                        })
                        .response
                        .on_hover_text(rate_mode_hover(options.rate_mode));
                    if let Some(note) = &data.stream_status.rate_note {
                        ui.label(egui::RichText::new(note).small().color(WARN_YELLOW));
                    }
                });
            });
            ui.end_row();
            for note in platform_notes(options, &data.stream_settings.destinations) {
                ui.label("");
                ui.label(egui::RichText::new(note).small().color(WARN_YELLOW));
                ui.end_row();
            }

            ui.label("");
            ui.add_enabled_ui(rate_editable, |ui| {
                let hardware = options.encoder == crate::streaming::StreamEncoder::Hardware;
                let mut on = options.adaptive && hardware;
                let check = ui
                    .add_enabled(
                        hardware,
                        egui::Checkbox::new(&mut on, "Lower it while the upload cannot keep up"),
                    )
                    .on_hover_text(
                        "When a destination's upload falls behind, the video bit rate comes down \
                         until it keeps up, and goes back up step by step after — a softer \
                         picture instead of one breaking up. Off: the rate stays as set. \
                         With the Hardware encoder it can be switched while live.",
                    )
                    .on_disabled_hover_text("Needs the hardware encoder");
                if check.changed() {
                    set(
                        actions,
                        StreamOptions {
                            adaptive: on,
                            ..options.clone()
                        },
                    );
                }
            });
            ui.end_row();

            ui.label("Encoder");
            ui.add_enabled_ui(!live, |ui| {
                egui::ComboBox::from_id_salt("stream_encoder")
                    .selected_text(options.encoder.label())
                    .width(220.0)
                    .show_ui(ui, |ui| {
                        for enc in [
                            crate::streaming::StreamEncoder::Hardware,
                            crate::streaming::StreamEncoder::Software,
                        ] {
                            let chosen = options.encoder == enc;
                            let item = ui.add_enabled(
                                enc.available(),
                                egui::Button::selectable(chosen, enc.label()),
                            );
                            if item.clicked() && !chosen {
                                set(
                                    actions,
                                    StreamOptions {
                                        encoder: enc,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    });
            });
            ui.end_row();

            ui.label("Frame rate");
            ui.add_enabled_ui(!live, |ui| {
                let label = |fps: Option<u32>| {
                    fps.map_or_else(|| "Same as Nida".to_string(), |f| format!("{f} fps"))
                };
                egui::ComboBox::from_id_salt("stream_fps")
                    .selected_text(label(options.fps))
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for fps in [None, Some(24), Some(25), Some(30), Some(50), Some(60)] {
                            let chosen = options.fps == fps;
                            if ui.selectable_label(chosen, label(fps)).clicked() && !chosen {
                                set(
                                    actions,
                                    StreamOptions {
                                        fps,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "Frames per second sent. Lower than Nida's own rate halves the upload \
                         for motion that does not need it (a talk at 30 or 25)",
                    );
            });
            ui.end_row();
            if options.codec == VideoCodec::Hevc {
                ui.label("");
                ui.label(
                    egui::RichText::new(
                        "HEVC needs a destination that takes it over Enhanced RTMP (YouTube). \
                         Most others need H.264.",
                    )
                    .small()
                    .color(WARN_YELLOW),
                );
                ui.end_row();
            }

            ui.label("Audio");
            ui.add_enabled_ui(!live, |ui| {
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("stream_audio_kbps")
                        .selected_text(format!("{} kbps", options.audio_kbps))
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for kbps in [96, 128, 160, 192, 256, 320] {
                                let chosen = options.audio_kbps == kbps;
                                if ui
                                    .selectable_label(chosen, format!("{kbps} kbps"))
                                    .clicked()
                                    && !chosen
                                {
                                    set(
                                        actions,
                                        StreamOptions {
                                            audio_kbps: kbps,
                                            ..options.clone()
                                        },
                                    );
                                }
                            }
                        });
                    ui.label("Keyframe every");
                    egui::ComboBox::from_id_salt("stream_keyframe")
                        .selected_text(format!("{} s", options.keyframe_secs))
                        .width(60.0)
                        .show_ui(ui, |ui| {
                            for secs in [1, 2, 3, 4] {
                                let chosen = options.keyframe_secs == secs;
                                if ui.selectable_label(chosen, format!("{secs} s")).clicked()
                                    && !chosen
                                {
                                    set(
                                        actions,
                                        StreamOptions {
                                            keyframe_secs: secs,
                                            ..options.clone()
                                        },
                                    );
                                }
                            }
                        });
                });
            });
            ui.end_row();

            ui.label("Network buffer");
            ui.add_enabled_ui(!live, |ui| {
                egui::ComboBox::from_id_salt("stream_buffer")
                    .selected_text(format!("{} s", options.buffer_secs))
                    .width(60.0)
                    .show_ui(ui, |ui| {
                        for secs in [3, 10, 20, 30] {
                            let chosen = options.buffer_secs == secs;
                            if ui.selectable_label(chosen, format!("{secs} s")).clicked() && !chosen
                            {
                                set(
                                    actions,
                                    StreamOptions {
                                        buffer_secs: secs,
                                        ..options.clone()
                                    },
                                );
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "How far each destination may fall behind when the internet dips, \
                         and catch up afterwards, before any of the stream is lost. \
                         Bigger rides out longer dips; an upload too slow all the time \
                         needs a lower bit rate instead.",
                    );
            });
            ui.end_row();
        });
    if live {
        let note = if data.stream_status.rate_live {
            "Stop the stream to change the sound, size, codec or encoder; the video bit rate changes while live"
        } else {
            "Stop the stream to change the sound, size, bit rate or encoder"
        };
        ui.label(egui::RichText::new(note).small().weak());
    }
}

/// A destination URL's host: `rtmp[s]://[user:password@]host[:port]/…`.
fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next()?;
    let host = authority.rsplit('@').next()?.split(':').next()?;
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// What a platform among the destinations switched on asks for that the
/// stream's settings do not give it, by the URL's host — advice, from the
/// platforms' own pages (see docs/17-streaming.md), not a block.
fn platform_notes(options: &StreamOptions, destinations: &[StreamDestination]) -> Vec<String> {
    use crate::streaming::RateMode;
    let on = |host: &str| {
        destinations
            .iter()
            .filter(|d| d.enabled)
            .filter_map(|d| url_host(&d.url))
            .any(|h| h == host || h.ends_with(&format!(".{host}")))
    };
    let mut notes = Vec::new();
    if on("youtube.com") && options.rate_mode != RateMode::Constant {
        notes.push("YouTube asks for Constant (CBR)".to_string());
    }
    if on("live.cloudflare.com") {
        let total = options.video_kbps + options.audio_kbps;
        let mut asks = Vec::new();
        if options.rate_mode != RateMode::Constant {
            asks.push("Constant (CBR)".to_string());
        }
        if options.codec != VideoCodec::H264 {
            asks.push("H.264".to_string());
        }
        if !(2000..=14_000).contains(&total) {
            asks.push(format!("2–14 Mbps with the sound (set: {:.1})", f64::from(total) / 1000.0));
        }
        if !asks.is_empty() {
            notes.push(format!("Cloudflare Stream asks for {}", asks.join(", ")));
        }
        // Within it as set, a stream can still go under it.
        let under = match (options.adaptive, options.rate_mode == RateMode::Variable) {
            (true, true) => Some("lowered for a slow upload, or Variable on slides,"),
            (true, false) => Some("lowered for a slow upload,"),
            (false, true) => Some("Variable on slides,"),
            (false, false) => None,
        };
        if let Some(how) = under.filter(|_| total >= 2000) {
            notes.push(format!("Cloudflare Stream asks for 2 Mbps at least: {how} it can go under"));
        }
    }
    notes
}

/// What a rate mode does, as measured (see docs/17-streaming.md).
fn rate_mode_hover(mode: crate::streaming::RateMode) -> &'static str {
    use crate::streaming::RateMode;
    match mode {
        RateMode::Constant => {
            "The bit rate set, held — padded where the picture needs less. What YouTube and \
             Cloudflare Stream ask for. On a talk's slides and camera bubbles the media engine \
             pads three quarters of it."
        }
        RateMode::Variable => {
            "Constant's picture without the padding: much less on slides and stills, about the \
             rate set on busy pictures. Some platforms want a steady rate, or a least one \
             (Cloudflare Stream: 2 Mbps)."
        }
        RateMode::Average => {
            "The bit rate set on average: a slightly better picture, but busy stretches go over \
             it — a quarter over in a second on normal motion, much more on noisy pictures. \
             Needs room in the upload."
        }
    }
}

/// A text field that edits its own copy and hands back the new value once,
/// when the operator leaves the field or presses Enter — one command per
/// edit, not one per keystroke, and never a half-typed URL sent live.
fn committed_text(
    ui: &mut egui::Ui,
    id: egui::Id,
    current: &str,
    hidden: bool,
    hint: &str,
    width: f32,
) -> Option<String> {
    let mut text: String = ui
        .data(|d| d.get_temp::<String>(id))
        .unwrap_or_else(|| current.to_string());
    let response = ui.add(
        egui::TextEdit::singleline(&mut text)
            .id(id)
            .password(hidden)
            .hint_text(hint)
            .desired_width(width),
    );
    if response.lost_focus() {
        ui.data_mut(|d| d.remove_temp::<String>(id));
        return (text != current).then_some(text);
    }
    if response.has_focus() {
        ui.data_mut(|d| d.insert_temp(id, text));
    } else {
        ui.data_mut(|d| d.remove_temp::<String>(id));
    }
    None
}

fn state_color(state: DestinationState) -> egui::Color32 {
    match state {
        DestinationState::Off => egui::Color32::GRAY,
        DestinationState::Live => LIVE_RED,
        DestinationState::Invalid => INVALID_ORANGE,
        DestinationState::Connecting
        | DestinationState::Struggling
        | DestinationState::Reconnecting => WARN_YELLOW,
    }
}

/// The rate going out, and — while it struggles below what the stream
/// needs — that too ("1.6 of 2.7 Mbps"): an operator streaming a still timer
/// over a phone's 5G doubted a bare "too slow", which was right.
fn rate_text(status: &DestinationStatus, needed_kbps: u32) -> String {
    let sent = format!("{:.1}", status.kbps / 1000.0);
    #[allow(clippy::cast_precision_loss)]
    let needed = format!("{:.1}", needed_kbps as f32 / 1000.0);
    // Compared as shown, too, so it never reads "2.7 of 2.7".
    #[allow(clippy::cast_precision_loss)]
    let below = status.kbps < needed_kbps as f32 && sent != needed;
    if status.state == DestinationState::Struggling && below {
        format!("  {sent} of {needed} Mbps")
    } else {
        format!("  {sent} Mbps")
    }
}

/// How much of the stream a slow upload has thrown away, as time: a count of
/// pieces of stream meant nothing to an operator.
fn lost_text(lost_secs: f32) -> Option<String> {
    if lost_secs <= 0.0 {
        return None;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let secs = lost_secs.round() as u64;
    let span = match secs {
        0 => "under a second".to_string(),
        s if s < 60 => format!("about {s} s"),
        s => format!("about {} min {} s", s / 60, s % 60),
    };
    Some(format!("{span} of the stream lost to a slow upload"))
}

/// The card's last line: reconnections and stream lost, when there are any.
fn trouble_text(status: &DestinationStatus) -> Option<String> {
    let reconnected = match status.reconnects {
        0 => None,
        1 => Some("Reconnected once".to_string()),
        n => Some(format!("Reconnected {n} times")),
    };
    match (reconnected, lost_text(status.dropped_secs)) {
        (Some(r), Some(lost)) => Some(format!("{r} · {lost}")),
        (Some(r), None) => Some(r),
        (None, Some(lost)) => {
            let mut chars = lost.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect())
        }
        (None, None) => None,
    }
}

fn render_destination(
    ui: &mut egui::Ui,
    i: usize,
    dest: &StreamDestination,
    status: &DestinationStatus,
    needed_kbps: u32,
    actions: &mut UIActions,
) {
    let command = |dest: &StreamDestination, key: Option<String>, password: Option<String>| {
        EngineCommand::SetStreamDestination {
            index: i,
            enabled: dest.enabled,
            name: dest.name.clone(),
            url: dest.url.clone(),
            key,
            username: dest.username.clone(),
            password,
        }
    };
    let reveal_id = egui::Id::new("stream_reveal").with(i);
    let reveal = ui.data(|d| d.get_temp::<bool>(reveal_id)).unwrap_or(false);
    ui.push_id(("stream_destination", i), |ui| {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let mut on = dest.enabled;
                if ui
                    .checkbox(&mut on, egui::RichText::new(format!("{}", i + 1)).strong())
                    .on_hover_text("Send to this destination — can be switched while live")
                    .changed()
                {
                    let edited = StreamDestination {
                        enabled: on,
                        ..dest.clone()
                    };
                    actions.commands.push(command(&edited, None, None));
                }
                if let Some(name) = committed_text(
                    ui,
                    ui.id().with("name"),
                    &dest.name,
                    false,
                    "Name, e.g. YouTube",
                    160.0,
                ) {
                    let edited = StreamDestination {
                        name,
                        ..dest.clone()
                    };
                    actions.commands.push(command(&edited, None, None));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let mut text = status.state.label().to_string();
                    if status.state == DestinationState::Live
                        || status.state == DestinationState::Struggling
                    {
                        text.push_str(&rate_text(status, needed_kbps));
                    }
                    ui.label(
                        egui::RichText::new(text)
                            .color(state_color(status.state))
                            .strong(),
                    );
                });
            });
            egui::Grid::new("fields")
                .num_columns(2)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    ui.label("URL");
                    if let Some(url) = committed_text(
                        ui,
                        ui.id().with("url"),
                        &dest.url,
                        false,
                        "rtmps://live.example.com/live",
                        360.0,
                    ) {
                        let edited = StreamDestination {
                            url,
                            ..dest.clone()
                        };
                        actions.commands.push(command(&edited, None, None));
                    }
                    ui.end_row();

                    ui.label("Stream key");
                    ui.horizontal(|ui| {
                        if let Some(key) = committed_text(
                            ui,
                            ui.id().with("key"),
                            &dest.key,
                            !reveal,
                            "from the platform",
                            260.0,
                        ) {
                            actions.commands.push(command(dest, Some(key), None));
                        }
                        let eye = if reveal { "Hide" } else { "Show" };
                        if ui
                            .small_button(eye)
                            .on_hover_text("Show or hide the key and password")
                            .clicked()
                        {
                            ui.data_mut(|d| d.insert_temp(reveal_id, !reveal));
                        }
                    });
                    ui.end_row();

                    ui.label("Login");
                    ui.horizontal(|ui| {
                        if let Some(username) = committed_text(
                            ui,
                            ui.id().with("username"),
                            &dest.username,
                            false,
                            "username (if asked)",
                            150.0,
                        ) {
                            let edited = StreamDestination {
                                username,
                                ..dest.clone()
                            };
                            actions.commands.push(command(&edited, None, None));
                        }
                        if let Some(password) = committed_text(
                            ui,
                            ui.id().with("password"),
                            &dest.password,
                            !reveal,
                            "password",
                            150.0,
                        ) {
                            actions.commands.push(command(dest, None, Some(password)));
                        }
                    });
                    ui.end_row();
                });
            if let Some(message) = &status.message {
                let response = ui.colored_label(state_color(status.state), message);
                if let Some(detail) = &status.detail {
                    response.on_hover_text(detail);
                }
            }
            if let Some(trouble) = trouble_text(status) {
                ui.label(egui::RichText::new(trouble).small().weak());
            }
        });
    });
}

fn render_start_stop(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let live = data.stream_status.live;
    let now = ui.input(|i| i.time);
    if !live {
        let any_on = data.stream_settings.destinations.iter().any(|d| d.enabled);
        let start = ui
            .add_enabled(
                any_on,
                egui::Button::new(egui::RichText::new("Start streaming").strong())
                    .min_size(egui::vec2(180.0, 30.0)),
            )
            .on_hover_text("Send to every destination switched on")
            .on_disabled_hover_text("Switch on a destination first");
        if start.clicked() {
            actions.commands.push(EngineCommand::StartStream);
        }
        return;
    }
    // Stopping a live stream by accident ends the event for everyone
    // watching: the first click arms, the second within three seconds stops.
    let armed_at = ui.data(|d| d.get_temp::<f64>(stop_armed_id()));
    let armed = armed_at.is_some_and(|at| now - at < STOP_CONFIRM_SECS);
    let text = if armed {
        "Click again to stop"
    } else {
        "Stop streaming"
    };
    let stop = ui
        .add(
            egui::Button::new(
                egui::RichText::new(text)
                    .strong()
                    .color(egui::Color32::WHITE),
            )
            .fill(LIVE_RED)
            .min_size(egui::vec2(180.0, 30.0)),
        )
        .on_hover_text("Stops every destination");
    if stop.clicked() {
        if armed {
            ui.data_mut(|d| d.remove_temp::<f64>(stop_armed_id()));
            actions.commands.push(EngineCommand::StopStream);
        } else {
            ui.data_mut(|d| d.insert_temp(stop_armed_id(), now));
        }
    }
    if armed {
        ui.ctx()
            .request_repaint_after(std::time::Duration::from_millis(250));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streaming::StreamStatus;
    use egui_kittest::kittest::Queryable;

    fn live(states: &[DestinationState]) -> StreamStatus {
        let mut s = StreamStatus::stopped();
        s.live = true;
        for (d, &state) in s.destinations.iter_mut().zip(states) {
            d.state = state;
        }
        s
    }

    #[test]
    fn the_button_is_plain_off_red_when_healthy_and_blinks_yellow_on_trouble() {
        assert_eq!(look(&StreamStatus::stopped(), true).fill, None);
        let healthy = live(&[DestinationState::Live, DestinationState::Off]);
        assert_eq!(look(&healthy, true).fill, Some(LIVE_RED));
        assert_eq!(look(&healthy, false).fill, Some(LIVE_RED), "steady red");
        for trouble in [
            DestinationState::Reconnecting,
            DestinationState::Struggling,
            DestinationState::Connecting,
            DestinationState::Invalid,
        ] {
            let s = live(&[DestinationState::Live, trouble]);
            let (on, off) = (look(&s, true), look(&s, false));
            assert_eq!(on.fill, Some(WARN_YELLOW), "{trouble:?}");
            assert_eq!(off.fill, Some(WARN_DIM), "{trouble:?} blinks");
            assert!(on.blinking);
        }
        let mut encoder = live(&[DestinationState::Live]);
        encoder.encoder_problem = Some("behind".into());
        assert_eq!(look(&encoder, true).fill, Some(WARN_YELLOW));
    }

    fn harness(data: UIData) -> egui_kittest::Harness<'static, UIActions> {
        let data = std::rc::Rc::new(data);
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .build_ui_state(
                // Commands pile up across frames: a click's command is
                // pushed in the one frame the click lands in.
                move |ui, actions: &mut UIActions| {
                    let mut frame = UIActions::new();
                    ui.horizontal(|ui| {
                        render_stream_button(ui, &data);
                        render_stream_stats(ui, &data, 0);
                    });
                    render_stream_window(ui.ctx(), &data, &mut frame);
                    actions.commands.extend(frame.commands);
                },
                UIActions::new(),
            )
    }

    fn open(harness: &mut egui_kittest::Harness<'static, UIActions>) {
        harness.get_by_label_contains("STREAM").click();
        harness.run();
    }

    #[test]
    fn the_button_opens_the_window_with_four_destinations() {
        let mut h = harness(UIData::test_fixture());
        h.run();
        assert!(
            h.query_by_label("Start streaming").is_none(),
            "closed at first"
        );
        open(&mut h);
        h.get_by_label("Start streaming");
        assert_eq!(h.get_all_by_label("Stream key").count(), 4);
    }

    #[test]
    fn switching_a_destination_on_keeps_its_secrets() {
        let mut data = UIData::test_fixture();
        data.stream_settings.destinations[1] = StreamDestination {
            url: "rtmp://h/live".into(),
            key: "secret".into(),
            ..StreamDestination::default()
        };
        let mut h = harness(data);
        open(&mut h);
        h.get_by_label("2").click();
        h.run();
        let cmd = h.state().commands.iter().find_map(|c| match c {
            EngineCommand::SetStreamDestination {
                index: 1,
                enabled,
                url,
                key,
                password,
                ..
            } => Some((*enabled, url.clone(), key.clone(), password.clone())),
            _ => None,
        });
        assert_eq!(
            cmd,
            Some((true, "rtmp://h/live".to_string(), None, None)),
            "on, same URL, key and password left as saved"
        );
    }

    #[test]
    fn a_typed_url_is_sent_once_when_the_field_is_left() {
        let mut h = harness(UIData::test_fixture());
        open(&mut h);
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .nth(1)
            .expect("the first URL field")
            .click();
        h.run();
        h.get_all_by_role(egui::accesskit::Role::TextInput)
            .nth(1)
            .unwrap()
            .type_text("rtmps://live.example.com/live");
        h.run();
        let sent = |h: &egui_kittest::Harness<'static, UIActions>| {
            h.state()
                .commands
                .iter()
                .filter(|c| matches!(c, EngineCommand::SetStreamDestination { .. }))
                .count()
        };
        assert_eq!(sent(&h), 0, "nothing is sent while typing");
        h.key_press(egui::Key::Enter);
        h.run();
        let url = h.state().commands.iter().find_map(|c| match c {
            EngineCommand::SetStreamDestination { index: 0, url, .. } => Some(url.clone()),
            _ => None,
        });
        assert_eq!(url.as_deref(), Some("rtmps://live.example.com/live"));
    }

    /// A struggling upload says what it sends against what the stream
    /// needs, and how much of the stream it has lost, as time.
    #[test]
    fn a_struggling_upload_shows_what_the_stream_needs_and_what_was_lost() {
        let mut data = UIData::test_fixture();
        data.stream_settings.options.video_kbps = 2500;
        data.stream_settings.options.audio_kbps = 160;
        data.stream_settings.destinations[0] = StreamDestination {
            enabled: true,
            name: "CDN".into(),
            url: "rtmps://live.example.com:443/live/".into(),
            key: "key".into(),
            ..StreamDestination::default()
        };
        data.stream_status = live(&[DestinationState::Struggling]);
        let d = &mut data.stream_status.destinations[0];
        d.kbps = 1600.0;
        d.dropped = 8822;
        // 30 s at the stream's 2.66 Mbit/s.
        d.dropped_bytes = 2_660_000 / 8 * 30;
        d.dropped_secs = 30.0;
        let mut h = harness(data);
        h.get_by_label_contains("STREAM").click();
        h.run_steps(2);
        h.get_by_label_contains("Struggling  1.6 of 2.7 Mbps");
        h.get_by_label("About 30 s of the stream lost to a slow upload");
    }

    /// A stream following its upload says so, and what it is sent at
    /// against the rate set; its destinations' need is the rate now. As low
    /// as it goes, it says that instead.
    #[test]
    fn a_stream_following_its_upload_says_at_what_rate() {
        // Live and adapting, the encoder now at `kbps` of 6000.
        let following = |kbps: u32| {
            let mut data = UIData::test_fixture();
            data.stream_settings.options.video_kbps = 6000;
            data.stream_settings.options.audio_kbps = 160;
            data.stream_settings.destinations[0] = StreamDestination {
                enabled: true,
                name: "Venue".into(),
                url: "rtmp://venue.example/live".into(),
                key: "key".into(),
                ..StreamDestination::default()
            };
            data.stream_status = live(&[DestinationState::Struggling]);
            data.stream_status.destinations[0].kbps = 1300.0;
            data.stream_status.adaptive = Some(crate::streaming::AdaptiveStatus {
                asked_kbps: 6000,
                kbps,
                floor_kbps: 414,
            });
            let mut h = harness(data);
            h.get_by_label_contains("STREAM").click();
            h.run_steps(2);
            h
        };
        let h = following(1200);
        h.get_by_label("Adapting to the upload: 1.2 of 6.0 Mbps");
        h.get_by_label_contains("Struggling  1.3 of 1.4 Mbps");
        let h = following(414);
        h.get_by_label_contains("Bit rate as low as it goes: 0.4 of 6.0 Mbps");
    }

    #[test]
    fn what_was_lost_reads_as_time_and_the_need_only_while_below_it() {
        assert_eq!(lost_text(0.0), None);
        assert_eq!(
            lost_text(0.003).as_deref(),
            Some("under a second of the stream lost to a slow upload")
        );
        assert_eq!(
            lost_text(75.0).as_deref(),
            Some("about 1 min 15 s of the stream lost to a slow upload")
        );
        let mut s = DestinationStatus {
            state: DestinationState::Struggling,
            kbps: 3100.0,
            reconnects: 1,
            ..DestinationStatus::off()
        };
        assert_eq!(rate_text(&s, 2660), "  3.1 Mbps", "catching up: no need shown");
        s.kbps = 2500.0;
        assert_eq!(rate_text(&s, 2660), "  2.5 of 2.7 Mbps");
        s.kbps = 2650.0;
        assert_eq!(rate_text(&s, 2660), "  2.7 Mbps", "never \"2.7 of 2.7\"");
        s.kbps = 2300.0;
        assert_eq!(rate_text(&s, 2350), "  2.3 Mbps", "nor \"2.3 of 2.3\"");
        s.state = DestinationState::Live;
        s.kbps = 2400.0;
        assert_eq!(rate_text(&s, 2660), "  2.4 Mbps", "live: just the rate");
        assert_eq!(trouble_text(&s).as_deref(), Some("Reconnected once"));
        s.reconnects = 0;
        assert_eq!(trouble_text(&s), None);
    }

    /// Two destinations, one struggling: the stats beside the button show
    /// its rate and bars, and the frames its upload lost, as OBS's bar does.
    fn two_destinations_one_struggling() -> UIData {
        let mut data = UIData::test_fixture();
        let options = &mut data.stream_settings.options;
        options.video_kbps = 2500;
        options.audio_kbps = 160;
        options.fps = Some(30);
        for (i, name) in ["YouTube", "Cloudflare"].into_iter().enumerate() {
            data.stream_settings.destinations[i] = StreamDestination {
                enabled: true,
                name: name.into(),
                url: "rtmps://live.example.com/live".into(),
                key: "key".into(),
                ..StreamDestination::default()
            };
        }
        data.stream_status = live(&[DestinationState::Live, DestinationState::Struggling]);
        data.stream_status.live_secs = 600.0;
        let d = &mut data.stream_status.destinations;
        d[0].kbps = 2900.0;
        d[1].kbps = 1600.0;
        // 30 s of the stream at 2.66 Mbit/s: 900 frames at 30 fps.
        d[1].dropped_bytes = 2_660_000 / 8 * 30;
        d[1].dropped_secs = 30.0;
        data
    }

    #[test]
    fn the_stats_beside_the_button_show_the_worst_upload_and_what_it_lost() {
        let mut h = harness(two_destinations_one_struggling());
        h.run_steps(2);
        h.get_by_label("1.6 Mbps");
        h.get_by_label("Dropped ~900 (5.0%)");
    }

    #[test]
    fn the_stats_follow_the_destination_doing_worst() {
        let data = two_destinations_one_struggling();
        let worst = worst_destination(&data).map(|(i, _)| i);
        assert_eq!(worst, Some(1), "the struggling one");
        let (bars, colour) = health(&data.stream_status, worst_destination(&data).map(|(_, s)| s));
        assert_eq!((bars, colour), (2, WARN));
        assert_eq!(
            stream_dropped(&data),
            Dropped {
                frames: 900,
                estimated: true,
                percent: 5.0
            }
        );
        assert_eq!(
            dropped_text(Dropped {
                frames: 0,
                estimated: false,
                percent: 0.0
            }),
            ("Dropped 0 (0.0%)".to_string(), egui::Color32::GRAY)
        );

        // All live: four green bars; the encoder's own trouble caps them.
        let mut fine = two_destinations_one_struggling();
        fine.stream_status.destinations[1].state = DestinationState::Live;
        let worst = worst_destination(&fine).map(|(_, s)| s.clone());
        assert_eq!(health(&fine.stream_status, worst.as_ref()), (4, GOOD));
        fine.stream_status.encoder_problem = Some("behind".into());
        assert_eq!(health(&fine.stream_status, worst.as_ref()), (2, WARN));
        // Reconnecting: one red bar. Nothing switched on: none.
        fine.stream_status.destinations[0].state = DestinationState::Reconnecting;
        let worst = worst_destination(&fine).map(|(_, s)| s.clone());
        assert_eq!(health(&fine.stream_status, worst.as_ref()).0, 1);
        assert_eq!(health(&fine.stream_status, None), (0, BAD));
        // The rate is the one still sending's; the other counts as down.
        assert_eq!(sending_rate(&fine), (1600.0, GOOD));
        assert_eq!(down_count(&fine), 1);
    }

    /// One destination live, another still connecting: the rate is the live
    /// one's, not the connecting one's nothing; neither is down.
    #[test]
    fn a_destination_connecting_does_not_stand_for_the_rate() {
        let mut data = two_destinations_one_struggling();
        data.stream_status.destinations[1].state = DestinationState::Connecting;
        data.stream_status.destinations[1].kbps = 0.0;
        assert_eq!(sending_rate(&data), (2900.0, GOOD));
        assert_eq!(down_count(&data), 0);
        data.stream_status.destinations[0].state = DestinationState::Connecting;
        data.stream_status.destinations[0].kbps = 0.0;
        assert_eq!(sending_rate(&data), (0.0, WARN), "all connecting");
    }

    /// Drops are counted as time, then frames at the stream's rate: frames
    /// the encoder missed come at Nida's rate (60 when uncapped), the
    /// upload's loss at the stream's bit rate.
    #[test]
    fn drops_are_counted_at_the_rate_the_stream_is_sent() {
        let mut data = two_destinations_one_struggling();
        // Uncapped, the stream at Nida's rate: the encoder's 60.
        data.target_fps = 0;
        data.stream_settings.options.fps = None;
        assert_eq!(
            stream_dropped(&data),
            Dropped {
                frames: 1800,
                estimated: true,
                percent: 5.0
            },
            "30 s lost at 60 fps"
        );
        // Nida at 60, the stream at 30: 60 frames the encoder missed over
        // ten seconds are one second — 30 frames sent, 10 %, not 20 %.
        data.target_fps = 60;
        data.stream_settings.options.fps = Some(30);
        data.stream_status.destinations[1].dropped_bytes = 0;
        data.stream_status.destinations[1].dropped_secs = 0.0;
        data.stream_status.encoder_dropped = 60;
        data.stream_status.live_secs = 10.0;
        assert_eq!(
            stream_dropped(&data),
            Dropped {
                frames: 30,
                estimated: true,
                percent: 10.0
            }
        );
    }

    /// The rates are the encoder's own, as it was started: the target frame
    /// rate changed to 30 while the stream goes on at 60 does not halve what
    /// was lost.
    #[test]
    fn drops_keep_the_rates_the_encoder_was_started_at() {
        let mut data = two_destinations_one_struggling();
        data.stream_settings.options.fps = None;
        data.stream_status.encoder_fps = 60;
        data.stream_status.stream_fps = 60;
        data.target_fps = 30;
        assert_eq!(stream_dropped(&data).frames, 1800, "30 s at the stream's 60");
        // And 600 frames the encoder missed are ten seconds at its 60.
        data.stream_status.encoder_dropped = 600;
        assert_eq!(stream_dropped(&data).frames, 2400, "40 s at 60");
        // Measured as dropped — 30 at the show's new 30 fps, a second — the
        // time counts, not the count at the encoder's rate.
        data.stream_status.encoder_dropped = 30;
        data.stream_status.encoder_dropped_secs = 1.0;
        assert_eq!(stream_dropped(&data).frames, 1860, "31 s at 60");
    }

    /// The STREAM button takes the room it says it does, idle and live:
    /// the top bar counts it as it is now, as it widens by its clock.
    #[test]
    fn the_button_takes_the_room_it_says() {
        for live in [false, true] {
            let mut data = two_destinations_one_struggling();
            data.stream_status.live = live;
            let shown = std::rc::Rc::new(data);
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1200.0, 100.0))
                .build_ui_state(
                    move |ui, measured: &mut (f32, f32)| {
                        ui.horizontal(|ui| {
                            let before = ui.cursor().left();
                            let said = button_width(ui, &shown);
                            render_stream_button(ui, &shown);
                            *measured = (ui.cursor().left() - before, said);
                        });
                    },
                    (0.0, 0.0),
                );
            harness.run_steps(1);
            let (took, said) = *harness.state();
            assert!(
                (took - said).abs() < 0.5,
                "live {live}: took {took}, said {said}"
            );
        }
    }

    /// Each form takes the room it says it does, the row's gap after it
    /// included: the top bar picks the form by these widths, and one off by
    /// that gap had it give a form up and take it back frame after frame.
    #[test]
    fn the_stats_take_the_room_they_say() {
        let data = std::rc::Rc::new(two_destinations_one_struggling());
        for tier in 0..3u8 {
            let shown = data.clone();
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(1200.0, 100.0))
                .build_ui_state(
                    move |ui, measured: &mut (f32, f32)| {
                        ui.horizontal(|ui| {
                            let before = ui.cursor().left();
                            let said = stats_width(ui, &shown, tier);
                            render_stream_stats(ui, &shown, tier);
                            *measured = (ui.cursor().left() - before, said);
                        });
                    },
                    (0.0, 0.0),
                );
            harness.run_steps(1);
            let (took, said) = *harness.state();
            assert!(
                (took - said).abs() < 0.5,
                "form {tier}: took {took}, said {said}"
            );
        }
    }

    /// Narrower, the stats drop the words: the drops as a share, only when
    /// there are any; narrowest, the bars and those down.
    #[test]
    fn the_stats_shorten_in_a_narrow_top_bar() {
        let data = two_destinations_one_struggling();
        let texts = |tier| {
            stats_pieces(&data, tier)
                .into_iter()
                .filter_map(|piece| match piece {
                    Piece::Text(text, _) => Some(text),
                    Piece::Bars(..) => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(texts(0), ["1.6 Mbps", "Dropped ~900 (5.0%)"]);
        assert_eq!(texts(1), ["1.6 Mbps", "5.0%"]);
        assert!(texts(2).is_empty(), "bars only");
        assert!(stats_pieces(&UIData::test_fixture(), 0).is_empty(), "not live: nothing");
    }

    /// One destination live, one still without its key: the stats give the
    /// live one's rate and say one is down, rather than "0.0 Mbps".
    #[test]
    fn a_destination_down_is_counted_not_shown_as_the_rate() {
        let mut data = two_destinations_one_struggling();
        data.stream_status.destinations[1].state = DestinationState::Invalid;
        data.stream_status.destinations[1].kbps = 0.0;
        data.stream_status.destinations[1].dropped_bytes = 0;
        data.stream_status.destinations[1].dropped_secs = 0.0;
        let mut h = harness(data);
        h.run_steps(2);
        h.get_by_label("2.9 Mbps");
        h.get_by_label("1 down");
        h.get_by_label("Dropped 0 (0.0%)");
    }

    /// While live, a bit rate picked in the window goes out where the
    /// stream's rate changes live (encoded in Nida), and the presets are
    /// shut where it does not.
    #[test]
    fn the_bit_rate_is_picked_while_live_where_it_changes_live() {
        for rate_live in [true, false] {
            let mut data = UIData::test_fixture();
            data.stream_status = live(&[DestinationState::Live]);
            data.stream_status.rate_live = rate_live;
            let mut h = harness(data);
            h.get_by_label_contains("STREAM").click();
            h.run_steps(2);
            h.get_by_label("Presets").click();
            h.run_steps(2);
            let preset = h.query_by_label_contains("2500 kbps");
            assert_eq!(preset.is_some(), rate_live, "presets open while live: {rate_live}");
            let Some(preset) = preset else { continue };
            preset.click();
            h.run_steps(2);
            let sent = h.state().commands.iter().find_map(|c| match c {
                EngineCommand::SetStreamOptions { options } => Some(options.video_kbps),
                _ => None,
            });
            assert_eq!(sent, Some(2500));
        }
    }

    /// What the stream needs of an upload: the rate set and the sound held
    /// Constant; held Variable or Average, what it is at as measured — a
    /// slide's megabit, not the four set.
    #[test]
    fn a_variable_stream_needs_what_it_is_at() {
        use crate::streaming::RateMode;
        let mut data = UIData::test_fixture();
        data.stream_settings.options.video_kbps = 4000;
        data.stream_settings.options.audio_kbps = 160;
        data.stream_status = live(&[DestinationState::Live]);
        data.stream_status.stream_kbps = Some(1100);
        data.stream_status.rate_mode = Some(RateMode::Constant);
        assert_eq!(needed_kbps(&data), 4160);
        data.stream_status.rate_mode = Some(RateMode::Variable);
        assert_eq!(needed_kbps(&data), 1100);
        data.stream_status.stream_kbps = None;
        assert_eq!(needed_kbps(&data), 4160, "not measured yet: the rate set");
    }

    /// A platform among the destinations switched on says what it asks for
    /// that the settings do not give it — `YouTube` a constant rate,
    /// Cloudflare Stream a constant rate, H.264 and 2–14 Mbps — matched on
    /// the host's own name, not anywhere in the URL.
    #[test]
    fn a_platform_says_what_it_asks_for() {
        use crate::streaming::RateMode;
        let dest = |url: &str| StreamDestination {
            enabled: true,
            url: url.into(),
            ..StreamDestination::default()
        };
        let mut options = StreamOptions {
            adaptive: false,
            ..StreamOptions::default()
        };
        let youtube = [dest("rtmp://a.rtmp.youtube.com/live2")];
        assert!(platform_notes(&options, &youtube).is_empty(), "Constant: nothing to say");
        options.rate_mode = RateMode::Variable;
        assert_eq!(platform_notes(&options, &youtube), ["YouTube asks for Constant (CBR)"]);
        let elsewhere = [dest("rtmp://example.com/youtube.com/live")];
        assert!(platform_notes(&options, &elsewhere).is_empty(), "the host, not the path");

        let cloudflare = [dest("rtmps://user:pw@live.cloudflare.com:443/live/")];
        options.rate_mode = RateMode::Constant;
        options.video_kbps = 6000;
        assert!(platform_notes(&options, &cloudflare).is_empty());
        options.video_kbps = 1500;
        options.audio_kbps = 160;
        let said = platform_notes(&options, &cloudflare);
        assert!(said.len() == 1 && said[0].contains("2–14 Mbps") && said[0].contains("1.7"), "{said:?}");
        options.video_kbps = 6000;
        options.rate_mode = RateMode::Variable;
        let said = platform_notes(&options, &cloudflare);
        assert!(said[0].contains("Constant (CBR)"), "{said:?}");
        assert!(said[1].contains("Variable on slides, it can go under"), "{said:?}");
        options.rate_mode = RateMode::Constant;
        options.adaptive = true;
        let said = platform_notes(&options, &cloudflare);
        assert_eq!(said, ["Cloudflare Stream asks for 2 Mbps at least: lowered for a slow upload, it can go under"]);
        let off = [StreamDestination {
            enabled: false,
            ..cloudflare[0].clone()
        }];
        assert!(platform_notes(&options, &off).is_empty(), "only those switched on");
    }

    #[test]
    fn stopping_a_live_stream_takes_two_clicks() {
        let mut data = UIData::test_fixture();
        data.stream_status = live(&[DestinationState::Live]);
        let mut h = harness(data);
        // A live button keeps repainting (its clock, its blink): step a few
        // frames rather than wait for it to settle.
        h.get_by_label_contains("STREAM").click();
        h.run_steps(2);
        h.get_by_label("Stop streaming").click();
        h.run_steps(2);
        assert!(
            !h.state()
                .commands
                .iter()
                .any(|c| matches!(c, EngineCommand::StopStream)),
            "one click only arms it"
        );
        h.get_by_label("Click again to stop").click();
        h.run_steps(2);
        assert!(h
            .state()
            .commands
            .iter()
            .any(|c| matches!(c, EngineCommand::StopStream)));
    }
}
