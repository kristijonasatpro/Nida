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
    // As large as the panel allows, leaving room for the lines under it.
    let budget = egui::vec2(
        ui.available_width() - 4.0,
        (ui.available_height() - 44.0).max(60.0),
    );
    let preview_size = super::utils::preview_size(budget, data.render_width, data.render_height);

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
    if cfg!(target_os = "windows") {
        egui::CollapsingHeader::new(egui::RichText::new("Performance").strong())
            .default_open(true)
            .show(ui, |ui| render_performance(ui, data, actions));
        ui.add_space(4.0);
    }

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

fn render_performance(ui: &mut egui::Ui, data: &UIData, actions: &mut UIActions) {
    let mut enabled = data.windows_hardware_decode;
    if ui.add_enabled(!data.software_decode_forced, egui::Checkbox::new(
        &mut enabled, "Use GPU video decoding (experimental)"))
        .on_hover_text("Experimental: frames still pass through RAM and CPU colour conversion. This can be slower and use more CPU than software decoding.")
        .changed()
    {
        actions.session.windows_hardware_decode = Some(enabled);
    }
    ui.label(
        egui::RichText::new(
            "Applies to videos and streams opened afterward. Unsupported videos use CPU decoding.",
        )
        .small()
        .weak(),
    );
    if data.software_decode_forced {
        ui.label("CPU decoding is forced by VARDA_SOFTWARE_DECODE. Remove it and restart to use GPU decoding.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use egui_kittest::kittest::{NodeT, Queryable};

    #[test]
    fn performance_checkbox_requests_a_decoding_change_without_reloading_inputs() {
        let data = UIData::test_fixture();
        let mut harness = egui_kittest::Harness::builder().build_ui_state(
            move |ui, requested: &mut Option<bool>| {
                let mut actions = UIActions::new();
                render_setup(ui, &data, &mut actions);
                if let Some(enabled) = actions.session.windows_hardware_decode {
                    assert!(actions.commands.is_empty());
                    *requested = Some(enabled);
                }
            },
            None,
        );
        harness.run();
        if cfg!(target_os = "windows") {
            harness
                .get_by_label("Use GPU video decoding (experimental)")
                .click();
            harness.run();
            assert_eq!(*harness.state(), Some(true));
        } else {
            assert!(harness
                .query_by_label("Use GPU video decoding (experimental)")
                .is_none());
        }
    }

    #[test]
    fn software_override_disables_the_performance_checkbox_and_explains_why() {
        let mut data = UIData::test_fixture();
        data.windows_hardware_decode = true;
        data.software_decode_forced = true;
        let harness = egui_kittest::Harness::new_ui(move |ui| {
            render_performance(ui, &data, &mut UIActions::new());
        });
        assert!(harness
            .get_by_label("Use GPU video decoding (experimental)")
            .accesskit_node()
            .is_disabled());
        assert!(harness
            .query_by_label_contains("CPU decoding is forced")
            .is_some());
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
