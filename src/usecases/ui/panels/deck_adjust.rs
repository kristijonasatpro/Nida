//! A layer's picture in the input settings window (see `channel_settings`):
//! the preview it is shown on — the layer alone, the channel, the main
//! output or a screen — and the gestures that place it there: drag to move,
//! scroll or pinch to zoom.

use super::super::{DeckUIInfo, UIActions, UIData};
use crate::engine::EngineCommand;
use crate::params::ParamValue;

/// What the adjuster's preview shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    /// The deck alone.
    Input,
    /// The channel the deck is in, every layer composited.
    Channel,
    /// The main output.
    Main,
    /// One output window: what a screen shows.
    Output(usize),
}

fn view_id() -> egui::Id {
    egui::Id::new("deck_adjust_view")
}

fn drag_start_id() -> egui::Id {
    egui::Id::new("deck_adjust_drag_start")
}

/// The corner of the picture's outline a drag began on, if it did.
fn drag_corner_id() -> egui::Id {
    egui::Id::new("deck_adjust_drag_corner")
}

/// How near a corner of the outline (in points) a drag takes it.
const CORNER_REACH: f32 = 10.0;

/// Open the input settings window on a deck — a layer of its channel. The
/// name is the adjuster's that the window replaced.
pub(super) fn open(ctx: &egui::Context, deck_uuid: &str) {
    super::channel_settings::open_layer(ctx, deck_uuid);
}

/// The preview of the input settings window: a row picking what it shows,
/// the picture, and — when a layer is being edited — its outline, the drag
/// and zoom gestures, and a reset. `deck` is that layer; without one the
/// channel is shown.
pub(super) fn render_preview(
    ui: &mut egui::Ui,
    ch_idx: usize,
    ch_name: &str,
    deck: Option<&DeckUIInfo>,
    max_height: f32,
    data: &UIData,
    actions: &mut UIActions,
) {
    let stored = ui
        .ctx()
        .memory(|m| m.data.get_temp::<View>(view_id()))
        .unwrap_or(View::Input);
    // The channel's own settings have no single layer to show.
    let view = match (stored, deck) {
        (View::Input, None) => View::Channel,
        (view, _) => view,
    };

    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Show:").small());
        let pick = |ui: &mut egui::Ui, v: View, label: &str, hover: &str| {
            if ui
                .selectable_label(view == v, egui::RichText::new(label).small())
                .on_hover_text(hover)
                .clicked()
            {
                ui.ctx().memory_mut(|m| m.data.insert_temp(view_id(), v));
            }
        };
        if deck.is_some() {
            pick(ui, View::Input, "This layer", "This layer on its own");
        }
        pick(
            ui,
            View::Channel,
            ch_name,
            "The channel, every layer in it composited",
        );
        pick(ui, View::Main, "Main output", "The main output");
        for (i, out) in data.outputs.iter().enumerate() {
            if out.is_windowed {
                pick(ui, View::Output(i), &out.name, "What this screen shows");
            }
        }
    });

    let (texture, width, height) = match view {
        View::Channel => (
            data.channel_preview_textures.get(&ch_idx).copied(),
            data.render_width,
            data.render_height,
        ),
        View::Main => (
            data.main_output_texture,
            data.render_width,
            data.render_height,
        ),
        View::Output(i) => (
            data.output_preview_textures.get(&i).copied(),
            data.outputs
                .get(i)
                .map_or(data.render_width, |o| o.preview_width),
            data.outputs
                .get(i)
                .map_or(data.render_height, |o| o.preview_height),
        ),
        View::Input => (
            deck.and_then(|d| data.deck_preview_textures.get(&d.uuid).copied()),
            data.render_width,
            data.render_height,
        ),
    };
    let size = super::utils::preview_size(
        egui::vec2(ui.available_width(), max_height.max(120.0)),
        width,
        height,
    );
    let response = if let Some(tex) = texture {
        ui.add(
            egui::Image::new(egui::load::SizedTexture::new(tex, size))
                .corner_radius(4.0)
                .sense(egui::Sense::click_and_drag()),
        )
    } else {
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click_and_drag());
        ui.painter()
            .rect_filled(rect, 4.0, egui::Color32::from_rgb(20, 20, 30));
        response
    };
    response
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Other, true, "Adjust preview"));

    // Drawing a mask: the picture is the frame the mask is in, so not a
    // screen's own picture; the drag draws it, and does not move the layer.
    if !matches!(view, View::Output(_))
        && super::mask_edit::handle_preview(ui, &response, data, actions)
    {
        ui.label(
            egui::RichText::new(
                "Drag on the picture to draw the mask · Done or Esc when it is right",
            )
            .small()
            .weak(),
        );
        return;
    }

    let Some(deck) = deck else {
        return;
    };
    let render = (data.render_width, data.render_height);
    let transform = Transform::of(deck, render);
    if let Some(t) = &transform {
        t.outline(ui, response.rect);
        // The channel's other layers, to line this one up with.
        let others: Vec<egui::Rect> = data
            .channels
            .get(ch_idx)
            .map(|ch| {
                ch.decks
                    .iter()
                    .filter(|d| d.uuid != deck.uuid && !d.mute)
                    .filter_map(|d| Transform::of(d, render))
                    .map(|o| o.visible(o.pos))
                    .collect()
            })
            .unwrap_or_default();
        let guides = t.handle_gestures(ui, &response, &others, actions);
        draw_guides(ui, response.rect, &guides);
    }

    ui.horizontal(|ui| {
        let hint = if transform.is_some() {
            "Drag the picture to move it · drag a corner, Shift-drag or scroll to resize"
        } else {
            "Switch on Transform under Position to move it here"
        };
        ui.label(egui::RichText::new(hint).small().weak());
        if let Some(t) = &transform {
            if ui
                .small_button("Reset position & size")
                .on_hover_text("Centred, full size, not rotated")
                .clicked()
            {
                for (name, value) in [
                    ("pos_x", 0.0),
                    ("pos_y", 0.0),
                    ("scale", 1.0),
                    ("rotation", 0.0),
                ] {
                    actions.commands.push(t.set(name, value));
                }
            }
        }
    });
}

/// The deck's transform effect: its UUID and where it has the picture.
struct Transform {
    effect_uuid: String,
    pos: egui::Vec2,
    scale: f32,
    /// The scale on each axis: `scale`, times X and Y when they are separate.
    stretch: egui::Vec2,
    rotation: f32,
    /// What the crop keeps of the picture, in its own units (0–1, +y down).
    kept: egui::Rect,
}

/// What a deck's crop keeps of its picture, in the picture's own units (0–1,
/// +y down) — all of it without a crop, or with it off or inverted. The crop
/// shader measures y upwards, so its top and bottom turn over here.
/// What shows of a deck's frame before Transform, in its own units (0–1,
/// +y down): the picture a scaling mode fits — without its transparent bars
/// — cut down to what the crop keeps.
fn visible_part(deck: &DeckUIInfo, render: (u32, u32)) -> egui::Rect {
    let kept = kept_by_crop(deck);
    match picture_rect(deck, render) {
        Some(picture) => kept.intersect(picture),
        None => kept,
    }
}

/// Where a fitted source's picture sits in the frame (0–1), where it leaves
/// transparent bars; `None` for sources that fill the frame.
fn picture_rect(deck: &DeckUIInfo, render: (u32, u32)) -> Option<egui::Rect> {
    let mode = deck.scaling_mode?;
    let [w, h] = deck.source_size?;
    if render.0 == 0 || render.1 == 0 {
        return None;
    }
    let (scale, offset) = mode.compute_uv_transform(w, h, render.0, render.1);
    // The picture is where source_uv = uv × scale + offset lands inside 0–1.
    let span = |i: usize| {
        let lo = (-offset[i] / scale[i]).clamp(0.0, 1.0);
        let hi = ((1.0 - offset[i]) / scale[i]).clamp(0.0, 1.0);
        (lo, hi)
    };
    let ((x0, x1), (y0, y1)) = (span(0), span(1));
    Some(egui::Rect::from_min_max(
        egui::pos2(x0, y0),
        egui::pos2(x1, y1),
    ))
}

/// `part` of a picture (0–1) as it shows flipped across and/or down: the
/// transform mirrors the picture about its own centre, the crop's kept part
/// with it.
fn mirrored(part: egui::Rect, across: bool, down: bool) -> egui::Rect {
    let (x0, x1) = if across {
        (1.0 - part.max.x, 1.0 - part.min.x)
    } else {
        (part.min.x, part.max.x)
    };
    let (y0, y1) = if down {
        (1.0 - part.max.y, 1.0 - part.min.y)
    } else {
        (part.min.y, part.max.y)
    };
    egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
}

fn kept_by_crop(deck: &DeckUIInfo) -> egui::Rect {
    let whole = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    let Some((_, _, true, params)) = deck.effects.iter().find(|(_, n, ..)| n == "crop") else {
        return whole;
    };
    let get = |name: &str, default: f32| {
        params
            .params
            .iter()
            .find(|p| p.name == name)
            .and_then(|p| match p.value {
                ParamValue::Float(v) => Some(v),
                _ => None,
            })
            .unwrap_or(default)
    };
    if get("invert_crop", 0.0) > 0.5 {
        return whole;
    }
    let (left, right) = (get("crop_left", 0.0), get("crop_right", 1.0));
    let (bottom, top) = (get("crop_bottom", 0.0), get("crop_top", 1.0));
    egui::Rect::from_min_max(
        egui::pos2(left.min(right), (1.0 - top).min(1.0 - bottom)),
        egui::pos2(left.max(right), (1.0 - top).max(1.0 - bottom)),
    )
}

/// Lines a moved layer snapped to, in frame units (0–1, +y down).
#[derive(Debug, Default, PartialEq)]
pub(super) struct Guides {
    pub x: Option<f32>,
    pub y: Option<f32>,
}

/// How near, in pixels of the preview, an edge or centre has to come to a
/// guide to snap to it.
const SNAP_PIXELS: f32 = 6.0;

/// Move `moving` onto the nearest guide within `threshold` on each axis, as
/// Apple's apps line things up: any of its left edge, centre and right edge
/// (top, middle, bottom) meets any target. Returns the move and the guides
/// it met.
pub(super) fn snap_move(
    moving: egui::Rect,
    targets_x: &[f32],
    targets_y: &[f32],
    threshold: egui::Vec2,
) -> (egui::Vec2, Guides) {
    let best = |edges: [f32; 3], targets: &[f32], limit: f32| -> Option<(f32, f32)> {
        edges
            .iter()
            .flat_map(|e| targets.iter().map(move |t| (t - e, *t)))
            .filter(|(d, _)| d.abs() <= limit)
            .min_by(|a, b| a.0.abs().total_cmp(&b.0.abs()))
    };
    let x = best(
        [moving.left(), moving.center().x, moving.right()],
        targets_x,
        threshold.x,
    );
    let y = best(
        [moving.top(), moving.center().y, moving.bottom()],
        targets_y,
        threshold.y,
    );
    (
        egui::vec2(x.map_or(0.0, |(d, _)| d), y.map_or(0.0, |(d, _)| d)),
        Guides {
            x: x.map(|(_, t)| t),
            y: y.map(|(_, t)| t),
        },
    )
}

/// The frame's edges and middle, and every other layer's edges and centre.
fn snap_targets(others: &[egui::Rect]) -> (Vec<f32>, Vec<f32>) {
    let mut xs = vec![0.0, 0.5, 1.0];
    let mut ys = vec![0.0, 0.5, 1.0];
    for r in others {
        xs.extend([r.left(), r.center().x, r.right()]);
        ys.extend([r.top(), r.center().y, r.bottom()]);
    }
    (xs, ys)
}

/// Snapped guides across the preview, in Apple's alignment yellow.
fn draw_guides(ui: &egui::Ui, rect: egui::Rect, guides: &Guides) {
    let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 204, 0));
    let painter = ui.painter_at(rect);
    if let Some(x) = guides.x {
        let x = rect.left() + x * rect.width();
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            stroke,
        );
    }
    if let Some(y) = guides.y {
        let y = rect.top() + y * rect.height();
        painter.line_segment(
            [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
            stroke,
        );
    }
}

impl Transform {
    /// The deck's first transform effect, if it has one switched on. `render`
    /// is the frame's size, which a fitted picture's bars depend on.
    fn of(deck: &DeckUIInfo, render: (u32, u32)) -> Option<Self> {
        let at = deck.effects.iter().position(|(_, n, ..)| n == "transform")?;
        let (uuid, _, enabled, params) = &deck.effects[at];
        if !enabled {
            return None;
        }
        let get = |name: &str, default: f32| {
            params
                .params
                .iter()
                .find(|p| p.name == name)
                .and_then(|p| match p.value {
                    ParamValue::Float(v) => Some(v),
                    _ => None,
                })
                .unwrap_or(default)
        };
        let scale = get("scale", 1.0);
        let switch = |name: &str| {
            params
                .params
                .iter()
                .any(|p| p.name == name && matches!(p.value, ParamValue::Bool(true)))
        };
        let separate = switch("scale_mode");
        let stretch = if separate {
            egui::vec2(scale * get("scale_x", 1.0), scale * get("scale_y", 1.0))
        } else {
            egui::vec2(scale, scale)
        };
        Some(Self {
            effect_uuid: uuid.clone(),
            pos: egui::vec2(get("pos_x", 0.0), get("pos_y", 0.0)),
            scale,
            stretch,
            rotation: get("rotation", 0.0),
            // Flipped, the part a crop before it keeps shows mirrored; a crop
            // after it cuts the frame, which the flip does not turn over.
            kept: {
                let part = visible_part(deck, render);
                let crop_first = deck
                    .effects
                    .iter()
                    .position(|(_, n, ..)| n == "crop")
                    .is_some_and(|crop| crop < at);
                if crop_first {
                    mirrored(part, switch("flip_x"), switch("flip_y"))
                } else {
                    part
                }
            },
        })
    }

    /// Where the picture's kept part sits in the frame (0–1, +y down) with
    /// the picture's centre moved to `pos`. Rotation is left out: guides line
    /// up upright edges.
    fn visible(&self, pos: egui::Vec2) -> egui::Rect {
        let centre = egui::vec2(0.5, 0.5) + pos;
        let min = centre + (self.kept.min.to_vec2() - egui::vec2(0.5, 0.5)) * self.stretch;
        let max = centre + (self.kept.max.to_vec2() - egui::vec2(0.5, 0.5)) * self.stretch;
        egui::Rect::from_min_max(min.to_pos2(), max.to_pos2())
    }

    fn set(&self, name: &str, value: f32) -> EngineCommand {
        EngineCommand::SetEffectParam {
            effect_uuid: self.effect_uuid.clone(),
            name: name.to_string(),
            value: ParamValue::Float(value),
        }
    }

    /// Drag moves the picture by as much as the pointer moves across the
    /// preview (position is in frame units, so one preview width is one
    /// unit); the scroll wheel or a pinch zooms it. A drag is one gesture,
    /// so it is one undo step.
    ///
    /// A moved picture snaps its edges and centre to the frame's edges and
    /// middle and to the other layers', and shows the guides it met; ⌘
    /// held drags it freely.
    fn handle_gestures(
        &self,
        ui: &egui::Ui,
        response: &egui::Response,
        others: &[egui::Rect],
        actions: &mut UIActions,
    ) -> Guides {
        let size = response.rect.size();
        if size.x <= 0.0 || size.y <= 0.0 {
            return Guides::default();
        }
        let mut guides = Guides::default();
        let rect = response.rect;
        let corners = self.corners(rect);
        let near = |at: egui::Pos2| {
            (0..4)
                .filter(|&i| corners[i].distance(at) <= CORNER_REACH)
                .min_by(|&a, &b| corners[a].distance(at).total_cmp(&corners[b].distance(at)))
        };
        // Over a corner, the pointer says it resizes — the corner being
        // dragged, while it is, whatever other corner it passes.
        let dragged_corner = response
            .dragged()
            .then(|| ui.ctx().memory(|m| m.data.get_temp::<usize>(drag_corner_id())))
            .flatten();
        let hovered_corner = response.hover_pos().and_then(near);
        if let Some(i) = dragged_corner.or(hovered_corner) {
            // Along the corner's diagonal as it lies on screen, turned or not.
            let diagonal = corners[i] - corners[(i + 2) % 4];
            ui.ctx().set_cursor_icon(if diagonal.x * diagonal.y >= 0.0 {
                egui::CursorIcon::ResizeNwSe
            } else {
                egui::CursorIcon::ResizeNeSw
            });
        }
        if response.drag_started() {
            let start = (self.pos, self.scale);
            let corner = ui.input(|i| i.pointer.press_origin()).and_then(near);
            ui.ctx().memory_mut(|m| {
                m.data.insert_temp(drag_start_id(), start);
                if let Some(i) = corner {
                    m.data.insert_temp(drag_corner_id(), i);
                } else {
                    m.data.remove::<usize>(drag_corner_id());
                }
            });
        }
        if response.dragged() {
            let (start, start_scale) = ui
                .ctx()
                .memory(|m| m.data.get_temp::<(egui::Vec2, f32)>(drag_start_id()))
                .unwrap_or((self.pos, self.scale));
            let corner = ui.ctx().memory(|m| m.data.get_temp::<usize>(drag_corner_id()));
            let (moved, shift, free) = ui.input(|i| {
                let moved = i
                    .pointer
                    .press_origin()
                    .zip(i.pointer.latest_pos())
                    .map_or(egui::Vec2::ZERO, |(from, to)| to - from);
                (moved, i.modifiers.shift, i.modifiers.command)
            });
            if let Some(i) = corner {
                // A corner drawn out or in: the picture grows or shrinks, its
                // opposite corner staying where it was.
                let begun = Self { pos: start, scale: start_scale, ..self.at_scale(start_scale) };
                let was = begun.corners(rect);
                let anchor = was[(i + 2) % 4];
                let diagonal = was[i] - anchor;
                let pulled = was[i] + moved - anchor;
                let ratio = pulled.dot(diagonal) / diagonal.length_sq().max(1e-6);
                // The centre moves with the size, from the anchor: at
                // `ratio`, position = `from + by × ratio`. How far it may
                // grow or shrink keeps that position within ±1 and the
                // scale within its range, so the anchor never moves.
                let centre = rect.center() + start * size;
                let from = (anchor - rect.center()) / size;
                let by = (centre - anchor) / size;
                let start_scale = start_scale.max(1e-6);
                let sizes = (0.01 / start_scale, 5.0 / start_scale);
                let (mut low, mut high) = sizes;
                for (f, b) in [(from.x, by.x), (from.y, by.y)] {
                    if b.abs() > 1e-6 {
                        let (a, z) = ((-1.0 - f) / b, (1.0 - f) / b);
                        low = low.max(a.min(z));
                        high = high.min(a.max(z));
                    }
                }
                // Placed past ±1 already (an old show): no size keeps the
                // anchor there, so only the scale's own range holds.
                if low > high {
                    (low, high) = sizes;
                }
                let mut scale = start_scale * ratio.clamp(low, high);
                if !free {
                    scale = begun.snap_size(scale, others, SNAP_PIXELS / size.x);
                }
                let ratio = (scale / start_scale).clamp(low, high);
                let scale = start_scale * ratio;
                let pos = from + by * ratio;
                actions.commands.push(self.set("scale", scale));
                actions.commands.push(self.set("pos_x", pos.x.clamp(-1.0, 1.0)));
                actions.commands.push(self.set("pos_y", pos.y.clamp(-1.0, 1.0)));
            } else if shift {
                // Shift-drag resizes, as in vMix: right or down grows it,
                // half the preview's width doubling it.
                let grow = (moved.x + moved.y) / size.x * 2.0;
                let mut scale = (start_scale * (1.0 + grow)).clamp(0.01, 5.0);
                if !free {
                    scale = self.snap_size(scale, others, SNAP_PIXELS / size.x);
                }
                actions.commands.push(self.set("scale", scale));
            } else {
                let mut to = start + egui::vec2(moved.x / size.x, moved.y / size.y);
                if !free {
                    let (xs, ys) = snap_targets(others);
                    let threshold = egui::vec2(SNAP_PIXELS / size.x, SNAP_PIXELS / size.y);
                    let (shift_by, met) = snap_move(self.visible(to), &xs, &ys, threshold);
                    to += shift_by;
                    guides = met;
                }
                actions
                    .commands
                    .push(self.set("pos_x", to.x.clamp(-1.0, 1.0)));
                actions
                    .commands
                    .push(self.set("pos_y", to.y.clamp(-1.0, 1.0)));
            }
            actions.session.gesture_active = true;
        }
        if response.drag_stopped() {
            ui.ctx().memory_mut(|m| {
                m.data.remove::<(egui::Vec2, f32)>(drag_start_id());
                m.data.remove::<usize>(drag_corner_id());
            });
        }
        if response.hovered() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            if scroll.abs() > 0.1 || (pinch - 1.0).abs() > 1e-3 {
                let factor = pinch * (scroll / 400.0).exp();
                let scale = (self.scale * factor).clamp(0.01, 5.0);
                actions.commands.push(self.set("scale", scale));
            }
        }
        guides
    }

    /// A resized picture snaps to full width, and to the width of any other
    /// layer: `scale` moved to the nearest within `limit` (in frame widths).
    fn snap_size(&self, scale: f32, others: &[egui::Rect], limit: f32) -> f32 {
        let per_scale = self.kept.width() * self.stretch.x / self.scale.max(1e-4);
        if per_scale <= 0.0 {
            return scale;
        }
        let width = per_scale * scale;
        std::iter::once(1.0)
            .chain(others.iter().map(egui::Rect::width))
            .filter(|w| (w - width).abs() <= limit)
            .min_by(|a, b| (a - width).abs().total_cmp(&(b - width).abs()))
            .map_or(scale, |w| w / per_scale)
    }

    /// The same picture at another `scale`: its stretch follows.
    fn at_scale(&self, scale: f32) -> Self {
        let per = self.stretch / self.scale.max(1e-6);
        Self {
            effect_uuid: self.effect_uuid.clone(),
            pos: self.pos,
            scale,
            stretch: per * scale,
            rotation: self.rotation,
            kept: self.kept,
        }
    }

    /// The corners of what shows of the picture in the preview `rect` —
    /// top left, top right, bottom right, bottom left — turned with it.
    fn corners(&self, rect: egui::Rect) -> [egui::Pos2; 4] {
        let centre =
            rect.center() + egui::vec2(self.pos.x * rect.width(), self.pos.y * rect.height());
        let (sin, cos) = self.rotation.sin_cos();
        let corner = |px: f32, py: f32| {
            let x = (px - 0.5) * self.stretch.x * rect.width();
            let y = (py - 0.5) * self.stretch.y * rect.height();
            centre + egui::vec2(x * cos - y * sin, x * sin + y * cos)
        };
        let k = self.kept;
        [
            corner(k.left(), k.top()),
            corner(k.right(), k.top()),
            corner(k.right(), k.bottom()),
            corner(k.left(), k.bottom()),
        ]
    }

    /// Outline what shows of the deck's picture in the preview — the part
    /// the crop keeps, turned with it — so a picture with transparent or
    /// black edges can still be found; its corners are handles to resize it.
    fn outline(&self, ui: &egui::Ui, rect: egui::Rect) {
        let corners = self.corners(rect);
        let stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 170, 255));
        let painter = ui.painter_at(rect);
        for i in 0..4 {
            painter.line_segment([corners[i], corners[(i + 1) % 4]], stroke);
        }
        for at in corners {
            let handle = egui::Rect::from_center_size(at, egui::vec2(9.0, 9.0));
            painter.rect_filled(handle, 1.0, egui::Color32::WHITE);
            painter.rect_stroke(handle, 1.0, stroke, egui::StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::kittest::{NodeT, Queryable};

    fn deck_with_transform(data: &mut UIData) -> String {
        use crate::usecases::ui::{ParamUIInfo, ShaderParamsUI};
        let deck = &mut data.channels[0].decks[0];
        let float = |name: &str, v: f32| ParamUIInfo {
            name: name.into(),
            label: None,
            value: ParamValue::Float(v),
            min: None,
            max: None,
        };
        deck.effects.push((
            "fxtrans1".into(),
            "transform".into(),
            true,
            ShaderParamsUI {
                shader_name: "transform".into(),
                params: vec![
                    float("pos_x", 0.1),
                    float("pos_y", 0.0),
                    float("scale", 0.5),
                    float("rotation", 0.0),
                ],
            },
        ));
        deck.uuid.clone()
    }

    /// Flipped, the outline and the snapping take the part the crop keeps
    /// where it shows — mirrored — not where it was before the flip.
    #[test]
    fn a_flipped_crop_is_outlined_where_it_shows() {
        use crate::usecases::ui::{ParamUIInfo, ShaderParamsUI};
        let setting = |name: &str, value: ParamValue| ParamUIInfo {
            name: name.into(),
            label: None,
            value,
            min: None,
            max: None,
        };
        let mut data = UIData::test_fixture();
        let uuid = deck_with_transform(&mut data);
        let deck = data.channels[0]
            .decks
            .iter_mut()
            .find(|d| d.uuid == uuid)
            .expect("the deck");
        deck.effects.insert(
            0,
            (
                "fxcrop01".into(),
                "crop".into(),
                true,
                ShaderParamsUI {
                    shader_name: "crop".into(),
                    params: vec![setting("crop_left", ParamValue::Float(0.5))],
                },
            ),
        );
        let kept = |deck: &DeckUIInfo| Transform::of(deck, (1920, 1080)).expect("transform").kept;
        let before = kept(deck);
        assert!((before.min.x - 0.5).abs() < 1e-6, "{before:?}");
        let transform = deck
            .effects
            .iter_mut()
            .find(|e| e.1 == "transform")
            .expect("transform");
        transform.3.params.push(setting("flip_x", ParamValue::Bool(true)));
        let after = kept(deck);
        assert!(
            after.min.x.abs() < 1e-6 && (after.max.x - 0.5).abs() < 1e-6,
            "the kept half shows on the left: {after:?}"
        );
        // A crop moved after Transform cuts the frame: the flip leaves it be.
        let crop = deck.effects.remove(0);
        deck.effects.push(crop);
        let reordered = kept(deck);
        assert!(
            (reordered.min.x - 0.5).abs() < 1e-6,
            "a crop after the transform is not mirrored: {reordered:?}"
        );
    }

    fn harness_for(data: UIData, deck: &str) -> egui_kittest::Harness<'static, UIActions> {
        let data = std::rc::Rc::new(data);
        let deck = deck.to_string();
        let mut harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 900.0))
            .build_ui_state(
                move |ui, actions: &mut UIActions| {
                    *actions = UIActions::new();
                    let layer = data.channels[0].decks.iter().find(|d| d.uuid == deck);
                    render_preview(ui, 0, "Ch A", layer, 500.0, &data, actions);
                },
                UIActions::new(),
            );
        harness.run();
        harness
    }

    fn sets(actions: &UIActions, name: &str) -> Option<f32> {
        actions.commands.iter().rev().find_map(|c| match c {
            EngineCommand::SetEffectParam {
                effect_uuid,
                name: n,
                value: ParamValue::Float(v),
            } if effect_uuid == "fxtrans1" && n == name => Some(*v),
            _ => None,
        })
    }

    #[test]
    fn dragging_on_the_preview_moves_the_deck() {
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let mut harness = harness_for(data, &deck);
        let preview = harness.get_by_label("Adjust preview").rect();
        let start = preview.center();
        // A quarter of the preview's width to the right.
        let end = start + egui::vec2(preview.width() * 0.25, 0.0);
        harness.hover_at(start);
        harness.drag_at(start);
        harness.run();
        for t in [0.3_f32, 0.6, 1.0] {
            harness.hover_at(start + (end - start) * t);
            harness.run();
        }
        let x = sets(harness.state(), "pos_x").expect("the drag moves the deck");
        assert!((x - 0.35).abs() < 0.02, "0.1 + a quarter of the frame: {x}");
        assert!(
            harness.state().session.gesture_active,
            "one undo step for the drag"
        );
    }

    /// Shift-drag resizes, as in vMix, and leaves the position alone.
    #[test]
    fn shift_dragging_on_the_preview_resizes_the_layer() {
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let mut harness = harness_for(data, &deck);
        let preview = harness.get_by_label("Adjust preview").rect();
        let start = preview.center();
        let end = start + egui::vec2(preview.width() * 0.25, 0.0);
        harness.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        harness.hover_at(start);
        harness.drag_at(start);
        harness.run();
        for t in [0.3_f32, 0.6, 1.0] {
            harness.hover_at(start + (end - start) * t);
            harness.run();
        }
        let scale = sets(harness.state(), "scale").expect("the drag resizes");
        assert!(
            (scale - 0.75).abs() < 0.02,
            "a quarter of the width grows 0.5 by half: {scale}"
        );
        assert_eq!(sets(harness.state(), "pos_x"), None, "not moved");
    }

    /// A corner of the outline dragged out grows the picture and dragged in
    /// shrinks it, the opposite corner staying where it was; over a corner
    /// the pointer says so.
    #[test]
    fn dragging_a_corner_resizes_the_layer_about_the_opposite_corner() {
        // At 0.1 across, half size: its corners a quarter of the preview
        // from its centre.
        let drag = |by: egui::Vec2| {
            let mut data = UIData::test_fixture();
            let deck = deck_with_transform(&mut data);
            let mut harness = harness_for(data, &deck);
            let preview = harness.get_by_label("Adjust preview").rect();
            let size = preview.size();
            let corner = preview.center() + egui::vec2(0.1 * size.x, 0.0) + size * 0.25;
            harness.hover_at(corner);
            harness.run();
            let cursor = harness.output().platform_output.cursor_icon;
            harness.drag_at(corner);
            harness.run();
            for t in [0.3_f32, 0.6, 1.0] {
                harness.hover_at(corner + size * by * t);
                harness.run();
            }
            let scale = sets(harness.state(), "scale").expect("the drag resizes");
            let pos = egui::vec2(
                sets(harness.state(), "pos_x").expect("placed"),
                sets(harness.state(), "pos_y").expect("placed"),
            );
            (cursor, scale, pos)
        };
        // The top left corner, in frame units, before: 0.5 + 0.1 − 0.25.
        let top_left = |scale: f32, pos: egui::Vec2| {
            egui::vec2(0.5, 0.5) + pos - egui::vec2(scale, scale) / 2.0
        };
        let (cursor, bigger, pos) = drag(egui::vec2(0.125, 0.125));
        assert_eq!(cursor, egui::CursorIcon::ResizeNwSe, "over the corner");
        assert!((bigger - 0.625).abs() < 0.01, "a quarter more: {bigger}");
        let kept = top_left(bigger, pos);
        assert!((kept - egui::vec2(0.35, 0.25)).length() < 0.01, "stays: {kept:?}");

        let (_, smaller, pos) = drag(egui::vec2(-0.1, -0.1));
        assert!((smaller - 0.4).abs() < 0.01, "a fifth less: {smaller}");
        let kept = top_left(smaller, pos);
        assert!((kept - egui::vec2(0.35, 0.25)).length() < 0.01, "stays: {kept:?}");
    }

    /// Pulled far out, a corner grows the picture only as far as its centre
    /// can go (±1): the opposite corner stays put. Turned a quarter, the
    /// pointer's arrow turns with the corner.
    #[test]
    fn a_corner_drag_stops_where_the_anchor_would_move_and_turns_its_arrow() {
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let mut harness = harness_for(data, &deck);
        let preview = harness.get_by_label("Adjust preview").rect();
        let size = preview.size();
        let corner = preview.center() + egui::vec2(0.1 * size.x, 0.0) + size * 0.25;
        harness.hover_at(corner);
        harness.drag_at(corner);
        harness.run();
        for t in [0.3_f32, 0.6, 1.0] {
            harness.hover_at(corner + egui::vec2(2500.0, 2500.0) * t);
            harness.run();
        }
        let scale = sets(harness.state(), "scale").expect("resized");
        let pos = egui::vec2(
            sets(harness.state(), "pos_x").expect("placed"),
            sets(harness.state(), "pos_y").expect("placed"),
        );
        assert!((pos.x - 1.0).abs() < 1e-3, "as far as its centre can go: {pos:?}");
        let top_left = egui::vec2(0.5, 0.5) + pos - egui::vec2(scale, scale) / 2.0;
        assert!((top_left - egui::vec2(0.35, 0.25)).length() < 0.01, "stays: {top_left:?} at {scale}");

        // Turned a quarter: the bottom right corner lies down and to the left.
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let fx = &mut data.channels[0].decks[0].effects.last_mut().unwrap().3;
        for p in &mut fx.params {
            if p.name == "rotation" {
                p.value = ParamValue::Float(std::f32::consts::FRAC_PI_2);
            }
        }
        let mut harness = harness_for(data, &deck);
        let centre = preview.center() + egui::vec2(0.1 * size.x, 0.0);
        let turned = centre + egui::vec2(-0.25 * size.y, 0.25 * size.x);
        harness.hover_at(turned);
        harness.run();
        assert_eq!(harness.output().platform_output.cursor_icon, egui::CursorIcon::ResizeNeSw);
    }

    /// A layer placed past ±1 already, at the largest size, is never made
    /// larger by a corner; and while a corner is dragged, its arrow stays its
    /// own as it passes another corner.
    #[test]
    fn a_corner_drag_keeps_the_size_range_and_its_own_arrow() {
        let set = |data: &mut UIData, values: &[(&str, f32)]| {
            let fx = &mut data.channels[0].decks[0].effects.last_mut().unwrap().3;
            for (name, v) in values {
                fx.params.iter_mut().find(|p| p.name == *name).unwrap().value =
                    ParamValue::Float(*v);
            }
        };
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        set(&mut data, &[("pos_x", 2.9), ("pos_y", 2.9), ("scale", 5.0)]);
        let mut harness = harness_for(data, &deck);
        let preview = harness.get_by_label("Adjust preview").rect();
        let size = preview.size();
        // Its top left corner, at 0.9 of the frame across and down.
        let corner = preview.min + size * 0.9;
        harness.hover_at(corner);
        harness.drag_at(corner);
        harness.run();
        for t in [0.5_f32, 1.0] {
            harness.hover_at(corner - egui::vec2(40.0, 40.0) * t);
            harness.run();
        }
        let scale = sets(harness.state(), "scale").expect("resized");
        assert!(scale <= 5.0 + 1e-4, "{scale}");

        // Half size at 0.1 across: its bottom right corner dragged over to
        // its bottom left one — the outline here is the one it began with.
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let mut harness = harness_for(data, &deck);
        let centre = preview.center() + egui::vec2(0.1 * size.x, 0.0);
        let bottom_right = centre + size * 0.25;
        let bottom_left = centre + egui::vec2(-0.25 * size.x, 0.25 * size.y);
        harness.hover_at(bottom_right);
        harness.drag_at(bottom_right);
        harness.run();
        for to in [centre, bottom_left + egui::vec2(2.0, 0.0)] {
            harness.hover_at(to);
            harness.run();
        }
        assert_eq!(harness.output().platform_output.cursor_icon, egui::CursorIcon::ResizeNwSe);
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1))
    }

    /// A 9:16 clip fitted into a 16:9 frame: what shows — and what the
    /// outline and the guides go by — is the tall picture in the middle, not
    /// the frame with its transparent bars.
    #[test]
    fn a_fitted_tall_picture_shows_only_its_middle() {
        let mut deck = UIData::test_fixture().channels[0].decks[0].clone();
        deck.scaling_mode = Some(crate::ScalingMode::Fit);
        deck.source_size = Some([1080, 1920]);
        let shown = visible_part(&deck, (1920, 1080));
        let width = 1080.0 * (1080.0 / 1920.0) / 1920.0;
        assert!((shown.width() - width).abs() < 1e-3, "{shown:?}");
        assert!((shown.center().x - 0.5).abs() < 1e-3);
        assert!((shown.height() - 1.0).abs() < 1e-3);
        deck.scaling_mode = Some(crate::ScalingMode::Fill);
        assert_eq!(visible_part(&deck, (1920, 1080)).width(), 1.0, "filled");
    }

    #[test]
    fn a_centre_near_the_middle_snaps_to_it() {
        let (xs, ys) = snap_targets(&[]);
        let (moved, guides) =
            snap_move(rect(0.26, 0.2, 0.76, 0.7), &xs, &ys, egui::vec2(0.02, 0.02));
        assert!((moved.x + 0.01).abs() < 1e-6, "{moved:?}");
        assert!(
            (moved.y - 0.05).abs() > 0.02,
            "y is too far to snap: {moved:?}"
        );
        assert_eq!(guides.x, Some(0.5));
        assert_eq!(guides.y, None);
    }

    #[test]
    fn an_edge_lines_up_with_another_layers_edge() {
        let other = rect(0.1, 0.1, 0.4, 0.4);
        let (xs, ys) = snap_targets(&[other]);
        // Its left edge 0.405 is 0.005 from the other's right edge.
        let (moved, guides) = snap_move(
            rect(0.405, 0.6, 0.605, 0.8),
            &xs,
            &ys,
            egui::vec2(0.01, 0.01),
        );
        assert!((moved.x + 0.005).abs() < 1e-6, "{moved:?}");
        assert_eq!(guides.x, Some(0.4));
    }

    #[test]
    fn nothing_near_means_no_snap() {
        let (xs, ys) = snap_targets(&[]);
        let (moved, guides) = snap_move(rect(0.1, 0.1, 0.3, 0.3), &xs, &ys, egui::vec2(0.01, 0.01));
        assert_eq!(moved, egui::Vec2::ZERO);
        assert_eq!(guides, Guides::default());
    }

    /// Drag a layer at 0.1 to just beside the middle: it lands on the middle,
    /// unless ⌘ is held.
    fn drag_near_the_middle(command: bool) -> f32 {
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let mut harness = harness_for(data, &deck);
        let preview = harness.get_by_label("Adjust preview").rect();
        let start = preview.center();
        // Back by 0.1 of the frame, and 3 pixels more.
        let end = start + egui::vec2(-0.1 * preview.width() + 3.0, 0.0);
        if command {
            harness.event(egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
        }
        harness.hover_at(start);
        harness.drag_at(start);
        harness.run();
        for t in [0.5_f32, 1.0] {
            harness.hover_at(start + (end - start) * t);
            harness.run();
        }
        sets(harness.state(), "pos_x").expect("moved")
    }

    #[test]
    fn a_dragged_layer_snaps_to_the_middle() {
        assert!(
            drag_near_the_middle(false).abs() < 1e-6,
            "snapped to the middle"
        );
    }

    #[test]
    fn holding_command_drags_freely() {
        assert!(drag_near_the_middle(true) > 1e-3, "left where it was put");
    }

    #[test]
    fn a_resized_layer_snaps_to_full_size() {
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let mut harness = harness_for(data, &deck);
        let preview = harness.get_by_label("Adjust preview").rect();
        let start = preview.center();
        // From 0.5 to just under 1.0: half the preview's width doubles it;
        // 2 px short of that.
        let end = start + egui::vec2(preview.width() * 0.5 - 2.0, 0.0);
        harness.event(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
        harness.hover_at(start);
        harness.drag_at(start);
        harness.run();
        harness.hover_at(end);
        harness.run();
        let scale = sets(harness.state(), "scale").expect("resized");
        assert!((scale - 1.0).abs() < 1e-6, "snapped to full size: {scale}");
    }

    #[test]
    fn the_preview_shows_the_layer_by_default() {
        let mut data = UIData::test_fixture();
        let deck = deck_with_transform(&mut data);
        let harness = harness_for(data, &deck);
        let own = harness.get_by_label("This layer");
        assert_eq!(
            own.accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True),
            "the layer on its own; the channel result is beside it"
        );
    }

    /// The channel's own settings have no layer to show alone.
    #[test]
    fn without_a_layer_the_preview_shows_the_channel() {
        let data = std::rc::Rc::new(UIData::test_fixture());
        let harness = egui_kittest::Harness::builder()
            .with_size(egui::vec2(1280.0, 900.0))
            .build_ui(move |ui| {
                let mut actions = UIActions::new();
                render_preview(ui, 0, "Ch A", None, 500.0, &data, &mut actions);
            });
        assert!(harness.query_by_label("This layer").is_none());
        assert_eq!(
            harness.get_by_label("Ch A").accesskit_node().toggled(),
            Some(egui::accesskit::Toggled::True)
        );
    }
}
