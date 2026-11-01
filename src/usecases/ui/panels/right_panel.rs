//! The Main Output and Setup panels, which were the right side panel before
//! panels could go anywhere.

use super::super::{UIActions, UIData};
use super::dnd::publish_master_surface_fx;
use super::midi::render_midi_section;
use super::modulation::render_modulation_section;
use super::outputs::render_output_section;
use super::stage::render_surface_editor;
use super::tonemap::{render_tonemap_section, tonemap_name};

fn effect_drag_active(ctx: &egui::Context) -> bool {
    use super::super::LibraryDrag;
    egui::DragAndDrop::payload::<LibraryDrag>(ctx)
        .is_some_and(|p| matches!(&*p, LibraryDrag::Effect(_)))
}

/// Right-click on the main output: "Cut to / Fade to" any bus, putting the
/// master mix on that bus the same way a deck or channel is sent.
fn master_take_menu(response: &egui::Response, data: &UIData, actions: &mut UIActions) {
    response.context_menu(|ui| {
        super::switcher::take_menu_items(ui, &crate::bus::BusInputRef::Master, data, actions);
    });
}

fn fx_accent() -> egui::Color32 {
    egui::Color32::from_rgb(100, 200, 255)
}

/// The Main Output panel: the picture (click for the master's effects,
/// right-click to send it to a bus) and, in Main mode, what Main carries.
pub(super) fn render_main_output(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    // As large as the panel allows, leaving room for the lines under it and
    // the meters beside it: what the recording, the stream and the speakers
    // hear.
    let meters = super::out_meters::width(data);
    let budget = egui::vec2(
        (ui.available_width() - 12.0 - meters).max(80.0),
        (ui.available_height() - 44.0).max(60.0),
    );
    let preview_size = super::utils::preview_size(budget, data.render_width, data.render_height);
    ui.horizontal_top(|ui| {
        render_main_picture(ui, preview_size, data, actions);
        super::out_meters::render(ui, preview_size.y, data);
    });
    render_main_lines(ui, data, actions);
}

/// The Main output picture: click it for the master effects, right-click to
/// take Master to a bus.
fn render_main_picture(
    ui: &mut egui::Ui,
    preview_size: egui::Vec2,
    data: &UIData,
    actions: &mut UIActions,
) {
    if let Some(texture_id) = data.main_output_texture {
        let img_response = ui.add(
            egui::Image::new(egui::load::SizedTexture::new(texture_id, preview_size))
                .corner_radius(4.0)
                .alt_text("Main output")
                .sense(egui::Sense::click()),
        );
        publish_master_surface_fx(ui.ctx(), img_response.rect);
        if effect_drag_active(ui.ctx()) && ui.rect_contains_pointer(img_response.rect) {
            ui.painter().rect_stroke(
                img_response.rect,
                4.0,
                egui::Stroke::new(2.0_f32, fx_accent()),
                egui::StrokeKind::Outside,
            );
        }
        if img_response.clicked() {
            actions.session.select_master = true;
        }
        master_take_menu(&img_response, data, actions);
    } else {
        ui.allocate_ui(preview_size, |ui| {
            let (rect, response) = ui.allocate_exact_size(preview_size, egui::Sense::click());
            publish_master_surface_fx(ui.ctx(), rect);
            ui.painter()
                .rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
            if effect_drag_active(ui.ctx()) && ui.rect_contains_pointer(rect) {
                ui.painter().rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(2.0_f32, fx_accent()),
                    egui::StrokeKind::Outside,
                );
            }
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "No Output",
                egui::FontId::proportional(14.0),
                egui::Color32::GRAY,
            );
            if response.clicked() {
                actions.session.select_master = true;
            }
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Main output")
            });
            master_take_menu(&response, data, actions);
        });
    }
}

/// Under the picture: what Main carries, and the hint to click it.
fn render_main_lines(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    // What Main carries, in Main mode.
    if super::main_desk::in_main_mode(data) {
        let overlays: Vec<&str> = data
            .channels
            .iter()
            .filter(|c| data.main.overlays_on.contains(&c.uuid))
            .map(|c| c.name.as_str())
            .collect();
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    " ON MAIN  {} ",
                    super::main_desk::source_name(data, &data.main.program)
                ))
                .strong()
                .color(egui::Color32::WHITE)
                .background_color(super::main_desk::ON_MAIN_RED),
            );
            if !overlays.is_empty() {
                ui.label(egui::RichText::new(format!("+ {}", overlays.join(", "))).small());
            }
        });
    }

    // Hint: click preview to see master effect chain
    let hint_resp = ui.add(
        egui::Label::new(
            egui::RichText::new("Click the picture to edit master effects")
                .small()
                .weak(),
        )
        .sense(egui::Sense::click()),
    );
    if hint_resp.clicked() {
        actions.session.select_master = true;
    }
}

/// The Setup panel: what is set up before a show rather than touched during
/// it.
pub(super) fn render_setup(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    // Named in full on the header so the active curve is legible without
    // opening anything.
    egui::CollapsingHeader::new(
        egui::RichText::new(format!("🎨 Tonemap — {}", tonemap_name(data.tonemap_mode))).strong(),
    )
    .default_open(false)
    .show(ui, |ui| {
        render_tonemap_section(ui, data, actions);
    });

    ui.add_space(4.0);

    egui::CollapsingHeader::new(egui::RichText::new("〰 Modulation").strong())
        .default_open(false)
        .show(ui, |ui| {
            render_modulation_section(ui, data, actions);
        });

    ui.add_space(4.0);

    egui::CollapsingHeader::new(egui::RichText::new("🎹 MIDI").strong())
        .default_open(false)
        .show(ui, |ui| {
            render_midi_section(ui, data, actions);
        });

    ui.add_space(4.0);

    egui::CollapsingHeader::new(egui::RichText::new("🗺 Stage Layout").strong())
        .default_open(false)
        .show(ui, |ui| {
            render_surface_editor(ui, data, actions);
        });

    ui.add_space(4.0);

    egui::CollapsingHeader::new(egui::RichText::new("📺 Outputs").strong())
        .default_open(false)
        .show(ui, |ui| {
            render_output_section(ui, data, actions);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Main Output panel has the REC and STREAM meters beside its
    /// picture.
    #[test]
    fn the_main_output_has_its_meters_beside_it() {
        use egui_kittest::kittest::Queryable;
        let data = UIData::test_fixture();
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(640.0, 400.0))
            .build_ui(|ui| render_main_output(ui, &data, &mut UIActions::new()));
        let picture = harness.get_by_label("Main output").rect();
        for meter in ["REC meter", "STREAM meter"] {
            let rect = harness.get_by_label(meter).rect();
            assert!(rect.left() >= picture.right(), "{meter} at {rect:?}");
        }
    }

    #[test]
    fn render_main_output_and_setup_smoke() {
        let data = UIData::test_fixture();
        let mut actions = UIActions::new();
        let _harness = egui_kittest::Harness::new_ui(|ui| {
            render_main_output(ui, &data, &mut actions);
            render_setup(ui, &data, &mut actions);
        });
    }
}
