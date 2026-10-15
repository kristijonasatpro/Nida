//! Live system telemetry: frame rate, GPU load, CPU, and memory.
//!
//! Grouped as one health cluster, because these are things you glance at when
//! something feels wrong, not settings you change. They sit in the top bar,
//! which every screen layout keeps, so no arrangement of panels loses the
//! frame rate.

use super::super::{UIActions, UIData};
use super::popovers::{render_fps_popover, render_gpu_popover, status_popover};

pub(super) const GOOD: egui::Color32 = egui::Color32::from_rgb(100, 220, 100);
pub(super) const WARN: egui::Color32 = egui::Color32::from_rgb(220, 200, 60);
pub(super) const BAD: egui::Color32 = egui::Color32::from_rgb(220, 60, 60);

/// Frame rate is the one metric where high is good, so it reads inverted.
/// Judged against the target, as OBS shows "30.00 / 30.00 FPS": a show at
/// 50 fps for an LED wall read yellow against a fixed 55. Uncapped, against
/// 55 and 30.
pub(super) fn fps_color(fps: f32, target: u32) -> egui::Color32 {
    if target == 0 {
        return if fps > 55.0 {
            GOOD
        } else if fps > 30.0 {
            WARN
        } else {
            BAD
        };
    }
    #[allow(clippy::cast_precision_loss)]
    let share = fps / target as f32;
    if share >= 0.97 {
        GOOD
    } else if share >= 0.85 {
        WARN
    } else {
        BAD
    }
}

/// "50/50 FPS" — the frame rate and its target — or, uncapped, "143 FPS".
fn fps_text(fps: f32, target: u32) -> String {
    if target == 0 {
        format!("{fps:.0} FPS")
    } else {
        format!("{fps:.0}/{target} FPS")
    }
}

/// Shared by GPU, CPU, and RAM, which are all "percent of a budget consumed".
fn load_color(percent: f32) -> egui::Color32 {
    if percent < 50.0 {
        GOOD
    } else if percent < 80.0 {
        WARN
    } else {
        BAD
    }
}

fn ram_gb(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn deck_count(data: &UIData) -> usize {
    data.channels.iter().map(|c| c.decks.len()).sum()
}

/// Colour targets the scene holds resident, in bytes.
///
/// Decks and channels each own a *pair* of full-resolution targets, since both
/// ping-pong through their effect chain (`Deck Texture` / `Deck Texture B`,
/// `composite_texture` / `effect_ping_texture`), and the mixer owns one more
/// pair. Deliberately an under-estimate of total VRAM: effect pass buffers,
/// video source textures, decoder pools, and camera buffers all sit on top of
/// it. The number exists to answer "is this scene getting heavy" rather than to
/// account for the card. Until residency windows land, an arrangement holds
/// every deck for the whole show, so this is the ceiling a long show runs
/// into. See /spec/arrangement.md § Deferred: Residency Windows.
fn estimated_vram(data: &UIData) -> u64 {
    let bytes_per_pixel = u64::from(
        crate::renderer::context::COLOR_PATH_FORMAT
            .block_copy_size(None)
            .unwrap_or(8),
    );
    let pairs = deck_count(data) as u64 + data.channels.len() as u64 + 1;
    pairs * 2 * u64::from(data.render_width) * u64::from(data.render_height) * bytes_per_pixel
}

fn ram_percent(data: &UIData) -> f32 {
    if data.ram_total == 0 {
        return 0.0;
    }
    data.ram_used as f32 / data.ram_total as f32 * 100.0
}

/// The full cluster, in the row it is drawn in — the top bar.
///
/// `actions` is unused today but kept in the signature: the popovers this opens
/// are shared with paths that emit commands, and threading it later would touch
/// every call site.
pub(super) fn render_monitoring_section(
    ui: &mut egui::Ui,
    data: &UIData,
    _actions: &mut UIActions,
) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;

        let fps_response = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(fps_text(data.fps, data.target_fps))
                        .color(fps_color(data.fps, data.target_fps))
                        .monospace()
                        .small(),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text("Click for render timing details");
        status_popover(&fps_response, |ui| {
            render_fps_popover(ui, data);
        });

        let gpu = data.gpu_utilization;
        let gpu_response = ui
            .add(
                egui::Label::new(
                    egui::RichText::new(format!("🖥 {gpu:.0}%"))
                        .color(load_color(gpu))
                        .monospace()
                        .small(),
                )
                .sense(egui::Sense::click()),
            )
            .on_hover_text(if data.gpu_utilization_measured {
                "How busy the GPU is, as macOS reports it — click for details"
            } else {
                "GPU load, estimated from the layers' costs — click for details"
            });
        status_popover(&gpu_response, |ui| {
            render_gpu_popover(ui, data);
        });

        ui.label(
            egui::RichText::new(format!("CPU {:.0}%", data.cpu_usage))
                .color(load_color(data.cpu_usage))
                .monospace()
                .small(),
        );

        ui.label(
            egui::RichText::new(format!(
                "RAM {:.1}/{:.0}G",
                ram_gb(data.ram_used),
                ram_gb(data.ram_total)
            ))
            .color(load_color(ram_percent(data)))
            .monospace()
            .small(),
        );

        let decks = deck_count(data);
        ui.label(
            egui::RichText::new(format!(
                "{decks} decks ~{:.1}G",
                ram_gb(estimated_vram(data))
            ))
            .monospace()
            .small()
            .weak(),
        )
        .on_hover_text(format!(
            "{decks} decks across {} channels, holding at least {:.2} GB of {}x{} colour targets. \
             Effect buffers and video decoders sit on top of that. Decks stay resident for the \
             whole show, so a long arrangement pays this the entire time.",
            data.channels.len(),
            ram_gb(estimated_vram(data)),
            data.render_width,
            data.render_height,
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fps_colour_is_inverted_relative_to_load() {
        assert_eq!(fps_color(60.0, 0), GOOD);
        assert_eq!(fps_color(45.0, 0), WARN);
        assert_eq!(fps_color(12.0, 0), BAD);

        assert_eq!(load_color(10.0), GOOD);
        assert_eq!(load_color(60.0), WARN);
        assert_eq!(load_color(95.0), BAD);
    }

    /// Against its target, the frame rate reads "50/50 FPS" and green: a
    /// fixed 55 called a perfect 50 a warning and a perfect 30 bad.
    #[test]
    fn the_frame_rate_is_judged_against_its_target() {
        assert_eq!(fps_text(50.0, 50), "50/50 FPS");
        assert_eq!(fps_text(143.4, 0), "143 FPS", "uncapped: no target");
        assert_eq!(fps_color(50.0, 50), GOOD);
        assert_eq!(fps_color(29.9, 30), GOOD);
        assert_eq!(fps_color(45.0, 50), WARN);
        assert_eq!(fps_color(30.0, 60), BAD);
    }

    /// A machine reporting no total memory must not divide by zero.
    #[test]
    fn ram_percent_survives_an_unknown_total() {
        let mut data = UIData::test_fixture();
        data.ram_total = 0;
        data.ram_used = 1024;
        assert_eq!(ram_percent(&data), 0.0);
    }

    /// The estimate has to move with the scene, or it is decoration.
    #[test]
    fn the_vram_estimate_grows_with_the_scene() {
        let mut data = UIData::test_fixture();
        data.render_width = 1920;
        data.render_height = 1080;
        let before = estimated_vram(&data);
        assert!(before > 0, "a scene with decks must estimate something");

        // A deck is a pair of 1080p Rgba16Float targets, about 33 MB. Anything
        // that costs less than that is counting the wrong thing.
        let per_deck = estimated_vram(&data) / (deck_count(&data) + data.channels.len() + 1) as u64;
        assert!(
            (30..40).contains(&(per_deck / (1024 * 1024))),
            "expected roughly 33 MB per deck at 1080p, got {} MB",
            per_deck / (1024 * 1024)
        );

        let decks_before = deck_count(&data);
        let deck = data.channels[0].decks[0].clone();
        data.channels[0].decks.push(deck);
        assert!(
            estimated_vram(&data) > before,
            "another deck must cost another pair of targets"
        );
        assert_eq!(deck_count(&data), decks_before + 1);
    }

    #[test]
    fn the_cluster_renders_in_a_row() {
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        {
            let _harness = egui_kittest::Harness::new_ui(|ui| {
                ui.horizontal(|ui| render_monitoring_section(ui, &data, &mut actions));
            });
        }
        assert!(actions.commands.is_empty());
    }
}
