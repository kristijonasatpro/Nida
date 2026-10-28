//! Beside the Program picture: a meter for each place the sound goes — the
//! recording, the stream, every audio bus that plays on a sound device (the
//! speakers) — showing what that place hears. A source sent to the speakers'
//! bus but not to the stream's shows as a STREAM meter standing still while
//! the speakers' moves: seen before anyone notices the stream is silent.
//!
//! REC and STREAM show the bus they would carry even before they start,
//! dimmed, so the sound is checked before going on air.

use super::super::UIData;
use super::audio_mixer::{meter_color, meter_travel};
use super::stream::LIVE_RED;
use crate::audio_mix::AudioBusSnapshot;

/// One meter's width: room for STREAM over it.
const COLUMN: f32 = 52.0;
/// Room for its name over it, and the bus or device under it.
const TOP: f32 = 18.0;
const FOOT: f32 = 14.0;
/// Grey of a meter not going out.
const OFF: egui::Color32 = egui::Color32::from_rgb(120, 124, 134);

/// One place the sound goes, as its meter shows it.
pub(super) struct Feed<'a> {
    /// What it is: REC, STREAM, or a bus's name.
    pub label: String,
    /// The bus it hears; none with no audio bus at all (it carries silence).
    pub bus: Option<&'a AudioBusSnapshot>,
    /// Going out now: recording, live, the device playing.
    pub on: bool,
    /// Under the meter: the bus a recording or stream carries, the device a
    /// bus plays on.
    pub foot: String,
    /// REC and STREAM go red when on; a bus on a device does not.
    pub red_when_on: bool,
    pub hover: String,
}

/// The bus a recording or stream carries. Running, the one it was started
/// with — none if that bus has since been removed, as its sound then stops.
/// Not yet, the one it would start with: the one chosen while it is still
/// there, else the first, as the engine picks it.
fn carried<'a>(
    data: &'a UIData,
    running: bool,
    started_with: Option<&str>,
    chosen: Option<&str>,
) -> Option<&'a AudioBusSnapshot> {
    let buses = &data.audio_mix.buses;
    let find = |uuid: &str| buses.iter().find(|b| b.config.uuid == uuid);
    if running {
        return started_with.and_then(find);
    }
    chosen.and_then(find).or_else(|| buses.first())
}

/// Every place the sound goes: REC, STREAM, then each bus on a device.
pub(super) fn feeds(data: &UIData) -> Vec<Feed<'_>> {
    let mut out = Vec::new();
    for (label, bus, on, verb, idle) in [
        (
            "REC",
            carried(
                data,
                data.record.status.recording,
                data.record.status.audio_bus.as_deref(),
                data.record.options.audio_bus.as_deref(),
            ),
            data.record.status.recording,
            "Recording",
            "Not recording — it would record",
        ),
        (
            "STREAM",
            carried(
                data,
                data.stream_status.live,
                data.stream_status.audio_bus.as_deref(),
                data.stream_settings.options.audio_bus.as_deref(),
            ),
            data.stream_status.live,
            "Streaming",
            "Not live — it would stream",
        ),
    ] {
        let name = bus.map_or_else(|| "silence".to_string(), |b| b.config.name.clone());
        let hover = if on {
            format!("{verb}: the sound of {name}")
        } else {
            format!("{idle} the sound of {name}")
        };
        out.push(Feed {
            label: label.to_string(),
            bus,
            on,
            foot: name,
            red_when_on: true,
            hover,
        });
    }
    for bus in &data.audio_mix.buses {
        let Some(device) = &bus.config.device else {
            continue;
        };
        let playing = bus.device_ok == Some(true);
        let hover = if playing {
            format!("{} plays on {device}", bus.config.name)
        } else {
            format!("{} is set to play on {device}, which is not playing", bus.config.name)
        };
        out.push(Feed {
            label: bus.config.name.clone(),
            bus: Some(bus),
            on: playing,
            foot: device.clone(),
            red_when_on: false,
            hover,
        });
    }
    out
}

/// How wide the meters stand, side by side.
pub(super) fn width(data: &UIData) -> f32 {
    #[allow(clippy::cast_precision_loss)]
    let n = feeds(data).len() as f32;
    n * COLUMN
}

/// The meters, `height` tall.
pub(super) fn render(ui: &mut egui::Ui, height: f32, data: &UIData) {
    let feeds = feeds(data);
    #[allow(clippy::cast_precision_loss)]
    let size = egui::vec2(feeds.len() as f32 * COLUMN, height.max(TOP + FOOT + 20.0));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    for (i, feed) in feeds.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let column = egui::Rect::from_min_size(
            rect.min + egui::vec2(i as f32 * COLUMN, 0.0),
            egui::vec2(COLUMN, size.y),
        );
        feed_meter(ui, column, feed);
    }
}

/// Text cut to `width`, ending in "…".
fn fitted(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    colour: egui::Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.to_string(),
        egui::FontId::proportional(size),
        colour,
    );
    job.wrap = egui::text::TextWrapping::truncate_at_width(width.max(0.0));
    painter.layout_job(job)
}

fn feed_meter(ui: &mut egui::Ui, rect: egui::Rect, feed: &Feed<'_>) {
    let response = ui.interact(
        rect,
        ui.id().with(("out_meter", &feed.label, feed.foot.as_str())),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let inner = rect.shrink2(egui::vec2(3.0, 0.0));

    // Its name: on a red badge while recording or live.
    let badge = egui::Rect::from_min_size(inner.min, egui::vec2(inner.width(), TOP - 3.0));
    let name_colour = if feed.on {
        if feed.red_when_on {
            painter.rect_filled(badge, 3.0, LIVE_RED);
        }
        egui::Color32::WHITE
    } else {
        OFF
    };
    let name = fitted(painter, &feed.label, 10.0, name_colour, badge.width() - 2.0);
    painter.galley(
        badge.center() - name.size() / 2.0,
        name,
        name_colour,
    );

    // Left and right, dimmed when it is not going out.
    let track = egui::Rect::from_min_max(
        egui::pos2(inner.left() + 4.0, inner.top() + TOP),
        egui::pos2(inner.right() - 4.0, inner.bottom() - FOOT),
    );
    let levels = feed.bus.map_or([-96.0, -96.0], |b| b.meter_db);
    let fade = if feed.on { 1.0 } else { 0.45 };
    let half = track.width() / 2.0;
    for (i, db) in levels.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let bar = egui::Rect::from_min_size(
            egui::pos2(track.left() + half * i as f32 + 0.5, track.top()),
            egui::vec2(half - 1.5, track.height()),
        );
        painter.rect_filled(bar, 1.0, egui::Color32::from_rgb(8, 30, 12));
        for (low, high, colour) in [
            (-60.0, -12.0, meter_color(-24.0)),
            (-12.0, 0.0, meter_color(-6.0)),
            (0.0, 0.5, meter_color(0.0)),
        ] {
            let top = (*db).min(high);
            if top > low {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(bar.left(), bar.bottom() - meter_travel(top) * bar.height()),
                        egui::pos2(bar.right(), bar.bottom() - meter_travel(low) * bar.height()),
                    ),
                    1.0,
                    colour.gamma_multiply(fade),
                );
            }
        }
    }
    // −12 and 0 dBFS marked across both.
    for db in [-12.0, 0.0] {
        let y = track.bottom() - meter_travel(db) * track.height();
        painter.line_segment(
            [egui::pos2(track.left(), y), egui::pos2(track.right(), y)],
            egui::Stroke::new(0.5, OFF.gamma_multiply(0.6)),
        );
    }

    // The bus or device under it.
    let foot = fitted(painter, &feed.foot, 9.0, OFF, inner.width());
    painter.galley(
        egui::pos2(
            inner.center().x - foot.size().x / 2.0,
            inner.bottom() - FOOT + 2.0,
        ),
        foot,
        OFF,
    );

    let label = format!("{} meter", feed.label);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, &label));
    response.on_hover_text(&feed.hover);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_mix::AudioBusConfig;

    fn bus(uuid: &str, name: &str, device: Option<&str>, meter: f32) -> AudioBusSnapshot {
        AudioBusSnapshot {
            peak_db: [meter; 2],
            dynamics: crate::audio_mix::DynamicsSnapshot::default(),
            config: AudioBusConfig {
                uuid: uuid.into(),
                device: device.map(str::to_string),
                ..AudioBusConfig::new(name)
            },
            device_ok: device.map(|_| true),
            meter_db: [meter; 2],
        }
    }

    fn show() -> UIData {
        let mut data = UIData::test_fixture();
        data.audio_mix.buses = vec![
            bus("m", "Master", None, -10.0),
            bus("a", "Bus A", None, -20.0),
            bus("b", "Bus B", Some("MacBook Pro Speakers"), -6.0),
        ];
        data
    }

    fn labels(data: &UIData) -> Vec<(String, String, bool)> {
        feeds(data)
            .into_iter()
            .map(|f| (f.label, f.foot, f.on))
            .collect()
    }

    /// REC and STREAM carry Master unless another bus is chosen; the
    /// speakers' bus has a meter of its own, named by its device.
    #[test]
    fn rec_stream_and_the_speakers_each_have_a_meter() {
        let mut data = show();
        data.stream_settings.options.audio_bus = Some("a".into());
        data.record.status.recording = true;
        data.record.status.audio_bus = Some("m".into());
        assert_eq!(
            labels(&data),
            vec![
                ("REC".into(), "Master".into(), true),
                ("STREAM".into(), "Bus A".into(), false),
                ("Bus B".into(), "MacBook Pro Speakers".into(), true),
            ]
        );
        let stream = &feeds(&data)[1];
        assert_eq!(stream.bus.map(|b| b.meter_db), Some([-20.0; 2]));
        assert!(stream.hover.starts_with("Not live"), "{}", stream.hover);
    }

    /// Running, REC and STREAM meter the bus they started with — not the
    /// one now chosen, nor the first when theirs has been removed (its
    /// sound has stopped).
    #[test]
    fn running_they_meter_the_bus_they_started_with() {
        let mut data = show();
        data.record.status.recording = true;
        data.record.status.audio_bus = Some("a".into());
        data.record.options.audio_bus = Some("m".into());
        assert_eq!(feeds(&data)[0].foot, "Bus A", "the one it started with");
        data.audio_mix.buses.retain(|b| b.config.uuid != "a");
        let rec = &feeds(&data)[0];
        assert!(rec.bus.is_none(), "removed: nothing, not Master");
        assert_eq!(rec.foot, "silence");
        data.stream_status.live = true;
        data.stream_status.audio_bus = Some("m".into());
        assert_eq!(feeds(&data)[1].foot, "Master");
    }

    /// A chosen bus since removed: the first, as the engine falls back.
    #[test]
    fn a_removed_bus_falls_back_to_the_first() {
        let mut data = show();
        data.record.options.audio_bus = Some("gone".into());
        assert_eq!(feeds(&data)[0].foot, "Master");
        data.audio_mix.buses.clear();
        let rec = &feeds(&data)[0];
        assert!(rec.bus.is_none());
        assert_eq!(rec.foot, "silence");
    }

    #[test]
    fn the_meters_draw_and_name_themselves() {
        use egui_kittest::kittest::Queryable;
        let data = show();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(200.0, 240.0))
            .build_ui(|ui| render(ui, 200.0, &data));
        for label in ["REC meter", "STREAM meter", "Bus B meter"] {
            harness.get_by_label(label);
        }
        assert!((width(&data) - 3.0 * COLUMN).abs() < 1e-3);
    }
}
