//! Masks in the UI: under each effect a **Mask** row — none, a rectangle, an
//! ellipse, or an outline drawn by hand, with its feather and Invert — and
//! the same for a layer's own mask, which cuts a hole in it (Input Settings
//! → **Mask**). While a mask is being drawn, the Input Settings picture
//! takes the drag to draw it instead of moving the layer.

use super::super::{UIActions, UIData};
use crate::engine::value::mask::{EffectMask, MaskShape, LAYER_MASK_EFFECT};
use crate::engine::EngineCommand;

/// Whose mask: an effect's — the effect shows only in the shape — or a
/// layer's own, which cuts the shape out of the layer.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum MaskOwner {
    Effect(String),
    Layer(String),
}

impl MaskOwner {
    /// The mask it has now.
    fn mask<'a>(&self, data: &'a UIData) -> Option<&'a EffectMask> {
        match self {
            Self::Effect(uuid) => data.effect_masks.get(uuid),
            Self::Layer(deck) => data.effect_masks.get(layer_mask_effect(data, deck)?),
        }
    }

    /// Whether it is still there to draw a mask for.
    fn exists(&self, data: &UIData) -> bool {
        match self {
            Self::Effect(effect) => data
                .channels
                .iter()
                .flat_map(|ch| ch.decks.iter().flat_map(|d| &d.effects).chain(&ch.effects))
                .chain(&data.master_effect_info)
                .any(|(uuid, ..)| uuid == effect),
            Self::Layer(deck) => data
                .channels
                .iter()
                .flat_map(|ch| &ch.decks)
                .any(|d| d.uuid == *deck),
        }
    }
}

/// The effect carrying the layer `deck`'s own mask, if it has one.
fn layer_mask_effect<'a>(data: &'a UIData, deck: &str) -> Option<&'a str> {
    data.channels
        .iter()
        .flat_map(|ch| &ch.decks)
        .find(|d| d.uuid == deck)?
        .effects
        .iter()
        .find(|(_, name, ..)| name == LAYER_MASK_EFFECT)
        .map(|(uuid, ..)| uuid.as_str())
}

/// What a mask row says, for an effect's mask or a layer's own.
struct Words {
    label: &'static str,
    hover: &'static str,
    invert: &'static str,
}

const EFFECT_WORDS: Words = Words {
    label: "Mask:",
    hover: "Show the effect only inside a shape (or outside it, inverted)",
    invert: "The effect outside the shape instead",
};

const LAYER_WORDS: Words = Words {
    label: "Shape:",
    hover: "Cut the shape out of the layer, so what is under it shows through \
            (inverted, only the shape is kept)",
    invert: "Keep only the shape: the rest of the layer goes",
};

/// What a drag on the picture draws.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Rectangle,
    Ellipse,
    Draw,
}

/// A mask being drawn: whose, with what, and the drag so far.
#[derive(Clone, Debug, PartialEq)]
struct Editing {
    owner: MaskOwner,
    tool: Tool,
    /// Where the drag started, and the points it has passed (UV).
    drag: Vec<[f32; 2]>,
}

fn editing_id() -> egui::Id {
    egui::Id::new("mask_editing")
}

fn editing(ctx: &egui::Context) -> Option<Editing> {
    ctx.data(|d| d.get_temp(editing_id()))
}

fn set_editing(ctx: &egui::Context, value: Option<Editing>) {
    ctx.data_mut(|d| match value {
        Some(v) => {
            d.insert_temp(editing_id(), v);
        }
        None => d.remove::<Editing>(editing_id()),
    });
}

/// Start drawing `owner`'s mask with `tool` on the picture.
fn start(ctx: &egui::Context, owner: &MaskOwner, tool: Tool) {
    set_editing(
        ctx,
        Some(Editing {
            owner: owner.clone(),
            tool,
            drag: Vec::new(),
        }),
    );
}

fn tool_of(mask: Option<&EffectMask>) -> Option<Tool> {
    mask.map(|m| match m.shape {
        MaskShape::Rectangle { .. } => Tool::Rectangle,
        MaskShape::Ellipse { .. } => Tool::Ellipse,
        MaskShape::Path { .. } => Tool::Draw,
    })
}

/// A rectangle or an ellipse in the middle, or where the mask already is.
fn shaped(tool: Tool, mask: Option<&EffectMask>) -> Option<MaskShape> {
    let (x, y, width, height) = match mask.map(|m| &m.shape) {
        Some(
            MaskShape::Rectangle {
                x,
                y,
                width,
                height,
            }
            | MaskShape::Ellipse {
                x,
                y,
                width,
                height,
            },
        ) => (*x, *y, *width, *height),
        _ => (0.3, 0.3, 0.4, 0.4),
    };
    match tool {
        Tool::Rectangle => Some(MaskShape::Rectangle {
            x,
            y,
            width,
            height,
        }),
        Tool::Ellipse => Some(MaskShape::Ellipse {
            x,
            y,
            width,
            height,
        }),
        Tool::Draw => None,
    }
}

fn set_mask(actions: &mut UIActions, owner: &MaskOwner, mask: Option<EffectMask>) {
    actions.commands.push(match owner {
        MaskOwner::Effect(uuid) => EngineCommand::SetEffectMask {
            effect_uuid: uuid.clone(),
            mask,
        },
        MaskOwner::Layer(deck) => EngineCommand::SetLayerMask {
            deck_uuid: deck.clone(),
            mask,
        },
    });
}

/// The **Mask** row under an effect: its shape, feather and Invert, and
/// drawing it on the picture.
pub(super) fn render_mask_row(
    ui: &mut egui::Ui,
    effect: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    render_row(ui, &MaskOwner::Effect(effect.to_string()), &EFFECT_WORDS, data, actions);
}

/// A layer's own mask (Input Settings → **Mask**): a shape cut out of the
/// layer — or, inverted, all of it but the shape — drawn on the picture.
pub(super) fn render_layer_mask_row(
    ui: &mut egui::Ui,
    deck_uuid: &str,
    data: &UIData,
    actions: &mut UIActions,
) {
    render_row(ui, &MaskOwner::Layer(deck_uuid.to_string()), &LAYER_WORDS, data, actions);
}

fn render_row(
    ui: &mut egui::Ui,
    owner: &MaskOwner,
    words: &Words,
    data: &UIData,
    actions: &mut UIActions,
) {
    let mask = owner.mask(data);
    let current = tool_of(mask);
    let drawing = editing(ui.ctx()).filter(|e| e.owner == *owner);
    ui.horizontal_wrapped(|ui| {
        ui.label(words.label).on_hover_text(words.hover);
        if ui
            .selectable_label(current.is_none() && drawing.is_none(), "None")
            .clicked()
        {
            set_mask(actions, owner, None);
            set_editing(ui.ctx(), None);
        }
        for (tool, label, hover) in [
            (
                Tool::Rectangle,
                "Rectangle",
                "A rectangle — drag on the picture to draw it; Shift for a square",
            ),
            (
                Tool::Ellipse,
                "Ellipse",
                "An ellipse — drag on the picture to draw it; Shift for a circle",
            ),
            (
                Tool::Draw,
                "Draw",
                "An outline drawn by hand on the picture",
            ),
        ] {
            let chosen = drawing
                .as_ref()
                .map_or(current == Some(tool), |d| d.tool == tool);
            if ui
                .selectable_label(chosen, label)
                .on_hover_text(hover)
                .clicked()
            {
                if let Some(shape) = shaped(tool, mask) {
                    set_mask(
                        actions,
                        owner,
                        Some(EffectMask {
                            shape,
                            feather: mask.map_or(0.0, |m| m.feather),
                            invert: mask.is_some_and(|m| m.invert),
                        }),
                    );
                }
                start(ui.ctx(), owner, tool);
            }
        }
    });
    let Some(mask) = mask else {
        if drawing.is_some() {
            ui.label(
                egui::RichText::new("Drag on the picture above to draw it")
                    .small()
                    .weak(),
            );
        }
        return;
    };
    ui.horizontal_wrapped(|ui| {
        let mut feather = mask.feather;
        let slider = ui
            .add(
                egui::Slider::new(&mut feather, 0.0..=300.0)
                    .text("Feather")
                    .suffix(" px"),
            )
            .on_hover_text("How soft the edge is, in pixels of a 1080-line picture");
        // One drag, one step to undo.
        if slider.dragged() {
            actions.session.gesture_active = true;
        }
        if slider.changed() {
            set_mask(
                actions,
                owner,
                Some(EffectMask {
                    feather,
                    ..mask.clone()
                }),
            );
        }
        let mut invert = mask.invert;
        if ui
            .checkbox(&mut invert, "Invert")
            .on_hover_text(words.invert)
            .changed()
        {
            set_mask(
                actions,
                owner,
                Some(EffectMask {
                    invert,
                    ..mask.clone()
                }),
            );
        }
        if drawing.is_some() {
            if ui.button("Done").clicked() {
                set_editing(ui.ctx(), None);
            }
        } else if ui
            .button("Redraw")
            .on_hover_text("Draw it again on the picture")
            .clicked()
        {
            start(ui.ctx(), owner, current.unwrap_or(Tool::Rectangle));
        }
    });
}

/// While a mask is being drawn: its outline on the picture `response`
/// shows, and a drag there draws it. True while drawing, when the picture
/// is not also to move the layer.
pub(super) fn handle_preview(
    ui: &egui::Ui,
    response: &egui::Response,
    data: &UIData,
    actions: &mut UIActions,
) -> bool {
    let Some(mut edit) = editing(ui.ctx()) else {
        return false;
    };
    if !edit.owner.exists(data) || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        set_editing(ui.ctx(), None);
        return false;
    }
    let rect = response.rect;
    let to_uv = |pos: egui::Pos2| {
        [
            ((pos.x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0),
            ((pos.y - rect.top()) / rect.height().max(1.0)).clamp(0.0, 1.0),
        ]
    };
    let mask = edit.owner.mask(data);

    if response.drag_started() {
        // From where the button went down, not where the drag was noticed.
        let origin = ui
            .input(|i| i.pointer.press_origin())
            .or_else(|| response.interact_pointer_pos());
        edit.drag = origin.map(to_uv).into_iter().collect();
    }
    if response.dragged() {
        if let Some(pos) = response.interact_pointer_pos() {
            let p = to_uv(pos);
            let far_enough = edit
                .drag
                .last()
                .is_none_or(|q| (p[0] - q[0]).hypot(p[1] - q[1]) > 0.004);
            match edit.tool {
                Tool::Draw if far_enough => edit.drag.push(p),
                Tool::Draw => {}
                Tool::Rectangle | Tool::Ellipse => {
                    if edit.drag.is_empty() {
                        edit.drag.push(p);
                    }
                    edit.drag.truncate(1);
                    edit.drag.push(p);
                }
            }
        }
    }
    let square = ui.input(|i| i.modifiers.shift);
    let drawn = drawn_shape(&edit, square, rect.size());
    if response.drag_stopped() {
        if let Some(shape) = drawn.clone() {
            set_mask(
                actions,
                &edit.owner,
                Some(EffectMask {
                    shape,
                    feather: mask.map_or(0.0, |m| m.feather),
                    invert: mask.is_some_and(|m| m.invert),
                }),
            );
        }
        edit.drag.clear();
    }

    // The shape as it is, or as it is being drawn.
    let showing = drawn.as_ref().or(mask.map(|m| &m.shape));
    if let Some(shape) = showing {
        outline(ui, rect, shape);
    }
    set_editing(ui.ctx(), Some(edit));
    true
}

/// The shape the drag so far draws: from where it started to where it is
/// (a square or a circle on the screen with Shift), or the outline passed.
fn drawn_shape(edit: &Editing, square: bool, screen: egui::Vec2) -> Option<MaskShape> {
    match edit.tool {
        Tool::Draw => (edit.drag.len() >= 3).then(|| MaskShape::Path {
            points: edit.drag.clone(),
        }),
        Tool::Rectangle | Tool::Ellipse => {
            let (&[x0, y0], &[x1, y1]) = (edit.drag.first()?, edit.drag.last()?);
            let (mut width, mut height) = (x1 - x0, y1 - y0);
            if square {
                // Equal on the screen: the longer side, in each axis's units.
                let side = (width.abs() * screen.x).max(height.abs() * screen.y);
                width = side / screen.x.max(1.0) * width.signum();
                height = side / screen.y.max(1.0) * height.signum();
            }
            let area = EffectMask {
                shape: if edit.tool == Tool::Rectangle {
                    MaskShape::Rectangle {
                        x: x0,
                        y: y0,
                        width,
                        height,
                    }
                } else {
                    MaskShape::Ellipse {
                        x: x0,
                        y: y0,
                        width,
                        height,
                    }
                },
                feather: 0.0,
                invert: false,
            };
            area.sanitized().map(|m| m.shape)
        }
    }
}

/// The mask's outline over the picture.
fn outline(ui: &egui::Ui, rect: egui::Rect, shape: &MaskShape) {
    let at = |x: f32, y: f32| rect.left_top() + egui::vec2(x * rect.width(), y * rect.height());
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 210, 60));
    let painter = ui.painter_at(rect);
    match shape {
        MaskShape::Rectangle {
            x,
            y,
            width,
            height,
        } => {
            painter.rect_stroke(
                egui::Rect::from_min_max(at(*x, *y), at(x + width, y + height)),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        MaskShape::Ellipse {
            x,
            y,
            width,
            height,
        } => {
            let points: Vec<egui::Pos2> = (0..64)
                .map(|i| {
                    let t = i as f32 / 64.0 * std::f32::consts::TAU;
                    at(
                        x + width * (0.5 + 0.5 * t.cos()),
                        y + height * (0.5 + 0.5 * t.sin()),
                    )
                })
                .collect();
            painter.add(egui::Shape::closed_line(points, stroke));
        }
        MaskShape::Path { points } => {
            let points: Vec<egui::Pos2> = points.iter().map(|p| at(p[0], p[1])).collect();
            painter.add(egui::Shape::closed_line(points, stroke));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(tool: Tool, drag: Vec<[f32; 2]>) -> Editing {
        Editing {
            owner: MaskOwner::Effect("fx".into()),
            tool,
            drag,
        }
    }

    /// A layer's own mask is set on the layer; an effect's on the effect.
    #[test]
    fn a_layers_mask_goes_to_the_layer() {
        let mut actions = UIActions::new();
        let mask = EffectMask {
            shape: MaskShape::Ellipse {
                x: 0.3,
                y: 0.3,
                width: 0.4,
                height: 0.4,
            },
            feather: 0.0,
            invert: false,
        };
        set_mask(&mut actions, &MaskOwner::Layer("dk000001".into()), Some(mask.clone()));
        set_mask(&mut actions, &MaskOwner::Effect("fx000001".into()), Some(mask));
        assert!(matches!(
            actions.commands.as_slice(),
            [
                EngineCommand::SetLayerMask { deck_uuid, mask: Some(_) },
                EngineCommand::SetEffectMask { effect_uuid, mask: Some(_) },
            ] if deck_uuid == "dk000001" && effect_uuid == "fx000001"
        ));
    }

    #[test]
    fn a_drag_draws_the_rectangle_from_corner_to_corner() {
        let shape = drawn_shape(
            &edit(Tool::Rectangle, vec![[0.6, 0.7], [0.4, 0.2]]),
            false,
            egui::vec2(160.0, 90.0),
        );
        match shape {
            Some(MaskShape::Rectangle {
                x,
                y,
                width,
                height,
            }) => {
                assert!((x - 0.4).abs() < 1e-6 && (y - 0.2).abs() < 1e-6);
                assert!((width - 0.2).abs() < 1e-6 && (height - 0.5).abs() < 1e-6);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn with_shift_the_ellipse_is_a_circle_on_the_screen() {
        let screen = egui::vec2(160.0, 90.0);
        let Some(MaskShape::Ellipse { width, height, .. }) = drawn_shape(
            &edit(Tool::Ellipse, vec![[0.1, 0.1], [0.3, 0.2]]),
            true,
            screen,
        ) else {
            panic!("an ellipse");
        };
        assert!((width * screen.x - height * screen.y).abs() < 1e-3);
    }

    #[test]
    fn an_outline_needs_three_points() {
        let screen = egui::vec2(160.0, 90.0);
        assert!(drawn_shape(
            &edit(Tool::Draw, vec![[0.1, 0.1], [0.2, 0.2]]),
            false,
            screen
        )
        .is_none());
        assert!(matches!(
            drawn_shape(
                &edit(Tool::Draw, vec![[0.1, 0.1], [0.2, 0.2], [0.1, 0.3]]),
                false,
                screen
            ),
            Some(MaskShape::Path { .. })
        ));
    }
}
