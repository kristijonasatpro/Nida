//! An input's colour, chosen by the operator from a short palette: its tile
//! and its switcher buttons wear it, so the cameras, the slides and the
//! clips are told apart at a glance. Unchosen, an input keeps the colour its
//! place gives it (`channel_color`).

use super::super::{ChannelUIInfo, UIActions};
use crate::engine::EngineCommand;

/// The colours offered, by name — distinct from each other and from the
/// red and green of Program and Preview.
pub(super) const PALETTE: [(&str, [u8; 3]); 10] = [
    ("Red", [205, 55, 55]),
    ("Orange", [230, 125, 35]),
    ("Yellow", [215, 185, 40]),
    ("Green", [70, 165, 75]),
    ("Teal", [35, 160, 155]),
    ("Blue", [55, 115, 220]),
    ("Purple", [140, 85, 210]),
    ("Pink", [215, 85, 160]),
    ("Brown", [140, 95, 60]),
    ("Grey", [130, 130, 135]),
];

const SWATCH: egui::Vec2 = egui::vec2(24.0, 24.0);

fn rgb([r, g, b]: [u8; 3]) -> egui::Color32 {
    egui::Color32::from_rgb(r, g, b)
}

/// The palette for one input, for its right-click menu: a swatch per
/// colour, the one it wears outlined, and **Automatic** to give it back the
/// colour its place gives.
pub(super) fn palette(ui: &mut egui::Ui, ch: &ChannelUIInfo, actions: &mut UIActions) {
    ui.label(egui::RichText::new(format!("Colour of {}", ch.numbered())).strong());
    let mut chosen = None;
    ui.horizontal_wrapped(|ui| {
        ui.set_max_width(5.0 * (SWATCH.x + ui.spacing().item_spacing.x));
        for (name, colour) in PALETTE {
            let wearing = ch.color == Some(rgb(colour));
            let (rect, response) = ui.allocate_exact_size(SWATCH, egui::Sense::click());
            ui.painter().rect_filled(rect, 4.0, rgb(colour));
            if wearing || response.hovered() {
                ui.painter().rect_stroke(
                    rect,
                    4.0,
                    egui::Stroke::new(2.0, egui::Color32::WHITE),
                    egui::StrokeKind::Inside,
                );
            }
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::Button, true, wearing, name)
            });
            if response.on_hover_text(name).clicked() && !wearing {
                chosen = Some(Some(colour));
            }
        }
    });
    if ui
        .add_enabled(ch.color.is_some(), egui::Button::new("Automatic"))
        .on_hover_text("The colour its place in the row gives it")
        .clicked()
    {
        chosen = Some(None);
    }
    if let Some(color) = chosen {
        actions.commands.push(EngineCommand::SetChannelColor {
            channel_uuid: ch.uuid.clone(),
            color,
        });
        ui.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usecases::ui::UIData;
    use egui_kittest::kittest::Queryable;

    fn pick(ch: &ChannelUIInfo, click: &str) -> Vec<EngineCommand> {
        let mut actions = UIActions::new();
        {
            let mut harness = egui_kittest::Harness::builder()
                .with_size(egui::vec2(300.0, 200.0))
                .build_ui(|ui| palette(ui, ch, &mut actions));
            harness.get_by_label(click).click();
            harness.run_steps(2);
        }
        actions.commands
    }

    #[test]
    fn a_swatch_gives_the_input_its_colour() {
        let data = UIData::test_fixture();
        let ch = &data.channels[1];
        let commands = pick(ch, "Blue");
        assert!(
            matches!(
                commands.as_slice(),
                [EngineCommand::SetChannelColor { channel_uuid, color: Some([55, 115, 220]) }]
                    if channel_uuid == &ch.uuid
            ),
            "{commands:?}"
        );
    }

    #[test]
    fn automatic_gives_back_the_colour_of_its_place() {
        let mut data = UIData::test_fixture();
        data.channels[0].color = Some(rgb([55, 115, 220]));
        let ch = &data.channels[0];
        assert_eq!(ch.accent(), rgb([55, 115, 220]), "it wears the one given");
        let commands = pick(ch, "Automatic");
        assert!(
            matches!(
                commands.as_slice(),
                [EngineCommand::SetChannelColor { color: None, .. }]
            ),
            "{commands:?}"
        );
        data.channels[0].color = None;
        assert_eq!(
            data.channels[0].accent(),
            super::super::utils::channel_color(0),
            "unchosen, the colour of its place"
        );
    }
}
